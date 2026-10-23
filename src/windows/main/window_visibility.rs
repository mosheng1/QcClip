use std::time::{Duration, Instant};

use gpui::{App, AppContext as _, BorrowAppContext as _, Pixels, Point, Size, px, size};

use crate::{
    config::ClipboardWindowPosition,
    config_runtime,
    platform::{input, window as platform_window},
    state_runtime::{self, PersistedEdgePosition, PersistedEdgeSide, PersistedWindowPosition},
    windows::main::{MainWindowService, window_drag, window_state},
};

const EDGE_REVEAL_SIZE_LOGICAL: f32 = 1.0;
const EDGE_TRIGGER_SIZE_LOGICAL: f32 = 2.0;
const EDGE_ANIMATION_DURATION: Duration = Duration::from_millis(200);
const NORMAL_SHOW_ANIMATION_DURATION: Duration = Duration::from_millis(400);
const NORMAL_HIDE_ANIMATION_DURATION: Duration = Duration::from_millis(140);
const COLLAPSED_WINDOW_HEIGHT_LOGICAL: f32 = 1.0;
const NORMAL_SHOW_SETTLE_START: f32 = 0.7;
const NEARBY_WINDOW_GAP_LOGICAL: f32 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutsideClickAction {
    Ignore,
    Hide,
}

/// 可见性切换的来源。
///
/// 固定窗口只拦截“顺带”发生的隐藏（粘贴后隐藏、点击外部、贴边隐藏等），
/// 快捷键与托盘图标/菜单这类明确切换可见状态的操作不受影响。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VisibilitySource {
    ExplicitToggle,
    Implicit,
}

pub(crate) fn initialize_edge_hide_enabled(cx: &mut App) {
    if config_runtime::clipboard_config(cx)
        .window_position
        .is_screen_edge()
    {
        return;
    }
    let window = cx.read_global::<MainWindowService, _>(|service, _| service.window);
    let persisted_edge = state_runtime::state(cx).main_window.edge;
    let Some(window) = window else {
        sync_mouse_leave_state(cx);
        return;
    };
    let result = window.update(cx, |_, window, cx| {
        let Some(persisted_edge) = persisted_edge else {
            return Ok::<Option<window_state::EdgeAttachment>, String>(None);
        };
        let Some(attachment) =
            window_drag::restore_persisted_edge(window, Some(persisted_edge), cx)?
        else {
            return Ok(None);
        };
        platform_window::install_window_drag_constraints_hook(
            window,
            window_drag::native_move_started,
            window_drag::native_move_finished,
        )?;
        Ok(Some(attachment))
    });
    match result {
        Ok(Ok(Some(attachment))) => attach_edge_and_remember(attachment, cx),
        Ok(Ok(None)) => {}
        Ok(Err(error)) => tracing::error!("启动时恢复主窗口贴边失败：{error}"),
        Err(error) => tracing::error!("启动时恢复主窗口贴边失败：{error}"),
    }
    sync_mouse_leave_state(cx);
}

pub(crate) fn apply_imported_window_state(cx: &mut App) {
    let persisted = state_runtime::state(cx).main_window;
    let window = cx.read_global::<MainWindowService, _>(|service, _| service.window);
    let Some(window) = window else {
        return;
    };
    cx.update_global::<MainWindowService, _>(|service, _| {
        service.window_state.disable_edge_mode();
        service.window_state.set_panel_attachment(None);
    });

    let result = window.update(cx, |_, window, cx| {
        let logical_size = persisted.restored_size();
        let logical_size = size(px(logical_size.width), px(logical_size.height));
        // Window::resize 在 Windows 上异步调整原生窗口，不能与下面的同步客户区定位并用，
        // 否则异步尺寸更新可能覆盖贴边窗口的尺寸，导致隐藏时露出过多内容。

        if config_runtime::clipboard_config(cx).edge_hide
            && let Some(edge) = persisted.edge
            && let Some(attachment) = window_drag::restore_persisted_edge(window, Some(edge), cx)?
        {
            platform_window::install_window_drag_constraints_hook(
                window,
                window_drag::native_move_started,
                window_drag::native_move_finished,
            )?;
            return Ok::<_, String>(Some(attachment));
        }

        position_initial_window(window, logical_size, cx)?;
        Ok(None)
    });

    match result {
        Ok(Ok(Some(attachment))) => {
            attach_edge_and_remember(attachment, cx);
            synchronize_attached_edge_window(attachment, cx);
        }
        Ok(Ok(None)) => {}
        Ok(Err(error)) => tracing::error!("应用导入的主窗口状态失败：{error}"),
        Err(error) => tracing::error!("应用导入的主窗口状态失败：{error}"),
    }
}

pub(crate) fn set_edge_hide_enabled(enabled: bool, cx: &mut App) {
    if config_runtime::clipboard_config(cx)
        .window_position
        .is_screen_edge()
        && (enabled
            || cx
                .global::<MainWindowService>()
                .window_state
                .edge_attachment()
                .is_none())
    {
        return;
    }
    if enabled {
        let (window, should_restore_normal_bounds, normal_bounds) = cx
            .read_global::<MainWindowService, _>(|service, _| {
                (
                    service.window,
                    service.window_state.edge_attachment().is_none()
                        && (!service.window_state.is_logically_visible()
                            || service.window_state.is_visibility_transition_active()),
                    service.window_state.normal_bounds(),
                )
            });
        let persisted_edge = state_runtime::state(cx).main_window.edge;
        if let Some(window) = window {
            let result = window.update(cx, |_, window, cx| {
                // 普通窗口隐藏或正在执行隐藏动画时，原生窗口可能已经收起到中间高度。
                // 先暂时恢复完整尺寸再判断是否贴边，避免把动画中的尺寸保存为贴边尺寸。
                let original_bounds = if should_restore_normal_bounds {
                    let original_bounds = platform_window::physical_client_bounds(window)?;
                    if let Some(normal_bounds) = normal_bounds
                        && normal_bounds != original_bounds
                    {
                        platform_window::set_client_bounds(window, normal_bounds)?;
                        window.bounds_changed(cx);
                    }
                    Some(original_bounds)
                } else {
                    None
                };
                platform_window::install_window_drag_constraints_hook(
                    window,
                    window_drag::native_move_started,
                    window_drag::native_move_finished,
                )?;
                let attachment_result =
                    match window_drag::restore_persisted_edge(window, persisted_edge, cx) {
                        Ok(Some(attachment)) => Ok(Some(attachment)),
                        Ok(None) => window_drag::attach_if_near_edge(window, cx),
                        Err(error) => {
                            tracing::error!(
                                "恢复主窗口贴边位置失败，将按当前窗口位置贴边：{error}"
                            );
                            window_drag::attach_if_near_edge(window, cx)
                        }
                    };
                match attachment_result {
                    Ok(Some(attachment)) => Ok(Some(attachment)),
                    Ok(None) => {
                        if let Some(original_bounds) = original_bounds {
                            platform_window::set_client_bounds(window, original_bounds)?;
                            window.bounds_changed(cx);
                        }
                        Ok(None)
                    }
                    Err(error) => {
                        if let Some(original_bounds) = original_bounds {
                            platform_window::set_client_bounds(window, original_bounds)?;
                            window.bounds_changed(cx);
                        }
                        Err(error)
                    }
                }
            });
            match result {
                Ok(Ok(Some(attachment))) => {
                    attach_edge_and_remember(attachment, cx);
                    synchronize_attached_edge_window(attachment, cx);
                }
                Ok(Ok(None)) => {}
                Ok(Err(error)) => tracing::error!("启用贴边隐藏时设置窗口拖拽边界失败：{error}"),
                Err(error) => tracing::error!("启用贴边隐藏时设置窗口拖拽边界失败：{error}"),
            }
        }
        sync_mouse_leave_state(cx);
        return;
    }

    let (window, attachment) = cx.read_global::<MainWindowService, _>(|service, _| {
        (service.window, service.window_state.edge_attachment())
    });
    if let Some(window) = window {
        let result = window.update(cx, |_, window, _| {
            platform_window::set_window_shadow_visible(window, true)?;
            if let Some(attachment) = attachment {
                platform_window::set_client_position(
                    window,
                    attachment.client_bounds.left,
                    attachment.client_bounds.top,
                )?;
            }
            platform_window::restore_window_drag_constraints_hook(window)?;
            Ok::<(), String>(())
        });
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::error!("关闭贴边隐藏后恢复主窗口失败：{error}"),
            Err(error) => tracing::error!("关闭贴边隐藏后恢复主窗口失败：{error}"),
        }
    }

    cx.update_global::<MainWindowService, _>(|service, _| {
        service.window_state.disable_edge_mode();
    });
    synchronize_disabled_edge_window(attachment, cx);
    state_runtime::set_main_window_edge(None, cx);
    sync_mouse_leave_state(cx);
}

fn synchronize_disabled_edge_window(
    previous_attachment: Option<window_state::EdgeAttachment>,
    cx: &mut App,
) {
    let (window, visible, normal_bounds) = cx.read_global::<MainWindowService, _>(|service, _| {
        (
            service.window,
            service.window_state.is_logically_visible(),
            service.window_state.normal_bounds(),
        )
    });
    let Some(window) = window else {
        return;
    };
    let result = window.update(cx, |_, window, cx| {
        platform_window::set_window_rendering_enabled(window, true)?;
        if let Some(attachment) = previous_attachment {
            // 关闭贴边后，贴边窗口回到当前贴边位置并保持显示。
            platform_window::set_client_position(
                window,
                attachment.client_bounds.left,
                attachment.client_bounds.top,
            )?;
            platform_window::set_window_opacity(window, 1.0)?;
            platform_window::set_window_shadow_visible(window, true)?;
            platform_window::set_window_visible(window, true)?;
            platform_window::raise_window_to_topmost(window)?;
            return Ok::<(), String>(());
        }

        let bounds = normal_bounds.unwrap_or(platform_window::physical_client_bounds(window)?);
        if visible {
            platform_window::set_client_bounds(window, bounds)?;
            window.bounds_changed(cx);
            platform_window::set_window_opacity(window, 1.0)?;
            platform_window::set_window_shadow_visible(window, true)?;
            platform_window::set_window_visible(window, true)?;
            platform_window::raise_window_to_topmost(window)?;
        } else {
            let collapsed = collapsed_bounds(
                bounds,
                platform_window::CoordinateConverter::new(window.scale_factor())
                    .logical_to_physical_ceil_i32(COLLAPSED_WINDOW_HEIGHT_LOGICAL),
            );
            platform_window::set_client_bounds(window, collapsed)?;
            window.bounds_changed(cx);
            platform_window::set_window_opacity(window, 0.0)?;
            platform_window::set_window_shadow_visible(window, true)?;
            platform_window::set_window_visible(window, false)?;
        }
        Ok::<(), String>(())
    });
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::error!("同步关闭贴边后的窗口状态失败：{error}"),
        Err(error) => tracing::error!("同步关闭贴边后的窗口状态失败：{error}"),
    }
}

