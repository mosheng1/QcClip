use gpui::{
    AnyElement, App, Context, Entity, InteractiveElement, IntoElement, MouseButton, ParentElement,
    Render, StatefulInteractiveElement, Styled, Window, div, px,
};
use gpui_component::ActiveTheme as _;

use crate::{
    config::{ClipboardListLayout, ListItemHeight, TitlebarPosition},
    config_runtime,
    ui::scale::ui_rem,
};

use super::{
    MainWindowService,
    components::{
        clipboard_list, emoji_picker, gallery_picker, multi_selection_bar, symbol_picker, tab_bar,
        tags_panel, title_bar,
    },
    state::{AdaptiveHeightCacheKey, MainWindow},
    tabs::TabId,
    uses_sidebar_layout,
};

fn is_sidebar_layout(window: &Window) -> bool {
    uses_sidebar_layout(
        f32::from(window.viewport_size().width) / crate::ui::scale::window_scale(window),
    )
}

fn effective_titlebar_position(
    position: TitlebarPosition,
    horizontal_list: bool,
) -> TitlebarPosition {
    if horizontal_list && position.is_vertical() {
        TitlebarPosition::Top
    } else {
        position
    }
}

impl Render for MainWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.is_list_pane {
            return self.render_list_pane(window, cx);
        }
        self.sync_split_layout(cx);
        let blocking_operation = MainWindowService::blocking_operation(cx);
        let is_blocked = blocking_operation.is_some();
        if is_blocked {
            self.cancel_hover_preview(cx);
            self.window_drag_start = None;
            self.floating_toolbar_drag = None;
            self.tags_panel_resize_start = None;
            self.heat_scrollbar.drag_origin = None;
            self.heat_scrollbar.dragging = false;
            self.focus_handle.focus(window, cx);
        }
        let appearance = config_runtime::appearance_config(cx);
        let layout = &appearance.tab_bar;
        let tabs = super::tabs::ordered_tabs(layout);
        let active_page_visible = layout.is_visible(self.active_tab.layout_button())
            || (self.split_panes.is_some() && self.active_tab != TabId::Emoji);
        if !active_page_visible {
            let fallback = tabs.first().map(|tab| tab.id).unwrap_or(TabId::Clipboard);
            self.select_tab(fallback, cx);
        }
        if !layout.is_visible(self.symbol_category.layout_button()) {
            let categories = super::tabs::ordered_symbol_categories(layout);
            let fallback = categories
                .first()
                .map(|category| category.id)
                .unwrap_or(super::tabs::SymbolCategoryId::Emoji);
            self.select_symbol_category(fallback, cx);
        }
        let clipboard = config_runtime::clipboard_config(cx);
        let horizontal = clipboard.window_position.uses_horizontal_list();
        self.sync_list_axis(horizontal);
        let mut filters_changed = self.filters.retain_visible(layout);
        if is_blocked || !clipboard.filter_hover_switch {
            filters_changed |= self.filters.clear_hovered();
        }
        if is_blocked
            || !window.is_window_hovered()
            || self
                .tab_bar_hover
                .pending_button()
                .is_some_and(|button| !layout.is_visible(button))
        {
            self.cancel_tab_bar_hover();
        }
        if !layout.is_visible(crate::config::TabBarButton::Tags) {
            self.tags_panel_open = false;
            self.tags_button_bounds = None;
            self.cancel_tags_panel_close();
            filters_changed |= !self.selected_tag_ids.is_empty();
            self.selected_tag_ids.clear();
        }
        if filters_changed {
            self.refresh_history(cx);
        }
        if !(self.active_tab == TabId::Emoji
            && self.symbol_category == super::tabs::SymbolCategoryId::Gallery)
        {
            self.clear_gallery_resources();
        }
        self.cleanup_gallery_image_atlas(window);
        self.advance_scroll_animation(
            window,
            !is_blocked
                && config_runtime::animation_enabled(
                    cx,
                    crate::config::AnimationKind::SmoothScroll,
                ),
        );
        self.cleanup_image_cache(window);
        self.cleanup_file_list_cache(window, cx);
        self.ensure_tags_search_input(window, cx);
        self.sync_tags_search_placeholder(window, cx);
        self.ensure_search_input(window, cx);
        self.sync_search_placeholder(window, cx);
        let theme = cx.theme().colors;
        self.prepare_adaptive_height_cache(
            AdaptiveHeightCacheKey {
                viewport_width: window.viewport_size().width,
                rem_size: window.rem_size(),
                font_metrics: crate::ui::fonts::AppFontMetrics::from_window(window, cx),
                list_item_style: appearance.list_item_style,
                card_item_gap: appearance.card_item_gap,
                adaptive_max_lines: appearance.adaptive_max_lines,
                only_show_notes: appearance.only_show_notes,
                show_info_row: appearance.item_display.shows_info_row(),
                display_priority: clipboard.display_priority,
                query_generation: self.query_generation,
            },
            appearance.list_item_height == ListItemHeight::Adaptive,
        );
        let titlebar_position =
            effective_titlebar_position(appearance.titlebar_position, horizontal);
        let sidebar_layout = !horizontal && self.split_panes.is_none() && is_sidebar_layout(window);
        if !sidebar_layout || !self.tags_panel_open {
            self.finish_tags_panel_resize(cx);
        }
        if !sidebar_layout {
            self.sidebar_collapsed = false;
        }
        let content = if self.active_tab == TabId::Emoji {
            self.ensure_symbol_search_input(window, cx);
            self.sync_symbol_search_placeholder(window, cx);
            match self.symbol_category {
                super::tabs::SymbolCategoryId::Symbols => symbol_picker::render(self, window, cx),
                super::tabs::SymbolCategoryId::Gallery => gallery_picker::render(self, window, cx),
                super::tabs::SymbolCategoryId::Emoji => emoji_picker::render(self, window, cx),
            }
        } else {
            self.sync_split_filters(cx);
            if let Some(panes) = self.split_panes.clone() {
                self.render_split_content(panes, window, cx)
            } else {
                self.render_clipboard_content(window, cx)
            }
        };
        let titlebar =
            title_bar::render(self, window, cx, titlebar_position, horizontal).into_any_element();
        let combined_width_rem =
            f32::from(window.viewport_size().width) / f32::from(window.rem_size()) / 2.0;
        let mut tab_bar = Some(
            tab_bar::render(self, sidebar_layout, horizontal, combined_width_rem, cx)
                .into_any_element(),
        );
        let workspace = if horizontal {
            div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .child(content)
        } else if sidebar_layout {
            div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_row()
                .children(tab_bar.take())
                .child(tags_panel::render(self, window, cx, true))
                .child(content)
        } else {
            div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .children(tab_bar.take())
                .child(content)
        };

        let mut root = div()
            .size_full()
            .bg(theme.background)
            .flex()
            .relative()
            .track_focus(&self.focus_handle);
        if !is_blocked && !clipboard.window_position.is_screen_edge() {
            root = root.child(super::window_drag::event_listener(clipboard.edge_hide, cx));
        }

        root = root.flex_col();
        if horizontal {
            let combined_bar = div()
                .w_full()
                .h(ui_rem(super::components::symbol_sidebar::TAB_BAR_HEIGHT))
                .flex_none()
                .flex()
                .items_center();
            let combined_bar = combined_bar.child(titlebar).children(tab_bar.take());
            match titlebar_position {
                TitlebarPosition::Top => {
                    root = root.child(combined_bar).child(workspace);
                }
                TitlebarPosition::Bottom => {
                    root = root.child(workspace).child(combined_bar);
                }
                TitlebarPosition::Left | TitlebarPosition::Right => unreachable!(),
            }
        } else {
            let body = div().flex_1().min_w_0().min_h_0().flex();
            let body = match titlebar_position {
                TitlebarPosition::Top => body.flex_col().child(titlebar).child(workspace),
                TitlebarPosition::Bottom => body.flex_col().child(workspace).child(titlebar),
                TitlebarPosition::Left => body.flex_row().child(titlebar).child(workspace),
                TitlebarPosition::Right => body.flex_row().child(workspace).child(titlebar),
            };
            root = root.child(body).children(tab_bar);
        }

        if !sidebar_layout {
            root = root.child(tags_panel::render(self, window, cx, false));
        }

        if let Some(title_key) = blocking_operation {
            root = root
                .capture_any_mouse_down(|_, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                })
                .capture_any_mouse_up(|_, _, cx| cx.stop_propagation())
                .capture_key_down(|_, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                })
                .capture_key_up(|_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .id("main-blocking-operation-overlay")
                        .absolute()
                        .inset_0()
                        .occlude()
                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                        .child(if self.gallery_import_progress.is_some() {
                            gallery_picker::render_gallery_import_overlay(self, cx)
                        } else {
                            render_blocking_operation_overlay(title_key, cx)
                        }),
                );
        }

        root.into_any_element()
    }
}

