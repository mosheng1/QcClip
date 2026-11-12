#[path = "download/routes.rs"]
mod routes;

use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use atomic_write_file::AtomicWriteFile;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::client::{self, Release, ReleaseAsset, ReleaseSource};
use crate::config::UpdateConfig;

const MAX_PACKAGE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct DownloadProgress {
    pub source: &'static str,
    pub probing: bool,
    pub downloaded: u64,
    pub total: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct CachedUpdate {
    pub tag: String,
    pub file_name: String,
    pub sha256: String,
    pub portable: bool,
}

impl CachedUpdate {
    pub(crate) fn path(&self, directory: &Path) -> PathBuf {
        directory.join(&self.file_name)
    }

    pub(crate) fn verify(&self, directory: &Path) -> Result<(), String> {
        if self.file_name != package_name(&self.tag, self.portable)? {
            return Err("更新缓存文件名无效，请重新下载".into());
        }
        let path = self.path(directory);
        if hash_file(&path)? != self.sha256 {
            return Err("更新包校验失败，请重新下载".into());
        }
        validate_package(&path, self.portable)
    }
}

pub(super) fn package_name(tag: &str, portable: bool) -> Result<String, String> {
    if !cfg!(target_os = "windows") {
        return Err("当前平台暂不支持自动安装，请通过发布页更新".into());
    }
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        _ => return Err("当前架构暂不支持自动更新".into()),
    };
    let version = tag.trim().trim_start_matches(['v', 'V']);
    if version.is_empty()
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-+".contains(&byte))
    {
        return Err("更新版本号无效".into());
    }
    let suffix = if portable {
        "portable.zip"
    } else {
        "setup.exe"
    };
    Ok(format!("QcClip_{version}_{arch}-{suffix}"))
}

#[derive(Clone, Copy)]
struct DownloadOptions<'a> {
    config: &'a UpdateConfig,
    directory: &'a Path,
    portable: bool,
    cancelled: &'a AtomicBool,
}

pub(super) fn download(
    release: Release,
    config: &UpdateConfig,
    directory: &Path,
    portable: bool,
    cancelled: &AtomicBool,
    progress: impl FnMut(DownloadProgress),
) -> Result<CachedUpdate, String> {
    let mirror_source = release.source.other();
    let local = release
        .assets
        .iter()
        .any(|asset| local_test_url(&asset.browser_download_url));
    download_with_mirror(
        release,
        DownloadOptions {
            config,
            directory,
            portable,
            cancelled,
        },
        progress,
        move |tag, config| {
            if (local || cfg!(test)) && client::api_url() == client::RELEASES_API {
                return Err("未配置本地镜像测试地址".into());
            }
            client::fetch_mirror(tag, config, mirror_source)
        },
    )
}

fn download_with_mirror(
    release: Release,
    options: DownloadOptions<'_>,
    mut progress: impl FnMut(DownloadProgress),
    mirror_lookup: impl FnOnce(&str, &UpdateConfig) -> Result<Release, String> + Send + 'static,
) -> Result<CachedUpdate, String> {
    let DownloadOptions {
        config,
        directory,
        portable,
        cancelled,
    } = options;
    let name = package_name(&release.tag_name, portable)?;
    fs::create_dir_all(directory).map_err(|e| format!("创建更新缓存目录失败：{e}"))?;
    if let Ok(bytes) = fs::read(directory.join("ready.json"))
        && let Ok(cached) = serde_json::from_slice::<CachedUpdate>(&bytes)
        && cached.tag == release.tag_name
        && cached.portable == portable
        && cached.verify(directory).is_ok()
        && release
            .assets
            .iter()
            .find(|asset| asset.name == name)
            .is_none_or(|asset| {
                asset
                    .digest
                    .as_deref()
                    .is_none_or(|digest| verify_digest(digest, &cached.sha256).is_ok())
                    && (asset.size == 0
                        || fs::metadata(cached.path(directory))
                            .is_ok_and(|m| m.len() == asset.size))
            })
    {
        return Ok(cached);
    }
    check_cancelled(cancelled)?;
    let mut routes =
        routes::Routes::new(release.clone(), name.clone(), config.clone(), mirror_lookup)?;
    let cached = loop {
        let mut source = routes.select(cancelled, &mut progress)?;
        match download_source(
            &release,
            &name,
            &mut source,
            options,
            &routes,
            &mut progress,
        ) {
            Ok(cached) => break cached,
            Err(error) => {
                check_cancelled(cancelled)?;
                routes.record_failure(source.label, error);
            }
        }
    };
    drop(routes);
    check_cancelled(cancelled)?;
    let mut record = AtomicWriteFile::open(directory.join("ready.json"))
        .map_err(|e| format!("保存更新缓存失败：{e}"))?;
    record
        .write_all(
            &serde_json::to_vec_pretty(&cached).map_err(|e| format!("编码更新缓存失败：{e}"))?,
        )
        .map_err(|e| format!("保存更新缓存失败：{e}"))?;
    record
        .commit()
        .map_err(|e| format!("保存更新缓存失败：{e}"))?;
    if let Ok(entries) = fs::read_dir(directory) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name != cached.file_name
                && name.starts_with("QcClip_")
                && (name.ends_with("-setup.exe") || name.ends_with("-portable.zip"))
                && entry.file_type().is_ok_and(|kind| kind.is_file())
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    Ok(cached)
}