pub(crate) fn refresh_edge_hide_position(cx: &mut App) {
    let (window, attachment, visible, transition_active) =
        cx.read_global::<MainWindowService, _>(|service, _| {
            (
                service.window,
                service.window_state.edge_attachment(),
                service.window_state.is_logically_visible(),
                service.window_state.is_visibility_transition_active(),
            )
        });
    if visible || transition_active {
        return;
    }
    let (Some(window), Some(attachment)) = (window, attachment) else {
        return;
    };
    let show_edge = config_runtime::clipboard_config(cx).edge_hide_show_edge;
    let result = window.update(cx, |_, window, _| {
        let hidden = hidden_bounds(
            attachment,
            edge_reveal_size(window.scale_factor(), show_edge),
        );
        platform_window::set_client_position(window, hidden.left, hidden.top)
    });
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::error!("更新贴边隐藏边缘显示位置失败：{error}"),
        Err(error) => tracing::error!("更新贴边隐藏边缘显示位置失败：{error}"),
    }
}

fn synchronize_attached_edge_window(attachment: window_state::EdgeAttachment, cx: &mut App) {
    let (window, visible) = cx.read_global::<MainWindowService, _>(|service, _| {
        (service.window, service.window_state.is_logically_visible())
    });
    let Some(window) = window else {
        return;
    };
    let show_edge = config_runtime::clipboard_config(cx).edge_hide_show_edge;
    let result = window.update(cx, |_, window, _| {
        platform_window::set_window_rendering_enabled(window, visible)?;
        let bounds = if visible {
            attachment.client_bounds
        } else {
            hidden_bounds(
                attachment,
                edge_reveal_size(window.scale_factor(), show_edge),
            )
        };
        platform_window::set_client_position(window, bounds.left, bounds.top)?;
        platform_window::set_window_opacity(window, 1.0)?;
        platform_window::set_window_shadow_visible(window, visible)?;
        // 贴边隐藏必须让 HWND 保持可见，仅通过屏幕坐标实现隐藏。
        platform_window::set_window_visible(window, true)?;
        if visible {
            platform_window::raise_window_to_topmost(window)?;
        }
        Ok::<(), String>(())
    });
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::error!("同步贴边窗口状态失败：{error}"),
        Err(error) => tracing::error!("同步贴边窗口状态失败：{error}"),
    }
}

#[derive(Clone, Copy, Debug)]
enum VisibilityAnimation {
    Edge {
        from: platform_window::PhysicalClientBounds,
        to: platform_window::PhysicalClientBounds,
        show: bool,
        hide_window: bool,
    },
    Normal {
        left: i32,
        top: i32,
        right: i32,
        from_height: i32,
        to_height: i32,
        show: bool,
    },
}

pub(crate) fn refresh_window_position(cx: &mut App) {
    window_drag::sync_window_size_after_native_resize(cx);
    record_normal_position(cx);
    let (window, visible, previous_panel, normal_bounds) =
        cx.read_global::<MainWindowService, _>(|service, _| {
            (
                service.window,
                service.window_state.is_logically_visible(),
                service.window_state.panel_attachment(),
                service.window_state.normal_bounds(),
            )
        });
    let Some(window) = window else { return };
    let restored = state_runtime::state(cx).main_window.restored_size();
    cx.update_global::<MainWindowService, _>(|service, _| {
        service.window_state.disable_edge_mode();
        service.window_state.set_panel_attachment(None);
        service.window_state.set_visibility(visible);
    });
    let result = window.update(cx, |_, window, cx| {
        platform_window::restore_window_drag_constraints_hook(window)?;
        if previous_panel.is_some()
            && !config_runtime::clipboard_config(cx)
                .window_position
                .is_screen_edge()
            && let Some(bounds) = normal_bounds
        {
            platform_window::set_client_bounds(window, bounds)?;
        }
        position_initial_window(window, size(px(restored.width), px(restored.height)), cx)?;
        if visible {
            platform_window::set_window_opacity(window, 1.0)?;
            platform_window::set_window_shadow_visible(window, true)?;
            platform_window::set_window_rendering_enabled(window, true)?;
            platform_window::set_window_visible(window, true)?;
        } else {
            hide_initial_window(window, true, cx)?;
        }
        window.refresh();
        Ok::<_, String>(())
    });
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::error!("更新主窗口定位失败：{error}"),
        Err(error) => tracing::error!("更新主窗口定位失败：{error}"),
    }
    MainWindowService::notify_main_view(cx);
    if config_runtime::clipboard_config(cx).edge_hide {
        set_edge_hide_enabled(true, cx);
    }
    sync_mouse_leave_state(cx);
}

fn panel_attachment_for_bounds(
    position: ClipboardWindowPosition,
    bounds: platform_window::PhysicalClientBounds,
    cx: &App,
) -> Option<window_state::EdgeAttachment> {
    let side = match position {
        ClipboardWindowPosition::ScreenLeftCenter => window_state::EdgeSide::Left,
        ClipboardWindowPosition::ScreenRightCenter => window_state::EdgeSide::Right,
        ClipboardWindowPosition::ScreenTopCenter => window_state::EdgeSide::Top,
        ClipboardWindowPosition::ScreenBottomCenter => window_state::EdgeSide::Bottom,
        _ => return None,
    };
    let display = platform_window::display_for_physical_point(
        cx,
        Point {
            x: (bounds.left as f32 + bounds.right as f32) / 2.0,
            y: (bounds.top as f32 + bounds.bottom as f32) / 2.0,
        },
    )?;
    Some(window_state::EdgeAttachment {
        side,
        client_bounds: bounds,
        screen_bounds: display.bounds,
    })
}

pub(crate) fn position_initial_window(
    window: &mut gpui::Window,
    initial_logical_size: Size<Pixels>,
    cx: &mut App,
) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let has_edge_attachment = cx.read_global::<MainWindowService, _>(|service, _| {
            service.window_state.edge_attachment().is_some()
        });
        if has_edge_attachment {
            return Ok(());
        }

        let current_bounds = platform_window::physical_client_bounds(window)?;
        let initial_converter = platform_window::CoordinateConverter::new(window.scale_factor());
        let initial_bounds = platform_window::PhysicalClientBounds {
            left: current_bounds.left,
            top: current_bounds.top,
            right: current_bounds.left.saturating_add(
                initial_converter.logical_to_physical_i32(f32::from(initial_logical_size.width)),
            ),
            bottom: current_bounds.top.saturating_add(
                initial_converter.logical_to_physical_i32(f32::from(initial_logical_size.height)),
            ),
        };
        let position = config_runtime::clipboard_config(cx).window_position;
        let remembered = state_runtime::state(cx).main_window.normal_position;
        let (bounds, _) = normal_bounds_for_position(
            initial_bounds,
            (
                f32::from(initial_logical_size.width),
                f32::from(initial_logical_size.height),
            ),
            position,
            remembered,
            window.scale_factor(),
            cx,
        );
        if bounds != current_bounds {
            platform_window::set_client_bounds(window, bounds)?;
        }
        window.bounds_changed(cx);
        if position.is_screen_edge() {
            if cx
                .global::<MainWindowService>()
                .window_state
                .normal_bounds()
                .is_none()
            {
                remember_normal_geometry(
                    initial_bounds,
                    (
                        f32::from(initial_logical_size.width),
                        f32::from(initial_logical_size.height),
                    ),
                    cx,
                );
            }
            let panel = panel_attachment_for_bounds(position, bounds, cx);
            cx.update_global::<MainWindowService, _>(|service, _| {
                service.window_state.set_panel_attachment(panel);
            });
            return Ok(());
        }
        remember_normal_geometry(
            bounds,
            (
                f32::from(initial_logical_size.width),
                f32::from(initial_logical_size.height),
            ),
            cx,
        );
        record_normal_position_from_window(window, cx)?;
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (window, initial_logical_size, cx);
        Ok(())
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn hide_initial_window(
    window: &mut gpui::Window,
    initially_visible: bool,
    cx: &mut App,
) -> Result<(), String> {
    let attachment =
        cx.read_global::<MainWindowService, _>(|service, _| service.window_state.edge_attachment());
    let panel = cx
        .global::<MainWindowService>()
        .window_state
        .panel_attachment();
    if let Some(panel) = panel {
        platform_window::set_client_bounds(window, hidden_bounds(panel, 0))?;
        window.bounds_changed(cx);
        platform_window::set_window_shadow_visible(window, false)?;
        platform_window::set_window_visible(window, false)?;
    } else if let Some(attachment) = attachment {
        let hidden = hidden_bounds(
            attachment,
            edge_reveal_size(
                window.scale_factor(),
                config_runtime::clipboard_config(cx).edge_hide_show_edge,
            ),
        );
        platform_window::set_client_position(window, hidden.left, hidden.top)?;
        platform_window::set_window_shadow_visible(window, false)?;
        // 贴边隐藏必须让 HWND 保持可见，仅通过屏幕坐标隐藏窗口。
        platform_window::set_window_visible(window, true)?;
    } else if initially_visible {
        // 贴边恢复失败时，窗口是按可见状态创建的，按普通窗口隐藏兜底，
        // 避免全局状态已经隐藏但 HWND 仍然留在屏幕上。
        platform_window::set_window_visible(window, false)?;
    }
    cx.update_global::<MainWindowService, _>(|service, _| {
        service.window_state.cancel_visibility_transition();
        service.window_state.set_visibility(false);
    });
    platform_window::set_window_rendering_enabled(window, false)?;
    crate::services::memory_trim::schedule(cx);
    sync_mouse_leave_state(cx);
    Ok(())
}

