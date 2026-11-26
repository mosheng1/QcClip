use crate::ui::scale::ui_rem;
use gpui_component::tooltip::ManagedTooltipExt as _;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Div, ElementId, FontWeight, IntoElement, SharedString, Stateful,
    StatefulInteractiveElement, Window, WindowControlArea, div, img, px,
};
use gpui_component::{
    ActiveTheme as _, InteractiveElementExt as _, ThemeColor, h_flex, tooltip::Tooltip,
};

use crate::ui::{hover_icon, hover_icon_path};

pub fn window_title_bar(title: impl Into<SharedString>, window: &mut Window, cx: &mut App) -> Div {
    let theme = cx.theme().colors;
    h_flex()
        .w_full()
        .h(ui_rem(36.0))
        .relative()
        .flex_shrink_0()
        .border_b_1()
        .border_color(theme.title_bar_border)
        .bg(theme.title_bar)
        .child(
            h_flex()
                .id("window-title-brand")
                .h_full()
                .min_w_0()
                .gap_2()
                .pl_3()
                .pr_2()
                .window_control_area(WindowControlArea::Drag)
                .on_double_click(|_, window, _| window.zoom_window())
                .child(
                    div()
                        .flex_shrink_0()
                        .size(ui_rem(24.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(img("assets/icon64.png").size(ui_rem(20.0))),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title.into()),
                ),
        )
        .child(
            div()
                .id("window-title-drag")
                .flex_1()
                .min_w_0()
                .h_full()
                .window_control_area(WindowControlArea::Drag)
                .on_double_click(|_, window, _| window.zoom_window()),
        )
        .child(window_controls(window, cx))
}

fn window_controls(window: &mut Window, cx: &mut App) -> AnyElement {
    let maximize_icon = if window.is_maximized() {
        "assets/icons/window-restore.svg"
    } else {
        "assets/icons/window-maximize.svg"
    };

    h_flex()
        .h_full()
        .flex_shrink_0()
        .child(window_control_button(
            "window-minimize",
            "assets/icons/window-minimize.svg",
            WindowControlArea::Min,
            false,
            |window, _| window.minimize_window(),
            window,
            cx,
        ))
        .child(window_control_button(
            "window-maximize",
            maximize_icon,
            WindowControlArea::Max,
            false,
            |window, _| window.zoom_window(),
            window,
            cx,
        ))
        .child(window_control_button(
            "window-close",
            "assets/icons/window-close.svg",
            WindowControlArea::Close,
            true,
            |window, _| window.remove_window(),
            window,
            cx,
        ))
        .into_any_element()
}

fn window_control_button(
    id: impl Into<ElementId>,
    icon_path: &'static str,
    control_area: WindowControlArea,
    is_danger: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let id = id.into();
    let group = format!("{id:?}");
    let theme = cx.theme().colors;
    let (hover_bg, hover_fg, active_bg) = if is_danger {
        (theme.danger, theme.danger_foreground, theme.danger_active)
    } else {
        (
            theme.secondary_hover,
            theme.foreground,
            theme.secondary_active,
        )
    };
    let hover_state = window.use_keyed_state(id.clone(), cx, |_, _| false);
    let icon_color = if *hover_state.read(cx) {
        hover_fg
    } else {
        theme.foreground
    };
    let hover_state_for_listener = hover_state.clone();

    div()
        .id(id)
        .group(group.clone())
        .flex()
        .w(ui_rem(42.0))
        .h_full()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_color(theme.foreground)
        .window_control_area(control_area)
        .hover(move |style| style.bg(hover_bg).text_color(hover_fg))
        .active(move |style| style.bg(active_bg).text_color(hover_fg))
        .on_click(move |_, window, cx| on_click(window, cx))
        .on_hover(move |hovered, _, cx| {
            let _ = hover_state_for_listener.update(cx, |state, cx| {
                if *state != *hovered {
                    *state = *hovered;
                    cx.notify();
                }
            });
        })
        .child(hover_icon_path(group, icon_path, 16.0, icon_color))
        .into_any_element()
}

pub fn divider(theme: ThemeColor) -> impl IntoElement {
    div()
        .flex_none()
        .w(px(0.5))
        .h(ui_rem(24.0))
        .bg(theme.border)
}

pub fn titlebar_button(
    name: &str,
    tooltip: impl Into<SharedString>,
    active: bool,
    theme: ThemeColor,
) -> Stateful<Div> {
    let tooltip = tooltip.into();
    let color = if active {
        theme.primary_foreground
    } else {
        theme.foreground
    };
    let group = format!("titlebar-button-{name}");
    let mut button = div()
        .id(name.to_owned())
        .group(group.clone())
        .size(ui_rem(28.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_lg()
        .cursor_pointer()
        .hover(move |this| {
            if active {
                this
            } else {
                this.bg(theme.secondary_hover)
            }
        })
        .aria_label(tooltip.clone())
        .managed_tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .child(hover_icon(group, name, 16.0, color));
    if active {
        button = button.bg(theme.primary).shadow_sm();
    }
    button
}
