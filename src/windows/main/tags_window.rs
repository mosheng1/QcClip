use gpui::{
    App, AppContext as _, BorrowAppContext as _, Bounds, Context, Entity, Global,
    InteractiveElement, IntoElement, MouseButton, MouseMoveEvent, MouseUpEvent, ParentElement,
    PlatformInput, Point, Render, Styled, Subscription, Window, WindowHandle, div, point, px, size,
};

use super::{MainWindow, MainWindowService, components::tags_panel, window_drag, window_state};
use crate::{config_runtime, platform::window as platform_window};
use gpui_component::ActiveTheme as _;

type Rect = Bounds<f32>;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Placement {
    bounds: Rect,
    horizontal: bool,
}

#[derive(Default)]
struct TagsWindowService {
    window: Option<WindowHandle<gpui_component::Root>>,
    placement: Option<Placement>,
    // 将窗口间隙纳入交互范围，避免跨越间隙时触发自动隐藏。
    bridge: Option<Rect>,
    sync_pending: bool,
    last_error: Option<String>,
    hovered_window: Option<gpui::AnyWindowHandle>,
}

impl Global for TagsWindowService {}

impl TagsWindowService {
    fn update_error(&mut self, error: Option<String>) -> bool {
        let changed = error.is_some() && self.last_error != error;
        self.last_error = error;
        changed
    }
}

pub(super) fn install(cx: &mut App) {
    cx.set_global(TagsWindowService::default());
    cx.on_window_closed(|cx, id| {
        if cx
            .global::<TagsWindowService>()
            .window
            .is_some_and(|window| window.window_id() == id)
        {
            cx.update_global::<TagsWindowService, _>(|service, _| {
                service.window = None;
                service.placement = None;
                service.bridge = None;
                service.hovered_window = None;
            });
        }
    })
    .detach();
}

pub(super) fn request_sync(cx: &mut App) {
    let Some(service) = cx.try_global::<TagsWindowService>() else {
        return;
    };
    if service.sync_pending {
        return;
    }
    cx.update_global::<TagsWindowService, _>(|service, _| service.sync_pending = true);
    cx.defer(|cx| {
        cx.update_global::<TagsWindowService, _>(|service, _| service.sync_pending = false);
        match sync(cx) {
            Ok(()) => {
                cx.update_global::<TagsWindowService, _>(|service, _| service.update_error(None));
            }
            Err(error) => {
                let changed = cx.update_global::<TagsWindowService, _>(|service, _| {
                    service.update_error(Some(error.clone()))
                });
                if changed {
                    tracing::error!("同步外置标签面板失败：{error}");
                }
                hide(cx);
            }
        }
    });
}

pub(super) fn hide(cx: &mut App) {
    let Some(service) = cx.try_global::<TagsWindowService>() else {
        return;
    };
    let window = service.window;
    if service.placement.is_none() {
        return;
    }
    cx.update_global::<TagsWindowService, _>(|service, _| {
        service.placement = None;
        service.bridge = None;
        service.hovered_window = None;
    });
    if let Some(window) = window {
        let _ = window.update(cx, |_, window, cx| {
            if let Err(error) = crate::services::focus::restore_previous_focus_for(window) {
                tracing::error!("隐藏标签面板时恢复焦点失败：{error}");
            }
            window.blur(cx);
            platform_window::BorderlessPopup.set_visible(window, false);
        });
    }
}

pub(super) fn close(cx: &mut App) {
    hide(cx);
    if let Some(window) = cx.global::<TagsWindowService>().window {
        let _ = window.update(cx, |_, window, _| window.remove_window());
    }
}

pub(super) fn has_modal(cx: &App) -> bool {
    service_window(cx).is_some_and(|window| {
        crate::windows::dialog::DialogWindowService::has_modal_for(window.window_id(), cx)
    })
}

pub(super) fn contains_point(position: Point<f32>, cx: &App) -> bool {
    cx.try_global::<TagsWindowService>().is_some_and(|service| {
        service
            .placement
            .is_some_and(|placement| placement.bounds.contains(&position))
            || service
                .bridge
                .is_some_and(|bridge| bridge.contains(&position))
    })
}

pub(super) fn sync_pointer(cx: &mut App) {
    let Some(service) = cx.try_global::<TagsWindowService>() else {
        return;
    };
    if service.window.is_none() {
        return;
    }
    if window_drag::native_move_in_progress() {
        hide(cx);
        return;
    }
    request_sync(cx);
}