pub(crate) fn set_edge_hide_temporarily_blocked(blocked: bool, cx: &mut App) {
    cx.update_global::<MainWindowService, _>(|service, _| {
        service.window_state.set_edge_reveal_blocked(blocked);
    });
    MainWindowService::request_pointer_check(cx);
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn hide_initial_window(
    _window: &mut gpui::Window,
    _initially_visible: bool,
    cx: &mut App,
) -> Result<(), String> {
    cx.update_global::<MainWindowService, _>(|service, _| {
        service.window_state.cancel_visibility_transition();
        service.window_state.set_visibility(false);
    });
    crate::services::memory_trim::schedule(cx);
    sync_mouse_leave_state(cx);
    Ok(())
}

pub(crate) fn attach_edge_and_remember(attachment: window_state::EdgeAttachment, cx: &mut App) {
    cx.update_global::<MainWindowService, _>(|service, _| {
        service.window_state.attach_edge(attachment);
    });
    #[cfg(target_os = "windows")]
    MainWindowService::sync_pointer_change_notifier(cx);
    persist_edge_position(attachment, cx);
}

pub(crate) fn persist_edge_position(attachment: window_state::EdgeAttachment, cx: &mut App) {
    let center = Point {
        x: (attachment.client_bounds.left + attachment.client_bounds.right) as f32 / 2.0,
        y: (attachment.client_bounds.top + attachment.client_bounds.bottom) as f32 / 2.0,
    };
    let Some(display) = platform_window::display_for_physical_point(cx, center) else {
        return;
    };
    let converter = platform_window::CoordinateConverter::new(display.scale_factor);
    let offset =
        match attachment.side {
            window_state::EdgeSide::Left | window_state::EdgeSide::Right => converter
                .physical_to_logical(attachment.client_bounds.top as f32 - display.bounds.y),
            window_state::EdgeSide::Top | window_state::EdgeSide::Bottom => converter
                .physical_to_logical(attachment.client_bounds.left as f32 - display.bounds.x),
        };
    let Some(display_key) = display.display_key else {
        return;
    };
    let side = match attachment.side {
        window_state::EdgeSide::Left => PersistedEdgeSide::Left,
        window_state::EdgeSide::Right => PersistedEdgeSide::Right,
        window_state::EdgeSide::Top => PersistedEdgeSide::Top,
        window_state::EdgeSide::Bottom => PersistedEdgeSide::Bottom,
    };
    state_runtime::set_main_window_edge(
        Some(PersistedEdgePosition {
            display_key,
            side,
            offset: offset.max(0.0),
        }),
        cx,
    );
}

pub(crate) fn set_visibility(
    visible: bool,
    source: VisibilitySource,
    cx: &mut App,
) -> Result<(), String> {
    window_drag::sync_window_size_after_native_resize(cx);
    let (
        window,
        attachment,
        current_visible,
        transition_active,
        stored_normal_bounds,
        stored_normal_logical_size,
        stored_panel,
        fixed,
    ) = cx.read_global::<MainWindowService, _>(|service, _| {
        (
            service.window,
            service.window_state.edge_attachment(),
            service.window_state.is_logically_visible(),
            service.window_state.is_visibility_transition_active(),
            service.window_state.normal_bounds(),
            service.window_state.normal_logical_size(),
            service.window_state.panel_attachment(),
            service.window_state.is_fixed(),
        )
    });
    if !visible && !can_hide_window(fixed, source) {
        return Ok(());
    }
    let Some(window) = window else {
        return Err("主窗口尚未创建".to_owned());
    };
    if current_visible == visible {
        sync_mouse_leave_state(cx);
        return Ok(());
    }
    if visible {
        MainWindowService::prepare_window_shown(cx);
    }
    if !visible {
        // 隐藏动画期间仍可能收到列表 hover 事件；先使正在进行的预览查询失效，
        // 避免查询完成后重新创建预览窗口。
        MainWindowService::cancel_hover_preview(cx);
        let main_view = cx.global::<MainWindowService>().main_view.clone();
        if let Some(main_view) = main_view {
            let _ = window.update(cx, |_, window, cx| {
                main_view.update(cx, |main, cx| {
                    main.cancel_tab_bar_hover();
                    if main.filters.clear_hovered() {
                        main.refresh_history(cx);
                    }
                    main.clear_gallery_animations();
                    main.cleanup_gallery_image_atlas(window);
                    cx.notify();
                });
            });
        }
    }

    let animation_enabled =
        config_runtime::animation_enabled(cx, crate::config::AnimationKind::WindowVisibility);
    let clipboard = config_runtime::clipboard_config(cx);
    let panel_mode = clipboard.window_position.is_screen_edge();
    let show_edge = clipboard.edge_hide_show_edge;
    let (current_bounds, mut reveal_size, window_scale_factor, window_logical_size) = window
        .update(cx, |_, window, _| {
            let current_bounds = platform_window::physical_client_bounds(window)?;
            Ok::<_, String>((
                current_bounds,
                edge_reveal_size(window.scale_factor(), show_edge),
                window.scale_factor(),
                (
                    f32::from(window.bounds().size.width),
                    f32::from(window.bounds().size.height),
                ),
            ))
        })
        .map_err(|error| error.to_string())??;
    let base_normal_bounds = stored_normal_bounds.unwrap_or(current_bounds);
    let logical_size = stored_normal_logical_size.unwrap_or_else(|| {
        stored_normal_bounds
            .map(|bounds| {
                platform_window::CoordinateConverter::new(window_scale_factor)
                    .logical_client_size(bounds)
            })
            .unwrap_or(window_logical_size)
    });
    let (normal_bounds, normal_scale_factor) = if panel_mode
        && (!visible || transition_active)
        && let Some(panel) = stored_panel
    {
        (panel.client_bounds, window_scale_factor)
    } else if attachment.is_none() {
        if visible {
            normal_bounds_for_position(
                base_normal_bounds,
                logical_size,
                config_runtime::clipboard_config(cx).window_position,
                state_runtime::state(cx).main_window.normal_position,
                window_scale_factor,
                cx,
            )
        } else if current_visible && !transition_active {
            (current_bounds, window_scale_factor)
        } else {
            (base_normal_bounds, window_scale_factor)
        }
    } else {
        (base_normal_bounds, window_scale_factor)
    };

    let panel = panel_mode
        .then(|| {
            panel_attachment_for_bounds(clipboard.window_position, normal_bounds, cx)
                .or(stored_panel)
        })
        .flatten();
    if panel.is_some() {
        reveal_size = 0;
        cx.update_global::<MainWindowService, _>(|service, _| {
            service.window_state.set_panel_attachment(panel);
        });
    }
    let sliding_attachment = panel.or(attachment);

    if !visible && sliding_attachment.is_none() {
        record_normal_position(cx);
        remember_normal_bounds(normal_bounds, cx);
    }

    let animation = match sliding_attachment {
        Some(attachment) => VisibilityAnimation::Edge {
            from: if panel.is_some() && visible && !transition_active {
                hidden_bounds(attachment, 0)
            } else {
                current_bounds
            },
            to: if visible {
                attachment.client_bounds
            } else {
                hidden_bounds(attachment, reveal_size)
            },
            show: visible,
            hide_window: panel.is_some(),
        },
        None => {
            let collapsed = collapsed_bounds(
                normal_bounds,
                platform_window::CoordinateConverter::new(normal_scale_factor)
                    .logical_to_physical_ceil_i32(COLLAPSED_WINDOW_HEIGHT_LOGICAL),
            );
            VisibilityAnimation::Normal {
                left: normal_bounds.left,
                top: normal_bounds.top,
                right: normal_bounds.right,
                from_height: client_height(current_bounds),
                to_height: if visible {
                    client_height(normal_bounds)
                } else {
                    client_height(collapsed)
                },
                show: visible,
            }
        }
    };

    if !animation_enabled {
        apply_immediate_visibility(
            window,
            sliding_attachment,
            panel.is_some(),
            visible,
            normal_bounds,
            reveal_size,
            cx,
        )?;
        cx.update_global::<MainWindowService, _>(|service, _| {
            service.window_state.cancel_visibility_transition();
            service.window_state.set_visibility(visible);
            if sliding_attachment.is_none() {
                service.window_state.remember_normal_bounds(normal_bounds);
            }
        });
        if visible && attachment.is_none() {
            record_normal_position(cx);
        }
        if visible {
            crate::services::memory_trim::cancel(cx);
        } else {
            crate::services::memory_trim::schedule(cx);
        }
        if !visible {
            crate::windows::preview::PreviewWindowService::destroy(cx);
        }
        if visible {
            let generation = cx.read_global::<MainWindowService, _>(|service, _| {
                service.window_state.visibility_transition_generation()
            });
            let _ = window.update(cx, |_, window, cx| {
                if attachment.is_some() {
                    MainWindowService::handle_window_shown(window, cx);
                } else {
                    // GPUI 的帧回调先于绘制执行，留出一帧派发隐藏期间积压的失焦事件，
                    // 避免旧输入会话的失焦处理覆盖本次显示的自动聚焦。
                    window.on_next_frame(move |window, _| {
                        window.on_next_frame(move |window, cx| {
                            let should_handle =
                                cx.read_global::<MainWindowService, _>(|service, _| {
                                    service.window_state.is_logically_visible()
                                        && service.window_state.visibility_transition_generation()
                                            == generation
                                });
                            if should_handle {
                                MainWindowService::handle_window_shown(window, cx);
                            }
                        });
                    });
                }
                Ok::<(), String>(())
            });
        }
        sync_mouse_leave_state(cx);
        return Ok(());
    }

    let generation = cx.update_global::<MainWindowService, _>(|service, _| {
        if sliding_attachment.is_none() {
            service.window_state.remember_normal_bounds(normal_bounds);
        }
        service.window_state.begin_visibility_transition(visible)
    });
    sync_mouse_leave_state(cx);

    let setup_result = window
        .update(cx, |_, window, cx| -> Result<_, String> {
            platform_window::set_window_rendering_enabled(window, true)?;
            if attachment.is_none() || visible {
                platform_window::set_window_shadow_visible(window, true)?;
            }
            match animation {
                VisibilityAnimation::Edge {
                    from, hide_window, ..
                } => {
                    platform_window::set_window_opacity(window, 1.0)?;
                    if hide_window {
                        // 完整尺寸先放到动画起点，再显示，避免屏幕内闪现。
                        platform_window::set_client_bounds(window, from)?;
                        window.bounds_changed(cx);
                    }
                    platform_window::set_window_visible(window, true)?;
                }
                VisibilityAnimation::Normal {
                    left,
                    top,
                    right,
                    from_height,
                    show,
                    ..
                } => {
                    if show {
                        platform_window::set_window_opacity(window, 0.0)?;
                        platform_window::set_client_bounds_from_top(
                            window,
                            left,
                            top,
                            right,
                            from_height,
                        )?;
                        platform_window::set_window_visible(window, true)?;
                    } else {
                        platform_window::set_window_opacity(window, 1.0)?;
                    }
                }
            }
            platform_window::window_animation_target(window)
        })
        .map_err(|error| error.to_string())?;
    let animation_target = match setup_result {
        Ok(target) => target,
        Err(error) => {
            let _ = window.update(cx, |_, window, cx| {
                if let Err(restore_error) =
                    restore_visibility_after_animation_failure(window, current_visible, cx)
                {
                    tracing::error!("窗口动画准备失败后恢复窗口失败：{restore_error}");
                }
            });
            cx.update_global::<MainWindowService, _>(|service, _| {
                service.window_state.cancel_visibility_transition();
                // 原生设置失败时回滚逻辑可见性，避免“记录为已显示但窗口仍在屏幕外”而无法再次恢复。
                service.window_state.set_visibility(current_visible);
            });
            return Err(error);
        }
    };
    let started_at = Instant::now();
    let result = window.update(cx, |_, window, _| {
        schedule_visibility_animation_frame(
            window,
            animation,
            animation_target,
            generation,
            started_at,
        );
    });
    result.map_err(|error| error.to_string())?;
    if !visible {
        crate::windows::preview::PreviewWindowService::destroy(cx);
    }
    if visible {
        crate::services::memory_trim::cancel(cx);
    } else {
        crate::services::memory_trim::schedule(cx);
    }
    Ok(())
}

fn apply_immediate_visibility(
    window: gpui::AnyWindowHandle,
    attachment: Option<window_state::EdgeAttachment>,
    hide_window: bool,
    visible: bool,
    normal_bounds: platform_window::PhysicalClientBounds,
    reveal_size: i32,
    cx: &mut App,
) -> Result<(), String> {
    let result = window
        .update(cx, |_, window, cx| {
            platform_window::set_window_shadow_visible(
                window,
                should_show_window_shadow(attachment, visible),
            )?;
            if let Some(attachment) = attachment {
                let (x, y) = if visible {
                    (attachment.client_bounds.left, attachment.client_bounds.top)
                } else {
                    let hidden = hidden_bounds(attachment, reveal_size);
                    (hidden.left, hidden.top)
                };
                if hide_window {
                    let bounds = if visible {
                        attachment.client_bounds
                    } else {
                        hidden_bounds(attachment, 0)
                    };
                    platform_window::set_client_bounds(window, bounds)?;
                    window.bounds_changed(cx);
                } else {
                    platform_window::set_client_position(window, x, y)?;
                }
                platform_window::set_window_visible(window, !hide_window || visible)
            } else {
                platform_window::set_client_bounds(window, normal_bounds)?;
                window.bounds_changed(cx);
                platform_window::set_window_visible(window, visible)
            }?;
            platform_window::set_window_opacity(
                window,
                if attachment.is_some() || visible {
                    1.0
                } else {
                    0.0
                },
            )?;
            if visible {
                platform_window::raise_window_to_topmost(window)?;
            }
            platform_window::set_window_rendering_enabled(window, visible)?;
            Ok::<(), String>(())
        })
        .map_err(|error| error.to_string())?;
    result
}

fn schedule_visibility_animation_frame(
    window: &mut gpui::Window,
    animation: VisibilityAnimation,
    animation_target: platform_window::WindowAnimationTarget,
    generation: u64,
    started_at: Instant,
) {
    window.on_next_frame(move |window, cx| {
        let (raw_progress, progress) = visibility_animation_progress(animation, started_at);
        let completed = raw_progress >= 1.0;
        match apply_visibility_animation_frame(
            animation,
            animation_target,
            generation,
            raw_progress,
            progress,
            completed,
            window,
            cx,
        ) {
            Ok(true) if !completed => schedule_visibility_animation_frame(
                window,
                animation,
                animation_target,
                generation,
                started_at,
            ),
            Ok(_) => {}
            Err(error) => {
                tracing::error!("窗口显示/隐藏动画执行失败：{error}");
                let previous_visible = !animation_show(animation);
                if let Err(restore_error) =
                    restore_visibility_after_animation_failure(window, previous_visible, cx)
                {
                    tracing::error!("窗口动画失败后恢复窗口失败：{restore_error}");
                }
                cx.update_global::<MainWindowService, _>(|service, _| {
                    service.window_state.cancel_visibility_transition();
                    // 动画失败时同样回滚逻辑可见性，保证下次指针移入触发区还能重新显示。
                    service.window_state.set_visibility(previous_visible);
                });
                sync_mouse_leave_state(cx);
            }
        }
    });
}

fn apply_visibility_animation_frame(
    animation: VisibilityAnimation,
    animation_target: platform_window::WindowAnimationTarget,
    generation: u64,
    raw_progress: f32,
    progress: f32,
    completed: bool,
    window: &mut gpui::Window,
    cx: &mut App,
) -> Result<bool, String> {
    let current = cx.read_global::<MainWindowService, _>(|service, _| {
        service
            .window_state
            .is_current_visibility_transition(generation)
    });
    if !current {
        return Ok(false);
    }
    let frame_bounds = match animation {
        VisibilityAnimation::Edge { from, to, .. } => interpolate_bounds(from, to, progress),
        VisibilityAnimation::Normal {
            left,
            top,
            right,
            from_height,
            to_height,
            ..
        } => platform_window::PhysicalClientBounds {
            left,
            top,
            right,
            bottom: top.saturating_add(interpolate_i32(from_height, to_height, progress)),
        },
    };
    match animation {
        VisibilityAnimation::Edge {
            show, hide_window, ..
        } => {
            platform_window::set_client_position_with_target(
                animation_target,
                frame_bounds.left,
                frame_bounds.top,
            )?;
            if completed && !show {
                platform_window::set_window_shadow_visible(window, false)?;
                if hide_window {
                    platform_window::set_window_visible(window, false)?;
                }
            }
            if completed && (show || !hide_window) {
                platform_window::raise_window_to_topmost(window)?;
            }
        }
        VisibilityAnimation::Normal {
            left,
            top,
            right,
            show,
            ..
        } => {
            platform_window::set_client_bounds_from_top_with_target(
                animation_target,
                left,
                top,
                right,
                frame_bounds.bottom.saturating_sub(top),
            )?;
            // 原生窗口尺寸变化不会始终同步 GPUI 的 viewport 缓存，显式通知以避免底部未重绘。
            window.bounds_changed(cx);
            platform_window::set_window_opacity_with_target(
                animation_target,
                visibility_animation_opacity(raw_progress, show),
            )?;
            if completed && !show {
                platform_window::set_window_visible(window, false)?;
            }
            if completed && show {
                platform_window::raise_window_to_topmost(window)?;
            }
        }
    }

    if completed {
        let finished = cx.update_global::<MainWindowService, _>(|service, _| {
            service
                .window_state
                .finish_visibility_transition(generation)
        });
        if finished {
            platform_window::set_window_rendering_enabled(window, animation_show(animation))?;
            if animation_show(animation) {
                MainWindowService::handle_window_shown(window, cx);
                if cx.read_global::<MainWindowService, _>(|service, _| {
                    service.window_state.edge_attachment().is_none()
                }) {
                    if let Err(error) = record_normal_position_from_window(window, cx) {
                        tracing::error!("记录主窗口位置失败：{error}");
                    }
                }
            }
            sync_mouse_leave_state(cx);
        }
    }
    Ok(true)
}

fn restore_visibility_after_animation_failure(
    window: &mut gpui::Window,
    visible: bool,
    cx: &mut App,
) -> Result<(), String> {
    let (attachment, panel, normal_bounds) =
        cx.read_global::<MainWindowService, _>(|service, _| {
            (
                service.window_state.edge_attachment(),
                service.window_state.panel_attachment(),
                service.window_state.normal_bounds(),
            )
        });
    if let Some(attachment) = panel.or(attachment) {
        let bounds = if visible {
            attachment.client_bounds
        } else {
            hidden_bounds(
                attachment,
                if panel.is_some() {
                    0
                } else {
                    edge_reveal_size(
                        window.scale_factor(),
                        config_runtime::clipboard_config(cx).edge_hide_show_edge,
                    )
                },
            )
        };
        platform_window::set_client_position(window, bounds.left, bounds.top)?;
    } else if let Some(bounds) = normal_bounds {
        platform_window::set_client_bounds(window, bounds)?;
        window.bounds_changed(cx);
    }
    platform_window::set_window_opacity(
        window,
        if attachment.is_some() || visible {
            1.0
        } else {
            0.0
        },
    )?;
    platform_window::set_window_shadow_visible(
        window,
        should_show_window_shadow(attachment, visible),
    )?;
    platform_window::set_window_visible(window, attachment.is_some() || visible)?;
    platform_window::set_window_rendering_enabled(window, visible)
}

fn visibility_animation_progress(
    animation: VisibilityAnimation,
    started_at: Instant,
) -> (f32, f32) {
    let raw_progress = (started_at.elapsed().as_secs_f32()
        / animation_duration(animation).as_secs_f32())
    .clamp(0.0, 1.0);
    let eased_progress = match animation {
        VisibilityAnimation::Edge { .. } => ease_out_quad(raw_progress),
        VisibilityAnimation::Normal { show: true, .. } => ease_with_settle_bounce(raw_progress),
        VisibilityAnimation::Normal { show: false, .. } => ease_in_quad(raw_progress),
    };
    (raw_progress, eased_progress)
}

fn animation_duration(animation: VisibilityAnimation) -> Duration {
    match animation {
        VisibilityAnimation::Edge { .. } => EDGE_ANIMATION_DURATION,
        VisibilityAnimation::Normal { show: true, .. } => NORMAL_SHOW_ANIMATION_DURATION,
        VisibilityAnimation::Normal { show: false, .. } => NORMAL_HIDE_ANIMATION_DURATION,
    }
}

fn animation_show(animation: VisibilityAnimation) -> bool {
    match animation {
        VisibilityAnimation::Edge { show, .. } | VisibilityAnimation::Normal { show, .. } => show,
    }
}

fn ease_in_quad(progress: f32) -> f32 {
    progress * progress
}

fn ease_out_quad(progress: f32) -> f32 {
    1.0 - (1.0 - progress) * (1.0 - progress)
}

fn ease_with_settle_bounce(progress: f32) -> f32 {
    if progress < NORMAL_SHOW_SETTLE_START {
        let normalized = progress / NORMAL_SHOW_SETTLE_START;
        return 1.0 - (1.0 - normalized).powi(3);
    }

    let settle_phase = (progress - NORMAL_SHOW_SETTLE_START) / (1.0 - NORMAL_SHOW_SETTLE_START);
    let overshoot = (settle_phase * std::f32::consts::PI).cos() * 0.012;
    1.0 - overshoot * (1.0 - settle_phase)
}

fn visibility_animation_opacity(progress: f32, show: bool) -> f32 {
    if show {
        (progress * 2.0).min(1.0)
    } else {
        1.0 - progress * progress
    }
}

fn interpolate_bounds(
    from: platform_window::PhysicalClientBounds,
    to: platform_window::PhysicalClientBounds,
    progress: f32,
) -> platform_window::PhysicalClientBounds {
    platform_window::PhysicalClientBounds {
        left: interpolate_i32(from.left, to.left, progress),
        top: interpolate_i32(from.top, to.top, progress),
        right: interpolate_i32(from.right, to.right, progress),
        bottom: interpolate_i32(from.bottom, to.bottom, progress),
    }
}

fn interpolate_i32(from: i32, to: i32, progress: f32) -> i32 {
    (from as f32 + (to - from) as f32 * progress).round() as i32
}

fn client_height(bounds: platform_window::PhysicalClientBounds) -> i32 {
    bounds.bottom.saturating_sub(bounds.top).max(1)
}

fn collapsed_bounds(
    bounds: platform_window::PhysicalClientBounds,
    collapsed_height: i32,
) -> platform_window::PhysicalClientBounds {
    platform_window::PhysicalClientBounds {
        left: bounds.left,
        top: bounds.top,
        right: bounds.right,
        bottom: bounds.top.saturating_add(collapsed_height.max(1)),
    }
}

fn should_show_window_shadow(
    attachment: Option<window_state::EdgeAttachment>,
    visible: bool,
) -> bool {
    attachment.is_none() || visible
}

pub(crate) fn toggle_visibility(cx: &mut App) -> Result<(), String> {
    let visible = cx.read_global::<MainWindowService, _>(|service, _| {
        service.window_state.is_logically_visible()
    });
    set_visibility(!visible, VisibilitySource::ExplicitToggle, cx)
}

pub(crate) fn record_normal_position_after_drag(cx: &mut App) {
    record_normal_position(cx);
}

fn record_normal_position(cx: &mut App) {
    let (window, should_record) = cx.read_global::<MainWindowService, _>(|service, _| {
        (
            service.window,
            service.window_state.edge_attachment().is_none()
                && service.window_state.can_record_window_geometry(),
        )
    });
    if !should_record {
        return;
    }
    let Some(window) = window else {
        return;
    };
    let result = window.update(cx, |_, window, cx| {
        record_normal_position_from_window(window, cx)
    });
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::error!("记录主窗口位置失败：{error}"),
        Err(error) => tracing::error!("记录主窗口位置失败：{error}"),
    }
}

