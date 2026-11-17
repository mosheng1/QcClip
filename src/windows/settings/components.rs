use crate::ui::scale::ui_rem;

use std::{cell::Cell, rc::Rc};

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Context, ElementId, FontWeight, IntoElement, Pixels, SharedString,
    div, px,
};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use super::{SETTINGS_DROPDOWN_HEIGHT, SETTINGS_DROPDOWN_WIDTH, SettingsWindowView};
use crate::{
    platform::window as platform_window,
    ui::hover_icon,
    windows::menu::{DropdownAnchor, DropdownItem, DropdownItemId, DropdownModel, MenuService},
};

pub(in crate::windows::settings) fn settings_dropdown<F>(
    id: &'static str,
    current: impl Into<SharedString>,
    items: Vec<DropdownItem>,
    on_select: F,
    cx: &mut Context<SettingsWindowView>,
) -> AnyElement
where
    F: Fn(DropdownItemId, &mut App) + 'static,
{
    settings_dropdown_with_font(id, current, items, None, on_select, cx)
}

pub(in crate::windows::settings) fn settings_dropdown_with_font<F>(
    id: &'static str,
    current: impl Into<SharedString>,
    items: Vec<DropdownItem>,
    font_family: Option<SharedString>,
    on_select: F,
    cx: &mut Context<SettingsWindowView>,
) -> AnyElement
where
    F: Fn(DropdownItemId, &mut App) + 'static,
{
    let current = current.into();
    let current_label = items
        .iter()
        .find(|item| item.id.as_str() == current.as_ref())
        .map(|item| item.label.clone())
        .unwrap_or_else(|| current.clone());
    let mut menu = DropdownModel::new(items);
    if let Some(family) = font_family.clone() {
        menu = menu.font_family(family);
    }
    let on_select: Rc<dyn Fn(DropdownItemId, &mut App)> = Rc::new(on_select);
    let trigger_bounds = Rc::new(Cell::new(None::<Bounds<Pixels>>));
    let trigger_bounds_for_paint = trigger_bounds.clone();
    let trigger_bounds_for_click = trigger_bounds.clone();
    let on_select_for_click = on_select.clone();

    div()
        .w(ui_rem(SETTINGS_DROPDOWN_WIDTH))
        .when_some(font_family, |this, family| this.font_family(family))
        .on_children_prepainted(move |bounds, _, _| {
            if let Some(bounds) = bounds.first().copied() {
                trigger_bounds_for_paint.set(Some(bounds));
            }
        })
        .child(
            div()
                .id(id)
                .group(format!("settings-dropdown-{id}"))
                .flex()
                .items_center()
                .justify_between()
                .h(ui_rem(SETTINGS_DROPDOWN_HEIGHT))
                .w_full()
                .gap_2()
                .rounded(px(6.0))
                .border_1()
                .border_color(cx.theme().input)
                .bg(cx.theme().background)
                .px_3()
                .text_sm()
                .cursor_pointer()
                .hover(|style| style.border_color(cx.theme().primary).bg(cx.theme().accent))
                .active(|style| style.border_color(cx.theme().primary_active))
                .on_click(move |_, window, cx| {
                    let Some(bounds) = trigger_bounds_for_click.get() else {
                        return;
                    };
                    // 设置窗口可能刚跨越 DPI 不同的显示器，先同步 GPUI 的窗口状态，
                    // 再把触发器逻辑尺寸转换为物理桌面坐标。
                    window.bounds_changed(cx);
                    let scale_factor = window.scale_factor().max(1.0);
                    let Some(origin) =
                        platform_window::logical_to_physical_desktop(window, bounds.origin)
                    else {
                        return;
                    };
                    let anchor = DropdownAnchor::new(
                        origin.x,
                        origin.y,
                        f32::from(bounds.size.width) * scale_factor,
                        f32::from(bounds.size.height) * scale_factor,
                        scale_factor,
                    );
                    let on_select = on_select_for_click.clone();
                    MenuService::open_dropdown_menu(
                        menu.clone(),
                        anchor,
                        move |id, cx| on_select(id, cx),
                        cx,
                    );
                })
                .child(div().flex_1().min_w_0().truncate().child(current_label))
                .child(hover_icon(
                    format!("settings-dropdown-{id}"),
                    "chevron-down",
                    12.0,
                    cx.theme().muted_foreground,
                )),
        )
        .into_any_element()
}