fn sync_hovered_window(cx: &mut App) -> Result<(), String> {
    let Some(panel) = service_window(cx) else {
        return Ok(());
    };
    let Some(main) = MainWindowService::window_handle(cx) else {
        return Ok(());
    };
    if crate::windows::menu::MenuService::has_open_menu(cx)
        || crate::windows::dialog::DialogWindowService::has_modal_for(main.window_id(), cx)
        || crate::windows::dialog::DialogWindowService::has_modal_for(panel.window_id(), cx)
        || cx.has_active_drag()
    {
        return Ok(());
    }
    let Some(position) = platform_window::physical_cursor_position() else {
        return Ok(());
    };
    let target = [main, panel.into()].into_iter().find(|handle| {
        handle
            .update(cx, |_, window, _| {
                platform_window::is_window_at_physical_point(window, position)
            })
            .unwrap_or(false)
    });
    let previous = cx.global::<TagsWindowService>().hovered_window;
    if previous.map(|handle| handle.window_id()) == target.map(|handle| handle.window_id()) {
        return Ok(());
    }
    cx.update_global::<TagsWindowService, _>(|service, _| service.hovered_window = target);
    if let Some(target) = target {
        target
            .update(cx, |_, window, _| {
                platform_window::raise_window_to_topmost(window)
            })
            .map_err(|error| error.to_string())??;
    }
    Ok(())
}

fn service_window(cx: &App) -> Option<WindowHandle<gpui_component::Root>> {
    let service = cx.try_global::<TagsWindowService>()?;
    service.placement.and(service.window)
}

fn sync(cx: &mut App) -> Result<(), String> {
    let clipboard = config_runtime::clipboard_config(cx);
    let service = cx.global::<MainWindowService>();
    let Some(main) = service.main_view.clone() else {
        hide(cx);
        return Ok(());
    };
    let main_state = main.read(cx);
    let visible = clipboard.tags_panel_external
        && main_state.tags_panel_open
        && main_state.active_tab != super::tabs::TabId::Emoji
        && service.window_state.is_logically_visible()
        && !service.window_state.is_visibility_transition_active()
        && service.window_state.drag_state() == window_state::DragState::Idle
        && !window_drag::native_move_in_progress()
        && MainWindowService::blocking_operation(cx).is_none();
    let Some(main_handle) = service.window.filter(|_| visible) else {
        hide(cx);
        return Ok(());
    };
    let (main_bounds, work_area, scale, display_id) = main_handle
        .update(cx, |_, window, cx| {
            let bounds = platform_window::physical_client_bounds(window)?;
            let (work_area, display_id) = platform_window::window_work_area(window, cx)?;
            Ok::<_, String>((
                Rect::new(
                    point(bounds.left as f32, bounds.top as f32),
                    size(
                        (bounds.right - bounds.left) as f32,
                        (bounds.bottom - bounds.top) as f32,
                    ),
                ),
                Rect::new(
                    point(work_area.x, work_area.y),
                    size(work_area.width, work_area.height),
                ),
                window.scale_factor() * crate::ui::scale::window_scale(window),
                display_id,
            ))
        })
        .map_err(|error| error.to_string())??;
    let placement = place_panel(main_bounds, work_area, scale);
    let previous = cx.global::<TagsWindowService>().placement;
    if previous == Some(placement) {
        return sync_hovered_window(cx);
    }
    let handle = match cx.global::<TagsWindowService>().window {
        Some(window) => window,
        None => {
            let window = platform_window::open_borderless_popup(
                cx,
                Bounds::new(point(px(0.0), px(0.0)), size(px(100.0), px(360.0))),
                Some(display_id),
                move |window, cx, _| {
                    window.on_window_should_close(cx, |_, cx| {
                        if let Some(main) = cx.global::<MainWindowService>().main_view.clone() {
                            main.update(cx, |main, cx| {
                                main.tags_panel_open = false;
                                cx.notify();
                            });
                        }
                        request_sync(cx);
                        false
                    });
                    let panel = cx.new(|cx| {
                        let subscription = cx.observe(&main, |_, _, cx| cx.notify());
                        TagsWindow {
                            main,
                            _subscription: subscription,
                        }
                    });
                    cx.new(|cx| crate::window_material::new_root(panel, window, cx))
                },
            )?;
            cx.update_global::<TagsWindowService, _>(|service, _| service.window = Some(window));
            window
        }
    };
    cx.update_global::<TagsWindowService, _>(|service, _| {
        service.placement = Some(placement);
        service.bridge = panel_bridge(main_bounds, placement.bounds);
        service.hovered_window = None;
    });
    handle
        .update(cx, |_, window, cx| {
            let bounds = placement.bounds;
            if !platform_window::BorderlessPopup.prepare_to_physical_bounds(
                window,
                cx,
                bounds.origin.x,
                bounds.origin.y,
                bounds.size.width,
                bounds.size.height,
            ) {
                return Err("调整外置标签面板位置失败".to_owned());
            }
            platform_window::show_popup(window)?;
            window.refresh();
            Ok(())
        })
        .map_err(|error| error.to_string())??;
    sync_hovered_window(cx)
}