fn record_normal_position_from_window(window: &gpui::Window, cx: &mut App) -> Result<(), String> {
    let should_record = cx.read_global::<MainWindowService, _>(|service, _| {
        service.window_state.edge_attachment().is_none()
            && service.window_state.can_record_window_geometry()
    });
    if !should_record {
        return Ok(());
    }
    let bounds = platform_window::physical_client_bounds(window)?;
    let logical_size = platform_window::CoordinateConverter::new(window.scale_factor())
        .logical_client_size(bounds);
    let size = state_runtime::PersistedWindowSize {
        width: logical_size.0,
        height: logical_size.1,
    };
    if !size.is_valid() {
        return Ok(());
    }
    remember_normal_geometry(bounds, logical_size, cx);
    let display = platform_window::display_for_physical_point(
        cx,
        Point {
            x: (bounds.left + bounds.right) as f32 / 2.0,
            y: (bounds.top + bounds.bottom) as f32 / 2.0,
        },
    );
    let persisted_position = display.map(|display| {
        let relative = platform_window::logical_client_offset_relative_to_display(bounds, display);
        PersistedWindowPosition {
            x: relative.x,
            y: relative.y,
            display_key: display.display_key,
        }
    });
    persist_window_position(persisted_position, cx);
    Ok(())
}

