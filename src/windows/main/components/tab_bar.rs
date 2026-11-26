use crate::ui::scale::ui_rem;

use gpui::prelude::*;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt, Context, ElementId, Hsla, IntoElement, SpringAnimation, SpringConfig,
    StatefulInteractiveElement, div, px, relative,
};
use gpui_component::ActiveTheme as _;
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::tooltip::Tooltip;

use crate::{
    config::{ClipboardWindowPosition, TabBarButton, TabBarSection},
    config_runtime, i18n,
    ui::hover_icon,
    windows::main::{
        state::MainWindow,
        tabs::{
            FilterDefinition, SymbolCategoryDefinition, TabDefinition, TabId, ordered_filters,
            ordered_symbol_categories, ordered_tabs,
        },
    },
};

use super::symbol_sidebar::{TAB_BAR_HEIGHT, TAB_BUTTON_HEIGHT};

const SIDEBAR_COLLAPSED_WIDTH: f32 = 56.0;
const SIDEBAR_BUTTON_HEIGHT: f32 = TAB_BAR_HEIGHT;
const TAB_SLOT_INSET: f32 = 2.0;

fn hover_sensor(button: TabBarButton, cx: &Context<MainWindow>) -> impl IntoElement {
    let view = cx.entity().downgrade();
    gpui::canvas(
        |bounds, window, _| window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal),
        move |_, hitbox, window, cx| {
            let view_for_paint = view.clone();
            let hitbox_id = hitbox.id;
            window.defer(cx, move |window, cx| {
                let _ = view_for_paint.update(cx, |this, cx| {
                    this.update_tab_bar_hover(button, Some(hitbox_id), window, cx);
                });
            });
            let view_for_move = view.clone();
            window.on_mouse_event(move |_: &gpui::MouseMoveEvent, phase, window, cx| {
                if phase == gpui::DispatchPhase::Capture {
                    let _ = view_for_move.update(cx, |this, cx| {
                        this.update_tab_bar_hover(button, Some(hitbox_id), window, cx);
                    });
                }
            });
            window.on_mouse_event(move |_: &gpui::MouseExitEvent, phase, window, cx| {
                if phase == gpui::DispatchPhase::Capture {
                    let _ = view.update(cx, |this, cx| {
                        this.update_tab_bar_hover(button, None, window, cx);
                    });
                }
            });
        },
    )
    .absolute()
    .inset_0()
}

fn spring<T: gpui::SpringTarget>(target: T) -> SpringAnimation<T> {
    SpringAnimation::new(SpringConfig::new(420.0, 36.0, 1.0)).to(target)
}

fn tab_slot(element: impl IntoElement) -> impl IntoElement {
    div()
        .h_full()
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .px(ui_rem(TAB_SLOT_INSET))
        .child(element)
}

fn horizontal_labels_fit(
    available_width_rem: f32,
    share: f32,
    button_count: usize,
    labels: impl Iterator<Item = String>,
) -> bool {
    if button_count == 0 {
        return false;
    }

    let widest_label_rem = labels
        .map(|label| {
            label
                .chars()
                .map(|character| if character.is_ascii() { 6.5 } else { 12.0 })
                .sum::<f32>()
                / 16.0
        })
        .fold(0.0_f32, f32::max);
    let button_width_rem = available_width_rem * share / button_count as f32;
    button_width_rem >= 1.0 + 0.5 + widest_label_rem + 0.5
}