fn download_source(
    release: &Release,
    name: &str,
    stream: &mut routes::Source,
    options: DownloadOptions<'_>,
    routes: &routes::Routes,
    progress: &mut impl FnMut(DownloadProgress),
) -> Result<CachedUpdate, String> {
    let DownloadOptions {
        directory,
        portable,
        cancelled,
        ..
    } = options;
    check_cancelled(cancelled)?;
    let asset = stream.asset.clone().ok_or("下载来源未返回更新包信息")?;
    let source = stream.label;
    let total = stream.total;
    progress(DownloadProgress {
        source,
        probing: false,
        downloaded: 0,
        total,
    });
    let mut file = atomic_write_file::OpenOptions::new()
        .read(true)
        .open(directory.join(name))
        .map_err(|e| format!("创建下载文件失败：{e}"))?;
    let mut hasher = Sha256::new();
    let mut downloaded = 0;
    let mut last_progress = Instant::now();
    let started = Instant::now();
    loop {
        check_cancelled(cancelled)?;
        if started.elapsed() > Duration::from_secs(3600) {
            return Err("下载超过一小时，请重试".into());
        }
        let Some(buffer) = stream.next_chunk(cancelled, || routes.has_alternative())? else {
            break;
        };
        downloaded += buffer.len() as u64;
        if downloaded > MAX_PACKAGE_BYTES || (total != 0 && downloaded > total) {
            return Err("更新包大小异常".into());
        }
        file.write_all(&buffer)
            .map_err(|e| format!("写入更新包失败：{e}"))?;
        hasher.update(&buffer);
        if last_progress.elapsed() >= Duration::from_millis(200) {
            progress(DownloadProgress {
                source,
                probing: false,
                downloaded,
                total,
            });
            last_progress = Instant::now();
        }
    }
    if downloaded == 0 || (total != 0 && downloaded != total) {
        return Err("更新包下载不完整".into());
    }
    let sha256 = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if let Some(expected) = asset.digest.as_deref() {
        verify_digest(expected, &sha256)?;
    }
    file.as_file()
        .sync_all()
        .map_err(|e| format!("保存更新包失败：{e}"))?;
    // 在原子提交前验证格式，失败的下载不会成为可安装缓存。
    validate_package_file(file.as_file(), portable)?;
    check_cancelled(cancelled)?;
    file.commit().map_err(|e| format!("提交更新包失败：{e}"))?;
    progress(DownloadProgress {
        source,
        probing: false,
        downloaded,
        total: downloaded,
    });
    Ok(CachedUpdate {
        tag: release.tag_name.clone(),
        file_name: name.to_owned(),
        sha256,
        portable,
    })
}

fn verify_digest(expected: &str, actual: &str) -> Result<(), String> {
    let expected = expected
        .strip_prefix("sha256:")
        .ok_or("更新包摘要算法不受支持")?;
    if !expected.eq_ignore_ascii_case(actual) {
        return Err("更新包 SHA-256 校验失败".into());
    }
    Ok(())
}

fn validate_asset_url(
    asset: &ReleaseAsset,
    release: &Release,
    local_test: bool,
) -> Result<(), String> {
    if local_test {
        return Ok(());
    }
    let url = ureq::get(&asset.browser_download_url)
        .request_url()
        .map_err(|_| "更新包下载地址无效")?;
    let (host, repository) = if release.source == ReleaseSource::Gitee {
        ("gitee.com", "/mk-mo-sheng/QcClip/releases/download/")
    } else {
        ("github.com", "/mosheng1/QcClip/releases/download/")
    };
    if url.scheme() != "https"
        || url.host() != host
        || !url.as_url().path().starts_with(repository)
        || !url.as_url().username().is_empty()
        || url.as_url().password().is_some()
    {
        return Err("更新包下载地址不属于官方仓库".into());
    }
    Ok(())
}

