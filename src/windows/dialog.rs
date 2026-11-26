use crate::ui::scale::ui_rem;

use gpui::prelude::*;
use gpui::{
    AnyElement, AnyWindowHandle, App, AvailableSpace, Bounds, Context, DisplayId, ElementId,
    Entity, FontWeight, Global, IntoElement, MouseButton, Pixels, Point, Render, SharedString,
    Size, Window, WindowControlArea, WindowHandle, WindowId, div, point, px, size,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Root, Sizable as _, ThemeColor};

use crate::platform::{
    input,
    window::{self, BorderlessNormalWindow},
};

pub(crate) const DIALOG_PADDING: f32 = 16.0;

#[derive(Clone, Copy, Debug)]
pub(crate) enum DialogWindowPosition {
    Center,
    /// 物理桌面坐标下的窗口左上角位置。
    #[allow(dead_code)]
    Physical(Point<f32>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DialogMode {
    /// 弹窗打开后，调用方窗口仍然可以交互。
    #[allow(dead_code)]
    Modeless,
    /// 弹窗打开后，调用方窗口不可交互，弹窗关闭后恢复。
    Modal,
}

#[derive(Default)]
pub(crate) struct DialogWindowService {
    modal_windows: Vec<ModalWindowEntry>,
}

impl Global for DialogWindowService {}

#[derive(Clone, Copy)]
struct ModalWindowEntry {
    modal_window_id: WindowId,
    owner: AnyWindowHandle,
}

impl DialogWindowService {
    pub(crate) fn install(cx: &mut App) {
        cx.set_global(Self::default());
        cx.on_window_closed(|cx, _| {
            cx.defer(|cx| {
                cx.update_global::<Self, _>(|service, cx| {
                    service.handle_window_closed(cx);
                });
            });
        })
        .detach();
    }

    pub(crate) fn has_modal_for(owner: gpui::WindowId, cx: &App) -> bool {
        cx.try_global::<Self>().is_some_and(|service| {
            service
                .modal_windows
                .iter()
                .any(|entry| entry.owner.window_id() == owner)
        })
    }

    fn register_modal_window(&mut self, modal_window_id: WindowId, owner: AnyWindowHandle) {
        self.modal_windows.push(ModalWindowEntry {
            modal_window_id,
            owner,
        });
    }

    fn handle_window_closed(&mut self, cx: &mut App) {
        let open_windows = cx.windows();
        let mut closed_windows = Vec::new();
        self.modal_windows.retain(|entry| {
            if window_is_open(&open_windows, entry.modal_window_id) {
                true
            } else {
                closed_windows.push(*entry);
                false
            }
        });

        for entry in closed_windows {
            if modal_owner_is_still_used(&self.modal_windows, entry.owner) {
                continue;
            }
            restore_owner_window(entry.owner, cx);
        }
    }
}

pub(crate) fn dialog_header(title: impl Into<SharedString>, theme: ThemeColor) -> impl IntoElement {
    div()
        .h(px(36.0))
        .w_full()
        .flex_none()
        .flex()
        .items_center()
        .px(px(DIALOG_PADDING))
        .border_b(px(0.5))
        .border_color(theme.title_bar_border)
        .window_control_area(WindowControlArea::Drag)
        .on_mouse_down(MouseButton::Left, |_, window: &mut Window, _| {
            window.start_window_move()
        })
        .child(
            div()
                .text_size(ui_rem(15.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.foreground)
                .child(title.into()),
        )
}

pub(crate) fn dialog_body(content: impl IntoElement) -> impl IntoElement {
    div().w_full().p(px(DIALOG_PADDING)).child(content)
}

pub(crate) fn dialog_footer(actions: Vec<AnyElement>, theme: ThemeColor) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(12.0))
        .child(
            div()
                .h(px(1.0))
                .w_full()
                .bg(gpui::ColorExt::opacity(&theme.border, 0.72)),
        )
        .child(div().flex().justify_end().gap(px(6.0)).children(actions))
}

pub(crate) fn dialog_button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    Button::new(id).label(label).small()
}

