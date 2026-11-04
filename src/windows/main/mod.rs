pub(crate) mod components;
mod file_list;
pub(crate) mod screen_boundary;
mod state;
pub(crate) mod tabs;
mod view;
mod window_drag;
mod window_state;
mod window_visibility;

const SIDEBAR_LAYOUT_MIN_WIDTH: f32 = 500.0;

#[cfg(target_os = "windows")]
use std::time::Duration;

use gpui::{
    AnyWindowHandle, App, AppContext as _, BorrowAppContext as _, Entity, Global, Pixels, Size,
    Window,
};

use crate::{
    config::SessionShortcutId,
    config_runtime,
    domain::clipboard::{ClipboardItemId, HistoryItemId},
    platform::{input, window as platform_window},
};

pub use state::MainWindow;
pub(crate) use window_visibility::sync_mouse_leave_state;

pub(crate) fn uses_sidebar_layout(logical_width: f32) -> bool {
    logical_width >= SIDEBAR_LAYOUT_MIN_WIDTH
}

#[derive(Default)]
pub(crate) struct MainWindowService {
    window: Option<AnyWindowHandle>,
    main_view: Option<Entity<MainWindow>>,
    history_cleanup_pending: bool,
    blocking_operation: Option<&'static str>,
    mouse_event_listener_id: Option<u64>,
    #[cfg(target_os = "windows")]
    pointer_check_sender: Option<async_channel::Sender<()>>,
    window_state: window_state::State,
    pub(crate) memory_trim_state: crate::services::memory_trim::State,
}

impl Global for MainWindowService {}

impl MainWindowService {
    pub(crate) fn install(cx: &mut App) {
        cx.set_global(Self::default());
        cx.on_window_closed(|cx, window_id| {
            let closes_main_window = cx.read_global::<Self, _>(|service, _| {
                service
                    .window
                    .is_some_and(|window| window.window_id() == window_id)
            });
            if closes_main_window {
                cx.update_global::<Self, _>(|service, _| {
                    service.window = None;
                    service.main_view = None;
                    service.window_state.reset();
                    service.memory_trim_state = Default::default();
                    if let Some(subscription_id) = service.mouse_event_listener_id.take() {
                        input::unsubscribe_mouse_events(subscription_id);
                    }
                });
                crate::hotkey::sync_session_shortcuts(false, false, cx);
            }
        })
        .detach();

        // 粘贴可能来自便捷粘贴、快捷键或主窗口自身，统一由粘贴计数变更事件同步列表。
        let pasted_items = crate::services::paste::subscribe_pasted_items();
        cx.spawn(async move |cx| {
            while let Ok(event) = pasted_items.recv().await {
                cx.update(|cx| {
                    let main_view = cx
                        .try_global::<Self>()
                        .and_then(|service| service.main_view.clone());
                    if let Some(main_view) = main_view {
                        main_view.update(cx, |view, cx| view.apply_pasted_item(event, cx));
                    }
                });
            }
        })
        .detach();

        let (subscription_id, receiver) = input::subscribe_mouse_events();
        cx.update_global::<Self, _>(|service, _| {
            service.mouse_event_listener_id = Some(subscription_id);
        });
        cx.spawn(async move |cx| {
            while let Ok(event) = receiver.recv().await {
                cx.update(|cx| match event {
                    input::GlobalMouseEvent::MouseDown {
                        sequence,
                        position,
                        foreground_window,
                    } => {
                        if !crate::windows::menu::MenuService::handle_global_mouse_down(
                            sequence, position, cx,
                        ) {
                            window_visibility::handle_global_mouse_down(
                                position,
                                foreground_window,
                                cx,
                            );
                        }
                    }
                    input::GlobalMouseEvent::MouseUp {
                        position,
                        left_pressed,
                    } if !left_pressed => {
                        if let Some(view) = cx.global::<Self>().main_view.clone() {
                            view.update(cx, |main, cx| main.finish_split_resize(cx));
                        }
                        window_drag::handle_mouse_up(position, cx);
                        Self::request_pointer_check(cx);
                    }
                    input::GlobalMouseEvent::MouseUp { .. } => {}
                });
            }
            tracing::debug!("全局鼠标事件监听任务已退出");
        })
        .detach();
    }

