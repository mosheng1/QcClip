use gpui::{
    App, AppContext, Bounds, TitlebarOptions, WindowBackgroundAppearance, WindowBounds, WindowKind,
    WindowOptions, px, size,
};
use gpui_component::init;

use crate::{
    app_paths::AppPaths,
    assets::Assets,
    platform::{
        clipboard::{ClipboardMonitor, ClipboardWriteSuppressor},
        window::configure_window,
    },
    services::{
        capture::CaptureService,
        custom_text::CustomTextService,
        data_management::{DataManagementService, apply_pending_import},
        database::{Database, DatabaseError, FavoriteRepository, HistoryRepository, TagRepository},
        paste::PasteService,
        recovery::reconcile_startup,
        storage::RawBlobStore,
    },
    state_runtime::PersistedMainWindowState,
    windows::dialog::DialogWindowService,
    windows::main::MainWindow,
    windows::menu::MenuService,
};

#[cfg(test)]
const WINDOW_WIDTH: f32 = crate::state_runtime::PersistedWindowSize::DEFAULT.width;
#[cfg(test)]
const WINDOW_HEIGHT: f32 = crate::state_runtime::PersistedWindowSize::DEFAULT.height;
const WINDOW_MIN_SIZE: f32 = crate::state_runtime::PersistedWindowSize::MIN_SIZE;

