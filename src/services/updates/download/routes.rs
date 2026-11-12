use std::{
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError},
    },
    time::{Duration, Instant},
};

use super::{
    DownloadProgress, MAX_PACKAGE_BYTES, Release, ReleaseAsset, UpdateConfig, check_cancelled,
    client, local_test_url, validate_asset_url,
};

const PROBE_BYTES: usize = 384 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(8);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const STALL_TIMEOUT: Duration = Duration::from_secs(15);
const SLOW_WINDOW: Duration = Duration::from_secs(20);
const MIN_BYTES_PER_SECOND: f64 = 16.0 * 1024.0;

enum Message {
    Started(ReleaseAsset, u64),
    Chunk(Vec<u8>),
    Finished,
    Failed(String),
}

pub(super) struct Source {
    pub(super) label: &'static str,
    pub(super) asset: Option<ReleaseAsset>,
    pub(super) total: u64,
    receiver: Receiver<Message>,
    abort: Arc<AtomicBool>,
    pending: Vec<u8>,
    finished: bool,
    last_data: Instant,
    window_started: Instant,
    window_bytes: u64,
}

impl Drop for Source {
    fn drop(&mut self) {
        self.abort.store(true, Ordering::Relaxed);
    }
}

impl Source {
    fn spawn(
        label: &'static str,
        name: String,
        config: UpdateConfig,
        resolve: impl FnOnce() -> Result<Release, String> + Send + 'static,
    ) -> Result<Self, String> {
        // 备用线路仅保留少量缓冲，避免未选中的连接持续占用内存和带宽。
        let (sender, receiver) = mpsc::sync_channel(2);
        let abort = Arc::new(AtomicBool::new(false));
        let stopped = abort.clone();
        std::thread::Builder::new()
            .name("qcclip-update-fetch".into())
            .spawn(move || {
                let result =
                    resolve().and_then(|release| fetch(release, &name, &config, &stopped, &sender));
                if let Err(error) = result
                    && !stopped.load(Ordering::Relaxed)
                {
                    stopped.store(true, Ordering::Relaxed);
                    let _ = sender.send(Message::Failed(error));
                }
            })
            .map_err(|e| format!("创建下载线程失败：{e}"))?;
        Ok(Self {
            label,
            asset: None,
            total: 0,
            receiver,
            abort,
            pending: Vec::new(),
            finished: false,
            last_data: Instant::now(),
            window_started: Instant::now(),
            window_bytes: 0,
        })
    }

    pub(super) fn next_chunk(
        &mut self,
        cancelled: &AtomicBool,
        has_alternative: impl Fn() -> bool,
    ) -> Result<Option<Vec<u8>>, String> {
        loop {
            check_cancelled(cancelled)?;
            if !self.pending.is_empty() {
                let data = std::mem::take(&mut self.pending);
                self.window_bytes += data.len() as u64;
                self.last_data = Instant::now();
                return Ok(Some(data));
            }
            if self.finished {
                return Ok(None);
            }
            if self.last_data.elapsed() >= STALL_TIMEOUT {
                return Err("下载连接长时间无数据".into());
            }
            let elapsed = self.window_started.elapsed();
            if elapsed >= SLOW_WINDOW {
                if has_alternative()
                    && self.window_bytes as f64 / elapsed.as_secs_f64() < MIN_BYTES_PER_SECOND
                {
                    return Err("下载速度过慢，切换下载线路".into());
                }
                self.window_started = Instant::now();
                self.window_bytes = 0;
            }
            match self.receiver.recv_timeout(POLL_INTERVAL) {
                Ok(Message::Chunk(data)) => {
                    self.window_bytes += data.len() as u64;
                    self.last_data = Instant::now();
                    return Ok(Some(data));
                }
                Ok(Message::Finished) => self.finished = true,
                Ok(Message::Failed(error)) => return Err(error),
                Ok(Message::Started(..)) => return Err("下载来源重复返回响应".into()),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err("下载线程意外结束".into()),
            }
        }
    }
}

pub(super) struct Routes {
    sources: Vec<Source>,
    errors: Vec<String>,
}

impl Routes {
    pub(super) fn new(
        release: Release,
        name: String,
        config: UpdateConfig,
        mirror_lookup: impl FnOnce(&str, &UpdateConfig) -> Result<Release, String> + Send + 'static,
    ) -> Result<Self, String> {
        let primary = release.clone();
        let first = Source::spawn(
            release.source.label(),
            name.clone(),
            config.clone(),
            move || Ok(primary),
        )?;
        let mirror_source = release.source.other();
        let mirror_name = name.clone();
        let mirror_config = config.clone();
        let second = Source::spawn(mirror_source.label(), name, config, move || {
            let mut mirror = mirror_lookup(&release.tag_name, &mirror_config)?;
            if mirror.draft || mirror.tag_name != release.tag_name {
                return Err("镜像版本不一致或尚未发布".into());
            }
            mirror.source = mirror_source;
            // 测速胜出的镜像也必须满足原发布的摘要和大小要求。
            if let Some(original) = release.assets.iter().find(|a| a.name == mirror_name)
                && let Some(asset) = mirror.assets.iter_mut().find(|a| a.name == mirror_name)
            {
                if original.digest.is_some() {
                    asset.digest = original.digest.clone();
                }
                if original.size != 0 {
                    asset.size = original.size;
                }
            }
            Ok(mirror)
        })?;
        Ok(Self {
            sources: vec![first, second],
            errors: Vec::new(),
        })
    }

    pub(super) fn has_alternative(&self) -> bool {
        self.sources
            .iter()
            .any(|source| !source.abort.load(Ordering::Relaxed))
    }