    pub(crate) fn startup_edge_bounds(
        cx: &App,
    ) -> Option<(gpui::Bounds<gpui::Pixels>, gpui::DisplayId)> {
        window_drag::startup_edge_bounds(cx)
    }

    pub(crate) fn set_main_view(main_view: Entity<MainWindow>, cx: &mut App) {
        cx.update_global::<Self, _>(|service, _| service.main_view = Some(main_view));
    }

    pub(crate) fn handle_window_shown(window: &mut Window, cx: &mut App) {
        let main_view = cx
            .try_global::<Self>()
            .and_then(|service| service.main_view.clone());
        if let Some(main_view) = main_view {
            let _ = main_view.update(cx, |view, cx| view.handle_window_shown(window, cx));
        }
    }

    pub(crate) fn prepare_window_shown(cx: &mut App) {
        let main_view = cx
            .try_global::<Self>()
            .and_then(|service| service.main_view.clone());
        if let Some(main_view) = main_view {
            let _ = main_view.update(cx, |view, cx| view.prepare_window_shown(cx));
        }
    }

    pub(crate) fn clipboard_write_suppressor(
        cx: &App,
    ) -> Option<crate::platform::clipboard::ClipboardWriteSuppressor> {
        cx.read_global::<Self, _>(|service, cx| {
            service
                .main_view
                .as_ref()
                .map(|main_view| main_view.read(cx).clipboard_write_suppressor.clone())
        })
    }

    pub(crate) fn refresh_history(cx: &mut App) {
        let main_view = cx
            .try_global::<Self>()
            .and_then(|service| service.main_view.clone());
        if let Some(main_view) = main_view {
            main_view.update(cx, |view, cx| view.refresh_history(cx));
        }
    }

    /// 保存复用列表增量更新，按数据库顺序同步内容和收藏关系。
    pub(crate) fn refresh_saved_item(cx: &mut App, item_id: ClipboardItemId) {
        let main_view = cx
            .try_global::<Self>()
            .and_then(|service| service.main_view.clone());
        if let Some(main_view) = main_view {
            main_view.update(cx, |view, cx| view.refresh_saved_item(item_id, cx));
        }
    }

    pub(crate) fn refresh_captured_history_item(cx: &mut App, history_id: HistoryItemId) {
        let main_view = cx
            .try_global::<Self>()
            .and_then(|service| service.main_view.clone());
        if let Some(main_view) = main_view {
            main_view.update(cx, |view, cx| {
                view.refresh_captured_history_item(history_id, cx)
            });
        }
    }

    pub(crate) fn receive_sync_changes(
        changes: Vec<crate::services::sync::ReceivedChange>,
        cx: &mut App,
    ) {
        let main_view = cx
            .try_global::<Self>()
            .and_then(|service| service.main_view.clone());
        if let Some(main_view) = main_view {
            main_view.update(cx, |view, cx| view.receive_sync_changes(changes, cx));
        }
    }