pub(in crate::windows::settings) fn settings_fused_dropdown<FL, FR>(
    id: &'static str,
    left_current: impl Into<SharedString>,
    left_items: Vec<DropdownItem>,
    right_current: impl Into<SharedString>,
    right_items: Vec<DropdownItem>,
    on_left_select: FL,
    on_right_select: FR,
    disabled: bool,
    cx: &mut Context<SettingsWindowView>,
) -> AnyElement
where
    FL: Fn(DropdownItemId, &mut App) + 'static,
    FR: Fn(DropdownItemId, &mut App) + 'static,
{
    let theme = cx.theme().colors;
    let left_current = left_current.into();
    let right_current = right_current.into();
    let right_fixed = right_items.is_empty();
    let left_label = left_items
        .iter()
        .find(|item| item.id.as_str() == left_current.as_ref())
        .map(|item| item.label.clone())
        .unwrap_or_else(|| left_current.clone());
    let right_label = right_items
        .iter()
        .find(|item| item.id.as_str() == right_current.as_ref())
        .map(|item| item.label.clone())
        .unwrap_or_else(|| right_current.clone());
    let left_menu = DropdownModel::new(left_items);
    let right_menu = DropdownModel::new(right_items);
    let left_handler: Rc<dyn Fn(DropdownItemId, &mut App)> = Rc::new(on_left_select);
    let right_handler: Rc<dyn Fn(DropdownItemId, &mut App)> = Rc::new(on_right_select);
    let left_bounds = Rc::new(Cell::new(None::<Bounds<Pixels>>));
    let right_bounds = Rc::new(Cell::new(None::<Bounds<Pixels>>));
    let left_bounds_for_click = left_bounds.clone();
    let right_bounds_for_click = right_bounds.clone();
    let left_handler_for_click = left_handler.clone();
    let right_handler_for_click = right_handler.clone();
    let left_menu_for_click = left_menu.clone();
    let right_menu_for_click = right_menu.clone();
    let open_menu = Rc::new(
        move |bounds: Option<Bounds<Pixels>>,
              menu: DropdownModel,
              handler: Rc<dyn Fn(DropdownItemId, &mut App)>,
              window: &mut gpui::Window,
              cx: &mut App| {
            let Some(bounds) = bounds else {
                return;
            };
            window.bounds_changed(cx);
            let scale_factor = window.scale_factor().max(1.0);
            let Some(origin) = platform_window::logical_to_physical_desktop(window, bounds.origin)
            else {
                return;
            };
            let anchor = DropdownAnchor::new(
                origin.x,
                origin.y,
                f32::from(bounds.size.width) * scale_factor,
                f32::from(bounds.size.height) * scale_factor,
                scale_factor,
            );
            MenuService::open_dropdown_menu(menu, anchor, move |id, cx| handler(id, cx), cx);
        },
    );
    let open_menu_for_left = open_menu.clone();
    let open_menu_for_right = open_menu.clone();

    div()
        .id(id)
        .flex()
        .items_center()
        .flex_none()
        .h(ui_rem(SETTINGS_DROPDOWN_HEIGHT))
        .rounded(px(6.0))
        .border_1()
        .border_color(theme.input)
        .bg(theme.background)
        .overflow_hidden()
        .when(disabled, |this| this.opacity(0.5))
        .when(!disabled, |this| {
            this.hover(|style| style.border_color(theme.primary).bg(theme.accent))
                .active(|style| style.border_color(theme.primary_active))
        })
        .child(
            div()
                .flex_none()
                .h_full()
                .on_children_prepainted(move |bounds, _, _| {
                    if let Some(bounds) = bounds.first().copied() {
                        left_bounds.set(Some(bounds));
                    }
                })
                .child(
                    div()
                        .id(format!("{id}-left"))
                        .group(format!("{id}-left"))
                        .flex()
                        .items_center()
                        .justify_between()
                        .h_full()
                        .gap_1()
                        .px_3()
                        .text_sm()
                        .when(!disabled, |this| {
                            this.cursor_pointer().on_click(move |_, window, cx| {
                                open_menu_for_left(
                                    left_bounds_for_click.get(),
                                    left_menu_for_click.clone(),
                                    left_handler_for_click.clone(),
                                    window,
                                    cx,
                                );
                            })
                        })
                        .child(div().whitespace_nowrap().child(left_label))
                        .child(hover_icon(
                            format!("{id}-left"),
                            "chevron-down",
                            12.0,
                            theme.muted_foreground,
                        )),
                ),
        )
        .child(
            div()
                .h(ui_rem(20.0))
                .w(px(1.0))
                .flex_shrink_0()
                .bg(theme.input),
        )
        .child(
            div()
                .flex_none()
                .h_full()
                .on_children_prepainted(move |bounds, _, _| {
                    if let Some(bounds) = bounds.first().copied() {
                        right_bounds.set(Some(bounds));
                    }
                })
                .child(
                    div()
                        .id(format!("{id}-right"))
                        .group(format!("{id}-right"))
                        .flex()
                        .items_center()
                        .justify_between()
                        .h_full()
                        .gap_1()
                        .px_3()
                        .text_sm()
                        .when(!right_fixed && !disabled, |this| {
                            this.cursor_pointer().on_click(move |_, window, cx| {
                                open_menu_for_right(
                                    right_bounds_for_click.get(),
                                    right_menu_for_click.clone(),
                                    right_handler_for_click.clone(),
                                    window,
                                    cx,
                                );
                            })
                        })
                        .when(right_fixed, |this| this.cursor_default())
                        .child(div().whitespace_nowrap().child(right_label))
                        .when(!right_fixed, |this| {
                            this.child(hover_icon(
                                format!("{id}-right"),
                                "chevron-down",
                                12.0,
                                theme.muted_foreground,
                            ))
                        }),
                ),
        )
        .into_any_element()
}