impl MainWindow {
    fn sync_list_axis(&mut self, horizontal: bool) {
        self.list_smooth_scroll.set_axis(if horizontal {
            gpui::Axis::Horizontal
        } else {
            gpui::Axis::Vertical
        });
        if self
            .list_layout_cache
            .as_ref()
            .is_some_and(|cache| cache.key.horizontal != horizontal)
        {
            self.cancel_reorder_preview();
            self.list_layout_cache = None;
            self.list_scroll_handle
                .set_offset(gpui::point(px(0.0), px(0.0)));
            self.heat_scrollbar = Default::default();
            if let Some(index) = self
                .selected_item_id
                .and_then(|id| self.list_order_positions.get(&id))
                .copied()
            {
                self.list_scroll_handle
                    .scroll_to_item(index, gpui::ScrollStrategy::Nearest);
            }
        }
    }

    fn render_split_content(
        &mut self,
        panes: [Entity<MainWindow>; 2],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let side_by_side = self.split_layout(cx) == ClipboardListLayout::SideBySide;
        let ratio = self.split_ratio(cx);
        let resizing = self.split_resize.is_some();
        let theme = cx.theme().colors;
        let divider_size = px(6.0 * crate::ui::scale::window_scale(window));
        let mut content = div()
            .id("main-split-content")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .relative()
            .flex()
            .on_prepaint(
                cx.listener(|this, prepaint: &gpui::InteractivityPrepaint, _, _| {
                    this.split_content_bounds = Some(prepaint.bounds);
                }),
            )
            .on_mouse_move_all({
                let main = cx.entity().downgrade();
                move |event: &gpui::MouseMoveEvent, phase, _, _, cx| {
                    if phase == gpui::DispatchPhase::Capture {
                        let _ = main.update(cx, |this, cx| {
                            if this.split_resize.is_some() {
                                if event.dragging() {
                                    this.update_split_resize(event.position, cx);
                                } else {
                                    this.finish_split_resize(cx);
                                }
                                cx.stop_propagation();
                            }
                        });
                    }
                }
            })
            .on_mouse_up_all({
                let main = cx.entity().downgrade();
                move |event, phase, _, _, cx| {
                    if phase == gpui::DispatchPhase::Capture && event.button == MouseButton::Left {
                        let _ = main.update(cx, |this, cx| {
                            if this.split_resize.is_some() {
                                this.update_split_resize(event.position, cx);
                                this.finish_split_resize(cx);
                                cx.stop_propagation();
                            }
                        });
                    }
                }
            });
        content = if side_by_side {
            content.flex_row()
        } else {
            content.flex_col()
        };
        let mut divider = div()
            .id("main-split-divider")
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.title_bar)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                    this.begin_split_resize(event.position, divider_size, cx);
                    if event.click_count == 2 {
                        this.reset_split_ratio(cx);
                    }
                }),
            );
        let grip = div().rounded_full().bg(theme.muted_foreground);
        divider = if side_by_side {
            divider
                .w(divider_size)
                .h_full()
                .cursor_col_resize()
                .child(grip.w(ui_rem(2.0)).h(ui_rem(24.0)))
        } else {
            divider
                .h(divider_size)
                .w_full()
                .cursor_row_resize()
                .child(grip.h(ui_rem(2.0)).w(ui_rem(24.0)))
        };
        let mut divider = Some(divider);
        for (index, (pane, tab)) in panes
            .into_iter()
            .zip([TabId::Clipboard, TabId::Favorites])
            .enumerate()
        {
            if index == 1 {
                content = content.children(divider.take());
            }
            content = content.child(
                div()
                    .id(("main-list-pane", tab as usize))
                    .flex_1()
                    .flex_grow(if index == 0 { ratio } else { 1.0 - ratio })
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if *hovered && this.can_follow_mouse_hover() {
                            this.activate_split_pane(tab, cx);
                        }
                    }))
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.can_follow_mouse_hover() {
                            this.activate_split_pane(tab, cx);
                        }
                    }))
                    .capture_any_mouse_down(cx.listener(move |this, _, _, cx| {
                        this.keyboard_selection_mouse_move_sequence = None;
                        this.activate_split_pane(tab, cx);
                    }))
                    .child(pane),
            );
        }
        if resizing {
            // 布局变化也会触发悬停，拖动期间遮挡列表，避免误选条目或弹出预览。
            let overlay = div().absolute().inset_0().occlude();
            content = content.child(if side_by_side {
                overlay.cursor_col_resize()
            } else {
                overlay.cursor_row_resize()
            });
        }
        content.into_any_element()
    }

    fn render_clipboard_content(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(clipboard_list::render(
                self.active_tab,
                self.can_reorder_items(),
                self.selected_item_id,
                &self.multi_selected_item_ids,
                &self.list_status,
                &self.list_scroll_handle,
                &mut self.list_heatmap,
                &self.heat_scrollbar,
                &mut self.adaptive_item_heights,
                &mut self.list_layout_cache,
                self.list_layout_generation,
                self.query_generation,
                self.total_item_count,
                &self.list_order_ids,
                &self.loaded_items,
                &self.loaded_html_images,
                &self.file_list_views,
                self.floating_toolbar_bottom,
                self.floating_toolbar_drag.is_some(),
                window,
                cx,
            ))
            .child(multi_selection_bar::render(
                self.multi_selected_item_ids.len(),
                self.active_tab,
                cx,
            ))
            .into_any_element()
    }

    fn render_list_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let blocked = MainWindowService::blocking_operation(cx).is_some();
        if blocked {
            self.cancel_hover_preview(cx);
            self.floating_toolbar_drag = None;
            self.cancel_reorder_preview();
            self.heat_scrollbar = Default::default();
        }
        self.sync_list_axis(
            config_runtime::clipboard_config(cx)
                .window_position
                .uses_horizontal_list(),
        );
        self.advance_scroll_animation(
            window,
            !blocked
                && config_runtime::animation_enabled(
                    cx,
                    crate::config::AnimationKind::SmoothScroll,
                ),
        );
        self.cleanup_image_cache(window);
        self.cleanup_file_list_cache(window, cx);
        let appearance = config_runtime::appearance_config(cx);
        let clipboard = config_runtime::clipboard_config(cx);
        let width = self.list_scroll_handle.bounds().size.width;
        self.prepare_adaptive_height_cache(
            AdaptiveHeightCacheKey {
                viewport_width: width,
                rem_size: window.rem_size(),
                font_metrics: crate::ui::fonts::AppFontMetrics::from_window(window, cx),
                list_item_style: appearance.list_item_style,
                card_item_gap: appearance.card_item_gap,
                adaptive_max_lines: appearance.adaptive_max_lines,
                only_show_notes: appearance.only_show_notes,
                show_info_row: appearance.item_display.shows_info_row(),
                display_priority: clipboard.display_priority,
                query_generation: self.query_generation,
            },
            appearance.list_item_height == ListItemHeight::Adaptive,
        );
        let theme = cx.theme().colors;
        div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .border_t_2()
            .border_color(if self.pane_active {
                theme.primary
            } else {
                gpui::transparent_black()
            })
            .child(self.render_clipboard_content(window, cx))
            .into_any_element()
    }
}