    pub(crate) fn blocking_operation(cx: &App) -> Option<&'static str> {
        cx.try_global::<Self>()
            .and_then(|service| service.blocking_operation)
    }

    pub(crate) fn begin_blocking_operation(title_key: &'static str, cx: &mut App) -> bool {
        if Self::blocking_operation(cx).is_some() {
            return false;
        }
        cx.update_global::<Self, _>(|service, _| service.blocking_operation = Some(title_key));
        // 调用方可能正在更新主窗口，延后清理以避免重复借用同一实体。
        cx.defer(|cx| {
            crate::windows::menu::MenuService::close_all(cx);
            Self::cancel_hover_preview(cx);
            crate::windows::preview::PreviewWindowService::hide(cx);
            crate::windows::quick_paste::QuickPasteWindowService::cancel(cx);
        });
        cx.refresh_windows();
        true
    }

    pub(crate) fn finish_blocking_operation(cx: &mut App) {
        cx.update_global::<Self, _>(|service, _| service.blocking_operation = None);
        cx.refresh_windows();
    }

    pub(crate) fn confirm_clear_history(keep_pinned: bool, cx: &mut App) {
        use crate::services::data_management::DataManagementService;
        use crate::windows::notification::{NotificationLevel, NotificationService};

        let Some(window) = Self::window_handle(cx) else {
            return;
        };
        if cx.global::<Self>().history_cleanup_pending || Self::blocking_operation(cx).is_some() {
            return;
        }
        let translator = crate::i18n::translator(cx);
        let title = translator.translate(if keep_pinned {
            "main.menu.clear_history_keep_pinned"
        } else {
            "main.menu.clear_history_all"
        });
        let detail = translator.translate(if keep_pinned {
            "main.confirm.clear_history_keep_pinned.detail"
        } else {
            "settings.data_management.clear_all_history_confirm_detail"
        });
        let answer = match window.update(cx, |_, window, cx| {
            window.prompt(
                gpui::PromptLevel::Warning,
                &title,
                Some(&detail),
                &[
                    gpui::PromptButton::cancel(translator.translate("main.form.cancel")),
                    gpui::PromptButton::ok(translator.translate("main.form.confirm")),
                ],
                cx,
            )
        }) {
            Ok(answer) => answer,
            Err(error) => {
                tracing::error!("打开清理剪贴板历史确认提示失败：{error}");
                return;
            }
        };
        cx.update_global::<Self, _>(|service, _| service.history_cleanup_pending = true);
        cx.spawn(async move |cx| {
            if !matches!(answer.await, Ok(1)) {
                cx.update(|cx| {
                    cx.update_global::<Self, _>(|service, _| {
                        service.history_cleanup_pending = false
                    });
                });
                return;
            }
            let started = cx.update(|cx| {
                if Self::begin_blocking_operation("settings.data_management.clear_all_history", cx)
                {
                    true
                } else {
                    cx.update_global::<Self, _>(|service, _| {
                        service.history_cleanup_pending = false
                    });
                    false
                }
            });
            if !started {
                return;
            }
            let service = cx.update(|cx| {
                crate::windows::quick_paste::QuickPasteWindowService::cancel(cx);
                Self::cancel_hover_preview(cx);
                crate::windows::preview::PreviewWindowService::destroy(cx);
                Self::stop_clipboard_listener(cx);
                cx.global::<DataManagementService>().clone()
            });
            let result = cx
                .background_executor()
                .spawn(async move {
                    if keep_pinned {
                        service.clear_unpinned_history()
                    } else {
                        service.clear_all_history()
                    }
                })
                .await;
            cx.update(|cx| {
                cx.update_global::<Self, _>(|service, _| service.history_cleanup_pending = false);
                let (detail, level) = match result {
                    Ok(report) => {
                        Self::refresh_history(cx);
                        (
                            translator.translate_with_args(
                                "settings.data_management.clear_all_history_success",
                                &[("history_count", report.history_items_removed.to_string())],
                            ),
                            NotificationLevel::Success,
                        )
                    }
                    Err(error) => {
                        tracing::error!("清理剪贴板历史失败：{error}");
                        (
                            translator
                                .translate("settings.data_management.clear_all_history_error"),
                            NotificationLevel::Error,
                        )
                    }
                };
                Self::restart_clipboard_listener(cx);
                NotificationService::show_message(title, detail, level, cx);
                Self::finish_blocking_operation(cx);
            });
        })
        .detach();
    }

    pub(crate) fn reload_after_data_import(cx: &mut App) {
        window_visibility::apply_imported_window_state(cx);
        let main_view = cx
            .try_global::<Self>()
            .and_then(|service| service.main_view.clone());
        if let Some(main_view) = main_view {
            main_view.update(cx, |view, cx| view.reload_after_data_import(cx));
        }
    }

    pub(crate) fn replace_data_sources(
        database: crate::services::database::Database,
        blob_store: crate::services::storage::RawBlobStore,
        data_root: &std::path::Path,
        cx: &mut App,
    ) {
        window_visibility::apply_imported_window_state(cx);
        let main_view = cx
            .try_global::<Self>()
            .and_then(|service| service.main_view.clone());
        if let Some(main_view) = main_view {
            main_view.update(cx, |view, cx| {
                view.replace_data_sources(database, blob_store, data_root, cx);
            });
        }
    }

    pub(crate) fn stop_clipboard_listener(cx: &mut App) {
        let main_view = cx
            .try_global::<Self>()
            .and_then(|service| service.main_view.clone());
        if let Some(main_view) = main_view {
            main_view.update(cx, |view, _| view.stop_clipboard_listener());
        }
    }

    pub(crate) fn restart_clipboard_listener(cx: &mut App) {
        let main_view = cx
            .try_global::<Self>()
            .and_then(|service| service.main_view.clone());
        if let Some(main_view) = main_view {
            main_view.update(cx, |view, cx| view.restart_clipboard_listener(cx));
        }
    }

    pub(crate) fn toggle_clipboard_listener(cx: &mut App) {
        if Self::blocking_operation(cx).is_some() {
            return;
        }
        let main_view = cx
            .try_global::<Self>()
            .and_then(|service| service.main_view.clone());
        let Some(main_view) = main_view else {
            tracing::error!("主窗口尚未就绪，无法切换剪贴板监听");
            return;
        };
        let result = main_view.update(cx, |view, cx| view.toggle_clipboard_listener(cx));
        let (title_key, detail_key, level) = match result {
            state::ClipboardListenerToggleResult::Enabled => (
                "notification.clipboard_monitor.enabled.title",
                "notification.clipboard_monitor.enabled.detail",
                crate::windows::notification::NotificationLevel::Success,
            ),
            state::ClipboardListenerToggleResult::Disabled => (
                "notification.clipboard_monitor.disabled.title",
                "notification.clipboard_monitor.disabled.detail",
                crate::windows::notification::NotificationLevel::Info,
            ),
            state::ClipboardListenerToggleResult::Failed => (
                "notification.clipboard_monitor.failed.title",
                "notification.clipboard_monitor.failed.detail",
                crate::windows::notification::NotificationLevel::Error,
            ),
        };
        let translator = crate::i18n::translator(cx);
        crate::windows::notification::NotificationService::show_message_for(
            crate::config::NotificationKind::FeatureStatus,
            translator.translate(title_key),
            translator.translate(detail_key),
            level,
            cx,
        );
        main_view.update(cx, |_, cx| cx.notify());
    }

    pub(crate) fn upload_latest_shortcut(cx: &mut App) {
        Self::run_sync_shortcut(components::sync_buttons::Action::Upload, cx);
    }

    pub(crate) fn pull_latest_shortcut(cx: &mut App) {
        Self::run_sync_shortcut(components::sync_buttons::Action::Pull, cx);
    }

    fn run_sync_shortcut(action: components::sync_buttons::Action, cx: &mut App) {
        let main_view = cx
            .try_global::<Self>()
            .and_then(|service| service.main_view.clone());
        let Some(main_view) = main_view else {
            tracing::error!("主窗口尚未就绪，无法执行同步快捷键");
            return;
        };
        main_view.update(cx, |view, cx| {
            components::sync_buttons::run_shortcut(view, action, cx)
        });
    }

    pub(crate) fn clipboard_listener_active(cx: &App) -> bool {
        cx.try_global::<Self>()
            .and_then(|service| service.main_view.clone())
            .map(|main_view| main_view.read(cx)._clipboard_monitor.is_some())
            .unwrap_or(true)
    }

    pub(crate) fn paste_plain_text_shortcut(cx: &mut App) {
        if Self::blocking_operation(cx).is_some() {
            return;
        }
        let visible = Self::is_logically_visible(cx);
        let main_view = cx
            .try_global::<Self>()
            .and_then(|service| service.main_view.clone());
        let Some(main_view) = main_view else {
            Self::show_plain_text_paste_unavailable(cx);
            return;
        };
        if visible {
            let main_view = main_view
                .read_with(cx, |view, cx| view.active_list_entity(cx))
                .unwrap_or(main_view);
            let into_main_input = crate::services::focus::input_window_is_foreground();
            let started = main_view.update(cx, |view, cx| {
                view.paste_selected_item_as_plain_text(into_main_input, cx)
            });
            if !started {
                Self::show_plain_text_paste_unavailable(cx);
            }
        } else {
            main_view.update(cx, |view, cx| view.paste_latest_item_as_plain_text(cx));
        }
    }

    pub(crate) fn paste_history_index(index: usize, cx: &mut App) {
        if Self::blocking_operation(cx).is_some() {
            return;
        }
        let main_view = cx
            .try_global::<Self>()
            .and_then(|service| service.main_view.clone());
        if let Some(main_view) = main_view {
            let main_view = main_view
                .read_with(cx, |view, cx| view.active_list_entity(cx))
                .unwrap_or(main_view);
            let into_main_input = Self::is_logically_visible(cx)
                && crate::services::focus::input_window_is_foreground();
            main_view.update(cx, |view, cx| {
                view.paste_history_index(index, into_main_input, cx)
            });
        }
    }

    pub(crate) fn show_plain_text_paste_unavailable(cx: &mut App) {
        let translator = crate::i18n::translator(cx);
        crate::windows::notification::NotificationService::show_message(
            translator.translate("notification.plain_text_paste.unavailable.title"),
            translator.translate("notification.plain_text_paste.unavailable.detail"),
            crate::windows::notification::NotificationLevel::Warning,
            cx,
        );
    }

    pub(crate) fn window_handle(cx: &App) -> Option<AnyWindowHandle> {
        cx.try_global::<Self>().and_then(|service| service.window)
    }

    pub(crate) fn set_window(
        window: AnyWindowHandle,
        initially_visible: bool,
        initial_logical_size: Size<Pixels>,
        cx: &mut App,
    ) {
        cx.update_global::<Self, _>(|service, _| {
            service.window = Some(window);
            service.window_state.set_visibility(initially_visible);
        });
        if config_runtime::clipboard_config(cx).edge_hide {
            window_visibility::initialize_edge_hide_enabled(cx);
        }
        cx.defer(move |cx| {
            let result = window.update(cx, |_, window, cx| {
                window.on_window_should_close(cx, |_, cx| {
                    if config_runtime::clipboard_config(cx)
                        .window_position
                        .is_screen_edge()
                    {
                        cx.defer(hide_main_window_explicitly);
                        false
                    } else {
                        true
                    }
                });
                let position_result =
                    window_visibility::position_initial_window(window, initial_logical_size, cx);
                let hide_result =
                    window_visibility::hide_initial_window(window, initially_visible, cx);
                let result = position_result.and(hide_result);
                if initially_visible && result.is_ok() {
                    Self::handle_window_shown(window, cx);
                }
                result
            });
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::error!("定位主窗口失败：{error}"),
                Err(error) => tracing::error!("定位主窗口失败：{error}"),
            }
        });
        crate::services::memory_trim::schedule_startup_cleanup(cx);

        #[cfg(target_os = "windows")]
        {
            let (sender, receiver) = async_channel::bounded(1);
            cx.update_global::<Self, _>(|service, _| service.pointer_check_sender = Some(sender));
            Self::sync_pointer_change_notifier(cx);
            cx.spawn(async move |cx| {
                while receiver.recv().await.is_ok() {
                    cx.background_executor()
                        .timer(Duration::from_millis(16))
                        .await;
                    let _ = receiver.try_recv();
                    let Some(window_is_open) =
                        cx.try_read_global::<MainWindowService, _>(|service, _| {
                            service.window.is_some()
                        })
                    else {
                        break;
                    };
                    if !window_is_open {
                        break;
                    }
                    let Some(position) =
                        input::pointer_snapshot().map(|snapshot| snapshot.position)
                    else {
                        continue;
                    };
                    let Some((visible, edge_attached)) =
                        cx.try_read_global::<Self, _>(|service, _| {
                            (
                                service.window_state.is_logically_visible(),
                                service.window_state.edge_attachment().is_some(),
                            )
                        })
                    else {
                        break;
                    };
                    // 普通隐藏窗口不需要鼠标移动轮询；贴边隐藏窗口仍需检测边缘唤出。
                    if !visible && !edge_attached {
                        continue;
                    }
                    if visible {
                        cx.update(|cx| {
                            let (visible, pointer_inside) = Self::session_shortcut_scope(cx);
                            crate::hotkey::sync_session_shortcuts(visible, pointer_inside, cx);
                        });
                    }
                    // 输入和窗口状态变化都会唤醒检查，指针静止时无需持续轮询。
                    let drag_request = cx.update(|cx| {
                        if visible {
                            window_drag::sync_edge_bounds_after_native_move(cx);
                        }
                        let drag_request = if visible {
                            crate::platform::external_drag::ExternalDragService::prepare_poll(cx)
                        } else {
                            None
                        };
                        let position = input::pointer_snapshot()
                            .map(|snapshot| snapshot.position)
                            .unwrap_or(position);
                        window_visibility::handle_mouse_move(position, cx);
                        drag_request
                    });
                    if let Some(request) = drag_request {
                        // DoDragDrop 会同步泵送 Windows 消息，因此不能持有 GPUI App 借用。
                        let completion =
                            crate::platform::external_drag::ExternalDragService::execute(request);
                        cx.update(|cx| {
                            crate::platform::external_drag::ExternalDragService::finish(
                                completion, cx,
                            );
                        });
                    }
                }
            })
            .detach();
        }
    }

    pub(crate) fn request_pointer_check(cx: &App) {
        #[cfg(target_os = "windows")]
        if let Some(sender) = cx
            .try_global::<Self>()
            .and_then(|service| service.pointer_check_sender.as_ref())
        {
            let _ = sender.try_send(());
        }
        #[cfg(not(target_os = "windows"))]
        let _ = cx;
    }

    pub(crate) fn cancel_hover_preview(cx: &mut App) {
        let Some(main_view) = cx
            .try_global::<Self>()
            .and_then(|service| service.main_view.clone())
        else {
            return;
        };
        main_view.update(cx, |main_window, cx| {
            main_window.cancel_hover_preview(cx);
            main_window.cancel_split_previews(cx);
        });
    }

    pub(crate) fn notify_main_view(cx: &mut App) {
        let Some(main_view) = cx
            .try_global::<Self>()
            .and_then(|service| service.main_view.clone())
        else {
            return;
        };
        let _ = main_view.update(cx, |_, cx| cx.notify());
    }

    pub(crate) fn open_new_text_editor(cx: &mut App) {
        let Some(main_view) = cx
            .try_global::<Self>()
            .and_then(|service| service.main_view.clone())
        else {
            tracing::error!("主窗口尚未就绪，无法打开新增文本窗口");
            return;
        };
        main_view.update(cx, |main_window, cx| {
            main_window.new_text_item(cx);
        });
    }

    pub(crate) fn is_logically_visible(cx: &App) -> bool {
        cx.global::<Self>().window_state.is_logically_visible()
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn sync_pointer_change_notifier(cx: &App) {
        let (notifier, hidden_attachment) = cx.read_global::<Self, _>(|service, _| {
            let visible = service.window_state.is_logically_visible();
            let needs_pointer_checks = service.window.is_some()
                && (visible || service.window_state.edge_attachment().is_some());
            (
                needs_pointer_checks
                    .then(|| service.pointer_check_sender.as_ref().cloned())
                    .flatten(),
                service.window_state.edge_attachment().filter(|_| !visible),
            )
        });
        let region = hidden_attachment
            .map(|attachment| window_visibility::hidden_edge_trigger_region(attachment, cx));
        input::set_pointer_change_notifier(notifier, region);
    }

    pub(crate) fn session_shortcut_scope(cx: &mut App) -> (bool, bool) {
        let (window, visible) = cx.read_global::<Self, _>(|service, _| {
            (service.window, service.window_state.is_logically_visible())
        });
        let Some(window) = window.filter(|_| visible) else {
            return (visible, false);
        };
        let pointer = input::pointer_snapshot().map(|snapshot| snapshot.position);
        let inside = pointer.is_some_and(|position| {
            window
                .update(cx, |_, window, _| {
                    platform_window::physical_client_bounds(window).is_ok_and(|bounds| {
                        window_visibility::point_in_client_bounds(position, bounds)
                    })
                })
                .unwrap_or(false)
        });
        (visible, inside)
    }

    pub(crate) fn set_window_fixed(fixed: bool, cx: &mut App) {
        cx.update_global::<Self, _>(|service, _| service.window_state.set_fixed(fixed));
    }

    pub(crate) fn set_edge_hide_enabled(enabled: bool, cx: &mut App) {
        // 当前窗口事件结束前，GPUI 会暂时移出当前窗口；等待窗口归还后再按句柄更新。
        cx.defer(move |cx| window_visibility::set_edge_hide_enabled(enabled, cx));
    }

    pub(crate) fn refresh_edge_hide_position(cx: &mut App) {
        window_visibility::refresh_edge_hide_position(cx);
    }

    pub(crate) fn refresh_window_position(cx: &mut App) {
        cx.defer(window_visibility::refresh_window_position);
    }

    pub(crate) fn set_edge_hide_temporarily_blocked(blocked: bool, cx: &mut App) {
        window_visibility::set_edge_hide_temporarily_blocked(blocked, cx);
    }
}

