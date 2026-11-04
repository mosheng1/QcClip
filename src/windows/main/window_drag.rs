use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use gpui::{
    App, AppContext as _, BorrowAppContext as _, Context, DispatchPhase, Entity,
    InteractiveElement, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Point, Styled, Window, canvas,
};

use crate::{
    config_runtime,
    platform::window as platform_window,
    state_runtime::{PersistedEdgePosition, PersistedEdgeSide, PersistedWindowSize},
    windows::main::{MainWindowService, screen_boundary, window_state},
};

const EDGE_SNAP_DISTANCE_LOGICAL: f32 = 16.0;
// 先等待鼠标移动超过阈值，短按才能继续触发子控件的点击事件。
const WINDOW_DRAG_THRESHOLD: f32 = 4.0;
static NATIVE_MOVE_FINISHED: AtomicBool = AtomicBool::new(true);
static NATIVE_MOVE_STARTED: AtomicBool = AtomicBool::new(false);

/// 清除拖拽源区域的窗口拖拽起点，同时保留该元素自身的 GPUI 拖拽事件。
pub(crate) fn exclude_from_window_drag<E>(
    element: E,
    main_window: Entity<crate::windows::main::MainWindow>,
) -> E
where
    E: InteractiveElement,
{
    element.capture_any_mouse_down(move |_, _, cx| {
        let _ = main_window.update(cx, |main, _| {
            main.window_drag_start = None;
        });
        if let Some(owner) = cx
            .try_global::<MainWindowService>()
            .and_then(|service| service.main_view.clone())
        {
            owner.update(cx, |main, _| main.window_drag_start = None);
        }
    })
}

pub(crate) fn event_listener(
    edge_hide_enabled: bool,
    cx: &Context<crate::windows::main::MainWindow>,
) -> impl IntoElement {
    let main_window = cx.entity();
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let main_window_for_down = main_window.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, phase, _, cx| {
                if phase == DispatchPhase::Capture
                    && event.button == MouseButton::Left
                    && bounds.contains(&event.position)
                {
                    main_window_for_down.update(cx, |main, _| {
                        main.window_drag_start = Some(event.position);
                    });
                }
            });

            let main_window_for_move = main_window.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || !event.dragging() {
                    return;
                }
                let should_start = main_window_for_move.update(cx, |main, _| {
                    let Some(start) = main.window_drag_start else {
                        return false;
                    };
                    let dx = f32::from(event.position.x) - f32::from(start.x);
                    let dy = f32::from(event.position.y) - f32::from(start.y);
                    if dx * dx + dy * dy < WINDOW_DRAG_THRESHOLD * WINDOW_DRAG_THRESHOLD {
                        return false;
                    }
                    main.window_drag_start = None;
                    true
                });
                if should_start {
                    cx.stop_propagation();
                    begin_or_start_window_move(window, edge_hide_enabled, cx);
                }
            });

            let main_window_for_up = main_window.clone();
            window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                    main_window_for_up.update(cx, |main, _| {
                        main.window_drag_start = None;
                    });
                }
            });
        },
    )
    .absolute()
    .inset_0()
}

fn begin_or_start_window_move(window: &mut Window, edge_hide_enabled: bool, cx: &mut App) {
    if edge_hide_enabled {
        begin(window, cx);
    } else {
        window.start_window_move();
    }
}