fn panel_bridge(main: Rect, panel: Rect) -> Option<Rect> {
    let left = main.origin.x.max(panel.origin.x);
    let right = main.right().min(panel.right());
    let top = main.origin.y.max(panel.origin.y);
    let bottom = main.bottom().min(panel.bottom());
    if bottom > top {
        let x = main.right().min(panel.right());
        let end = main.origin.x.max(panel.origin.x);
        if end >= x {
            return Some(Rect::new(point(x, top), size(end - x, bottom - top)));
        }
    }
    if right > left {
        let y = main.bottom().min(panel.bottom());
        let end = main.origin.y.max(panel.origin.y);
        if end >= y {
            return Some(Rect::new(point(left, y), size(right - left, end - y)));
        }
    }
    None
}

fn place_panel(main: Rect, screen: Rect, scale: f32) -> Placement {
    use window_state::EdgeSide::*;
    let scale = scale.max(0.5);
    let gap = 6.0 * scale;
    let sides = [Right, Left, Bottom, Top];
    let screen_right = screen.origin.x + screen.size.width;
    // 先尝试完整尺寸，再按可用空间缩小；两轮都无法相邻放置时才覆盖主窗口。
    for compact in [false, true] {
        for side in sides {
            let horizontal = matches!(side, Top | Bottom);
            let desired = if horizontal {
                size(main.size.width, (120.0 * scale).min(main.size.height))
            } else {
                size(100.0 * scale, main.size.height)
            };
            // 只检查放置方向的空间，另一轴始终沿用主窗口的位置和尺寸。
            let available = match side {
                Right => (screen_right - main.right() - gap).min(screen.size.width),
                Left => (main.origin.x - screen.origin.x - gap).min(screen.size.width),
                Bottom => (screen.bottom() - main.bottom() - gap).min(screen.size.height),
                Top => (main.origin.y - screen.origin.y - gap).min(screen.size.height),
            };
            let minimum = if horizontal {
                (100.0 * scale).min(desired.height)
            } else {
                96.0 * scale
            };
            let dimensions = if compact && horizontal {
                size(desired.width, desired.height.min(available))
            } else if compact {
                size(desired.width.min(available), desired.height)
            } else {
                desired
            };
            let extent = if horizontal {
                dimensions.height
            } else {
                dimensions.width
            };
            if extent < minimum || extent > available {
                continue;
            }
            let mut x = match side {
                Right => main.origin.x + main.size.width + gap,
                Left => main.origin.x - gap - dimensions.width,
                Top | Bottom => main.origin.x,
            };
            let mut y = match side {
                Bottom => main.origin.y + main.size.height + gap,
                Top => main.origin.y - gap - dimensions.height,
                Right | Left => main.origin.y,
            };
            if horizontal {
                y = y.clamp(screen.origin.y, screen.bottom() - dimensions.height);
            } else {
                x = x.clamp(screen.origin.x, screen_right - dimensions.width);
            }
            return Placement {
                bounds: Rect::new(point(x, y), dimensions),
                horizontal,
            };
        }
    }
    let dimensions = size(100.0 * scale, main.size.height);
    let dimensions = size(
        dimensions.width.min(screen.size.width).max(1.0),
        dimensions.height.max(1.0),
    );
    Placement {
        bounds: Rect::new(
            point(
                (main.origin.x + main.size.width - dimensions.width)
                    .clamp(screen.origin.x, screen_right - dimensions.width),
                main.origin.y,
            ),
            dimensions,
        ),
        horizontal: false,
    }
}