fn horizontal_active_indicator(
    id: impl Into<ElementId>,
    index: usize,
    count: usize,
    color: Hsla,
    inset: f32,
    animations_enabled: bool,
) -> gpui::AnyElement {
    let left = index as f32 / count as f32;
    let width = 1.0 / count as f32;
    let id = id.into();

    let indicator = div()
        .absolute()
        .top(ui_rem(4.0))
        .left(relative(left))
        .w(relative(width))
        .h(ui_rem(TAB_BUTTON_HEIGHT))
        .flex()
        .items_center()
        .child({
            let bar = div()
                .flex_1()
                .h_full()
                .mx(ui_rem(inset))
                .rounded_lg()
                .bg(color)
                .shadow_sm();
            if animations_enabled {
                bar.with_animation(
                    (id.clone(), format!("stretch-{index}")),
                    Animation::new(Duration::from_millis(150)).with_easing(gpui::ease_in_out),
                    move |this, progress| {
                        let expansion = (std::f32::consts::PI * progress).sin() * 0.9;
                        this.mx(ui_rem((inset - expansion).max(0.0)))
                    },
                )
                .into_any_element()
            } else {
                bar.into_any_element()
            }
        });
    if animations_enabled {
        indicator
            .with_spring((id, "left"), spring(left), |this, value| {
                this.left(relative(value))
            })
            .into_any_element()
    } else {
        indicator.into_any_element()
    }
}