pub fn show_main_window(cx: &mut App) {
    if let Err(error) =
        window_visibility::set_visibility(true, window_visibility::VisibilitySource::Implicit, cx)
    {
        tracing::error!("显示主窗口失败：{error}");
    }
}

/// 由交互顺带触发的隐藏（例如粘贴后隐藏），固定窗口状态下不会隐藏。
pub fn hide_main_window(cx: &mut App) {
    if let Err(error) =
        window_visibility::set_visibility(false, window_visibility::VisibilitySource::Implicit, cx)
    {
        tracing::error!("隐藏主窗口失败：{error}");
    }
}

/// 主窗口发起粘贴前的统一准备：未固定时隐藏主窗口（固定时保持显示），
/// 再把焦点交还给进入主窗口前的目标窗口。`action` 用于日志，如“列表粘贴”。
/// 只恢复主窗口自身输入框的焦点会话；便签等其他窗口的输入框持有焦点时粘贴到该输入框。
pub(crate) fn prepare_paste_target(action: &str, cx: &mut App) {
    hide_main_window(cx);
    let restored = MainWindowService::window_handle(cx).map(|window| {
        window
            .update(cx, |_, window, _| {
                crate::services::focus::restore_previous_focus_for(window)
            })
            .map_err(|error| error.to_string())
            .flatten()
    });
    if let Some(Err(error)) = restored {
        tracing::error!("{action}前恢复窗口焦点失败：{error}");
    }
    if let Err(error) = crate::services::focus::ensure_external_paste_target() {
        tracing::error!("{action}前交还前台窗口失败：{error}");
    }
}