struct TagsWindow {
    main: Entity<MainWindow>,
    _subscription: Subscription,
}

impl Render for TagsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(cx.theme().font_size);
        if !config_runtime::clipboard_config(cx).tags_panel_external || service_window(cx).is_none()
        {
            return div().into_any_element();
        }
        let horizontal = cx
            .global::<TagsWindowService>()
            .placement
            .is_some_and(|placement| placement.horizontal);
        let content = self.main.update(cx, |main, cx| {
            main.sync_tags_panel_host(true);
            main.ensure_tags_search_input(window, cx);
            main.sync_tags_search_placeholder(window, cx);
            tags_panel::render_external(main, window, cx, horizontal).into_any_element()
        });
        div()
            .size_full()
            .child(content)
            .on_mouse_move_all(|event, phase, _, window, cx| {
                if phase == gpui::DispatchPhase::Capture && event.dragging() {
                    forward_drag(window, false, cx);
                }
            })
            .on_mouse_up_all(|event, phase, _, window, cx| {
                if phase == gpui::DispatchPhase::Capture && event.button == MouseButton::Left {
                    forward_drag(window, true, cx);
                }
            })
            .into_any_element()
    }
}

// Windows 捕获鼠标后仍向源窗口发送事件；将主窗口范围内的标签拖放交给原有命中和 on_drop 处理。
fn forward_drag(source: &Window, drop: bool, cx: &mut App) {
    if !cx
        .active_drag()
        .is_some_and(|drag| drag.value.is::<tags_panel::TagDrag>())
    {
        return;
    }
    let Some(position) = platform_window::physical_cursor_position() else {
        return;
    };
    let Some(main) = MainWindowService::window_handle(cx) else {
        return;
    };
    let modifiers = source.modifiers();
    let _ = main.update(cx, |_, window, cx| {
        let Ok(bounds) = platform_window::physical_client_bounds(window) else {
            return;
        };
        let inside = super::window_visibility::point_in_client_bounds(position, bounds);
        if drop && !inside {
            return;
        }
        let scale = window.scale_factor().max(0.1);
        let position = point(
            px((position.x - bounds.left as f32) / scale),
            px((position.y - bounds.top as f32) / scale),
        );
        dispatch_tag_drag(position, modifiers, drop, window, cx);
    });
}

