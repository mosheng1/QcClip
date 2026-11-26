use crate::ui::scale::ui_rem;
use gpui_component::tooltip::ManagedTooltipExt as _;

use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    Animation, AnimationExt, AnyElement, Context, ElementId, IntoElement, SpringAnimation,
    SpringConfig, Window, div, px,
};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme as _, scroll::ScrollableElement};

use crate::{config_runtime, ui::hover_icon, windows::main::state::MainWindow};

pub(crate) const TAB_BAR_HEIGHT: f32 = 36.0;
pub(crate) const TAB_BUTTON_HEIGHT: f32 = 28.0;

#[derive(Clone)]
pub(crate) struct SymbolSidebarItem {
    pub(crate) id: u32,
    pub(crate) icon: &'static str,
    pub(crate) label: String,
}

impl SymbolSidebarItem {
    pub(crate) fn new(id: u32, icon: &'static str, label: String) -> Self {
        Self { id, icon, label }
    }
}

pub(crate) type SymbolSidebarSelectHandler =
    Rc<dyn Fn(&mut MainWindow, u32, &mut Window, &mut Context<MainWindow>)>;

pub(crate) fn horizontal_labels_fit<'a>(
    labels: impl Iterator<Item = &'a str>,
    available_width: f32,
    icon_width: f32,
) -> bool {
    let labels = labels.collect::<Vec<_>>();
    if labels.is_empty() {
        return false;
    }

    let required_width = labels
        .iter()
        .map(|label| {
            icon_width
                + 4.0
                + label
                    .chars()
                    .map(|character| if character.is_ascii() { 6.5 } else { 12.0 })
                    .sum::<f32>()
                + 16.0
        })
        .sum::<f32>()
        + (labels.len() - 1) as f32 * 4.0
        + 8.0;
    required_width <= available_width
}

fn spring<T: gpui::SpringTarget>(target: T) -> SpringAnimation<T> {
    SpringAnimation::new(SpringConfig::new(420.0, 36.0, 1.0)).to(target)
}

pub(crate) fn active_indicator(
    id: impl Into<ElementId>,
    index: usize,
    color: gpui::Hsla,
    animations_enabled: bool,
) -> impl IntoElement {
    active_indicator_at(id, index, 4.0, color, animations_enabled)
}

pub(crate) fn active_indicator_at(
    id: impl Into<ElementId>,
    index: usize,
    top_offset: f32,
    color: gpui::Hsla,
    animations_enabled: bool,
) -> impl IntoElement {
    let top = top_offset + index as f32 * (TAB_BUTTON_HEIGHT + 4.0);
    let id = id.into();

    let indicator = div()
        .absolute()
        .left(ui_rem(4.0))
        .top(ui_rem(top))
        .w(ui_rem(TAB_BUTTON_HEIGHT))
        .h(ui_rem(TAB_BUTTON_HEIGHT))
        .flex()
        .items_center()
        .justify_center()
        .child({
            let bar = div().size_full().rounded_lg().bg(color).shadow_sm();
            if animations_enabled {
                bar.with_animation(
                    (id.clone(), format!("stretch-{index}")),
                    Animation::new(Duration::from_millis(150)).with_easing(gpui::ease_in_out),
                    |this, progress| {
                        let expansion = (std::f32::consts::PI * progress).sin() * 0.9;
                        let inset = (2.0 * (1.0 - progress) - expansion).max(0.0);
                        this.mx(ui_rem(inset)).my(ui_rem(inset))
                    },
                )
                .into_any_element()
            } else {
                bar.into_any_element()
            }
        });
    if animations_enabled {
        indicator
            .with_spring((id, "top"), spring(top), |this, value| {
                this.top(ui_rem(value))
            })
            .into_any_element()
    } else {
        indicator.into_any_element()
    }
}

pub(crate) fn render(
    items: &[SymbolSidebarItem],
    active_id: u32,
    on_select: SymbolSidebarSelectHandler,
    horizontal: bool,
    show_labels: bool,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    let theme = cx.theme().colors;
    let animations_enabled =
        config_runtime::animation_enabled(cx, crate::config::AnimationKind::SingleSelectIndicator);
    let active_index = items.iter().position(|item| item.id == active_id);
    let buttons = items
        .iter()
        .map(|item| {
            let active = item.id == active_id;
            let color = if active {
                theme.primary_foreground
            } else {
                theme.foreground
            };
            let item_id = item.id;
            let label = item.label.clone();
            let on_select = on_select.clone();
            let group = format!("symbol-sidebar-{}", item.id);
            let mut button = div()
                .id(("symbol-sidebar", item_id))
                .group(group.clone())
                .flex()
                .items_center()
                .rounded_lg()
                .cursor_pointer()
                .managed_tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
                .on_click(cx.listener(move |this, _, window, cx| {
                    on_select(this, item_id, window, cx);
                }));
            button = if horizontal && show_labels {
                button
                    .h(ui_rem(TAB_BUTTON_HEIGHT))
                    .min_w_auto()
                    .flex_1()
                    .justify_center()
                    .gap_1()
                    .px_2()
                    .child(hover_icon(group.clone(), item.icon, 16.0, color))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(ui_rem(12.0))
                            .text_color(color)
                            .child(item.label.clone()),
                    )
            } else if horizontal {
                button
                    .size(ui_rem(TAB_BUTTON_HEIGHT))
                    .min_w(ui_rem(TAB_BUTTON_HEIGHT))
                    .flex_1()
                    .justify_center()
                    .child(hover_icon(group.clone(), item.icon, 16.0, color))
            } else {
                button
                    .size(ui_rem(TAB_BUTTON_HEIGHT))
                    .flex_none()
                    .justify_center()
                    .child(hover_icon(group.clone(), item.icon, 16.0, color))
            };
            if horizontal && active {
                button = button.bg(theme.primary).shadow_sm();
            } else if !active {
                button = button.hover(|this| this.bg(theme.secondary_hover));
            }
            button.into_any_element()
        })
        .collect::<Vec<_>>();
    let content = if horizontal {
        div()
            .w_full()
            .min_w_0()
            .h_full()
            .flex()
            .flex_row()
            .items_center()
            .relative()
            .overflow_x_scrollbar()
            .gap_1()
            .px_1()
            .bg(theme.tab_bar)
            .children(buttons)
            .into_any_element()
    } else {
        div()
            .w(ui_rem(TAB_BAR_HEIGHT))
            .min_w(ui_rem(TAB_BAR_HEIGHT))
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .relative()
            .overflow_y_scrollbar()
            .gap_1()
            .py_1()
            .bg(theme.tab_bar)
            .when_some(active_index, |this, index| {
                this.child(active_indicator(
                    "symbol-sidebar-active-indicator",
                    index,
                    theme.primary,
                    animations_enabled,
                ))
            })
            .children(buttons)
            .into_any_element()
    };

    let mut root = div().h_full().flex_none().child(content);
    if horizontal {
        root = root
            .w_full()
            .min_w_0()
            .h(px(TAB_BAR_HEIGHT))
            .border_l(px(0.5))
            .border_color(theme.border);
    } else {
        root = root
            .w(ui_rem(TAB_BAR_HEIGHT))
            .min_w(ui_rem(TAB_BAR_HEIGHT))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .w(px(0.5))
                    .bg(theme.border),
            );
    }
    root.into_any_element()
}