pub fn run() {
    let _logging_guard = crate::logging::init(&crate::logging::fallback_directory());
    let single_instance = match crate::platform::single_instance::SingleInstance::acquire() {
        Ok(Some(instance)) => instance,
        Ok(None) => {
            tracing::info!("QcClip 已在运行，已请求显示已有实例的主窗口");
            return;
        }
        Err(error) => {
            tracing::error!("单实例检查失败，已停止启动：{error}");
            return;
        }
    };
    let activation_path = single_instance.activation_path();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"), os = std::env::consts::OS,
        arch = std::env::consts::ARCH, pid = std::process::id(),
        windows_build = ?crate::platform::os_version::build_number(),
        "QcClip 开始启动，正在解析数据目录"
    );
    let app_paths =
        AppPaths::discover().unwrap_or_else(|error| panic!("无法初始化应用路径：{error}"));
    if let Err(error) = crate::logging::switch_directory(&app_paths.log_directory()) {
        tracing::error!(%error, path = ?app_paths.log_directory(), "无法切换应用日志目录，继续使用备用日志");
    }
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"), pid = std::process::id(),
        os = std::env::consts::OS, arch = std::env::consts::ARCH,
        windows_build = ?crate::platform::os_version::build_number(),
        data_root = ?app_paths.data_root(), portable = app_paths.is_portable(),
        "QcClip 数据目录已确定"
    );
    match apply_pending_import(app_paths.data_root()) {
        Ok(true) => tracing::info!("已应用暂存的数据导入内容"),
        Ok(false) => {}
        Err(error) => {
            tracing::error!("应用暂存的数据导入失败：{error}");
            if matches!(
                &error,
                crate::services::data_management::DataManagementError::ImportRollback { .. }
            ) {
                panic!("无法恢复导入前的数据，应用已停止启动：{error}");
            }
        }
    }
    let _database = initialize_database(&app_paths).unwrap_or_else(|error| {
        tracing::error!("无法初始化数据库：{error}");
        panic!("无法初始化数据库：{error}");
    });
    let _raw_blob_store = RawBlobStore::new(app_paths.data_root()).unwrap_or_else(|error| {
        tracing::error!("无法初始化原始数据存储：{error}");
        panic!("无法初始化原始数据存储：{error}");
    });
    match reconcile_startup(&_database, &_raw_blob_store) {
        Ok(report) => {
            if report.pending_items_marked_corrupt > 0
                || report.pending_blobs_marked_corrupt > 0
                || report.pending_files_removed > 0
            {
                tracing::warn!(
                    pending_items = report.pending_items_marked_corrupt,
                    pending_blobs = report.pending_blobs_marked_corrupt,
                    pending_files = report.pending_files_removed,
                    "启动恢复完成，发现并处理了未完成数据"
                );
            } else {
                tracing::info!("启动恢复完成，未发现未完成数据");
            }
        }
        Err(error) => {
            tracing::error!("启动恢复失败，列表将跳过受影响的原始数据：{error}");
        }
    }
    crate::platform::image_temp_file::cleanup_opened_files_directory();
    let capture_service = CaptureService::new(_database.clone(), _raw_blob_store.clone());
    let clipboard_write_suppressor = ClipboardWriteSuppressor::default();
    let paste_service = PasteService::with_clipboard_write_suppressor(
        _database.clone(),
        _raw_blob_store.clone(),
        clipboard_write_suppressor.clone(),
    );
    let config_path = app_paths.config_path();
    crate::platform::clipboard::set_capture_format_config(
        crate::config_runtime::load_config(&config_path)
            .clipboard
            .capture_formats,
    );
    let (clipboard_monitor, clipboard_events) =
        match ClipboardMonitor::start(clipboard_write_suppressor.clone()) {
            Ok((monitor, receiver)) => {
                tracing::info!("剪贴板监听已启动");
                (Some(monitor), Some(receiver))
            }
            Err(error) => {
                tracing::error!("无法启动剪贴板监听：{error}");
                (None, None)
            }
        };

    let history_repository = HistoryRepository::new(_database.clone(), _raw_blob_store.clone());
    let favorite_repository =
        FavoriteRepository::with_blob_store(_database.clone(), _raw_blob_store.clone());
    let tag_repository = TagRepository::new(_database.clone());

    let mut clipboard_monitor = clipboard_monitor;
    let mut clipboard_events = clipboard_events;
    let state_path = app_paths.state_path();
    gpui_platform::application()
        .with_assets(Assets::new())
        .run(move |cx: &mut App| {
            init(cx);
            crate::config_runtime::ConfigRuntime::install(&config_path, cx);
            if crate::config_runtime::run_as_administrator(cx) {
                match crate::platform::elevation::is_elevated() {
                    Ok(true) => {}
                    Ok(false) => match crate::platform::elevation::relaunch_elevated() {
                        Ok(()) => {
                            cx.quit();
                            return;
                        }
                        Err(error) => tracing::error!("请求管理员权限启动失败：{error}"),
                    },
                    Err(error) => tracing::error!("读取管理员权限状态失败：{error}"),
                }
            }
            if crate::config_runtime::launch_at_login(cx) {
                if let Err(error) = crate::platform::auto_start::set_enabled(true) {
                    tracing::error!("同步开机自启设置失败：{error}");
                }
            }
            DataManagementService::install(
                _database.clone(),
                _raw_blob_store.clone(),
                app_paths.data_root().to_path_buf(),
                app_paths.default_data_root().to_path_buf(),
                app_paths.location_path(),
                app_paths.is_portable(),
                cx,
            );
            let history_policy = crate::config_runtime::history_config(cx);
            if let Err(error) = cx
                .global::<DataManagementService>()
                .cleanup_history_on_startup(&history_policy)
            {
                tracing::error!("启动时清理待清理历史失败：{error}");
            }
            crate::state_runtime::StateRuntime::install(&state_path, cx);
            crate::services::sound::install(
                crate::config_runtime::sound_config(cx),
                app_paths.sound_directory(),
            );
            let language = crate::config_runtime::interface_language(cx);
            crate::i18n::install(crate::i18n::Locale::new(language.code()), cx);
            crate::window_material::install(cx);
            crate::ui::theme::install(cx);
            match crate::platform::input::start() {
                Ok(()) => tracing::info!("全局鼠标事件监听已启动"),
                Err(error) => tracing::error!("无法启动全局鼠标事件监听：{error}"),
            }
            #[cfg(target_os = "windows")]
            crate::platform::drag_preview::DragPreviewService::install(cx);
            #[cfg(target_os = "windows")]
            crate::platform::external_drag::ExternalDragService::install(cx);
            DialogWindowService::install(cx);
            MenuService::install(cx);
            crate::windows::preview::PreviewWindowService::install(
                crate::services::image_preview::ImagePreviewService::new(_raw_blob_store.clone()),
                cx,
            );
            crate::windows::settings::SettingsWindowService::install(cx);
            crate::windows::notification::NotificationService::install(
                history_repository.clone(),
                crate::services::image_preview::ImagePreviewService::new(_raw_blob_store.clone()),
                cx,
            );
            crate::windows::main::MainWindowService::install(cx);
            crate::windows::quick_paste::QuickPasteWindowService::install(
                history_repository.clone(),
                favorite_repository.clone(),
                paste_service.clone(),
                crate::services::image_preview::ImagePreviewService::new(_raw_blob_store.clone()),
                cx,
            );
            crate::services::quick_favorite::QuickFavoriteService::install(
                tag_repository.clone(),
                CustomTextService::new(_database.clone(), _raw_blob_store.clone()),
                favorite_repository.clone(),
                capture_service.clone(),
                clipboard_write_suppressor.clone(),
                cx,
            );
            crate::hotkey::HotkeyRuntime::install(cx);
            #[cfg(target_os = "windows")]
            crate::services::tray::TrayService::install(cx);

            let (window_width, window_height) =
                initial_window_size(&crate::state_runtime::state(cx).main_window);
            let default_bounds =
                Bounds::centered(None, size(px(window_width), px(window_height)), cx);
            let startup_edge = if crate::config_runtime::clipboard_config(cx).edge_hide {
                crate::windows::main::MainWindowService::startup_edge_bounds(cx)
            } else {
                None
            };
            let (bounds, display_id, initially_visible) =
                if let Some((bounds, display_id)) = startup_edge {
                    (bounds, Some(display_id), true)
                } else {
                    (default_bounds, None, false)
                };
            let initial_logical_size = bounds.size;

            let main_window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        titlebar: Some(TitlebarOptions {
                            appears_transparent: true,
                            ..Default::default()
                        }),
                        focus: false,
                        show: initially_visible,
                        inactive_frame_interval: None,
                        kind: WindowKind::Normal,
                        is_movable: true,
                        is_resizable: true,
                        is_minimizable: false,
                        window_min_size: Some(size(px(WINDOW_MIN_SIZE), px(WINDOW_MIN_SIZE))),
                        window_background: WindowBackgroundAppearance::Transparent,
                        display_id,
                        ..Default::default()
                    },
                    {
                        let history_repository = history_repository.clone();
                        let favorite_repository = favorite_repository.clone();
                        let tag_repository = tag_repository.clone();
                        let paste_service = paste_service.clone();
                        let image_library =
                            crate::services::image_library::ImageLibraryService::new(
                                app_paths.image_library_directory(),
                            );
                        move |window, cx| {
                            configure_window(window);
                            // 客户区在调整框架后变大，窗口创建时读到的 viewport 尺寸已经过期。
                            window.bounds_changed(cx);
                            crate::window_material::register_window(window, cx);
                            let monitor = clipboard_monitor.take();
                            let events = clipboard_events.take();
                            let view = cx.new(|cx| {
                                let mut main_window = MainWindow::new(
                                    history_repository.clone(),
                                    favorite_repository.clone(),
                                    tag_repository.clone(),
                                    capture_service.clone(),
                                    CustomTextService::new(
                                        _database.clone(),
                                        _raw_blob_store.clone(),
                                    ),
                                    paste_service.clone(),
                                    _raw_blob_store.clone(),
                                    image_library.clone(),
                                    monitor,
                                    clipboard_write_suppressor.clone(),
                                    cx,
                                );
                                if initially_visible {
                                    main_window.prepare_window_shown(cx);
                                }
                                main_window.refresh_tags(cx);
                                main_window.refresh_history(cx);
                                if let Some(events) = events {
                                    main_window.start_clipboard_listener(events, cx);
                                }
                                main_window
                            });
                            view.update(cx, |main_window, cx| {
                                main_window.observe_window_bounds(window, cx);
                            });
                            crate::windows::main::MainWindowService::set_main_view(
                                view.clone(),
                                cx,
                            );
                            cx.new(|cx| crate::window_material::new_root(view, window, cx))
                        }
                    },
                )
                .unwrap_or_else(|error| {
                    tracing::error!("无法创建剪贴板窗口：{error}");
                    panic!("无法创建剪贴板窗口：{error}");
                });
            crate::windows::main::MainWindowService::set_window(
                main_window.into(),
                initially_visible,
                initial_logical_size,
                cx,
            );
            match crate::platform::single_instance::activation_requests(activation_path) {
                Ok(receiver) => cx
                    .spawn(async move |cx| {
                        while let Ok(result) = receiver.recv().await {
                            match result {
                                Ok(()) => {
                                    cx.update(|cx| {
                                        crate::windows::main::show_main_window(cx);
                                        if let Some(window) =
                                            crate::windows::main::MainWindowService::window_handle(
                                                cx,
                                            )
                                        {
                                            let result = window.update(cx, |_, window, cx| {
                                                if crate::config_runtime::clipboard_config(cx)
                                                    .window_position
                                                    .is_screen_edge()
                                                {
                                                    crate::platform::window::activate_window_preserving_bounds(window)
                                                } else {
                                                    window.activate_window();
                                                    Ok(())
                                                }
                                            });
                                            match result {
                                                Ok(Ok(())) => {}
                                                Ok(Err(error)) => tracing::error!("激活已有实例主窗口失败：{error}"),
                                                Err(error) => tracing::error!("激活已有实例主窗口失败：{error}"),
                                            }
                                        }
                                        cx.activate(true);
                                    });
                                }
                                Err(error) => {
                                    tracing::error!("读取单实例窗口显示请求失败：{error}");
                                    break;
                                }
                            }
                        }
                    })
                    .detach(),
                Err(error) => tracing::error!("启动单实例窗口显示监听失败：{error}"),
            }
            crate::services::application_filter::ApplicationFilterService::install(cx);
            #[cfg(target_os = "windows")]
            crate::services::system::check_win_v_on_startup(cx);
            crate::windows::notification::NotificationService::show_startup(cx);
            crate::windows::settings::offer_legacy_migration(cx);
            crate::services::updates::UpdateService::install(
                config_path.with_extension("updates.json"),
                cx,
            );
            tracing::info!("QcClip 启动流程完成");
        });
    tracing::info!("QcClip 正常退出");
}