fn remember_normal_bounds(bounds: platform_window::PhysicalClientBounds, cx: &mut App) {
    cx.update_global::<MainWindowService, _>(|service, _| {
        service.window_state.remember_normal_bounds(bounds);
    });
}

fn remember_normal_geometry(
    bounds: platform_window::PhysicalClientBounds,
    logical_size: (f32, f32),
    cx: &mut App,
) {
    cx.update_global::<MainWindowService, _>(|service, _| {
        service
            .window_state
            .remember_normal_geometry(bounds, logical_size);
    });
}

fn persist_window_position(position: Option<PersistedWindowPosition>, cx: &mut App) {
    if config_runtime::clipboard_config(cx).window_position == ClipboardWindowPosition::Remember
        && let Some(position) = position
    {
        state_runtime::set_main_window_normal_position(position, cx);
    }
}

fn normal_bounds_for_position(
    base_bounds: platform_window::PhysicalClientBounds,
    logical_size: (f32, f32),
    position: ClipboardWindowPosition,
    remembered: Option<PersistedWindowPosition>,
    fallback_scale_factor: f32,
    cx: &App,
) -> (platform_window::PhysicalClientBounds, f32) {
    let cursor = platform_window::physical_cursor_position()
        .or_else(|| input::pointer_snapshot().map(|snapshot| snapshot.position));
    let cursor_display =
        cursor.and_then(|cursor| platform_window::display_for_physical_point(cx, cursor));

    if position == ClipboardWindowPosition::Remember {
        let Some(remembered) = remembered else {
            return (base_bounds, fallback_scale_factor);
        };
        let display = remembered
            .display_key
            .and_then(|key| platform_window::display_for_key(cx, key))
            .or(cursor_display);
        let Some(display) = display else {
            return (base_bounds, fallback_scale_factor);
        };
        let screen = platform_window::CoordinateConverter::new(display.scale_factor)
            .logical_display_bounds(display.bounds);
        let position = if remembered.display_key.is_some() {
            Point {
                x: screen.x + remembered.x,
                y: screen.y + remembered.y,
            }
        } else {
            // 旧版本保存的是 GPUI 全局逻辑坐标，保留一次兼容解释。
            Point {
                x: remembered.x,
                y: remembered.y,
            }
        };
        let width = logical_size.0.max(1.0);
        let height = logical_size.1.max(1.0);
        let logical_bounds = platform_window::LogicalClientBounds {
            left: clamp_position(position.x, screen.x, screen.x + screen.width, width),
            top: clamp_position(position.y, screen.y, screen.y + screen.height, height),
            width,
            height,
        };
        return (
            platform_window::physical_client_bounds_from_logical(
                logical_bounds,
                display.scale_factor,
            ),
            display.scale_factor,
        );
    }

    let (Some(display), Some(cursor)) = (cursor_display, cursor) else {
        return (base_bounds, fallback_scale_factor);
    };
    let converter = platform_window::CoordinateConverter::new(display.scale_factor);
    let logical_size = if position.is_screen_edge() {
        let state = state_runtime::state(cx);
        let side = match position {
            ClipboardWindowPosition::ScreenLeftCenter => state_runtime::PersistedEdgeSide::Left,
            ClipboardWindowPosition::ScreenRightCenter => state_runtime::PersistedEdgeSide::Right,
            ClipboardWindowPosition::ScreenTopCenter => state_runtime::PersistedEdgeSide::Top,
            ClipboardWindowPosition::ScreenBottomCenter => state_runtime::PersistedEdgeSide::Bottom,
            _ => unreachable!(),
        };
        let thickness = state.main_window.restored_edge_panel_size(side);
        if position.uses_horizontal_list() {
            (logical_size.0, thickness)
        } else {
            (thickness, logical_size.1)
        }
    } else {
        logical_size
    };
    let logical_bounds = positioned_bounds(
        logical_size,
        position,
        converter.logical_display_bounds(display.bounds),
        converter.physical_point_to_logical(cursor),
    );
    (
        platform_window::physical_client_bounds_from_logical(logical_bounds, display.scale_factor),
        display.scale_factor,
    )
}