pub(crate) fn begin(window: &mut Window, cx: &mut App) {
    let (attachment, normal_bounds, transition_active) =
        cx.read_global::<MainWindowService, _>(|service, _| {
            (
                service.window_state.edge_attachment(),
                service.window_state.normal_bounds(),
                service.window_state.is_visibility_transition_active(),
            )
        });
    if attachment.is_none()
        && transition_active
        && let Some(bounds) = normal_bounds
    {
        if let Err(error) = platform_window::set_client_bounds(window, bounds) {
            tracing::error!("开始拖拽时恢复主窗口尺寸失败：{error}");
            return;
        }
        window.bounds_changed(cx);
        if let Err(error) = platform_window::set_window_opacity(window, 1.0) {
            tracing::error!("开始拖拽时恢复主窗口透明度失败：{error}");
            return;
        }
    }
    if attachment.is_some()
        && let Err(error) = platform_window::set_window_shadow_visible(window, true)
    {
        tracing::error!("开始拖拽时恢复主窗口阴影失败：{error}");
        return;
    }
    if let Some(attachment) = attachment
        && let Err(error) = platform_window::set_client_position(
            window,
            attachment.client_bounds.left,
            attachment.client_bounds.top,
        )
    {
        tracing::error!("恢复贴边窗口位置失败：{error}");
        return;
    }

    if let Err(error) = platform_window::install_window_drag_constraints_hook(
        window,
        native_move_started,
        native_move_finished,
    ) {
        tracing::error!("安装窗口拖拽边界处理失败：{error}");
        return;
    }

    cx.update_global::<MainWindowService, _>(|service, _| {
        service.window_state.begin_drag();
    });
    crate::state_runtime::set_main_window_edge(None, cx);
    window.start_window_move();
}

pub(crate) fn handle_mouse_up(position: Point<f32>, cx: &mut App) {
    sync_edge_bounds_after_native_move(cx);
    let (window, drag_state, generation) = cx.read_global::<MainWindowService, _>(|service, _| {
        (
            service.window,
            service.window_state.drag_state(),
            service.window_state.drag_generation(),
        )
    });
    let Some(window) = window else {
        return;
    };
    if drag_state != window_state::DragState::NativeMove {
        return;
    }

    cx.update_global::<MainWindowService, _>(|service, _| {
        service.window_state.finish_drag();
    });
    let edge_hide_enabled = config_runtime::clipboard_config(cx).edge_hide;

    // Windows 的系统移动循环会在 WM_LBUTTONUP 之后才真正退出，等待
    // WM_EXITSIZEMOVE 后再读取窗口位置和执行释放吸附。
    cx.spawn(async move |cx| {
        for _ in 0..20 {
            if NATIVE_MOVE_FINISHED.load(Ordering::Acquire) {
                break;
            }
            cx.background_executor()
                .timer(Duration::from_millis(5))
                .await;
        }
        cx.update(|cx| {
            super::window_visibility::record_normal_position_after_drag(cx);
            if edge_hide_enabled {
                apply_release_snap(window, position, generation, cx);
            }
        });
    })
    .detach();
}

pub(crate) fn native_move_started() {
    NATIVE_MOVE_FINISHED.store(false, Ordering::Release);
    NATIVE_MOVE_STARTED.store(true, Ordering::Release);
}

pub(crate) fn native_move_finished() {
    NATIVE_MOVE_FINISHED.store(true, Ordering::Release);
}

pub(crate) fn native_move_in_progress() -> bool {
    !NATIVE_MOVE_FINISHED.load(Ordering::Acquire)
}

#[cfg(target_os = "windows")]
pub(crate) fn attach_if_near_edge(
    window: &Window,
    cx: &App,
) -> Result<Option<window_state::EdgeAttachment>, String> {
    let bounds = platform_window::physical_client_bounds(window)?;
    let boundaries = screen_boundary::ScreenBoundaries::capture_for_app(cx);
    let center_x = bounds.left + (bounds.right - bounds.left) / 2;
    let center_y = bounds.top + (bounds.bottom - bounds.top) / 2;
    let Some(monitor) = boundaries.monitor_for_point(center_x, center_y) else {
        return Ok(None);
    };
    let Some((snapped, side)) = snapped_bounds(bounds, monitor, window.scale_factor()) else {
        return Ok(None);
    };

    platform_window::set_client_position(window, snapped.left, snapped.top)?;
    let client_bounds = platform_window::physical_client_bounds(window)?;
    Ok(Some(window_state::EdgeAttachment {
        side,
        client_bounds,
        screen_bounds: monitor.physical_bounds(),
    }))
}