    pub(super) fn record_failure(&mut self, label: &str, error: String) {
        tracing::warn!("{label} 更新包下载失败：{error}");
        self.errors.push(format!("{label}：{error}"));
    }

    pub(super) fn select(
        &mut self,
        cancelled: &AtomicBool,
        progress: &mut impl FnMut(DownloadProgress),
    ) -> Result<Source, String> {
        let started = Instant::now();
        let mut last_emit = started - Duration::from_secs(1);
        loop {
            check_cancelled(cancelled)?;
            if self.sources.is_empty() {
                return Err(format!("所有下载线路均失败：{}", self.errors.join("；")));
            }
            let mut active = false;
            let mut index = 0;
            while index < self.sources.len() {
                let source = &mut self.sources[index];
                let error = match source.receiver.try_recv() {
                    Ok(Message::Started(asset, total)) => {
                        source.asset = Some(asset);
                        source.total = total;
                        None
                    }
                    Ok(Message::Chunk(data)) => {
                        source.pending.extend_from_slice(&data);
                        None
                    }
                    Ok(Message::Finished) => {
                        source.finished = true;
                        None
                    }
                    Ok(Message::Failed(error)) => Some(error),
                    Err(TryRecvError::Disconnected) => Some("下载线程意外结束".into()),
                    Err(TryRecvError::Empty) => {
                        index += 1;
                        continue;
                    }
                };
                active = true;
                if let Some(error) = error {
                    let source = self.sources.remove(index);
                    self.record_failure(source.label, error);
                    continue;
                }
                index += 1;
            }
            let winner = self
                .sources
                .iter()
                .position(|s| s.asset.is_some() && (s.finished || s.pending.len() >= PROBE_BYTES))
                .or_else(|| {
                    let ready = self
                        .sources
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| s.asset.is_some());
                    if self.sources.len() == 1 || started.elapsed() >= PROBE_TIMEOUT {
                        ready
                            .max_by_key(|(_, s)| s.pending.len())
                            .map(|(index, _)| index)
                    } else {
                        None
                    }
                });
            if let Some(index) = winner {
                let mut source = self.sources.remove(index);
                source.last_data = Instant::now();
                source.window_started = Instant::now();
                tracing::info!("已选择 {} 更新下载线路", source.label);
                return Ok(source);
            }
            if last_emit.elapsed() >= Duration::from_millis(200) {
                progress(DownloadProgress {
                    source: self.sources.first().map_or("GitHub / Gitee", |s| s.label),
                    probing: true,
                    downloaded: 0,
                    total: 0,
                });
                last_emit = Instant::now();
            }
            if !active {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

fn fetch(
    release: Release,
    name: &str,
    config: &UpdateConfig,
    abort: &AtomicBool,
    sender: &SyncSender<Message>,
) -> Result<(), String> {
    check_cancelled(abort)?;
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == name)
        .ok_or("此版本尚未提供匹配当前架构和安装方式的更新包")?;
    let local_test = local_test_url(&asset.browser_download_url);
    validate_asset_url(asset, &release, local_test)?;
    let response = client::http_agent(config, local_test)?
        .get(&asset.browser_download_url)
        .timeout(Duration::from_secs(3600))
        .set("User-Agent", concat!("QcClip/", env!("CARGO_PKG_VERSION")))
        .call()
        .map_err(|e| format!("连接下载地址失败：{e}"))?;
    let length = response
        .header("Content-Length")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    if length > MAX_PACKAGE_BYTES || asset.size > MAX_PACKAGE_BYTES {
        return Err("更新包超过大小限制".into());
    }
    if length != 0 && asset.size != 0 && length != asset.size {
        return Err("更新包大小与发布信息不一致".into());
    }
    let total = if length != 0 { length } else { asset.size };
    let send = |message| sender.send(message).map_err(|_| "下载已结束".to_owned());
    send(Message::Started(asset.clone(), total))?;
    let mut reader = response.into_reader();
    let mut buffer = [0; 64 * 1024];
    let mut downloaded = 0;
    loop {
        check_cancelled(abort)?;
        let count = reader
            .read(&mut buffer)
            .map_err(|e| format!("读取更新包失败：{e}"))?;
        if count == 0 {
            return send(Message::Finished);
        }
        downloaded += count as u64;
        if downloaded > MAX_PACKAGE_BYTES || (total != 0 && downloaded > total) {
            return Err("更新包大小异常".into());
        }
        check_cancelled(abort)?;
        send(Message::Chunk(buffer[..count].to_vec()))?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switches_stalled_or_slow_streams_without_rejecting_the_only_slow_source() {
        for (stalled, has_alternative) in [(true, true), (false, true), (false, false)] {
            let (sender, receiver) = mpsc::sync_channel(1);
            sender.send(Message::Chunk(vec![1])).unwrap();
            let mut source = Source {
                label: "GitHub",
                asset: None,
                total: 0,
                receiver,
                abort: Arc::new(AtomicBool::new(false)),
                pending: Vec::new(),
                finished: false,
                last_data: if stalled {
                    Instant::now() - STALL_TIMEOUT
                } else {
                    Instant::now()
                },
                window_started: Instant::now() - SLOW_WINDOW,
                window_bytes: 0,
            };
            let result = source.next_chunk(&AtomicBool::new(false), || has_alternative);
            if stalled || has_alternative {
                assert!(
                    result
                        .unwrap_err()
                        .contains(if stalled { "无数据" } else { "过慢" })
                );
            } else {
                assert_eq!(result.unwrap(), Some(vec![1]));
            }
        }
    }
}