fn positioned_bounds(
    logical_size: (f32, f32),
    position: ClipboardWindowPosition,
    screen: platform_window::LogicalDisplayBounds,
    cursor: Point<f32>,
) -> platform_window::LogicalClientBounds {
    let width = if position.uses_horizontal_list() {
        screen.width.max(1.0)
    } else if position.is_screen_edge() {
        logical_size.0.max(1.0).min((screen.width / 2.0).max(1.0))
    } else {
        logical_size.0.max(1.0)
    };
    let height = if position.is_screen_edge() && !position.uses_horizontal_list() {
        screen.height.max(1.0)
    } else if position.is_screen_edge() {
        logical_size.1.max(1.0).min((screen.height / 2.0).max(1.0))
    } else {
        logical_size.1.max(1.0)
    };
    let screen_right = screen.x + screen.width;
    let screen_bottom = screen.y + screen.height;
    let center_x = screen.x + screen.width / 2.0;
    let center_y = screen.y + screen.height / 2.0;

    let (x, y) = match position {
        ClipboardWindowPosition::MouseCenter => (cursor.x - width / 2.0, cursor.y - height / 2.0),
        ClipboardWindowPosition::MouseNearby => nearby_position(
            cursor.x,
            cursor.y,
            width,
            height,
            screen.x,
            screen.y,
            screen_right,
            screen_bottom,
        ),
        ClipboardWindowPosition::Remember => (screen.x, screen.y),
        ClipboardWindowPosition::ScreenCenter => (center_x - width / 2.0, center_y - height / 2.0),
        ClipboardWindowPosition::ScreenLeftCenter => (screen.x, center_y - height / 2.0),
        ClipboardWindowPosition::ScreenRightCenter => {
            (screen_right - width, center_y - height / 2.0)
        }
        ClipboardWindowPosition::ScreenTopCenter => (center_x - width / 2.0, screen.y),
        ClipboardWindowPosition::ScreenBottomCenter => {
            (center_x - width / 2.0, screen_bottom - height)
        }
    };

    platform_window::LogicalClientBounds {
        left: clamp_position(x, screen.x, screen_right, width),
        top: clamp_position(y, screen.y, screen_bottom, height),
        width,
        height,
    }
}

fn nearby_position(
    cursor_x: f32,
    cursor_y: f32,
    width: f32,
    height: f32,
    screen_x: f32,
    screen_y: f32,
    screen_right: f32,
    screen_bottom: f32,
) -> (f32, f32) {
    let candidates = [
        (
            cursor_x + NEARBY_WINDOW_GAP_LOGICAL,
            cursor_y + NEARBY_WINDOW_GAP_LOGICAL,
        ),
        (
            cursor_x - width - NEARBY_WINDOW_GAP_LOGICAL,
            cursor_y + NEARBY_WINDOW_GAP_LOGICAL,
        ),
        (
            cursor_x + NEARBY_WINDOW_GAP_LOGICAL,
            cursor_y - height - NEARBY_WINDOW_GAP_LOGICAL,
        ),
        (
            cursor_x - width - NEARBY_WINDOW_GAP_LOGICAL,
            cursor_y - height - NEARBY_WINDOW_GAP_LOGICAL,
        ),
    ];
    candidates
        .into_iter()
        .find(|(x, y)| {
            *x >= screen_x
                && *y >= screen_y
                && *x + width <= screen_right
                && *y + height <= screen_bottom
        })
        .unwrap_or((cursor_x, cursor_y))
}

fn clamp_position(position: f32, screen_start: f32, screen_end: f32, size: f32) -> f32 {
    if size >= screen_end - screen_start {
        screen_start
    } else {
        position.clamp(screen_start, screen_end - size)
    }
}

pub(crate) fn sync_mouse_leave_state(cx: &mut App) {
    let attachment =
        cx.read_global::<MainWindowService, _>(|service, _| service.window_state.edge_attachment());
    let (visible, pointer_inside) = MainWindowService::session_shortcut_scope(cx);
    let edge_mouse_inside = attachment.is_some_and(|attachment| {
        input::pointer_snapshot().is_some_and(|snapshot| {
            point_in_client_bounds(snapshot.position, attachment.client_bounds)
        })
    });

    cx.update_global::<MainWindowService, _>(|service, _| {
        service
            .window_state
            .set_mouse_leave_state(mouse_leave_state_for(
                attachment,
                visible,
                edge_mouse_inside,
            ));
    });
    #[cfg(target_os = "windows")]
    MainWindowService::sync_pointer_change_notifier(cx);
    crate::hotkey::sync_session_shortcuts(visible, pointer_inside, cx);
    MainWindowService::request_pointer_check(cx);
}

fn mouse_leave_state_for(
    attachment: Option<window_state::EdgeAttachment>,
    visible: bool,
    mouse_inside: bool,
) -> window_state::MouseLeaveState {
    if attachment.is_some() && visible && mouse_inside {
        window_state::MouseLeaveState::Armed
    } else {
        window_state::MouseLeaveState::Disarmed
    }
}

pub(crate) fn handle_global_mouse_down(
    position: Point<f32>,
    foreground_window: Option<isize>,
    cx: &mut App,
) {
    window_drag::sync_edge_bounds_after_native_move(cx);
    let window = cx.read_global::<MainWindowService, _>(|service, _| service.window);
    let Some(window) = window else {
        return;
    };

    let action = outside_click_action(cx);
    let action = window
        .update(cx, |_, window, cx| {
            #[cfg(target_os = "windows")]
            if crate::services::tray::TrayService::contains_point(cx, position) {
                if let Err(error) =
                    crate::services::focus::restore_captured_focus(foreground_window)
                {
                    tracing::debug!("恢复托盘点击前窗口焦点失败：{error}");
                }
                return Ok::<OutsideClickAction, String>(OutsideClickAction::Ignore);
            }

            if action == OutsideClickAction::Ignore {
                return Ok(action);
            }
            if platform_window::is_window_resize_hit_test(window, position)? {
                return Ok(OutsideClickAction::Ignore);
            }
            let bounds = platform_window::physical_client_bounds(window)?;
            let in_main_window = point_in_client_bounds(position, bounds);
            Ok(if !in_main_window {
                action
            } else {
                OutsideClickAction::Ignore
            })
        })
        .ok()
        .and_then(|result| result.ok())
        .unwrap_or(OutsideClickAction::Ignore);

    if action == OutsideClickAction::Hide
        && let Err(error) = set_visibility(false, VisibilitySource::Implicit, cx)
    {
        tracing::error!("点击外部隐藏主窗口失败：{error}");
    }
}

fn outside_click_action(cx: &App) -> OutsideClickAction {
    cx.read_global::<MainWindowService, _>(|service, _| {
        let state = &service.window_state;
        if state.is_fixed() || !state.is_logically_visible() {
            OutsideClickAction::Ignore
        } else {
            OutsideClickAction::Hide
        }
    })
}

pub(crate) fn handle_mouse_move(position: Point<f32>, cx: &mut App) {
    let (
        window,
        drag_state,
        fixed,
        attachment,
        visible,
        mouse_leave_state,
        reveal_suppressed,
        reveal_blocked,
    ) = cx.read_global::<MainWindowService, _>(|service, _| {
        (
            service.window,
            service.window_state.drag_state(),
            service.window_state.is_fixed(),
            service.window_state.edge_attachment(),
            service.window_state.is_logically_visible(),
            service.window_state.mouse_leave_state(),
            service.window_state.edge_reveal_suppressed(),
            service.window_state.edge_reveal_blocked(),
        )
    });
    if drag_state != window_state::DragState::Idle
        || attachment.is_none()
        || window_drag::native_move_in_progress()
    {
        return;
    }
    if !config_runtime::clipboard_config(cx).edge_hide {
        MainWindowService::set_edge_hide_enabled(false, cx);
        return;
    }
    let menu_is_open = crate::windows::menu::MenuService::has_open_menu(cx);

    let Some(attachment) = attachment else {
        return;
    };
    let trigger_size = window
        .and_then(|window| {
            window
                .update(cx, |_, window, _| {
                    physical_edge_trigger_size(window.scale_factor())
                })
                .ok()
        })
        .unwrap_or_else(|| physical_edge_trigger_size(1.0));
    let in_reveal_region = point_in_edge_trigger_region(position, attachment, trigger_size);
    let reveal_suppressed =
        sync_edge_reveal_suppressed(in_reveal_region, visible, reveal_suppressed, cx);

    if !visible {
        if !reveal_blocked && in_reveal_region && !reveal_suppressed {
            match set_visibility(true, VisibilitySource::Implicit, cx) {
                Ok(()) => {
                    // 进入触发区后立即武装离开检测，避免快速划过时轮询未采到窗口内部位置。
                    cx.update_global::<MainWindowService, _>(|service, _| {
                        service
                            .window_state
                            .set_mouse_leave_state(window_state::MouseLeaveState::Armed);
                    });
                }
                Err(error) => {
                    tracing::error!("恢复贴边隐藏窗口失败：{error}");
                    // 失败后等指针离开触发区再重试，避免每帧重复尝试。
                    cx.update_global::<MainWindowService, _>(|service, _| {
                        service.window_state.set_edge_reveal_suppressed(true);
                    });
                }
            }
        }
        return;
    }

    let scrollbar_pressed = cx.read_global::<MainWindowService, _>(|service, cx| {
        service
            .main_view
            .as_ref()
            .is_some_and(|view| view.read(cx).heat_scrollbar.drag_origin.is_some())
    });
    if point_in_client_bounds(position, attachment.client_bounds) {
        if mouse_leave_state != window_state::MouseLeaveState::Armed {
            cx.update_global::<MainWindowService, _>(|service, _| {
                service
                    .window_state
                    .set_mouse_leave_state(window_state::MouseLeaveState::Armed);
            });
        }
    } else if should_hide_for_pointer(
        position,
        attachment.client_bounds,
        fixed,
        mouse_leave_state,
        menu_is_open,
        scrollbar_pressed,
    ) {
        let is_resize_hit_test = window.is_some_and(|window| {
            window
                .update(cx, |_, window, _| {
                    platform_window::is_window_resize_hit_test(window, position)
                })
                .ok()
                .and_then(|result| result.ok())
                .unwrap_or(false)
        });
        if is_resize_hit_test {
            return;
        }
        if let Err(error) = set_visibility(false, VisibilitySource::Implicit, cx) {
            tracing::error!("贴边隐藏主窗口失败：{error}");
        }
    }
}

/// 固定窗口状态下，只有明确切换可见状态的操作才允许隐藏窗口。
fn can_hide_window(fixed: bool, source: VisibilitySource) -> bool {
    !fixed || source == VisibilitySource::ExplicitToggle
}