fn vertical_active_indicator(
    id: impl Into<ElementId>,
    index: usize,
    color: Hsla,
    top_inset: f32,
    horizontal_inset: f32,
    animations_enabled: bool,
) -> gpui::AnyElement {
    let top = top_inset + index as f32 * (SIDEBAR_BUTTON_HEIGHT + 4.0);
    let id = id.into();

    let indicator = div()
        .absolute()
        .top(ui_rem(top))
        .left(ui_rem(horizontal_inset))
        .right(ui_rem(horizontal_inset))
        .h(ui_rem(SIDEBAR_BUTTON_HEIGHT))
        .child({
            let bar = div().size_full().rounded_lg().bg(color).shadow_sm();
            if animations_enabled {
                bar.with_animation(
                    (id.clone(), format!("stretch-{index}")),
                    Animation::new(Duration::from_millis(150)).with_easing(gpui::ease_in_out),
                    move |this, progress| {
                        let expansion = (std::f32::consts::PI * progress).sin() * 0.9;
                        let vertical_margin = (2.0 * (1.0 - progress) - expansion).max(0.0);
                        this.my(ui_rem(vertical_margin))
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

fn sidebar_button_item(element: impl IntoElement) -> impl IntoElement {
    div()
        .w_auto()
        .min_w_auto()
        .h(ui_rem(SIDEBAR_BUTTON_HEIGHT))
        .min_h(ui_rem(SIDEBAR_BUTTON_HEIGHT))
        .flex_none()
        .child(element)
}

fn sidebar_tab_item(element: impl IntoElement) -> impl IntoElement {
    div()
        .w_full()
        .min_w_auto()
        .h(ui_rem(SIDEBAR_BUTTON_HEIGHT))
        .min_h(ui_rem(SIDEBAR_BUTTON_HEIGHT))
        .flex_none()
        .child(element)
}

fn tab_button(
    definition: &TabDefinition,
    active: bool,
    sidebar_layout: bool,
    show_label: bool,
    cx: &Context<MainWindow>,
) -> impl IntoElement {
    let theme = cx.theme().colors;
    let color = if active {
        theme.primary_foreground
    } else {
        theme.foreground
    };
    let label = i18n::translator(cx).translate(definition.label_key);
    let tooltip_label = label.clone();
    let tab_id = definition.id;
    let icon_group = format!("main-tab-{}", definition.id as u32);
    let mut button = div()
        .group(icon_group.clone())
        .relative()
        .h(ui_rem(TAB_BUTTON_HEIGHT))
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded_lg()
        .cursor_pointer()
        .child(hover_icon(icon_group, definition.icon, 16.0, color));

    if sidebar_layout {
        button = button
            .w_full()
            .min_w_auto()
            .h(ui_rem(SIDEBAR_BUTTON_HEIGHT))
            .min_h(ui_rem(SIDEBAR_BUTTON_HEIGHT))
            .flex_none();
        if show_label {
            button = button.justify_start().gap_2().px_3().child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(ui_rem(12.0))
                    .text_color(color)
                    .child(label),
            );
        } else {
            button = button.justify_center().px_0();
        }
    } else if show_label {
        button = button.gap_1().px_1().child(
            div()
                .min_w_0()
                .truncate()
                .text_size(ui_rem(12.0))
                .text_color(color)
                .child(label),
        );
    }

    if !active {
        button = button.hover(|this| this.bg(theme.secondary_hover));
    }

    button
        .id(("main-tab", definition.id as u32))
        .tooltip(move |window, cx| Tooltip::new(tooltip_label.clone()).build(window, cx))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.suppress_tab_bar_hover(tab_id.layout_button());
            this.select_tab(tab_id, cx);
        }))
        .child(hover_sensor(tab_id.layout_button(), cx))
}

fn filter_button(
    definition: &FilterDefinition,
    active: bool,
    hovered: bool,
    sidebar_layout: bool,
    show_label: bool,
    cx: &Context<MainWindow>,
) -> impl IntoElement {
    let theme = cx.theme().colors;
    let color = if active {
        if sidebar_layout {
            theme.primary_foreground
        } else {
            theme.primary
        }
    } else if hovered {
        theme.primary
    } else {
        theme.foreground
    };
    let label = i18n::translator(cx).translate(definition.label_key);
    let tooltip_label = label.clone();
    let filter_id = definition.id;
    let icon_group = format!("filter-button-{}", definition.id as u32);
    let mut button = div()
        .group(icon_group.clone())
        .h(ui_rem(TAB_BUTTON_HEIGHT))
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .justify_center()
        .relative()
        .cursor_pointer()
        .child(hover_icon(icon_group, definition.icon, 16.0, color));

    if sidebar_layout {
        button = button.rounded_lg();
        if !active {
            button = button.hover(|this| this.bg(theme.secondary_hover));
        }
    } else {
        button = button.hover(|this| this.bg(theme.secondary_hover));
    }

    if active {
        if sidebar_layout {
            button = button.bg(theme.primary).shadow_sm();
        } else {
            button = button.child(
                div()
                    .absolute()
                    .left(ui_rem(4.0))
                    .right(ui_rem(4.0))
                    .bottom(px(0.0))
                    .h(ui_rem(2.0))
                    .rounded_full()
                    .bg(color),
            );
        }
    }

    if hovered {
        let indicator = div().absolute().rounded_full().bg(color);
        button = button.child(if sidebar_layout {
            indicator
                .left(px(0.0))
                .top(ui_rem(4.0))
                .bottom(ui_rem(4.0))
                .w(ui_rem(2.0))
        } else {
            indicator
                .left(ui_rem(4.0))
                .right(ui_rem(4.0))
                .top(px(0.0))
                .h(ui_rem(2.0))
        });
    }

    if sidebar_layout {
        button = button
            .w_auto()
            .min_w_auto()
            .h(ui_rem(SIDEBAR_BUTTON_HEIGHT))
            .min_h(ui_rem(SIDEBAR_BUTTON_HEIGHT))
            .flex_none();
        if show_label {
            button = button.justify_start().gap_2().px_3().child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(ui_rem(12.0))
                    .text_color(color)
                    .child(label),
            );
        } else {
            button = button.justify_center().px_0();
        }
    } else if show_label {
        button = button.gap_1().px_1().child(
            div()
                .min_w_0()
                .truncate()
                .text_size(ui_rem(12.0))
                .text_color(color)
                .child(label),
        );
    }

    button
        .id(("filter-button", definition.id as u32))
        .tooltip(move |window, cx| Tooltip::new(tooltip_label.clone()).build(window, cx))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.suppress_tab_bar_hover(filter_id.layout_button());
            this.filters.toggle(filter_id);
            this.refresh_history(cx);
        }))
        .child(hover_sensor(filter_id.layout_button(), cx))
}

