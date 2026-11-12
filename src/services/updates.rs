mod client;
mod download;
mod installer;
mod installer_window;
pub(crate) use installer_window::run_if_requested as run_installer_if_requested;
mod schedule;

use std::{
    fs,
    io::Write,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use atomic_write_file::AtomicWriteFile;
use gpui::{App, AppContext, BorrowAppContext, Global, Task};
use serde::{Deserialize, Serialize};

use crate::windows::notification::{
    NotificationAction, NotificationLevel, NotificationRequest, NotificationService,
};
use crate::{config::NotificationKind, config_runtime, i18n};
#[cfg(debug_assertions)]
use client::{RELEASES_API, api_url};
pub(crate) use client::{Release, valid_proxy_url};
use client::{fetch_latest, newer_than_current};

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct CheckRecord {
    checked_at: u64,
    retry_after: u64,
    latest: Option<Release>,
    notified_tag: Option<String>,
}

#[derive(Default)]
pub(crate) enum DownloadState {
    #[default]
    Idle,
    Downloading(download::DownloadProgress),
    Ready(download::CachedUpdate),
    Failed(String),
}

pub(crate) struct UpdateService {
    pub(crate) checking: bool,
    pub(crate) status_key: &'static str,
    pub(crate) error: Option<String>,
    pub(crate) download: DownloadState,
    pub(crate) installing: bool,
    download_cancel: Option<Arc<AtomicBool>>,
    download_retry: Option<Task<()>>,
    deferred_tag: Option<String>,
    auto_download_enabled: bool,
    record: CheckRecord,
    path: PathBuf,
    scheduled_check: Option<Task<()>>,
}

impl Global for UpdateService {}

impl UpdateService {
    pub(crate) fn install(path: PathBuf, cx: &mut App) {
        #[cfg(debug_assertions)]
        let path = if api_url() != RELEASES_API {
            path.with_file_name("config.test-updates.json")
        } else {
            path
        };
        let record = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let status_key = check_status(&record);
        cx.set_global(Self {
            checking: false,
            status_key,
            error: None,
            download: DownloadState::Idle,
            installing: false,
            download_cancel: None,
            download_retry: None,
            deferred_tag: None,
            auto_download_enabled: config_runtime::update_config(cx).auto_download,
            record,
            path,
            scheduled_check: None,
        });
        cx.refresh_windows();
        cx.observe_global::<config_runtime::ConfigRuntime>(Self::settings_changed)
            .detach();
        Self::schedule(cx);
        let cache_directory = cx.global::<Self>().cache_directory();
        cx.background_spawn(async move {
            installer::cleanup_workers(&cache_directory);
        })
        .detach();
        Self::restore_download(cx);
    }

    fn schedule(cx: &mut App) {
        let delay = {
            let service = cx.global::<Self>();
            if service.checking {
                return;
            }
            schedule::next_check_delay(
                config_runtime::update_config(cx).frequency.seconds(),
                service.record.checked_at,
                service.record.retry_after,
                unix_time(),
            )
        };
        // 只保留下一次到期任务；“永不更新”不创建定时器。
        cx.update_global::<Self, _>(|service, _| service.scheduled_check = None);
        let Some(delay) = delay else {
            return;
        };
        let task = cx.spawn(async move |cx| {
            cx.background_executor().timer(delay).await;
            cx.update(|cx| Self::check(false, cx));
        });
        cx.update_global::<Self, _>(|service, _| {
            service.scheduled_check = Some(task);
        })
    }

    pub(crate) fn available(cx: &App) -> Option<Release> {
        cx.try_global::<Self>()?
            .record
            .latest
            .clone()
            .filter(|release| newer_than_current(release))
    }

    pub(crate) fn open_release(cx: &mut App) {
        if let Some(release) = Self::available(cx) {
            cx.open_url(&release.url());
        }
    }

    pub(crate) fn check(manual: bool, cx: &mut App) {
        if cx.global::<Self>().busy() {
            return;
        }
        cx.update_global::<Self, _>(|service, _| {
            service.scheduled_check = None;
            service.checking = true;
            service.error = None;
            service.status_key = "updates.status.checking";
            // 请求前保存重试期限，避免异常退出或反复启动导致重复请求。
            service.record.retry_after = unix_time().saturating_add(3600);
            if let Err(error) = service.persist() {
                tracing::warn!("保存更新检查记录失败：{error}");
            }
        });
        cx.refresh_windows();
        let config = config_runtime::update_config(cx);
        let task = cx.background_spawn(async move { fetch_latest(&config) });
        cx.spawn(async move |cx| {
            let result = task.await;
            cx.update(|cx| Self::finish(result, manual, cx));
        })
        .detach();
    }

    fn finish(result: Result<Option<Release>, String>, manual: bool, cx: &mut App) {
        let notifications = (manual && config_runtime::global_notification_enabled(cx))
            || config_runtime::notification_enabled(NotificationKind::VersionUpdate, cx);
        let mut notify_release = None;
        cx.update_global::<Self, _>(|service, _| {
            service.checking = false;
            match result {
                Ok(latest) => {
                    service.record.checked_at = unix_time();
                    service.record.latest = latest;
                    service.record.retry_after = 0;
                    service.status_key = check_status(&service.record);
                    if let Some(release) = service
                        .record
                        .latest
                        .as_ref()
                        .filter(|r| newer_than_current(r))
                    {
                        if notifications
                            && (manual
                                || service.record.notified_tag.as_deref()
                                    != Some(&release.tag_name))
                        {
                            notify_release = Some(release.clone());
                            service.record.notified_tag = Some(release.tag_name.clone());
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!("检查版本更新失败：{error}");
                    service.error = Some(error);
                    service.status_key = "updates.status.failed";
                    service.record.retry_after = unix_time().saturating_add(3600);
                }
            }
            if let Err(error) = service.persist() {
                tracing::warn!("保存更新检查记录失败：{error}");
            }
        });
        Self::schedule(cx);
        cx.refresh_windows();
        let translator = i18n::translator(cx);
        if let Some(release) = notify_release {
            let title = translator
                .translate_with_args("updates.found", &[("version", release.version_label())]);
            let detail = translator.translate_with_args(
                "updates.notification.detail",
                &[
                    ("current", format!("v{}", env!("CARGO_PKG_VERSION"))),
                    ("latest", release.version_label()),
                ],
            );
            let mut request = NotificationRequest::text(title, detail);
            request.kind = if manual {
                NotificationKind::OperationResult
            } else {
                NotificationKind::VersionUpdate
            };
            request.duration = Duration::from_secs(12);
            request.action = Some(NotificationAction::OpenUpdates);
            request.action_label = Some(translator.translate("updates.view_progress"));
            NotificationService::show(request, cx);
        } else if manual && cx.global::<Self>().status_key != "updates.status.available" {
            let service = cx.global::<Self>();
            let message = service
                .error
                .clone()
                .unwrap_or_else(|| translator.translate(service.status_key));
            NotificationService::show_message(
                translator.translate("updates.check"),
                message,
                if service.error.is_some() {
                    NotificationLevel::Error
                } else {
                    NotificationLevel::Info
                },
                cx,
            );
        }
        Self::maybe_download(cx);
    }

    fn settings_changed(cx: &mut App) {
        let enabled = config_runtime::update_config(cx).auto_download;
        let changed = enabled != cx.global::<Self>().auto_download_enabled;
        cx.update_global::<Self, _>(|service, _| {
            service.auto_download_enabled = enabled;
            if changed && enabled {
                service.deferred_tag = None;
            }
        });
        if changed && !enabled {
            Self::cancel_download(cx);
        }
        Self::schedule(cx);
        Self::maybe_download(cx);
    }

    pub(crate) fn busy(&self) -> bool {
        self.checking || self.installing || self.download_cancel.is_some()
    }

    pub(crate) fn cancelling(&self) -> bool {
        self.download_cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
    }

    fn cache_directory(&self) -> PathBuf {
        self.path.with_extension("cache")
    }

    fn restore_download(cx: &mut App) {
        let Some(release) = Self::available(cx) else {
            return;
        };
        let directory = cx.global::<Self>().cache_directory();
        let portable =
            crate::app_paths::AppPaths::discover().is_ok_and(|paths| paths.is_portable());
        let tag = release.tag_name;
        let task = cx.background_spawn(async move {
            let cached: download::CachedUpdate =
                serde_json::from_slice(&fs::read(directory.join("ready.json")).ok()?).ok()?;
            (cached.tag == tag && cached.portable == portable && cached.verify(&directory).is_ok())
                .then_some(cached)
        });
        cx.spawn(async move |cx| {
            let cached = task.await;
            cx.update(|cx| {
                if !cx.global::<Self>().busy()
                    && matches!(cx.global::<Self>().download, DownloadState::Idle)
                {
                    if let Some(cached) = cached
                        && Self::available(cx).is_some_and(|r| r.tag_name == cached.tag)
                    {
                        cx.update_global::<Self, _>(|service, _| {
                            service.download = DownloadState::Ready(cached)
                        });
                        cx.refresh_windows();
                    }
                    Self::maybe_download(cx);
                }
            });
        })
        .detach();
    }

    fn maybe_download(cx: &mut App) {
        let service = cx.global::<Self>();
        if !cfg!(target_os = "windows")
            || !config_runtime::update_config(cx).auto_download
            || service.busy()
            || service.download_retry.is_some()
        {
            return;
        }
        if let Some(release) = Self::available(cx) {
            if service.deferred_tag.as_deref() == Some(&release.tag_name) {
                return;
            }
            if matches!(&service.download, DownloadState::Ready(cached) if cached.tag == release.tag_name)
            {
                return;
            }
            Self::start_download(false, cx);
        }
    }

    pub(crate) fn cancel_download(cx: &mut App) {
        let tag = Self::available(cx).map(|r| r.tag_name);
        cx.update_global::<Self, _>(|service, _| {
            if let Some(cancel) = &service.download_cancel {
                cancel.store(true, Ordering::Relaxed);
            }
            service.download_retry = None;
            service.deferred_tag = tag;
        });
        cx.refresh_windows();
    }

    pub(crate) fn start_download(manual: bool, cx: &mut App) {
        if cx.global::<Self>().busy() {
            return;
        }
        let Some(release) = Self::available(cx) else {
            return;
        };
        let config = config_runtime::update_config(cx);
        let directory = cx.global::<Self>().cache_directory();
        let portable =
            crate::app_paths::AppPaths::discover().is_ok_and(|paths| paths.is_portable());
        let cancelled = Arc::new(AtomicBool::new(false));
        cx.update_global::<Self, _>(|service, _| {
            service.download_cancel = Some(cancelled.clone());
            service.download_retry = None;
            service.deferred_tag = None;
            service.download = DownloadState::Downloading(download::DownloadProgress {
                source: release.source.label(),
                probing: true,
                downloaded: 0,
                total: 0,
            });
        });
        cx.refresh_windows();
        let translator = i18n::translator(cx);
        let mut request = NotificationRequest::text(
            translator.translate("updates.downloading"),
            translator.translate("updates.download_started"),
        );
        request.kind = if manual {
            NotificationKind::OperationResult
        } else {
            NotificationKind::VersionUpdate
        };
        request.action = Some(NotificationAction::OpenUpdates);
        request.action_label = Some(translator.translate("updates.view_progress"));
        NotificationService::show(request, cx);
        let (sender, receiver) = async_channel::unbounded();
        let task = cx.background_spawn(async move {
            download::download(
                release,
                &config,
                &directory,
                portable,
                &cancelled,
                |progress| {
                    let _ = sender.try_send(progress);
                },
            )
        });
        cx.spawn(async move |cx| {
            while let Ok(progress) = receiver.recv().await {
                cx.update(|cx| {
                    cx.update_global::<Self, _>(|service, _| {
                        service.download = DownloadState::Downloading(progress)
                    });
                    cx.refresh_windows();
                });
            }
            let result = task.await;
            cx.update(|cx| Self::download_finished(result, manual, cx));
        })
        .detach();
    }

    fn download_finished(
        result: Result<download::CachedUpdate, String>,
        manual: bool,
        cx: &mut App,
    ) {
        let cancelled = cx
            .global::<Self>()
            .download_cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed));
        let mut ready = None;
        let mut failure = None;
        cx.update_global::<Self, _>(|service, _| {
            service.download_cancel = None;
            if cancelled {
                service.download = DownloadState::Idle;
                return;
            }
            match result {
                Ok(cached) => {
                    ready = Some(cached.tag.clone());
                    service.download = DownloadState::Ready(cached);
                }
                Err(error) => {
                    tracing::warn!("下载更新失败：{error}");
                    failure = Some(error.clone());
                    service.download = DownloadState::Failed(error);
                }
            }
        });
        let translator = i18n::translator(cx);
        if let Some(tag) = ready {
            let mut request = NotificationRequest::text(
                translator.translate("updates.ready"),
                translator.translate("updates.ready_detail"),
            );
            request.kind = if manual {
                NotificationKind::OperationResult
            } else {
                NotificationKind::VersionUpdate
            };
            request.action = Some(NotificationAction::InstallUpdate(tag));
            request.action_label = Some(translator.translate("updates.install"));
            NotificationService::show(request, cx);
        } else if let Some(error) = failure {
            let mut request =
                NotificationRequest::text(translator.translate("updates.download_failed"), error);
            request.level = NotificationLevel::Error;
            request.action = Some(NotificationAction::OpenUpdates);
            request.action_label = Some(translator.translate("updates.retry"));
            NotificationService::show(request, cx);
            if config_runtime::update_config(cx).auto_download {
                let task = cx.spawn(async move |cx| {
                    cx.background_executor()
                        .timer(Duration::from_secs(3600))
                        .await;
                    cx.update(|cx| {
                        cx.update_global::<Self, _>(|service, _| service.download_retry = None);
                        Self::maybe_download(cx);
                    });
                });
                cx.update_global::<Self, _>(|service, _| service.download_retry = Some(task));
            }
        }
        cx.refresh_windows();
        Self::schedule(cx);
        if cancelled {
            Self::maybe_download(cx);
        }
    }

    pub(crate) fn install_downloaded(expected_tag: Option<&str>, cx: &mut App) {
        let service = cx.global::<Self>();
        if service.busy() {
            return;
        }
        let DownloadState::Ready(cached) = &service.download else {
            return;
        };
        if expected_tag.is_some_and(|tag| tag != cached.tag)
            || !Self::available(cx).is_some_and(|release| release.tag_name == cached.tag)
        {
            return;
        }
        let cached = cached.clone();
        let directory = service.cache_directory();
        if !crate::windows::main::MainWindowService::begin_blocking_operation(
            "updates.installing",
            cx,
        ) {
            let translator = i18n::translator(cx);
            NotificationService::show_message(
                translator.translate("updates.install"),
                translator.translate("updates.wait_for_operation"),
                NotificationLevel::Warning,
                cx,
            );
            return;
        }
        cx.update_global::<Self, _>(|service, _| {
            service.installing = true;
            service.download_retry = None;
        });
        cx.refresh_windows();
        let task = cx.background_spawn(async move { installer::prepare(cached, directory) });
        cx.spawn(async move |cx| {
            let result = match task.await {
                Ok(mut prepared) => {
                    let started = std::time::Instant::now();
                    loop {
                        if let Ok(error) = fs::read_to_string(prepared.directory.join("error.txt"))
                        {
                            break Err(error);
                        }
                        match prepared.child.try_wait() {
                            Ok(Some(status)) => break Err(format!("更新进程提前退出：{status}")),
                            Err(error) => break Err(format!("读取更新进程状态失败：{error}")),
                            Ok(None) => {}
                        }
                        if prepared.directory.join("ready").exists() {
                            break fs::write(prepared.directory.join("proceed"), b"proceed")
                                .map_err(|e| format!("确认安装失败：{e}"));
                        }
                        if started.elapsed() > Duration::from_secs(30) {
                            let _ = prepared.child.kill();
                            break Err("更新进程启动超时，主程序保持运行，请重试".into());
                        }
                        cx.background_executor()
                            .timer(Duration::from_millis(100))
                            .await;
                    }
                }
                Err(error) => Err(error),
            };
            cx.update(|cx| match result {
                Ok(()) => cx.quit(),
                Err(error) => {
                    tracing::error!("启动更新安装失败：{error}");
                    crate::windows::main::MainWindowService::finish_blocking_operation(cx);
                    cx.update_global::<Self, _>(|service, _| {
                        service.installing = false;
                        service.download = DownloadState::Failed(error.clone());
                    });
                    NotificationService::show_message(
                        i18n::translator(cx).translate("updates.install_failed"),
                        error,
                        NotificationLevel::Error,
                        cx,
                    );
                    cx.refresh_windows();
                }
            });
        })
        .detach();
    }

    fn persist(&self) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(&self.record).map_err(|e| e.to_string())?;
        let mut file = AtomicWriteFile::open(&self.path).map_err(|e| e.to_string())?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        file.commit().map_err(|e| e.to_string())
    }
}

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn check_status(record: &CheckRecord) -> &'static str {
    match record.latest.as_ref() {
        Some(release) if newer_than_current(release) => "updates.status.available",
        Some(_) => "updates.status.current",
        None if record.checked_at == 0 => "updates.status.idle",
        None => "updates.status.empty",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_record_preserves_published_records_and_retry_deadlines() {
        let mut record: CheckRecord = serde_json::from_str(
            r#"{"checked_at":1000,"latest":{"tag_name":"v99.0.0"},"notified_tag":"v99.0.0"}"#,
        )
        .unwrap();
        assert_eq!(record.retry_after, 0);
        record.retry_after = 4600;
        let restored: CheckRecord =
            serde_json::from_slice(&serde_json::to_vec(&record).unwrap()).unwrap();
        assert_eq!(restored.checked_at, 1000);
        assert_eq!(restored.retry_after, 4600);
        assert_eq!(restored.latest.unwrap().tag_name, "v99.0.0");
        assert_eq!(restored.notified_tag.as_deref(), Some("v99.0.0"));
    }
}