pub(crate) fn dialog_primary_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
) -> Button {
    dialog_button(id, label).primary()
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PhysicalBounds {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

pub(crate) fn open_adaptive_window<V>(
    source_window: Option<&Window>,
    cx: &mut App,
    position: DialogWindowPosition,
    mode: DialogMode,
    build_content: impl FnOnce(&mut Window, &mut App) -> Entity<V> + 'static,
) -> Result<WindowHandle<Root>, String>
where
    V: 'static + Render,
{
    let owner = match mode {
        DialogMode::Modeless => None,
        DialogMode::Modal => source_window
            .map(Window::window_handle)
            .or_else(|| cx.active_window()),
    };
    if mode == DialogMode::Modal && owner.is_none() {
        return Err("无法打开模态窗口：未找到调用方窗口".to_owned());
    }

    let display_id =
        source_window.and_then(|window| window.display(cx).map(|display| display.id()));
    if let Some(owner) = owner {
        set_owner_enabled(owner, source_window, false, cx)?;
    }

    let result = open_adaptive_window_with_display(display_id, cx, position, build_content);
    match result {
        Ok(handle) => {
            if let Some(owner) = owner {
                let modal_window_id = handle.window_id();
                cx.update_global::<DialogWindowService, _>(|service, _| {
                    service.register_modal_window(modal_window_id, owner);
                });
            }
            Ok(handle)
        }
        Err(error) => {
            if let Some(owner) = owner {
                restore_owner_window_after_open_failure(owner, source_window, cx);
            }
            Err(error)
        }
    }
}

fn set_owner_enabled(
    owner: AnyWindowHandle,
    source_window: Option<&Window>,
    enabled: bool,
    cx: &mut App,
) -> Result<(), String> {
    if let Some(source_window) = source_window {
        return window::set_window_enabled(source_window, enabled);
    }

    let result = owner
        .update(cx, |_, window, _| {
            window::set_window_enabled(window, enabled)
        })
        .map_err(|error| format!("无法更新调用方窗口状态：{error}"))?;
    result
}

fn restore_owner_window_after_open_failure(
    owner: AnyWindowHandle,
    source_window: Option<&Window>,
    cx: &mut App,
) {
    if let Err(error) = set_owner_enabled(owner, source_window, true, cx) {
        tracing::error!("恢复调用方窗口失败：{error}");
    }
}

fn restore_owner_window(owner: AnyWindowHandle, cx: &mut App) {
    if !window_is_open(&cx.windows(), owner.window_id()) {
        return;
    }

    match owner.update(cx, |_, window, _| {
        if let Err(error) = window::set_window_enabled(window, true) {
            tracing::error!("恢复调用方窗口失败：{error}");
        } else if let Err(error) = window::activate_window_preserving_bounds(window) {
            tracing::error!("激活调用方窗口失败：{error}");
        }
    }) {
        Ok(()) => {}
        Err(error) => tracing::error!("恢复调用方窗口失败：{error}"),
    }
}

fn window_is_open(open_windows: &[AnyWindowHandle], window_id: WindowId) -> bool {
    open_windows
        .iter()
        .any(|handle| handle.window_id() == window_id)
}

fn modal_owner_is_still_used(entries: &[ModalWindowEntry], owner: AnyWindowHandle) -> bool {
    entries
        .iter()
        .any(|entry| entry.owner.window_id() == owner.window_id())
}

fn open_adaptive_window_with_display<V>(
    preferred_display_id: Option<DisplayId>,
    cx: &mut App,
    position: DialogWindowPosition,
    build_content: impl FnOnce(&mut Window, &mut App) -> Entity<V> + 'static,
) -> Result<WindowHandle<Root>, String>
where
    V: 'static + Render,
{
    let (display_id, initial_bounds) = initial_window_bounds(position, preferred_display_id, cx);
    window::open_borderless_normal_window(
        cx,
        initial_bounds,
        display_id,
        move |window, cx, normal_window| {
            let content = build_content(window, cx);
            let adaptive_window = cx.new(|_| AdaptiveWindow::new(content, position, normal_window));
            cx.new(|cx| crate::window_material::new_root(adaptive_window, window, cx))
        },
    )
}

pub(crate) struct AdaptiveWindow<V> {
    content: Entity<V>,
    position: DialogWindowPosition,
    normal_window: BorderlessNormalWindow,
    current_bounds: std::cell::Cell<Option<PhysicalBounds>>,
    pending_bounds: std::cell::Cell<Option<PhysicalBounds>>,
}

impl<V: 'static + Render> AdaptiveWindow<V> {
    fn new(
        content: Entity<V>,
        position: DialogWindowPosition,
        normal_window: BorderlessNormalWindow,
    ) -> Self {
        Self {
            content,
            position,
            normal_window,
            current_bounds: std::cell::Cell::new(None),
            pending_bounds: std::cell::Cell::new(None),
        }
    }

    fn measure_content_size(&self, window: &mut Window, cx: &mut Context<Self>) -> Size<Pixels> {
        self.content.clone().into_any_element().layout_as_root(
            size(AvailableSpace::MinContent, AvailableSpace::MinContent),
            window,
            cx,
        )
    }

    fn place_window(
        &self,
        measured_size: Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let scale_factor = window.scale_factor().max(1.0);
        let bounds = physical_bounds(
            self.position,
            measured_size,
            scale_factor,
            screen_bounds(window, cx, scale_factor),
        );
        if self.current_bounds.get() == Some(bounds) || self.pending_bounds.get() == Some(bounds) {
            return;
        }

        self.pending_bounds.set(Some(bounds));
        let current_bounds = self.current_bounds.clone();
        let pending_bounds = self.pending_bounds.clone();
        let normal_window = self.normal_window;
        window.defer(cx, move |window, cx| {
            let updated = normal_window.resize_to_physical_bounds(
                window,
                cx,
                bounds.x,
                bounds.y,
                bounds.width,
                bounds.height,
            );
            pending_bounds.set(None);
            if updated {
                current_bounds.set(Some(bounds));
                window.refresh();
            }
        });
    }
}

impl<V> Render for AdaptiveWindow<V>
where
    V: 'static + Render,
{
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(crate::ui::scale::standalone_rem_size(16.0, cx));
        let measured_size = self.measure_content_size(window, cx);
        self.place_window(measured_size, window, cx);
        div().size_full().child(self.content.clone())
    }
}