fn render_blocking_operation_overlay(title_key: &str, cx: &App) -> AnyElement {
    use crate::ui::{icon, scale::ui_rem};

    let theme = cx.theme().colors;
    let translator = crate::i18n::translator(cx);
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui::ColorExt::opacity(&theme.popover, 0.58))
        .child(
            div()
                .w_full()
                .max_w(ui_rem(320.0))
                .mx(ui_rem(16.0))
                .p(ui_rem(20.0))
                .flex()
                .flex_col()
                .items_center()
                .gap(ui_rem(16.0))
                .rounded_xl()
                .bg(theme.popover)
                .shadow_lg()
                .child(icon("refresh", 36.0, theme.primary))
                .child(
                    div()
                        .text_center()
                        .text_size(ui_rem(18.0))
                        .text_color(theme.foreground)
                        .child(translator.translate(title_key)),
                )
                .child(
                    div()
                        .text_center()
                        .text_size(ui_rem(13.0))
                        .text_color(theme.muted_foreground)
                        .child(translator.translate("settings.data_management.busy")),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use crate::config::TitlebarPosition;

    use super::effective_titlebar_position;

    #[test]
    fn horizontal_list_uses_horizontal_titlebar_position() {
        assert_eq!(
            effective_titlebar_position(TitlebarPosition::Left, true),
            TitlebarPosition::Top
        );
        assert_eq!(
            effective_titlebar_position(TitlebarPosition::Right, true),
            TitlebarPosition::Top
        );
        assert_eq!(
            effective_titlebar_position(TitlebarPosition::Bottom, true),
            TitlebarPosition::Bottom
        );
        assert_eq!(
            effective_titlebar_position(TitlebarPosition::Left, false),
            TitlebarPosition::Left
        );
    }

    use super::super::uses_sidebar_layout;

    #[test]
    fn sidebar_layout_uses_logical_width_boundary() {
        assert!(!uses_sidebar_layout(499.0));
        assert!(uses_sidebar_layout(500.0));
    }
}