pub fn toggle_main_window(cx: &mut App) {
    if let Err(error) = window_visibility::toggle_visibility(cx) {
        tracing::error!("切换主窗口失败：{error}");
    }
}

/// 切换到指定页面后显示主窗口；`symbol_category` 仅在表情页下生效。
pub(crate) fn show_main_window_page(
    tab: tabs::TabId,
    symbol_category: Option<tabs::SymbolCategoryId>,
    cx: &mut App,
) {
    let main_view = cx
        .try_global::<MainWindowService>()
        .and_then(|service| service.main_view.clone());
    let Some(main_view) = main_view else {
        tracing::error!("主窗口尚未就绪，无法打开指定页面");
        return;
    };
    main_view.update(cx, |main, cx| {
        main.select_tab(tab, cx);
        if let Some(category) = symbol_category {
            main.select_symbol_category(category, cx);
        }
    });
    if let Err(error) = window_visibility::set_visibility(
        true,
        window_visibility::VisibilitySource::ExplicitToggle,
        cx,
    ) {
        tracing::error!("显示主窗口失败：{error}");
    }
}

pub fn hide_main_window_explicitly(cx: &mut App) {
    if let Err(error) = window_visibility::set_visibility(
        false,
        window_visibility::VisibilitySource::ExplicitToggle,
        cx,
    ) {
        tracing::error!("主动隐藏主窗口失败：{error}");
    }
}