fn initial_window_bounds(
    position: DialogWindowPosition,
    preferred_display_id: Option<DisplayId>,
    cx: &mut App,
) -> (Option<DisplayId>, Bounds<Pixels>) {
    match position {
        DialogWindowPosition::Center => {
            let display_id = preferred_display_id
                .or_else(|| mouse_display_id(cx))
                .or_else(|| active_display_id(cx));
            (
                display_id,
                Bounds::centered(display_id, size(px(1.0), px(1.0)), cx),
            )
        }
        DialogWindowPosition::Physical(physical_point) => {
            let display = window::display_for_physical_point(cx, physical_point);
            let scale_factor = display
                .map(|display| display.scale_factor)
                .unwrap_or(1.0)
                .max(1.0);
            let bounds = Bounds::new(
                point(
                    px(physical_point.x / scale_factor),
                    px(physical_point.y / scale_factor),
                ),
                size(px(1.0), px(1.0)),
            );
            (display.map(|display| display.display_id), bounds)
        }
    }
}

fn active_display_id(cx: &mut App) -> Option<DisplayId> {
    let active_window = cx.active_window()?;
    active_window
        .update(cx, |_, window, cx| {
            window.display(cx).map(|display| display.id())
        })
        .ok()
        .flatten()
}

fn mouse_display_id(cx: &mut App) -> Option<DisplayId> {
    let pointer = input::pointer_snapshot()?;
    window::display_for_physical_point(cx, pointer.position).map(|display| display.display_id)
}