fn symbol_category_button(
    definition: &SymbolCategoryDefinition,
    active: bool,
    sidebar_layout: bool,
    show_label: bool,
    cx: &Context<MainWindow>,
) -> impl IntoElement {
    let theme = cx.theme().colors;
    let color = if active {
        theme.primary_foreground
    } else {
        theme.foreground
    };
    let label = i18n::translator(cx).translate(definition.label_key);
    let tooltip_label = label.clone();
    let category = definition.id;
    let icon_group = format!("symbol-category-{}", definition.id as u32);
    let category_icon = if definition.icon == "😀" {
        div()
            .size(ui_rem(16.0))
            .flex()
            .items_center()
            .justify_center()
            .text_size(ui_rem(14.0))
            .line_height(ui_rem(16.0))
            .text_color(color)
            .font_family("Segoe UI Emoji")
            .child(definition.icon)
            .into_any_element()
    } else {
        hover_icon(icon_group.clone(), definition.icon, 16.0, color).into_any_element()
    };

    let mut button = div()
        .group(icon_group)
        .h(ui_rem(TAB_BUTTON_HEIGHT))
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .justify_center()
        .relative()
        .rounded_lg()
        .cursor_pointer()
        .child(category_icon)
        .id(("symbol-category", category as u32))
        .tooltip(move |window, cx| Tooltip::new(tooltip_label.clone()).build(window, cx))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.suppress_tab_bar_hover(category.layout_button());
            this.select_symbol_category(category, cx);
        }))
        .child(hover_sensor(category.layout_button(), cx));

    if sidebar_layout {
        button = button
            .w_full()
            .min_w_auto()
            .h(ui_rem(SIDEBAR_BUTTON_HEIGHT))
            .min_h(ui_rem(SIDEBAR_BUTTON_HEIGHT))
            .flex_none();
        if show_label {
            button = button.justify_start().gap_2().px_3().child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(ui_rem(12.0))
                    .text_color(color)
                    .child(label),
            );
        } else {
            button = button.justify_center().px_0();
        }
    } else if show_label {
        button = button.gap_1().px_1().child(
            div()
                .min_w_0()
                .truncate()
                .text_size(ui_rem(12.0))
                .text_color(color)
                .child(label),
        );
    }

    if !active {
        button = button.hover(|this| this.bg(theme.secondary_hover));
    }

    button
}