pub(in crate::windows::settings) fn settings_preset_dropdown<F>(
    id: &'static str,
    items: Vec<DropdownItem>,
    on_select: F,
    cx: &mut Context<SettingsWindowView>,
) -> AnyElement
where
    F: Fn(DropdownItemId, &mut App) + 'static,
{
    let menu = DropdownModel::new(items);
    let on_select: Rc<dyn Fn(DropdownItemId, &mut App)> = Rc::new(on_select);
    let trigger_bounds = Rc::new(Cell::new(None::<Bounds<Pixels>>));
    let trigger_bounds_for_paint = trigger_bounds.clone();
    let trigger_bounds_for_click = trigger_bounds.clone();
    let on_select_for_click = on_select.clone();
    let theme = cx.theme().colors;

    div()
        .w(ui_rem(32.0))
        .h_full()
        .on_children_prepainted(move |bounds, _, _| {
            if let Some(bounds) = bounds.first().copied() {
                trigger_bounds_for_paint.set(Some(bounds));
            }
        })
        .child(
            div()
                .id(id)
                .group(format!("settings-preset-dropdown-{id}"))
                .flex()
                .items_center()
                .justify_center()
                .size_full()
                .border_l_1()
                .border_color(theme.input)
                .cursor_pointer()
                .hover(|style| style.bg(theme.accent))
                .active(|style| style.bg(theme.input))
                .on_click(move |_, window, cx| {
                    let Some(bounds) = trigger_bounds_for_click.get() else {
                        return;
                    };
                    window.bounds_changed(cx);
                    let scale_factor = window.scale_factor().max(1.0);
                    let Some(origin) =
                        platform_window::logical_to_physical_desktop(window, bounds.origin)
                    else {
                        return;
                    };
                    let anchor = DropdownAnchor::new(
                        origin.x,
                        origin.y,
                        f32::from(bounds.size.width) * scale_factor,
                        f32::from(bounds.size.height) * scale_factor,
                        scale_factor,
                    );
                    let on_select = on_select_for_click.clone();
                    MenuService::open_dropdown_menu(
                        menu.clone(),
                        anchor,
                        move |id, cx| on_select(id, cx),
                        cx,
                    );
                })
                .child(hover_icon(
                    format!("settings-preset-dropdown-{id}"),
                    "chevron-down",
                    12.0,
                    theme.muted_foreground,
                )),
        )
        .into_any_element()
}

pub(in crate::windows::settings) fn settings_error_row(
    theme: gpui_component::ThemeColor,
    error: &str,
) -> AnyElement {
    div()
        .border_t_1()
        .border_color(theme.border)
        .px_4()
        .py_2()
        .text_sm()
        .text_color(theme.danger)
        .child(error.to_owned())
        .into_any_element()
}

pub(in crate::windows::settings) fn settings_page_header(
    theme: gpui_component::ThemeColor,
    title: String,
    description: String,
    action: impl IntoElement,
) -> AnyElement {
    h_flex()
        .w_full()
        .min_h(ui_rem(60.0))
        .flex_shrink_0()
        .items_center()
        .gap_3()
        .border_b_1()
        .border_color(theme.border)
        .px_4()
        .child(
            div()
                .flex_shrink_0()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.foreground)
                .child(title),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(description),
        )
        .child(div().flex_shrink_0().child(action))
        .into_any_element()
}