fn screen_bounds<V>(
    window: &Window,
    cx: &Context<AdaptiveWindow<V>>,
    scale_factor: f32,
) -> PhysicalBounds
where
    V: 'static + Render,
{
    window
        .display(cx)
        .or_else(|| cx.primary_display())
        .map(|display| {
            let bounds = display.bounds();
            PhysicalBounds {
                x: f32::from(bounds.origin.x) * scale_factor,
                y: f32::from(bounds.origin.y) * scale_factor,
                width: f32::from(bounds.size.width) * scale_factor,
                height: f32::from(bounds.size.height) * scale_factor,
            }
        })
        .unwrap_or(PhysicalBounds {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
        })
}

fn physical_bounds(
    position: DialogWindowPosition,
    measured_size: Size<Pixels>,
    scale_factor: f32,
    screen: PhysicalBounds,
) -> PhysicalBounds {
    let width = (f32::from(measured_size.width) * scale_factor)
        .ceil()
        .max(1.0);
    let height = (f32::from(measured_size.height) * scale_factor)
        .ceil()
        .max(1.0);
    let (x, y) = match position {
        DialogWindowPosition::Center => (
            screen.x + ((screen.width - width) / 2.0).max(0.0),
            screen.y + ((screen.height - height) / 2.0).max(0.0),
        ),
        DialogWindowPosition::Physical(point) => (point.x, point.y),
    };
    PhysicalBounds {
        x,
        y,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DialogWindowService, ModalWindowEntry, PhysicalBounds, modal_owner_is_still_used,
        physical_bounds,
    };
    use gpui::{AnyWindowHandle, WindowHandle, WindowId, point, px, size};
    use gpui_component::Root;

    fn dialog_handle(id: u64) -> AnyWindowHandle {
        WindowHandle::<Root>::new(WindowId::from(id)).into()
    }

    #[test]
    fn default_position_centers_window_on_current_screen() {
        let bounds = physical_bounds(
            super::DialogWindowPosition::Center,
            size(px(300.0), px(200.0)),
            1.0,
            PhysicalBounds {
                x: 100.0,
                y: 50.0,
                width: 1200.0,
                height: 800.0,
            },
        );

        assert_eq!(bounds.x, 550.0);
        assert_eq!(bounds.y, 350.0);
    }

    #[test]
    fn physical_position_preserves_window_origin() {
        let bounds = physical_bounds(
            super::DialogWindowPosition::Physical(point(30.0, 40.0)),
            size(px(300.0), px(200.0)),
            1.0,
            PhysicalBounds {
                x: 0.0,
                y: 0.0,
                width: 1200.0,
                height: 800.0,
            },
        );

        assert_eq!((bounds.x, bounds.y), (30.0, 40.0));
    }

    #[test]
    fn logical_size_is_converted_once_on_high_dpi_screen() {
        let bounds = physical_bounds(
            super::DialogWindowPosition::Center,
            size(px(320.0), px(200.0)),
            1.5,
            PhysicalBounds {
                x: 0.0,
                y: 0.0,
                width: 1920.0,
                height: 1080.0,
            },
        );

        assert_eq!((bounds.width, bounds.height), (480.0, 300.0));
    }

    #[test]
    fn modal_owner_is_not_restored_while_other_modal_windows_exist() {
        let owner = dialog_handle(1);
        let service = DialogWindowService {
            modal_windows: vec![ModalWindowEntry {
                modal_window_id: WindowId::from(2),
                owner,
            }],
        };

        assert!(modal_owner_is_still_used(&service.modal_windows, owner));
    }
}