pub(crate) fn dispatch_session_shortcut(id: SessionShortcutId, cx: &mut App) {
    if MainWindowService::blocking_operation(cx).is_some() {
        return;
    }
    if id == SessionShortcutId::HideWindow {
        hide_main_window_explicitly(cx);
        return;
    }
    let Some((window, main_view)) = cx
        .try_global::<MainWindowService>()
        .and_then(|service| Some((service.window?, service.main_view.clone()?)))
    else {
        return;
    };
    let list_action = !matches!(
        id,
        SessionShortcutId::PreviousTab
            | SessionShortcutId::NextTab
            | SessionShortcutId::ToggleWindowFixed
            | SessionShortcutId::FocusSearch
    );
    let main_view = if list_action {
        main_view
            .read_with(cx, |main, cx| main.active_list_entity(cx))
            .unwrap_or(main_view)
    } else {
        main_view
    };
    let favorite_removal =
        main_view.read_with(cx, |main, _| main.favorite_removal_target_for_shortcut(id));
    if let Some((favorite_id, history_id)) = favorite_removal {
        crate::windows::main::components::clipboard_list::confirm_remove_from_favorites(
            main_view,
            window,
            favorite_id,
            history_id,
            cx,
        );
        return;
    }
    if let Err(error) = window.update(cx, |_, window, cx| {
        main_view.update(cx, |main, cx| match id {
            SessionShortcutId::SelectPreviousItem => main.move_selection(-1, window, cx),
            SessionShortcutId::SelectNextItem => main.move_selection(1, window, cx),
            SessionShortcutId::SelectFirstItem => main.select_list_boundary(false, window, cx),
            SessionShortcutId::SelectLastItem => main.select_list_boundary(true, window, cx),
            SessionShortcutId::PreviousTab => main.move_tab(-1, cx),
            SessionShortcutId::NextTab => main.move_tab(1, cx),
            SessionShortcutId::ToggleWindowFixed => {
                main.titlebar_window_fixed = !main.titlebar_window_fixed;
                MainWindowService::set_window_fixed(main.titlebar_window_fixed, cx);
                cx.notify();
            }
            SessionShortcutId::FocusSearch => {
                main.toggle_search_input(window, cx);
            }
            SessionShortcutId::ExecutePaste => main.paste_selected_item(cx),
            SessionShortcutId::ToggleFavorite => main.toggle_selected_favorite(cx),
            SessionShortcutId::TogglePinned => main.toggle_selected_pinned(cx),
            SessionShortcutId::PinToScreen => main.pin_selected_to_screen(false, cx),
            SessionShortcutId::SplitWords => main.pin_selected_to_screen(true, cx),
            SessionShortcutId::DeleteItem => main.delete_selected_item(cx),
            SessionShortcutId::HideWindow => {}
        });
    }) {
        tracing::debug!(%error, "执行窗口快捷键失败");
    }
}