pub fn render(
    main: &MainWindow,
    sidebar_layout: bool,
    combined_horizontal: bool,
    combined_width_rem: f32,
    cx: &Context<MainWindow>,
) -> impl IntoElement {
    let theme = cx.theme().colors;
    let animations_enabled =
        config_runtime::animation_enabled(cx, crate::config::AnimationKind::SingleSelectIndicator);
    let translator = i18n::translator(cx);
    let layout = config_runtime::appearance_config(cx).tab_bar;
    let main_tabs = ordered_tabs(&layout);
    let filters = ordered_filters(&layout);
    let symbol_categories = ordered_symbol_categories(&layout);
    let main_tab_index = main_tabs
        .iter()
        .position(|definition| definition.id == main.active_tab);
    let symbol_category_index = symbol_categories
        .iter()
        .position(|definition| definition.id == main.symbol_category);
    let middle_visible = if main.active_tab == TabId::Emoji {
        !symbol_categories.is_empty()
    } else {
        !filters.is_empty()
    };
    let tags_visible = layout.is_visible(TabBarButton::Tags) && main.active_tab != TabId::Emoji;
    let horizontal_content_width_rem =
        (combined_width_rem - if tags_visible { 4.5 } else { 0.0 } - 1.5).max(0.0);
    let horizontal_pages_show_label = combined_horizontal
        && horizontal_labels_fit(
            horizontal_content_width_rem,
            if middle_visible { 0.35 } else { 1.0 },
            main_tabs.len(),
            main_tabs
                .iter()
                .map(|definition| translator.translate(definition.label_key)),
        );
    let horizontal_filters_show_label = if main.active_tab == TabId::Emoji {
        combined_horizontal
            && horizontal_labels_fit(
                horizontal_content_width_rem,
                0.65,
                symbol_categories.len(),
                symbol_categories
                    .iter()
                    .map(|definition| translator.translate(definition.label_key)),
            )
    } else {
        combined_horizontal
            && horizontal_labels_fit(
                horizontal_content_width_rem,
                0.65,
                filters.len(),
                filters
                    .iter()
                    .map(|definition| translator.translate(definition.label_key)),
            )
    };
    let tags_panel_open = main.tags_panel_open;
    let sidebar_show_label = !main.sidebar_collapsed;
    let tags_button_color = if tags_panel_open {
        theme.primary_foreground
    } else {
        theme.foreground
    };
    let mut tags_button = div()
        .id("tags-toggle")
        .relative()
        .group("main-tags-button")
        .w(ui_rem(60.0))
        .h(ui_rem(TAB_BUTTON_HEIGHT))
        .flex()
        .items_center()
        .justify_center()
        .gap_1()
        .rounded_lg()
        .flex_none()
        .cursor_pointer()
        .hover(|this| {
            if tags_panel_open {
                this.bg(theme.primary_hover)
            } else {
                this.bg(theme.secondary_hover)
            }
        })
        .child(hover_icon(
            "main-tags-button",
            "tags",
            16.0,
            tags_button_color,
        ))
        .on_click(cx.listener(|this, _, window, cx| {
            this.suppress_tab_bar_hover(TabBarButton::Tags);
            let opening = !this.tags_panel_open;
            this.tags_panel_open = opening;
            if !opening {
                this.cancel_tag_reorder_preview();
            }
            if opening && !config_runtime::clipboard_config(cx).tags_panel_external {
                if let Some(tags_search_input) = this.tags_search_input.clone() {
                    if let Err(error) = crate::services::focus::focus_input_window(window) {
                        tracing::error!("展开标签面板时激活搜索框失败：{error}");
                    } else {
                        window.defer(cx, move |window, cx| {
                            tags_search_input.update(cx, |input, cx| input.focus(window, cx));
                        });
                    }
                }
            }
            cx.notify();
        }))
        .on_prepaint(
            cx.listener(|this, prepaint: &gpui::InteractivityPrepaint, _, cx| {
                if this.tags_button_bounds != Some(prepaint.bounds) {
                    this.tags_button_bounds = Some(prepaint.bounds);
                    if this.tags_panel_open {
                        cx.notify();
                    }
                }
            }),
        );

    if sidebar_layout {
        tags_button = tags_button
            .w_auto()
            .min_w_auto()
            .h(ui_rem(SIDEBAR_BUTTON_HEIGHT))
            .min_h(ui_rem(SIDEBAR_BUTTON_HEIGHT))
            .flex_none();
        if sidebar_show_label {
            tags_button = tags_button.justify_start().gap_2().px_3().child(
                div()
                    .text_size(ui_rem(12.0))
                    .text_color(tags_button_color)
                    .child(translator.translate("main.tags.title")),
            );
        } else {
            tags_button = tags_button.justify_center().px_0();
        }
    } else {
        tags_button = tags_button.child(
            div()
                .text_size(ui_rem(11.0))
                .text_color(tags_button_color)
                .child(translator.translate("main.tags.title")),
        );
    }

    if tags_panel_open {
        tags_button = tags_button.bg(theme.primary).shadow_sm();
    }

    if sidebar_layout {
        let collapse_icon = if main.sidebar_collapsed {
            "layout-sidebar-right-expand"
        } else {
            "layout-sidebar-left-collapse"
        };
        let collapse_label = if main.sidebar_collapsed {
            translator.translate("main.sidebar.expand")
        } else {
            translator.translate("main.sidebar.collapse")
        };
        let collapse_tooltip = collapse_label.clone();
        let mut collapse_button = div()
            .id("sidebar-collapse")
            .group("main-sidebar-collapse")
            .w_auto()
            .min_w_auto()
            .h(ui_rem(SIDEBAR_BUTTON_HEIGHT))
            .min_h(ui_rem(SIDEBAR_BUTTON_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .cursor_pointer()
            .rounded_lg()
            .hover(|this| this.bg(theme.secondary_hover))
            .tooltip(move |window, cx| Tooltip::new(collapse_tooltip.clone()).build(window, cx))
            .child(hover_icon(
                "main-sidebar-collapse",
                collapse_icon,
                16.0,
                theme.foreground,
            ))
            .on_click(cx.listener(|this, _, _, _| {
                this.toggle_sidebar_collapsed();
            }));
        if sidebar_show_label {
            collapse_button = collapse_button.justify_start().gap_2().px_3().child(
                div()
                    .text_size(ui_rem(12.0))
                    .text_color(theme.foreground)
                    .child(collapse_label),
            );
        } else {
            collapse_button = collapse_button.justify_center().px_0();
        }

        let mut sidebar = div()
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .relative()
            .bg(theme.tab_bar);
        if main.sidebar_collapsed {
            sidebar = sidebar.w(ui_rem(SIDEBAR_COLLAPSED_WIDTH));
        } else {
            sidebar = sidebar.w_auto().min_w_auto();
        }

        let mut tags_button = Some(tags_button);
        let mut has_section = false;
        for section in layout.sections.iter().copied() {
            let content = match section {
                TabBarSection::Pages if !main_tabs.is_empty() => div()
                    .w_auto()
                    .min_w_auto()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .relative()
                    .when_some(main_tab_index, |this, index| {
                        this.child(vertical_active_indicator(
                            "sidebar-main-tab-active-indicator",
                            index,
                            theme.primary,
                            8.0,
                            8.0,
                            animations_enabled,
                        ))
                    })
                    .children(main_tabs.iter().map(|definition| {
                        sidebar_tab_item(tab_button(
                            definition,
                            definition.id == main.active_tab,
                            true,
                            sidebar_show_label,
                            cx,
                        ))
                    }))
                    .into_any_element(),
                TabBarSection::Filters if middle_visible => div()
                    .flex_1()
                    .min_h_0()
                    .w_auto()
                    .min_w_auto()
                    .overflow_y_scrollbar()
                    .flex()
                    .flex_col()
                    .p_2()
                    .child(if main.active_tab == TabId::Emoji {
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .relative()
                            .when_some(symbol_category_index, |this, index| {
                                this.child(vertical_active_indicator(
                                    "sidebar-symbol-category-active-indicator",
                                    index,
                                    theme.primary,
                                    0.0,
                                    0.0,
                                    animations_enabled,
                                ))
                            })
                            .children(symbol_categories.iter().map(|definition| {
                                sidebar_tab_item(symbol_category_button(
                                    definition,
                                    main.symbol_category == definition.id,
                                    true,
                                    sidebar_show_label,
                                    cx,
                                ))
                            }))
                            .into_any_element()
                    } else {
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .children(filters.iter().map(|definition| {
                                sidebar_button_item(filter_button(
                                    definition,
                                    main.filters.is_selected(definition.id),
                                    main.filters.is_hovered(definition.id),
                                    true,
                                    sidebar_show_label,
                                    cx,
                                ))
                            }))
                            .into_any_element()
                    })
                    .into_any_element(),
                TabBarSection::Tags if layout.is_visible(TabBarButton::Tags) => div()
                    .w_auto()
                    .min_w_auto()
                    .flex_none()
                    .p_2()
                    .children(tags_button.take())
                    .into_any_element(),
                _ => continue,
            };
            if has_section {
                sidebar = sidebar.child(div().h(px(0.5)).flex_none().mx_2().bg(theme.border));
            }
            sidebar = sidebar.child(content);
            has_section = true;
        }
        if !middle_visible {
            sidebar = sidebar.child(div().flex_1());
        }
        return sidebar
            .child(
                div()
                    .w_auto()
                    .min_w_auto()
                    .flex_none()
                    .p_2()
                    .pt_1()
                    .child(collapse_button),
            )
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

    let mut tab_bar = div()
        .when(!combined_horizontal, |this| {
            this.h(ui_rem(TAB_BAR_HEIGHT)).flex_none()
        })
        .when(combined_horizontal, |this| this.h_full().flex_1().min_w_0())
        .flex()
        .items_center()
        .relative()
        .bg(theme.tab_bar);
    let mut tags_button = Some(tags_button);
    let mut has_section = false;
    for section in layout.sections.iter().copied() {
        let content = match section {
            TabBarSection::Pages if !main_tabs.is_empty() => div()
                .h(ui_rem(TAB_BAR_HEIGHT))
                .min_w_0()
                .px(ui_rem(6.0))
                .when(middle_visible, |this| {
                    this.flex_none().flex_basis(relative(0.35))
                })
                .when(!middle_visible, |this| this.flex_1())
                .flex()
                .items_center()
                .child(
                    div()
                        .relative()
                        .w_full()
                        .h_full()
                        .flex()
                        .items_center()
                        .when_some(main_tab_index, |this, index| {
                            this.child(horizontal_active_indicator(
                                "main-tab-active-indicator",
                                index,
                                main_tabs.len(),
                                theme.primary,
                                TAB_SLOT_INSET,
                                animations_enabled,
                            ))
                        })
                        .children(main_tabs.iter().map(|definition| {
                            tab_slot(tab_button(
                                definition,
                                definition.id == main.active_tab,
                                false,
                                horizontal_pages_show_label,
                                cx,
                            ))
                        })),
                )
                .into_any_element(),
            TabBarSection::Filters if middle_visible => div()
                .h(ui_rem(TAB_BAR_HEIGHT))
                .flex_1()
                .min_w_0()
                .px_1()
                .flex()
                .items_center()
                .child(if main.active_tab == TabId::Emoji {
                    div()
                        .h_full()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .relative()
                        .when_some(symbol_category_index, |this, index| {
                            this.child(horizontal_active_indicator(
                                "symbol-category-active-indicator",
                                index,
                                symbol_categories.len(),
                                theme.primary,
                                TAB_SLOT_INSET,
                                animations_enabled,
                            ))
                        })
                        .children(symbol_categories.iter().map(|definition| {
                            tab_slot(symbol_category_button(
                                definition,
                                main.symbol_category == definition.id,
                                false,
                                horizontal_filters_show_label,
                                cx,
                            ))
                        }))
                        .into_any_element()
                } else {
                    div()
                        .h_full()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .children(filters.iter().map(|definition| {
                            filter_button(
                                definition,
                                main.filters.is_selected(definition.id),
                                main.filters.is_hovered(definition.id),
                                false,
                                horizontal_filters_show_label,
                                cx,
                            )
                        }))
                        .into_any_element()
                })
                .into_any_element(),
            TabBarSection::Tags if tags_visible => div()
                .flex_none()
                .px_1()
                .children(tags_button.take())
                .into_any_element(),
            _ => continue,
        };
        if has_section {
            tab_bar = tab_bar.child(crate::ui::divider(theme));
        }
        tab_bar = tab_bar.child(content);
        has_section = true;
    }
    tab_bar.child(
        div()
            .absolute()
            .left_0()
            .right_0()
            .when(
                config_runtime::clipboard_config(cx).window_position
                    == ClipboardWindowPosition::ScreenBottomCenter,
                |this| this.top_0(),
            )
            .when(
                config_runtime::clipboard_config(cx).window_position
                    != ClipboardWindowPosition::ScreenBottomCenter,
                |this| this.bottom_0(),
            )
            .h(px(0.5))
            .bg(theme.border),
    )
}