#[cfg(target_os = "windows")]
pub(crate) fn restore_persisted_edge(
    window: &mut Window,
    persisted: Option<PersistedEdgePosition>,
    cx: &mut App,
) -> Result<Option<window_state::EdgeAttachment>, String> {
    let Some(persisted) = persisted else {
        return Ok(None);
    };
    let cursor = platform_window::physical_cursor_position()
        .or_else(|| crate::platform::input::pointer_snapshot().map(|snapshot| snapshot.position));
    let display = platform_window::display_for_key(cx, persisted.display_key).or_else(|| {
        cursor.and_then(|point| platform_window::display_for_physical_point(cx, point))
    });
    let Some(display) = display else {
        return Ok(None);
    };

    let clipboard = config_runtime::clipboard_config(cx);
    let restored_size = crate::state_runtime::state(cx)
        .main_window
        .restored_layout_size(clipboard.effective_list_layout());
    let logical_size = (restored_size.width, restored_size.height);
    let (bounds, side) = persisted_edge_bounds(persisted, logical_size, display);

    platform_window::set_client_bounds(window, bounds)?;
    window.bounds_changed(cx);
    let client_bounds = platform_window::physical_client_bounds(window)?;
    Ok(Some(window_state::EdgeAttachment {
        side,
        client_bounds,
        screen_bounds: display.bounds,
    }))
}

fn persisted_edge_bounds(
    persisted: PersistedEdgePosition,
    logical_size: (f32, f32),
    display: platform_window::DisplayPlacement,
) -> (
    platform_window::PhysicalClientBounds,
    window_state::EdgeSide,
) {
    let converter = platform_window::CoordinateConverter::new(display.scale_factor);
    let width = converter.logical_to_physical_i32(logical_size.0.max(1.0));
    let height = converter.logical_to_physical_i32(logical_size.1.max(1.0));
    let along_edge_limit = match persisted.side {
        PersistedEdgeSide::Left | PersistedEdgeSide::Right => {
            converter.physical_to_logical((display.bounds.height - height as f32).max(0.0))
        }
        PersistedEdgeSide::Top | PersistedEdgeSide::Bottom => {
            converter.physical_to_logical((display.bounds.width - width as f32).max(0.0))
        }
    };
    let offset = converter.logical_to_physical_i32(persisted.offset.clamp(0.0, along_edge_limit));
    let screen_left = display.bounds.x.round() as i32;
    let screen_top = display.bounds.y.round() as i32;
    let screen_right = (display.bounds.x + display.bounds.width).round() as i32;
    let screen_bottom = (display.bounds.y + display.bounds.height).round() as i32;
    let (bounds, side) = match persisted.side {
        PersistedEdgeSide::Left => (
            platform_window::PhysicalClientBounds {
                left: screen_left,
                top: screen_top.saturating_add(offset),
                right: screen_left.saturating_add(width),
                bottom: screen_top.saturating_add(offset).saturating_add(height),
            },
            window_state::EdgeSide::Left,
        ),
        PersistedEdgeSide::Right => {
            let left = screen_right.saturating_sub(width);
            (
                platform_window::PhysicalClientBounds {
                    left,
                    top: screen_top.saturating_add(offset),
                    right: screen_right,
                    bottom: screen_top.saturating_add(offset).saturating_add(height),
                },
                window_state::EdgeSide::Right,
            )
        }
        PersistedEdgeSide::Top => (
            platform_window::PhysicalClientBounds {
                left: screen_left.saturating_add(offset),
                top: screen_top,
                right: screen_left.saturating_add(offset).saturating_add(width),
                bottom: screen_top.saturating_add(height),
            },
            window_state::EdgeSide::Top,
        ),
        PersistedEdgeSide::Bottom => {
            let top = screen_bottom.saturating_sub(height);
            (
                platform_window::PhysicalClientBounds {
                    left: screen_left.saturating_add(offset),
                    top,
                    right: screen_left.saturating_add(offset).saturating_add(width),
                    bottom: screen_bottom,
                },
                window_state::EdgeSide::Bottom,
            )
        }
    };
    (bounds, side)
}