/// 页面内的分组标题，用于把同一页的设置按用途分段。
///
/// 标题不画分割线：上方由前一组最后一行的底边分隔，下方直接贴住本组第一行，
/// 让标题在视觉上归属于它下面的设置。
pub(in crate::windows::settings) fn settings_section_title(
    theme: gpui_component::ThemeColor,
    title: String,
) -> AnyElement {
    div()
        .w_full()
        .flex_shrink_0()
        .px_4()
        .pt(ui_rem(18.0))
        .pb_1()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.muted_foreground)
        .child(title)
        .into_any_element()
}

pub(in crate::windows::settings) fn setting_row(
    theme: gpui_component::ThemeColor,
    title: String,
    description: Option<SharedString>,
    control: impl IntoElement,
) -> AnyElement {
    setting_row_with_min_height(theme, title, description, control, ui_rem(62.0))
}

pub(in crate::windows::settings) fn clickable_setting_row<F>(
    id: impl Into<ElementId>,
    theme: gpui_component::ThemeColor,
    title: String,
    description: Option<SharedString>,
    on_click: F,
) -> AnyElement
where
    F: Fn(&gpui::ClickEvent, &mut gpui::Window, &mut App) + 'static,
{
    clickable_setting_row_with_min_height(id, theme, title, description, on_click, ui_rem(62.0))
}

pub(in crate::windows::settings) fn compact_clickable_setting_row<F>(
    id: impl Into<ElementId>,
    theme: gpui_component::ThemeColor,
    title: String,
    on_click: F,
) -> AnyElement
where
    F: Fn(&gpui::ClickEvent, &mut gpui::Window, &mut App) + 'static,
{
    clickable_setting_row_with_min_height(id, theme, title, None, on_click, ui_rem(52.0))
}

fn clickable_setting_row_with_min_height<F>(
    id: impl Into<ElementId>,
    theme: gpui_component::ThemeColor,
    title: String,
    description: Option<SharedString>,
    on_click: F,
    min_height: gpui::Rems,
) -> AnyElement
where
    F: Fn(&gpui::ClickEvent, &mut gpui::Window, &mut App) + 'static,
{
    h_flex()
        .id(id)
        .w_full()
        .min_w_0()
        .min_h(min_height)
        .flex_shrink_0()
        .items_center()
        .justify_between()
        .gap_4()
        .border_b_1()
        .border_color(theme.border)
        .px_4()
        .py_2()
        .cursor_pointer()
        .hover(|style| style.bg(theme.accent))
        .on_click(on_click)
        .child(
            v_flex()
                .min_w_0()
                .flex_1()
                .gap_1()
                .child(
                    div()
                        .min_w_0()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child(title),
                )
                .when_some(description, |this, description| {
                    this.child(
                        div()
                            .min_w_0()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(description),
                    )
                }),
        )
        .child(hover_icon(
            "settings-animation-row",
            "chevron-right",
            16.0,
            theme.muted_foreground,
        ))
        .into_any_element()
}

pub(in crate::windows::settings) fn compact_setting_row(
    theme: gpui_component::ThemeColor,
    title: String,
    control: impl IntoElement,
) -> AnyElement {
    setting_row_with_min_height(theme, title, None, control, ui_rem(52.0))
}

fn setting_row_with_min_height(
    theme: gpui_component::ThemeColor,
    title: String,
    description: Option<SharedString>,
    control: impl IntoElement,
    min_height: gpui::Rems,
) -> AnyElement {
    let control_id = format!("settings-control-{title}");
    h_flex()
        .w_full()
        .min_w_0()
        .min_h(min_height)
        .flex_wrap()
        .flex_shrink_0()
        .items_center()
        .justify_between()
        .gap_4()
        .border_b_1()
        .border_color(theme.border)
        .px_4()
        .py_2()
        .child(
            v_flex()
                .min_w(ui_rem(180.0))
                .max_w_full()
                .flex_1()
                .gap_1()
                .child(
                    div()
                        .min_w_0()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child(title),
                )
                .when_some(description, |this, description| {
                    this.child(
                        div()
                            .min_w_0()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(description),
                    )
                }),
        )
        .child(
            div()
                .id(control_id)
                .flex_shrink_0()
                .max_w_full()
                .overflow_x_scroll()
                .child(control),
        )
        .into_any_element()
}

pub(super) fn settings_placeholder_page(
    page: super::SettingsPage,
    cx: &mut Context<SettingsWindowView>,
) -> AnyElement {
    let theme = cx.theme().colors;
    v_flex()
        .size_full()
        .child(
            div()
                .text_size(ui_rem(20.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.foreground)
                .child(crate::i18n::translator(cx).translate(page.title_key())),
        )
        .child(div().flex_1().min_h_0())
        .into_any_element()
}