/// 计算下一次的“禁止移入恢复”标记。
///
/// 窗口可见时按指针是否停在触发区记录；窗口隐藏时，若指针本来就在触发区里
/// （例如通过快捷键隐藏），不立即弹回，需要先离开触发区再进入才恢复显示。
fn edge_reveal_suppression_for(in_reveal_region: bool, visible: bool, suppressed: bool) -> bool {
    if visible {
        in_reveal_region
    } else if in_reveal_region {
        suppressed
    } else {
        false
    }
}

fn sync_edge_reveal_suppressed(
    in_reveal_region: bool,
    visible: bool,
    suppressed: bool,
    cx: &mut App,
) -> bool {
    let next = edge_reveal_suppression_for(in_reveal_region, visible, suppressed);
    if next != suppressed {
        cx.update_global::<MainWindowService, _>(|service, _| {
            service.window_state.set_edge_reveal_suppressed(next);
        });
    }
    next
}

fn hidden_bounds(
    attachment: window_state::EdgeAttachment,
    reveal_size: i32,
) -> platform_window::PhysicalClientBounds {
    let mut bounds = attachment.client_bounds;
    let width = bounds.right - bounds.left;
    let height = bounds.bottom - bounds.top;
    match attachment.side {
        window_state::EdgeSide::Left => {
            bounds.left = attachment.screen_bounds.x.round() as i32 - width + reveal_size;
        }
        window_state::EdgeSide::Right => {
            bounds.left = (attachment.screen_bounds.x + attachment.screen_bounds.width).round()
                as i32
                - reveal_size;
        }
        window_state::EdgeSide::Top => {
            bounds.top = attachment.screen_bounds.y.round() as i32 - height + reveal_size;
        }
        window_state::EdgeSide::Bottom => {
            bounds.top = (attachment.screen_bounds.y + attachment.screen_bounds.height).round()
                as i32
                - reveal_size;
        }
    }
    bounds.right = bounds.left + width;
    bounds.bottom = bounds.top + height;
    bounds
}

fn point_in_edge_trigger_region(
    position: Point<f32>,
    attachment: window_state::EdgeAttachment,
    trigger_size: i32,
) -> bool {
    input::PointerRegionWatch::contains(edge_trigger_region(attachment, trigger_size), position)
}

#[cfg(target_os = "windows")]
pub(super) fn hidden_edge_trigger_region(
    attachment: window_state::EdgeAttachment,
    cx: &App,
) -> gpui::Bounds<f32> {
    let screen = attachment.screen_bounds;
    let center = Point {
        x: screen.x + screen.width / 2.0,
        y: screen.y + screen.height / 2.0,
    };
    let scale_factor = platform_window::display_for_physical_point(cx, center)
        .map_or(1.0, |display| display.scale_factor);
    edge_trigger_region(attachment, physical_edge_trigger_size(scale_factor))
}

fn edge_trigger_region(
    attachment: window_state::EdgeAttachment,
    trigger_size: i32,
) -> gpui::Bounds<f32> {
    let screen_left = attachment.screen_bounds.x;
    let screen_top = attachment.screen_bounds.y;
    let screen_right = screen_left + attachment.screen_bounds.width;
    let screen_bottom = screen_top + attachment.screen_bounds.height;
    let bounds = attachment.client_bounds;
    let (left, top, right, bottom) = match attachment.side {
        window_state::EdgeSide::Left => (
            screen_left,
            bounds.top as f32,
            screen_left + trigger_size as f32,
            bounds.bottom as f32,
        ),
        window_state::EdgeSide::Right => (
            screen_right - trigger_size as f32,
            bounds.top as f32,
            screen_right,
            bounds.bottom as f32,
        ),
        window_state::EdgeSide::Top => (
            bounds.left as f32,
            screen_top,
            bounds.right as f32,
            screen_top + trigger_size as f32,
        ),
        window_state::EdgeSide::Bottom => (
            bounds.left as f32,
            screen_bottom - trigger_size as f32,
            bounds.right as f32,
            screen_bottom,
        ),
    };
    gpui::Bounds {
        origin: Point { x: left, y: top },
        size: Size {
            width: right - left,
            height: bottom - top,
        },
    }
}

fn physical_edge_reveal_size(scale_factor: f32) -> i32 {
    platform_window::CoordinateConverter::new(scale_factor)
        .logical_to_physical_ceil_i32(EDGE_REVEAL_SIZE_LOGICAL)
        .max(1)
}

fn edge_reveal_size(scale_factor: f32, show_edge: bool) -> i32 {
    if show_edge {
        physical_edge_reveal_size(scale_factor)
    } else {
        0
    }
}

fn physical_edge_trigger_size(scale_factor: f32) -> i32 {
    platform_window::CoordinateConverter::new(scale_factor)
        .logical_to_physical_ceil_i32(EDGE_TRIGGER_SIZE_LOGICAL)
        .max(1)
}

pub(crate) fn point_in_client_bounds(
    position: Point<f32>,
    bounds: platform_window::PhysicalClientBounds,
) -> bool {
    position.x >= bounds.left as f32
        && position.x < bounds.right as f32
        && position.y >= bounds.top as f32
        && position.y < bounds.bottom as f32
}