#[cfg(target_os = "windows")]
pub(crate) fn startup_edge_bounds(
    cx: &App,
) -> Option<(gpui::Bounds<gpui::Pixels>, gpui::DisplayId)> {
    let state = crate::state_runtime::state(cx);
    let persisted = state.main_window.edge?;
    let clipboard = config_runtime::clipboard_config(cx);
    let logical_size = state
        .main_window
        .restored_layout_size(clipboard.effective_list_layout());
    let cursor = platform_window::physical_cursor_position()
        .or_else(|| crate::platform::input::pointer_snapshot().map(|snapshot| snapshot.position));
    let display = platform_window::display_for_key(cx, persisted.display_key).or_else(|| {
        cursor.and_then(|point| platform_window::display_for_physical_point(cx, point))
    })?;
    Some(startup_edge_bounds_for_persisted(
        persisted,
        logical_size,
        display,
    ))
}

#[cfg(target_os = "windows")]
fn startup_edge_bounds_for_persisted(
    persisted: PersistedEdgePosition,
    logical_size: PersistedWindowSize,
    display: platform_window::DisplayPlacement,
) -> (gpui::Bounds<gpui::Pixels>, gpui::DisplayId) {
    let converter = platform_window::CoordinateConverter::new(display.scale_factor);
    let screen = converter.logical_display_bounds(display.bounds);
    let width = logical_size.width.max(1.0);
    let height = logical_size.height.max(1.0);
    let offset_limit = match persisted.side {
        PersistedEdgeSide::Left | PersistedEdgeSide::Right => (screen.height - height).max(0.0),
        PersistedEdgeSide::Top | PersistedEdgeSide::Bottom => (screen.width - width).max(0.0),
    };
    let offset = persisted.offset.clamp(0.0, offset_limit);
    let (left, top) = match persisted.side {
        PersistedEdgeSide::Left => (screen.x, screen.y + offset),
        PersistedEdgeSide::Right => (screen.x + screen.width - width, screen.y + offset),
        PersistedEdgeSide::Top => (screen.x + offset, screen.y),
        PersistedEdgeSide::Bottom => (screen.x + offset, screen.y + screen.height - height),
    };
    (
        gpui::Bounds::new(
            gpui::point(gpui::px(left), gpui::px(top)),
            gpui::size(gpui::px(width), gpui::px(height)),
        ),
        display.display_id,
    )
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn startup_edge_bounds(
    _cx: &App,
) -> Option<(gpui::Bounds<gpui::Pixels>, gpui::DisplayId)> {
    None
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn restore_persisted_edge(
    _window: &mut Window,
    _persisted: Option<PersistedEdgePosition>,
    _cx: &mut App,
) -> Result<Option<window_state::EdgeAttachment>, String> {
    Ok(None)
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn attach_if_near_edge(
    _window: &Window,
    _cx: &App,
) -> Result<Option<window_state::EdgeAttachment>, String> {
    Ok(None)
}

pub(crate) fn sync_window_size_after_native_resize(cx: &mut App) {
    let (window, can_record, panel) = cx.read_global::<MainWindowService, _>(|service, _| {
        (
            service.window,
            service.window_state.can_record_window_geometry(),
            service.window_state.panel_attachment(),
        )
    });
    let Some(window) = window else {
        return;
    };
    let result = window.update(cx, |_, window, _| {
        if !platform_window::take_window_resize_finished(window)?
            || (!can_record && panel.is_none())
        {
            return Ok::<_, String>(None);
        }
        let mut bounds = platform_window::physical_client_bounds(window)?;
        let logical_size = platform_window::CoordinateConverter::new(window.scale_factor())
            .logical_client_size(bounds);
        if let Some(panel) = panel {
            let screen = panel.screen_bounds;
            let horizontal = matches!(
                panel.side,
                window_state::EdgeSide::Top | window_state::EdgeSide::Bottom
            );
            let axis_limit = if horizontal {
                screen.height
            } else {
                screen.width
            };
            let thickness = platform_window::CoordinateConverter::new(window.scale_factor())
                .logical_to_physical_i32(
                    (if horizontal {
                        logical_size.1
                    } else {
                        logical_size.0
                    })
                    .max(PersistedWindowSize::MIN_SIZE),
                )
                .min((axis_limit / 2.0) as i32);
            let screen_left = screen.x.round() as i32;
            let screen_top = screen.y.round() as i32;
            let screen_right = (screen.x + screen.width).round() as i32;
            let screen_bottom = (screen.y + screen.height).round() as i32;
            match panel.side {
                window_state::EdgeSide::Left => {
                    bounds.left = screen_left;
                    bounds.right = bounds.left.saturating_add(thickness);
                    bounds.top = screen_top;
                    bounds.bottom = screen_bottom;
                }
                window_state::EdgeSide::Right => {
                    bounds.right = screen_right;
                    bounds.left = bounds.right.saturating_sub(thickness);
                    bounds.top = screen_top;
                    bounds.bottom = screen_bottom;
                }
                window_state::EdgeSide::Top => {
                    bounds.top = screen_top;
                    bounds.bottom = bounds.top.saturating_add(thickness);
                    bounds.left = screen_left;
                    bounds.right = screen_right;
                }
                window_state::EdgeSide::Bottom => {
                    bounds.bottom = screen_bottom;
                    bounds.top = bounds.bottom.saturating_sub(thickness);
                    bounds.left = screen_left;
                    bounds.right = screen_right;
                }
            }
            let logical = platform_window::CoordinateConverter::new(window.scale_factor())
                .physical_to_logical(thickness as f32);
            return Ok(Some((bounds, logical_size, Some((panel, logical)))));
        }
        Ok(Some((bounds, logical_size, None)))
    });
    match result {
        Ok(Ok(Some((bounds, logical_size, panel)))) => {
            if let Some((mut attachment, thickness)) = panel {
                attachment.client_bounds = bounds;
                if let Err(error) = window.update(cx, |_, window, cx| {
                    platform_window::set_client_bounds(window, bounds)?;
                    window.bounds_changed(cx);
                    Ok::<_, String>(())
                }) {
                    tracing::error!("调整边缘窗口尺寸失败：{error}");
                    return;
                }
                cx.update_global::<MainWindowService, _>(|service, _| {
                    service.window_state.set_panel_attachment(Some(attachment));
                });
                let side = match attachment.side {
                    window_state::EdgeSide::Left => PersistedEdgeSide::Left,
                    window_state::EdgeSide::Right => PersistedEdgeSide::Right,
                    window_state::EdgeSide::Top => PersistedEdgeSide::Top,
                    window_state::EdgeSide::Bottom => PersistedEdgeSide::Bottom,
                };
                crate::state_runtime::set_main_window_edge_panel_size(side, thickness, cx);
                return;
            }
            let size = PersistedWindowSize {
                width: logical_size.0,
                height: logical_size.1,
            };
            if !size.is_valid() {
                return;
            }
            cx.update_global::<MainWindowService, _>(|service, _| {
                if service.window_state.edge_attachment().is_none() {
                    service
                        .window_state
                        .remember_normal_geometry(bounds, logical_size);
                }
            });
            crate::state_runtime::set_main_window_size(size, cx);
        }
        Ok(Ok(None)) => {}
        Ok(Err(error)) => tracing::error!("记录主窗口缩放尺寸失败：{error}"),
        Err(error) => tracing::error!("记录主窗口缩放尺寸失败：{error}"),
    }
}

pub(crate) fn sync_edge_bounds_after_native_move(cx: &mut App) {
    sync_window_size_after_native_resize(cx);
    if !NATIVE_MOVE_FINISHED.load(Ordering::Acquire)
        || !NATIVE_MOVE_STARTED.swap(false, Ordering::AcqRel)
    {
        return;
    }

    let (window, drag_state, attachment, can_record_bounds) = cx
        .read_global::<MainWindowService, _>(|service, _| {
            (
                service.window,
                service.window_state.drag_state(),
                service.window_state.edge_attachment(),
                service.window_state.can_record_edge_bounds(),
            )
        });
    if drag_state != window_state::DragState::Idle {
        return;
    }
    let Some(window) = window else {
        return;
    };
    let Some(attachment) = attachment else {
        return;
    };
    // 窗口隐藏或正在做显示/隐藏动画时，窗口当前坐标是屏幕外的贴边位置，
    // 记录成贴边可见位置会让后续“显示”把窗口动画到屏幕外。
    if !can_record_bounds {
        return;
    }

    let result = window.update(cx, |_, window, cx| {
        let client_bounds = platform_window::physical_client_bounds(window)?;
        let boundaries = screen_boundary::ScreenBoundaries::capture_for_app(cx);
        let center_x = client_bounds.left + (client_bounds.right - client_bounds.left) / 2;
        let center_y = client_bounds.top + (client_bounds.bottom - client_bounds.top) / 2;
        let screen_bounds = boundaries
            .monitor_for_point(center_x, center_y)
            .map(|monitor| monitor.physical_bounds())
            .unwrap_or(attachment.screen_bounds);
        Ok::<_, String>((client_bounds, screen_bounds))
    });
    let Ok(Ok((client_bounds, screen_bounds))) = result else {
        return;
    };

    cx.update_global::<MainWindowService, _>(|service, _| {
        service
            .window_state
            .update_edge_bounds(client_bounds, screen_bounds);
    });
    super::window_visibility::persist_edge_position(
        window_state::EdgeAttachment {
            side: attachment.side,
            client_bounds,
            screen_bounds,
        },
        cx,
    );
}

fn apply_release_snap(
    window: gpui::AnyWindowHandle,
    position: Point<f32>,
    generation: u64,
    cx: &mut App,
) {
    let is_current_release = cx.read_global::<MainWindowService, _>(|service, _| {
        service.window_state.drag_state() == window_state::DragState::Idle
            && service.window_state.drag_generation() == generation
            && service.window_state.edge_attachment().is_none()
            && service.window_state.is_logically_visible()
    });
    if !is_current_release {
        return;
    }

    let result = window.update(cx, |_, window, cx| {
        let bounds = platform_window::physical_client_bounds(window)?;
        let boundaries = screen_boundary::ScreenBoundaries::capture_for_app(cx);
        let Some(monitor) =
            boundaries.monitor_for_point(position.x.round() as i32, position.y.round() as i32)
        else {
            return Ok::<Option<window_state::EdgeAttachment>, String>(None);
        };
        let scale_factor = platform_window::display_for_physical_point(cx, position)
            .map(|display| display.scale_factor)
            .unwrap_or_else(|| window.scale_factor());
        let Some((snapped, side)) = snapped_bounds(bounds, monitor, scale_factor) else {
            return Ok(None);
        };
        platform_window::set_client_position(window, snapped.left, snapped.top)?;
        let client_bounds = platform_window::physical_client_bounds(window)?;
        Ok(Some(window_state::EdgeAttachment {
            side,
            client_bounds,
            screen_bounds: monitor.physical_bounds(),
        }))
    });
    let attachment = match result {
        Ok(Ok(attachment)) => attachment,
        Ok(Err(error)) => {
            tracing::error!("主窗口贴边失败：{error}");
            None
        }
        Err(error) => {
            tracing::error!("处理主窗口拖拽失败：{error}");
            None
        }
    };
    if let Some(attachment) = attachment
        && cx.read_global::<MainWindowService, _>(|service, _| {
            service.window_state.drag_generation() == generation
        })
    {
        super::window_visibility::attach_edge_and_remember(attachment, cx);
    }
    super::window_visibility::sync_mouse_leave_state(cx);
}

fn snapped_bounds(
    bounds: platform_window::PhysicalClientBounds,
    monitor: screen_boundary::MonitorBoundary,
    scale_factor: f32,
) -> Option<(
    platform_window::PhysicalClientBounds,
    window_state::EdgeSide,
)> {
    let screen_bounds = monitor.physical_bounds();
    let candidates = [
        monitor.left_edge.then_some((
            (bounds.left as f32 - screen_bounds.x).abs(),
            window_state::EdgeSide::Left,
        )),
        monitor.right_edge.then_some((
            (bounds.right as f32 - (screen_bounds.x + screen_bounds.width)).abs(),
            window_state::EdgeSide::Right,
        )),
        monitor.top_edge.then_some((
            (bounds.top as f32 - screen_bounds.y).abs(),
            window_state::EdgeSide::Top,
        )),
        monitor.bottom_edge.then_some((
            (bounds.bottom as f32 - (screen_bounds.y + screen_bounds.height)).abs(),
            window_state::EdgeSide::Bottom,
        )),
    ];
    let (distance, side) = candidates
        .into_iter()
        .flatten()
        .min_by(|left, right| left.0.total_cmp(&right.0))?;
    let snap_distance = platform_window::CoordinateConverter::new(scale_factor)
        .logical_to_physical(EDGE_SNAP_DISTANCE_LOGICAL);
    if distance > snap_distance {
        return None;
    }

    let width = bounds.right - bounds.left;
    let height = bounds.bottom - bounds.top;
    let mut snapped = bounds;
    match side {
        window_state::EdgeSide::Left => snapped.left = screen_bounds.x.round() as i32,
        window_state::EdgeSide::Right => {
            snapped.left = (screen_bounds.x + screen_bounds.width).round() as i32 - width
        }
        window_state::EdgeSide::Top => snapped.top = screen_bounds.y.round() as i32,
        window_state::EdgeSide::Bottom => {
            snapped.top = (screen_bounds.y + screen_bounds.height).round() as i32 - height
        }
    }
    snapped.right = snapped.left + width;
    snapped.bottom = snapped.top + height;
    Some((snapped, side))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display() -> platform_window::DisplayPlacement {
        platform_window::DisplayPlacement {
            display_id: gpui::DisplayId::new(1),
            display_key: Some(1),
            scale_factor: 1.5,
            bounds: platform_window::PhysicalDisplayBounds {
                x: 1920.0,
                y: -100.0,
                width: 2560.0,
                height: 1440.0,
            },
        }
    }

    #[test]
    fn native_move_state_follows_system_move_callbacks() {
        NATIVE_MOVE_FINISHED.store(true, Ordering::Release);
        NATIVE_MOVE_STARTED.store(false, Ordering::Release);
        assert!(!native_move_in_progress());

        native_move_started();
        assert!(native_move_in_progress());

        native_move_finished();
        assert!(!native_move_in_progress());
        NATIVE_MOVE_STARTED.store(false, Ordering::Release);
    }

    #[test]
    fn window_snaps_to_the_nearest_screen_edge() {
        let screen = screen_boundary::MonitorBoundary {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            left_edge: true,
            right_edge: true,
            top_edge: true,
            bottom_edge: true,
        };
        let bounds = platform_window::PhysicalClientBounds {
            left: 5,
            top: 200,
            right: 365,
            bottom: 720,
        };

        let (snapped, side) = snapped_bounds(bounds, screen, 1.0).expect("窗口应当贴到左边");

        assert_eq!(side, window_state::EdgeSide::Left);
        assert_eq!(snapped.left, 0);
        assert_eq!(snapped.top, 200);
        assert!(
            snapped_bounds(
                platform_window::PhysicalClientBounds {
                    left: 100,
                    top: 200,
                    right: 460,
                    bottom: 720,
                },
                screen,
                1.0,
            )
            .is_none()
        );
    }

    #[test]
    fn internal_monitor_edge_is_not_a_snap_candidate() {
        let monitor = screen_boundary::MonitorBoundary {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            left_edge: true,
            right_edge: false,
            top_edge: true,
            bottom_edge: true,
        };
        let bounds = platform_window::PhysicalClientBounds {
            left: 1557,
            top: 200,
            right: 1917,
            bottom: 720,
        };

        assert!(snapped_bounds(bounds, monitor, 1.0).is_none());
    }

    #[test]
    fn window_snap_distance_uses_sixteen_logical_pixels() {
        let screen = screen_boundary::MonitorBoundary {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            left_edge: true,
            right_edge: true,
            top_edge: true,
            bottom_edge: true,
        };

        let mut bounds = platform_window::PhysicalClientBounds {
            left: 16,
            top: 200,
            right: 376,
            bottom: 720,
        };
        assert!(snapped_bounds(bounds, screen, 1.0).is_some());

        bounds.left = 17;
        bounds.right = 377;
        assert!(snapped_bounds(bounds, screen, 1.0).is_none());

        bounds.left = 24;
        bounds.right = 384;
        assert!(snapped_bounds(bounds, screen, 1.5).is_some());

        bounds.left = 25;
        bounds.right = 385;
        assert!(snapped_bounds(bounds, screen, 1.5).is_none());
    }

    #[test]
    fn persisted_edge_position_restores_right_edge_in_logical_pixels() {
        let (bounds, side) = persisted_edge_bounds(
            PersistedEdgePosition {
                display_key: 1,
                side: PersistedEdgeSide::Right,
                offset: 140.0,
            },
            (360.0, 520.0),
            display(),
        );

        assert_eq!(side, window_state::EdgeSide::Right);
        assert_eq!(bounds.left, 3940);
        assert_eq!(bounds.right, 4480);
        assert_eq!(bounds.top, 110);
        assert_eq!(bounds.bottom, 890);
    }

    #[test]
    fn persisted_edge_position_clamps_offset_to_the_target_display() {
        let (bounds, side) = persisted_edge_bounds(
            PersistedEdgePosition {
                display_key: 1,
                side: PersistedEdgeSide::Bottom,
                offset: 2000.0,
            },
            (360.0, 520.0),
            display(),
        );

        assert_eq!(side, window_state::EdgeSide::Bottom);
        assert_eq!(bounds.left, 3940);
        assert_eq!(bounds.right, 4480);
        assert_eq!(bounds.top, 560);
        assert_eq!(bounds.bottom, 1340);
    }

    #[test]
    fn startup_edge_bounds_remain_logical_on_a_scaled_display() {
        let (bounds, display_id) = startup_edge_bounds_for_persisted(
            PersistedEdgePosition {
                display_key: 1,
                side: PersistedEdgeSide::Right,
                offset: 140.0,
            },
            PersistedWindowSize {
                width: 360.0,
                height: 520.0,
            },
            display(),
        );

        assert_eq!(display_id, gpui::DisplayId::new(1));
        assert!((f32::from(bounds.origin.x) - 2626.6667).abs() < 0.001);
        assert!((f32::from(bounds.origin.y) - 73.3333).abs() < 0.001);
        assert_eq!(f32::from(bounds.size.width), 360.0);
        assert_eq!(f32::from(bounds.size.height), 520.0);
    }

    #[test]
    fn edge_restore_uses_default_size_for_a_saved_animation_frame() {
        let state = crate::state_runtime::PersistedMainWindowState {
            size: Some(PersistedWindowSize {
                width: 360.0,
                height: 1.0,
            }),
            ..Default::default()
        };
        let edge = PersistedEdgePosition {
            display_key: 1,
            side: PersistedEdgeSide::Right,
            offset: 140.0,
        };
        let size = state.restored_size();
        let (startup, _) = startup_edge_bounds_for_persisted(edge, size, display());
        let (restored, _) = persisted_edge_bounds(edge, (size.width, size.height), display());
        assert_eq!(f32::from(startup.size.height), 520.0);
        assert_eq!(restored.bottom - restored.top, 780);
    }
}