fn initialize_database(paths: &AppPaths) -> Result<Database, DatabaseError> {
    Database::open(paths.database_path())
}

fn initial_window_size(state: &PersistedMainWindowState) -> (f32, f32) {
    let size = state.restored_size();
    (size.width, size.height)
}

#[cfg(test)]
mod tests {
    use super::{WINDOW_HEIGHT, WINDOW_MIN_SIZE, WINDOW_WIDTH, initial_window_size};
    use crate::state_runtime::{PersistedMainWindowState, PersistedWindowSize};

    #[test]
    fn initial_window_size_uses_persisted_logical_size() {
        let state = PersistedMainWindowState {
            size: Some(PersistedWindowSize {
                width: 640.0,
                height: 720.0,
            }),
            ..PersistedMainWindowState::default()
        };

        assert_eq!(initial_window_size(&state), (640.0, 720.0));
    }

    #[test]
    fn initial_window_size_falls_back_for_missing_or_invalid_size() {
        assert_eq!(
            initial_window_size(&PersistedMainWindowState::default()),
            (WINDOW_WIDTH, WINDOW_HEIGHT)
        );

        for size in [
            PersistedWindowSize {
                width: f32::NAN,
                height: 520.0,
            },
            PersistedWindowSize {
                width: 360.0,
                height: f32::INFINITY,
            },
            PersistedWindowSize {
                width: WINDOW_MIN_SIZE - 1.0,
                height: 520.0,
            },
        ] {
            let state = PersistedMainWindowState {
                size: Some(size),
                ..PersistedMainWindowState::default()
            };
            assert_eq!(initial_window_size(&state), (WINDOW_WIDTH, WINDOW_HEIGHT));
        }
    }
}