pub(super) fn dispatch_tag_drag(
    position: Point<gpui::Pixels>,
    modifiers: gpui::Modifiers,
    drop: bool,
    window: &mut Window,
    cx: &mut App,
) {
    window.dispatch_event(
        PlatformInput::MouseMove(MouseMoveEvent {
            position,
            pressed_button: Some(MouseButton::Left),
            modifiers,
        }),
        cx,
    );
    if drop {
        window.dispatch_event(
            PlatformInput::MouseUp(MouseUpEvent {
                position,
                button: MouseButton::Left,
                modifiers,
                click_count: 1,
            }),
            cx,
        );
        if let Some(main) = cx.global::<MainWindowService>().main_view.clone() {
            main.update(cx, |main, cx| main.finish_tag_drag_preview(cx));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use window_state::EdgeSide::*;

    #[test]
    fn placement_checks_space_only_in_the_placement_direction() {
        let screen = Rect::new(point(-1920.0, 0.0), size(1920.0, 1040.0));
        for (main, expected_side) in [
            (Rect::new(point(-1200.0, 100.0), size(360.0, 520.0)), Right),
            (Rect::new(point(-400.0, 100.0), size(360.0, 520.0)), Left),
            (Rect::new(point(-1920.0, 0.0), size(1920.0, 300.0)), Bottom),
            (Rect::new(point(-1920.0, 740.0), size(1920.0, 300.0)), Top),
            (Rect::new(point(-1920.0, 0.0), size(360.0, 1040.0)), Right),
            (Rect::new(point(-360.0, 0.0), size(360.0, 1040.0)), Left),
        ] {
            let panel = place_panel(main, screen, 1.0);
            assert_eq!(panel.horizontal, matches!(expected_side, Top | Bottom));
            match expected_side {
                Right => assert_eq!(panel.bounds.origin.x, main.right() + 6.0),
                Left => assert_eq!(panel.bounds.right(), main.origin.x - 6.0),
                Bottom => assert_eq!(panel.bounds.origin.y, main.bottom() + 6.0),
                Top => assert_eq!(panel.bounds.bottom(), main.origin.y - 6.0),
            }
            assert!(!main.intersects(&panel.bounds));
            if panel.horizontal {
                assert_eq!(panel.bounds.size.width, main.size.width);
                assert_eq!(panel.bounds.origin.x, main.origin.x);
                assert!(panel.bounds.origin.y >= screen.origin.y);
                assert!(panel.bounds.bottom() <= screen.bottom());
            } else {
                assert_eq!(panel.bounds.size.height, main.size.height);
                assert_eq!(panel.bounds.origin.y, main.origin.y);
                assert!(panel.bounds.origin.x >= screen.origin.x);
                assert!(panel.bounds.right() <= screen.right());
            }
            assert!(panel.bounds.size.height <= main.size.height);
            assert!(
                panel.bounds.right() + 6.0 == main.origin.x
                    || panel.bounds.origin.x == main.right() + 6.0
                    || panel.bounds.bottom() + 6.0 == main.origin.y
                    || panel.bounds.origin.y == main.bottom() + 6.0
            );
            let bridge = panel_bridge(main, panel.bounds).unwrap();
            assert_eq!(bridge.size.width.min(bridge.size.height), 6.0);
            assert!(bridge.contains(&bridge.center()));
        }
        let main = Rect::new(point(-1200.0, 100.0), size(360.0, 100.0));
        let short = place_panel(main, screen, 1.0);
        assert_eq!(short.bounds.size, size(100.0, 100.0));
        let scaled = place_panel(main, screen, 1.5);
        assert_eq!(scaled.bounds.origin.x - main.right(), 9.0);

        let narrow_main = Rect::new(point(-1200.0, 0.0), size(280.0, 180.0));
        let panel = place_panel(narrow_main, screen, 1.0);
        assert!(!panel.horizontal);
        assert_eq!(panel.bounds.origin.x, narrow_main.right() + 6.0);

        let fallback = place_panel(screen, screen, 1.0);
        assert!(!fallback.horizontal);
        assert_eq!(fallback.bounds.size.height, screen.size.height);

        let narrow = Rect::new(point(0.0, 0.0), size(260.0, 1040.0));
        let main = Rect::new(point(104.0, 0.0), size(650.0, 1040.0));
        let compact = place_panel(main, narrow, 1.0);
        assert_eq!(compact.bounds.size.width, 98.0);
        assert!(!compact.bounds.intersects(&main));

        for y in [-800.0, 800.0] {
            let main = Rect::new(point(-1200.0, y), size(360.0, 1600.0));
            let panel = place_panel(main, screen, 1.0);
            assert!(!panel.horizontal);
            assert_eq!(panel.bounds.origin.y, main.origin.y);
            assert_eq!(panel.bounds.size.height, main.size.height);
        }
        let main = Rect::new(point(-2100.0, 740.0), size(2280.0, 300.0));
        let panel = place_panel(main, screen, 1.0);
        assert!(panel.horizontal);
        assert_eq!(panel.bounds.origin.x, main.origin.x);
        assert_eq!(panel.bounds.size.width, main.size.width);
        assert_eq!(panel.bounds.bottom(), main.origin.y - 6.0);

        let main = Rect::new(point(-1920.0, 110.0), size(1920.0, 820.0));
        let compact = place_panel(main, screen, 1.0);
        assert!(compact.horizontal);
        assert_eq!(compact.bounds.origin.y, main.bottom() + 6.0);
        assert_eq!(compact.bounds.size.height, 104.0);

        let small = Rect::new(point(0.0, 0.0), size(500.0, 300.0));
        let panel = place_panel(small, small, 2.0);
        assert!(panel.bounds.origin.x >= small.origin.x);
        assert!(panel.bounds.right() <= small.right());
    }
}

#[cfg(test)]
mod error_tests {
    use super::TagsWindowService;

    #[test]
    fn repeated_sync_errors_are_logged_once_until_recovery() {
        let mut service = TagsWindowService::default();
        assert!(service.update_error(Some("工作区查询失败".to_owned())));
        assert!(!service.update_error(Some("工作区查询失败".to_owned())));
        assert!(!service.update_error(None));
        assert!(service.update_error(Some("工作区查询失败".to_owned())));
    }
}