fn local_test_url(url: &str) -> bool {
    #[cfg(any(debug_assertions, test))]
    {
        (cfg!(test) || client::api_url() != client::RELEASES_API)
            && ureq::get(url).request_url().ok().is_some_and(|url| {
                url.scheme() == "http" && matches!(url.host(), "127.0.0.1" | "localhost" | "[::1]")
            })
    }
    #[cfg(not(any(debug_assertions, test)))]
    {
        let _ = url;
        false
    }
}

fn check_cancelled(cancelled: &AtomicBool) -> Result<(), String> {
    if cancelled.load(Ordering::Relaxed) {
        Err("下载已取消".into())
    } else {
        Ok(())
    }
}

pub(crate) fn hash_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("读取更新包失败：{e}"))?;
    if file
        .metadata()
        .map_err(|e| format!("读取更新包信息失败：{e}"))?
        .len()
        > MAX_PACKAGE_BYTES
    {
        return Err("更新包超过大小限制".into());
    }
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|e| format!("校验更新包失败：{e}"))?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>())
}

fn validate_package(path: &Path, portable: bool) -> Result<(), String> {
    validate_package_file(
        &File::open(path).map_err(|e| format!("读取更新包失败：{e}"))?,
        portable,
    )
}

fn validate_package_file(mut file: &File, portable: bool) -> Result<(), String> {
    file.seek(SeekFrom::Start(0))
        .map_err(|e| format!("读取更新包失败：{e}"))?;
    if portable {
        let mut archive =
            zip::ZipArchive::new(file).map_err(|e| format!("便携更新包格式错误：{e}"))?;
        for name in ["QcClip/QcClip.exe", "QcClip/QcClip.portable"] {
            archive
                .by_name(name)
                .map_err(|_| "便携更新包缺少必要文件".to_owned())?;
        }
    } else {
        let mut magic = [0; 2];
        file.read_exact(&mut magic)
            .map_err(|_| "安装包不完整".to_owned())?;
        if &magic != b"MZ" {
            return Err("下载内容不是 Windows 安装包".into());
        }
    }
    Ok(())
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn server(body: Vec<u8>, declared_size: usize) -> (String, std::thread::JoinHandle<()>) {
        let (url, task, _) = server_with_delay(body, declared_size, Duration::ZERO);
        (url, task)
    }

    fn server_with_delay(
        body: Vec<u8>,
        declared_size: usize,
        delay: Duration,
    ) -> (
        String,
        std::thread::JoinHandle<()>,
        std::sync::mpsc::Receiver<()>,
    ) {
        let (connected, connection) = std::sync::mpsc::sync_channel(1);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/package", listener.local_addr().unwrap());
        let task = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = [0; 2048];
            assert!(stream.read(&mut request).unwrap() > 0);
            let _ = connected.send(());
            std::thread::sleep(delay);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {declared_size}\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(&body);
        });
        (url, task, connection)
    }

    fn release(url: &str, size: u64, digest: Option<String>) -> Release {
        serde_json::from_value(serde_json::json!({
            "tag_name": "v99.0.0", "source": "Gitee",
            "assets": [{"name": package_name("v99.0.0", false).unwrap(), "browser_download_url": url, "size": size, "digest": digest}]
        })).unwrap()
    }

    #[test]
    fn downloads_atomically_reports_progress_and_reuses_verified_cache() {
        let directory = tempfile::tempdir().unwrap();
        let body = b"MZ-test-installer".to_vec();
        let digest = Sha256::digest(&body)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let (url, server) = server(body.clone(), body.len());
        let release = release(&url, body.len() as u64, Some(format!("sha256:{digest}")));
        let mut events = Vec::new();
        let cached = download(
            release.clone(),
            &UpdateConfig::default(),
            directory.path(),
            false,
            &AtomicBool::new(false),
            |p| events.push(p),
        )
        .unwrap();
        server.join().unwrap();
        assert_eq!(fs::read(cached.path(directory.path())).unwrap(), body);
        cached.verify(directory.path()).unwrap();
        assert_eq!(events.last().unwrap().downloaded, body.len() as u64);
        assert_eq!(events.last().unwrap().source, "Gitee");
        // 服务已关闭，仍应从经过验证的缓存恢复安装入口。
        let reused = download(
            release,
            &UpdateConfig::default(),
            directory.path(),
            false,
            &AtomicBool::new(false),
            |_| panic!("缓存不应重新下载"),
        )
        .unwrap();
        assert_eq!(reused.sha256, cached.sha256);
        fs::write(cached.path(directory.path()), b"tampered").unwrap();
        assert!(
            cached
                .verify(directory.path())
                .unwrap_err()
                .contains("校验失败")
        );
    }

    #[test]
    fn rejects_truncated_corrupt_and_digest_mismatched_downloads() {
        for (body, length, digest) in [
            (b"MZ-partial".to_vec(), 100, None),
            (b"<html>error</html>".to_vec(), 18, None),
            (
                b"MZ-wrong-hash".to_vec(),
                13,
                Some(format!("sha256:{}", "0".repeat(64))),
            ),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let (url, server) = server(body, length);
            let release = release(&url, 0, digest);
            assert!(
                download(
                    release,
                    &UpdateConfig::default(),
                    directory.path(),
                    false,
                    &AtomicBool::new(false),
                    |_| {}
                )
                .is_err()
            );
            server.join().unwrap();
            assert!(
                !directory
                    .path()
                    .join(package_name("v99.0.0", false).unwrap())
                    .exists()
            );
            assert!(!directory.path().join("ready.json").exists());
        }
    }

    #[test]
    fn failed_selected_transfer_restarts_from_gitee_and_keeps_the_original_digest() {
        let directory = tempfile::tempdir().unwrap();
        let mut body = vec![1; 512 * 1024];
        body[..2].copy_from_slice(b"MZ");
        let digest = Sha256::digest(&body)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let (primary_url, primary_server) = server(body[..384 * 1024].to_vec(), body.len());
        let (mirror_url, mirror_server) = server(body.clone(), body.len());
        let mut primary = release(
            &primary_url,
            body.len() as u64,
            Some(format!("sha256:{digest}")),
        );
        primary.source = ReleaseSource::GitHub;
        let mirror = release(&mirror_url, 0, None);
        let config = UpdateConfig::default();
        let cancelled = AtomicBool::new(false);
        let mut sources = Vec::new();
        let (selected, ready) = std::sync::mpsc::sync_channel(1);
        let cached = download_with_mirror(
            primary,
            DownloadOptions {
                config: &config,
                directory: directory.path(),
                portable: false,
                cancelled: &cancelled,
            },
            |p| {
                if !p.probing && p.source == "GitHub" {
                    let _ = selected.try_send(());
                }
                sources.push(p.source);
            },
            move |tag, _| {
                ready.recv_timeout(Duration::from_secs(5)).unwrap();
                assert_eq!(tag, mirror.tag_name);
                Ok(mirror)
            },
        )
        .unwrap();
        primary_server.join().unwrap();
        mirror_server.join().unwrap();
        assert_eq!(cached.sha256, digest);
        assert_eq!(fs::read(cached.path(directory.path())).unwrap(), body);
        assert_eq!(sources.first(), Some(&"GitHub"));
        assert_eq!(sources.last(), Some(&"Gitee"));
    }

    #[test]
    fn selects_the_faster_mirror_without_waiting_for_the_primary_to_fail() {
        for source in [ReleaseSource::GitHub, ReleaseSource::Gitee] {
            let directory = tempfile::tempdir().unwrap();
            let body = b"MZ-fast-mirror".to_vec();
            let digest = format!(
                "sha256:{}",
                Sha256::digest(&body)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            );
            let (primary_url, primary_server, connected) = server_with_delay(
                b"MZ-wrong-content".to_vec(),
                body.len(),
                Duration::from_secs(2),
            );
            let (mirror_url, mirror_server) = server(body.clone(), body.len());
            let primary = Release {
                source,
                ..release(&primary_url, body.len() as u64, Some(digest))
            };
            let mirror = release(&mirror_url, 0, None);
            let config = UpdateConfig::default();
            let cancelled = AtomicBool::new(false);
            let started = Instant::now();
            let cached = download_with_mirror(
                primary,
                DownloadOptions {
                    config: &config,
                    directory: directory.path(),
                    portable: false,
                    cancelled: &cancelled,
                },
                |_| {},
                move |_, _| {
                    connected.recv_timeout(Duration::from_secs(5)).unwrap();
                    Ok(mirror)
                },
            )
            .unwrap();
            let elapsed = started.elapsed();
            primary_server.join().unwrap();
            mirror_server.join().unwrap();
            assert!(elapsed < Duration::from_secs(1), "下载耗时：{elapsed:?}");
            assert_eq!(fs::read(cached.path(directory.path())).unwrap(), body);
        }
    }

    #[test]
    fn mirror_cannot_supply_an_older_release_or_different_content() {
        for wrong_tag in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let primary = Release {
                source: ReleaseSource::GitHub,
                ..release(
                    "https://github.com/invalid",
                    0,
                    Some(format!("sha256:{}", "0".repeat(64))),
                )
            };
            let mut mirror = release("https://gitee.com/invalid", 0, None);
            let mut server_task = None;
            if wrong_tag {
                mirror.tag_name = "v98.0.0".into();
            } else {
                let (url, server) = server(b"MZ-wrong-mirror".to_vec(), b"MZ-wrong-mirror".len());
                mirror.assets[0].browser_download_url = url;
                server_task = Some(server);
            }
            let config = UpdateConfig::default();
            let cancelled = AtomicBool::new(false);
            let error = download_with_mirror(
                primary,
                DownloadOptions {
                    config: &config,
                    directory: directory.path(),
                    portable: false,
                    cancelled: &cancelled,
                },
                |_| {},
                move |_, _| Ok(mirror),
            )
            .unwrap_err();
            if let Some(server) = server_task {
                server.join().unwrap();
            }
            assert!(error.contains(if wrong_tag {
                "版本不一致"
            } else {
                "校验失败"
            }));
            assert!(!directory.path().join("ready.json").exists());
            assert!(
                !directory
                    .path()
                    .join(package_name("v99.0.0", false).unwrap())
                    .exists()
            );
        }
    }

    #[test]
    fn cancellation_does_not_commit_an_installer() {
        let directory = tempfile::tempdir().unwrap();
        let body = b"MZ-cancel".to_vec();
        let (url, server) = server(body.clone(), body.len());
        let flag = AtomicBool::new(false);
        let result = download(
            release(&url, 0, None),
            &UpdateConfig::default(),
            directory.path(),
            false,
            &flag,
            |progress| {
                if !progress.probing {
                    flag.store(true, Ordering::Relaxed);
                }
            },
        );
        server.join().unwrap();
        assert!(result.unwrap_err().contains("取消"));
        assert!(!directory.path().join("ready.json").exists());
    }

    #[test]
    fn cancellation_during_probe_does_not_wait_for_a_blocked_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/package", listener.local_addr().unwrap());
        let (connected, connection) = std::sync::mpsc::sync_channel(1);
        let (release_server, wait) = std::sync::mpsc::sync_channel(1);
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = [0; 2048];
            assert!(stream.read(&mut request).unwrap() > 0);
            connected.send(()).unwrap();
            let _ = wait.recv_timeout(Duration::from_secs(5));
        });
        let directory = tempfile::tempdir().unwrap();
        let config = UpdateConfig::default();
        let cancelled = AtomicBool::new(false);
        let started = Instant::now();
        let result = download_with_mirror(
            release(&url, 0, None),
            DownloadOptions {
                config: &config,
                directory: directory.path(),
                portable: false,
                cancelled: &cancelled,
            },
            |_| {
                connection.recv_timeout(Duration::from_secs(5)).unwrap();
                cancelled.store(true, Ordering::Relaxed);
            },
            |_, _| Err("镜像未同步".into()),
        );
        let elapsed = started.elapsed();
        release_server.send(()).unwrap();
        server.join().unwrap();
        assert!(result.unwrap_err().contains("取消"));
        assert!(elapsed < Duration::from_secs(1), "取消耗时：{elapsed:?}");
        assert!(!directory.path().join("ready.json").exists());
    }

    #[test]
    fn rejects_external_download_urls_wrong_packages_and_cache_paths() {
        let mut release = release("https://evil.example/QcClip.exe", 0, None);
        assert!(validate_asset_url(&release.assets[0], &release, false).is_err());
        release.assets[0].browser_download_url =
            "https://gitee.com/other/QcClip/releases/download/v99.0.0/app.exe".into();
        assert!(validate_asset_url(&release.assets[0], &release, false).is_err());
        let directory = tempfile::tempdir().unwrap();
        let cached = CachedUpdate {
            tag: "v99.0.0".into(),
            file_name: "../outside.exe".into(),
            sha256: String::new(),
            portable: false,
        };
        assert!(cached.verify(directory.path()).is_err());
        assert!(package_name("v1.0/../../outside", false).is_err());
        release.assets[0].name = package_name("v99.0.0", true).unwrap();
        assert!(
            download(
                release,
                &UpdateConfig::default(),
                directory.path(),
                false,
                &AtomicBool::new(false),
                |_| {}
            )
            .unwrap_err()
            .contains("匹配")
        );
    }
}