fn should_hide_for_pointer(
    position: Point<f32>,
    client_bounds: platform_window::PhysicalClientBounds,
    fixed: bool,
    mouse_leave_state: window_state::MouseLeaveState,
    menu_is_open: bool,
    scrollbar_pressed: bool,
) -> bool {
    !fixed
        && mouse_leave_state == window_state::MouseLeaveState::Armed
        && !point_in_client_bounds(position, client_bounds)
        && !menu_is_open
        && !scrollbar_pressed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attachment() -> window_state::EdgeAttachment {
        window_state::EdgeAttachment {
            side: window_state::EdgeSide::Right,
            client_bounds: platform_window::PhysicalClientBounds {
                left: 1560,
                top: 200,
                right: 1920,
                bottom: 720,
            },
            screen_bounds: platform_window::PhysicalDisplayBounds {
                x: 0.0,
                y: 0.0,
                width: 1920.0,
                height: 1080.0,
            },
        }
    }

    #[test]
    fn point_in_client_bounds_uses_client_edges() {
        let bounds = platform_window::PhysicalClientBounds {
            left: 100,
            top: 200,
            right: 460,
            bottom: 720,
        };

        assert!(point_in_client_bounds(Point { x: 100.0, y: 200.0 }, bounds));
        assert!(point_in_client_bounds(Point { x: 459.0, y: 719.0 }, bounds));
        assert!(!point_in_client_bounds(
            Point { x: 460.0, y: 719.0 },
            bounds
        ));
        assert!(!point_in_client_bounds(Point { x: 99.0, y: 200.0 }, bounds));
    }

    #[test]
    fn collapsed_bounds_keeps_the_top_edge_fixed() {
        let bounds = platform_window::PhysicalClientBounds {
            left: 100,
            top: 200,
            right: 460,
            bottom: 720,
        };

        assert_eq!(collapsed_bounds(bounds, 1).left, 100);
        assert_eq!(collapsed_bounds(bounds, 1).right, 460);
        assert_eq!(collapsed_bounds(bounds, 1).top, 200);
        assert_eq!(collapsed_bounds(bounds, 1).bottom, 201);
    }

    #[test]
    fn normal_animation_keeps_the_top_anchor_while_height_changes() {
        let normal = platform_window::PhysicalClientBounds {
            left: 100,
            top: 200,
            right: 460,
            bottom: 720,
        };
        let animation = VisibilityAnimation::Normal {
            left: normal.left,
            top: normal.top,
            right: normal.right,
            from_height: 1,
            to_height: client_height(normal),
            show: true,
        };
        let middle = match animation {
            VisibilityAnimation::Normal {
                left,
                top,
                right,
                from_height,
                to_height,
                ..
            } => platform_window::PhysicalClientBounds {
                left,
                top,
                right,
                bottom: top.saturating_add(interpolate_i32(from_height, to_height, 0.5)),
            },
            VisibilityAnimation::Edge { .. } => unreachable!(),
        };

        assert_eq!(middle.top, normal.top);
        assert_eq!(middle.bottom, 461);
    }

    #[test]
    fn interpolated_bounds_move_all_edges_linearly() {
        let from = platform_window::PhysicalClientBounds {
            left: 0,
            top: 100,
            right: 360,
            bottom: 620,
        };
        let to = platform_window::PhysicalClientBounds {
            left: -357,
            top: 100,
            right: 3,
            bottom: 620,
        };

        assert_eq!(interpolate_bounds(from, to, 0.5).left, -179);
        assert_eq!(interpolate_bounds(from, to, 0.5).right, 182);
        assert_eq!(interpolate_bounds(from, to, 0.0), from);
        assert_eq!(interpolate_bounds(from, to, 1.0), to);
    }

    #[test]
    fn normal_show_easing_settles_with_a_small_bounce() {
        assert!(ease_with_settle_bounce(NORMAL_SHOW_SETTLE_START) < 1.0);
        assert!(ease_with_settle_bounce(0.9) > 1.0);
        assert_eq!(ease_with_settle_bounce(1.0), 1.0);
    }

    #[test]
    fn visibility_animation_opacity_matches_reference_animation() {
        assert_eq!(visibility_animation_opacity(0.25, true), 0.5);
        assert_eq!(visibility_animation_opacity(0.5, true), 1.0);
        assert_eq!(visibility_animation_opacity(0.5, false), 0.75);
        assert_eq!(visibility_animation_opacity(1.0, false), 0.0);
    }

    #[test]
    fn open_menu_prevents_edge_hide_when_pointer_is_outside() {
        let bounds = platform_window::PhysicalClientBounds {
            left: 1560,
            top: 200,
            right: 1920,
            bottom: 720,
        };
        let position = Point {
            x: 1500.0,
            y: 300.0,
        };

        assert!(!should_hide_for_pointer(
            position,
            bounds,
            false,
            window_state::MouseLeaveState::Armed,
            true,
            false,
        ));
        assert!(should_hide_for_pointer(
            position,
            bounds,
            false,
            window_state::MouseLeaveState::Armed,
            false,
            false,
        ));
        assert!(!should_hide_for_pointer(
            position,
            bounds,
            false,
            window_state::MouseLeaveState::Disarmed,
            false,
            false,
        ));
    }

    #[test]
    fn scrollbar_press_blocks_edge_hide_until_release_even_outside_window() {
        let bounds = attachment().client_bounds;
        let mut scrollbar = super::super::state::HeatScrollbarState::default();
        let outside_positions = [
            Point {
                x: bounds.left as f32 - 1.0,
                y: 300.0,
            },
            Point {
                x: bounds.right as f32 + 1.0,
                y: 300.0,
            },
            Point {
                x: 1700.0,
                y: bounds.top as f32 - 1.0,
            },
            Point {
                x: 1700.0,
                y: bounds.bottom as f32 + 1.0,
            },
        ];
        let should_hide = |position, scrollbar: &super::super::state::HeatScrollbarState| {
            should_hide_for_pointer(
                position,
                bounds,
                false,
                window_state::MouseLeaveState::Armed,
                false,
                scrollbar.drag_origin.is_some(),
            )
        };
        for position in outside_positions {
            assert!(should_hide(position, &scrollbar));
        }
        scrollbar.drag_origin = Some((gpui::point(px(20.0), px(200.0)), 0.5));
        for position in outside_positions {
            assert!(!should_hide(position, &scrollbar));
        }
        scrollbar.dragging = true;
        scrollbar.clear_on_window_exit();
        for position in outside_positions {
            assert!(!should_hide(position, &scrollbar));
        }
        scrollbar.drag_origin = None;
        scrollbar.dragging = false;
        for position in outside_positions {
            assert!(should_hide(position, &scrollbar));
        }
        assert!(!should_hide(
            Point {
                x: 1700.0,
                y: 300.0
            },
            &scrollbar
        ));
    }

    #[test]
    fn mouse_leave_state_requires_visible_edge_window_and_inside_pointer() {
        let attachment = Some(attachment());

        assert_eq!(
            mouse_leave_state_for(attachment, true, true),
            window_state::MouseLeaveState::Armed,
        );
        assert_eq!(
            mouse_leave_state_for(attachment, true, false),
            window_state::MouseLeaveState::Disarmed,
        );
        assert_eq!(
            mouse_leave_state_for(attachment, false, true),
            window_state::MouseLeaveState::Disarmed,
        );
    }

    #[test]
    fn edge_reveal_suppression_needs_leaving_the_reveal_region() {
        // 可见时按指针是否停在触发区记录。
        assert!(edge_reveal_suppression_for(true, true, false));
        assert!(!edge_reveal_suppression_for(false, true, true));
        // 隐藏时只有指针离开触发区才解除禁止。
        assert!(edge_reveal_suppression_for(true, false, true));
        assert!(!edge_reveal_suppression_for(false, false, true));
    }

    #[test]
    fn fixed_window_only_hides_for_explicit_toggles() {
        assert!(!can_hide_window(true, VisibilitySource::Implicit));
        assert!(can_hide_window(true, VisibilitySource::ExplicitToggle));
        assert!(can_hide_window(false, VisibilitySource::Implicit));
    }

    #[test]
    fn edge_reveal_size_is_at_least_one_logical_pixel() {
        assert_eq!(physical_edge_reveal_size(1.0), 1);
        assert_eq!(physical_edge_reveal_size(1.25), 2);
        assert_eq!(physical_edge_reveal_size(2.0), 2);
    }

    #[test]
    fn hidden_client_area_keeps_one_logical_pixel_and_uses_two_pixel_trigger() {
        let reveal_size = physical_edge_reveal_size(1.0);
        let hidden = hidden_bounds(attachment(), reveal_size);

        assert_eq!(hidden.left, 1919);
        assert_eq!(hidden.right, 2279);
        assert!(point_in_edge_trigger_region(
            Point {
                x: 1919.0,
                y: 300.0
            },
            attachment(),
            physical_edge_trigger_size(1.0),
        ));
        assert!(point_in_edge_trigger_region(
            Point {
                x: 1918.0,
                y: 300.0
            },
            attachment(),
            physical_edge_trigger_size(1.0),
        ));
        assert!(!point_in_edge_trigger_region(
            Point {
                x: 1917.0,
                y: 300.0
            },
            attachment(),
            physical_edge_trigger_size(1.0),
        ));
    }

    #[test]
    fn window_shadow_is_disabled_only_for_hidden_edge_windows() {
        assert!(should_show_window_shadow(None, false));
        assert!(should_show_window_shadow(None, true));
        assert!(should_show_window_shadow(Some(attachment()), true));
        assert!(!should_show_window_shadow(Some(attachment()), false));
    }

    #[test]
    fn screen_position_is_centered_and_clamped_to_the_target_screen() {
        let screen = platform_window::PhysicalDisplayBounds {
            x: 1920.0,
            y: -100.0,
            width: 1920.0,
            height: 1080.0,
        };

        let centered = positioned_bounds(
            (360.0, 520.0),
            ClipboardWindowPosition::ScreenCenter,
            platform_window::CoordinateConverter::new(1.0).logical_display_bounds(screen),
            Point { x: 2000.0, y: 0.0 },
        );
        assert_eq!(centered.left, 2700.0);
        assert_eq!(centered.top, 180.0);

        let left = positioned_bounds(
            (360.0, 520.0),
            ClipboardWindowPosition::ScreenLeftCenter,
            platform_window::CoordinateConverter::new(1.0).logical_display_bounds(screen),
            Point { x: 2000.0, y: 0.0 },
        );
        assert_eq!(left.left, 1920.0);
        assert_eq!(left.top, -100.0);
        assert_eq!(left.height, 1080.0);
        assert_eq!(left.width, 360.0);
        let capped = positioned_bounds(
            (1200.0, 1400.0),
            ClipboardWindowPosition::ScreenTopCenter,
            platform_window::CoordinateConverter::new(1.0).logical_display_bounds(screen),
            Point { x: 2000.0, y: 0.0 },
        );
        assert_eq!(capped.height, 540.0);
        assert_eq!(capped.width, 1920.0);
    }

    #[test]
    fn edge_panels_span_the_screen_and_slide_completely_outside_it() {
        let screen = platform_window::PhysicalDisplayBounds {
            x: -1920.0,
            y: -100.0,
            width: 1920.0,
            height: 1080.0,
        };
        for (position, side) in [
            (
                ClipboardWindowPosition::ScreenLeftCenter,
                window_state::EdgeSide::Left,
            ),
            (
                ClipboardWindowPosition::ScreenRightCenter,
                window_state::EdgeSide::Right,
            ),
            (
                ClipboardWindowPosition::ScreenTopCenter,
                window_state::EdgeSide::Top,
            ),
            (
                ClipboardWindowPosition::ScreenBottomCenter,
                window_state::EdgeSide::Bottom,
            ),
        ] {
            for scale in [1.0, 1.5, 2.0] {
                let converter = platform_window::CoordinateConverter::new(scale);
                let bounds = positioned_bounds(
                    (360.0, 520.0),
                    position,
                    converter.logical_display_bounds(screen),
                    Point { x: -500.0, y: 0.0 },
                );
                let bounds = platform_window::physical_client_bounds_from_logical(bounds, scale);
                let panel = window_state::EdgeAttachment {
                    side,
                    client_bounds: bounds,
                    screen_bounds: screen,
                };
                let hidden = hidden_bounds(panel, 0);
                if position.uses_horizontal_list() {
                    assert_eq!((bounds.left, bounds.right), (-1920, 0));
                    assert_eq!(
                        bounds.bottom - bounds.top,
                        (520.0 * scale).min(screen.height / 2.0) as i32
                    );
                } else {
                    assert_eq!((bounds.top, bounds.bottom), (-100, 980));
                    assert_eq!(bounds.right - bounds.left, (360.0 * scale) as i32);
                }
                match side {
                    window_state::EdgeSide::Left => assert_eq!(hidden.right, -1920),
                    window_state::EdgeSide::Right => assert_eq!(hidden.left, 0),
                    window_state::EdgeSide::Top => assert_eq!(hidden.bottom, -100),
                    window_state::EdgeSide::Bottom => assert_eq!(hidden.top, 980),
                }
                assert_eq!(client_height(hidden), client_height(bounds));
                assert_eq!(hidden.right - hidden.left, bounds.right - bounds.left);
            }
        }
    }

    #[test]
    fn mouse_nearby_flips_from_right_down_to_left_down_then_up() {
        let screen = platform_window::PhysicalDisplayBounds {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
        };

        let right_down = positioned_bounds(
            (360.0, 520.0),
            ClipboardWindowPosition::MouseNearby,
            platform_window::CoordinateConverter::new(1.0).logical_display_bounds(screen),
            Point { x: 800.0, y: 200.0 },
        );
        assert_eq!((right_down.left, right_down.top), (808.0, 208.0));

        let left_down = positioned_bounds(
            (360.0, 520.0),
            ClipboardWindowPosition::MouseNearby,
            platform_window::CoordinateConverter::new(1.0).logical_display_bounds(screen),
            Point {
                x: 1800.0,
                y: 200.0,
            },
        );
        assert_eq!((left_down.left, left_down.top), (1432.0, 208.0));

        let left_up = positioned_bounds(
            (360.0, 520.0),
            ClipboardWindowPosition::MouseNearby,
            platform_window::CoordinateConverter::new(1.0).logical_display_bounds(screen),
            Point {
                x: 1800.0,
                y: 1000.0,
            },
        );
        assert_eq!((left_up.left, left_up.top), (1432.0, 472.0));
    }

    #[test]
    fn mouse_nearby_gap_stays_eight_logical_pixels_on_high_dpi() {
        let screen = platform_window::LogicalDisplayBounds {
            x: 0.0,
            y: 0.0,
            width: 1280.0,
            height: 720.0,
        };
        let bounds = positioned_bounds(
            (360.0, 520.0),
            ClipboardWindowPosition::MouseNearby,
            screen,
            Point {
                x: 800.0 / 1.5,
                y: 200.0 / 1.5,
            },
        );

        let physical = platform_window::physical_client_bounds_from_logical(bounds, 1.5);
        assert_eq!((physical.left, physical.top), (812, 212));
        assert_eq!(physical.right - physical.left, 540);
        assert_eq!(physical.bottom - physical.top, 780);
    }
}
