use crate::ui::scale::ui_rem;

use super::super::{SettingsPage, notify_settings_error};
use gpui::prelude::*;
use gpui::{AnyElement, App, ColorExt as _, Context, Entity, div, img, px};

use super::super::{
    SettingsWindowView,
    components::{settings_dropdown, settings_error_row, settings_page_header},
};
use crate::ui::{hover_icon, icon};
use crate::{
    config::{
        ClipboardConfig, ClipboardDisplayPriority, ClipboardMouseLeftBehavior,
        ClipboardMouseMiddleBehavior, ClipboardPreviewPriority, ClipboardWindowPosition,
        SearchEngine,
    },
    config_runtime, i18n,
    windows::menu::{DropdownItem, DropdownItemId},
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::Input;
use gpui_component::slider::Slider;
use gpui_component::switch::Switch;
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme as _, Sizable as _, collapsible::Collapsible, h_flex, v_flex};

#[derive(Clone, Copy)]
enum HoverBar {
    Page,
    Filter,
}

impl SettingsWindowView {
    pub(in crate::windows::settings) fn render_clipboard(
        &self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let translator = i18n::translator(cx);
        let clipboard = config_runtime::clipboard_config(cx);
        let view = cx.entity().clone();
        let reset_error = translator.translate("settings.clipboard.reset_error");
        let reset_view = view.clone();
        let reset_page_hover_delay_slider = self.page_hover_delay_slider.clone();
        let reset_filter_hover_delay_slider = self.filter_hover_delay_slider.clone();
        let reset_button = Button::new("settings-clipboard-reset")
            .warning()
            .small()
            .label(translator.translate("settings.action.reset_page"))
            .on_click(move |_, window, cx| {
                let result = config_runtime::reset_clipboard_config(cx);
                let error_message = result.as_ref().err().map(|error| {
                    tracing::error!("重置剪贴板设置失败：{error}");
                    reset_error.clone()
                });
                if result.is_ok() {
                    let default_delay = f32::from(ClipboardConfig::TAB_BAR_HOVER_DELAY_DEFAULT_MS);
                    reset_page_hover_delay_slider.update(cx, |slider, cx| {
                        slider.set_value(default_delay, window, cx);
                    });
                    reset_filter_hover_delay_slider.update(cx, |slider, cx| {
                        slider.set_value(default_delay, window, cx);
                    });
                }
                reset_view.update(cx, |view, cx| {
                    view.clipboard_error =
                        notify_settings_error(SettingsPage::Clipboard, error_message, cx);
                    cx.notify();
                });
            });

        let priority_items = ClipboardDisplayPriority::ALL
            .into_iter()
            .map(|priority| {
                DropdownItem::option(
                    DropdownItemId::new(priority.code()),
                    translator.translate(clipboard_display_priority_label_key(priority)),
                )
                .selected(priority == clipboard.display_priority)
            })
            .collect::<Vec<_>>();
        let save_error = translator.translate("settings.clipboard.save_error");
        let mouse_left_behavior_items = ClipboardMouseLeftBehavior::ALL
            .into_iter()
            .map(|behavior| {
                DropdownItem::option(
                    DropdownItemId::new(behavior.code()),
                    translator.translate(clipboard_mouse_left_behavior_label_key(behavior)),
                )
                .selected(behavior == clipboard.mouse_left_behavior)
            })
            .collect::<Vec<_>>();
        let mouse_left_behavior_selector = settings_dropdown(
            "settings-clipboard-mouse-left-behavior-select",
            clipboard.mouse_left_behavior.code(),
            mouse_left_behavior_items,
            {
                let view = view.clone();
                let save_error = save_error.clone();
                move |action_id, cx| {
                    let Some(behavior) = ClipboardMouseLeftBehavior::ALL
                        .into_iter()
                        .find(|behavior| behavior.code() == action_id.as_str())
                    else {
                        return;
                    };
                    persist_clipboard_change(
                        view.clone(),
                        move |clipboard| clipboard.mouse_left_behavior = behavior,
                        save_error.clone(),
                        cx,
                    );
                }
            },
            cx,
        );
        let mouse_middle_behavior_items = ClipboardMouseMiddleBehavior::ALL
            .into_iter()
            .map(|behavior| {
                DropdownItem::option(
                    DropdownItemId::new(behavior.code()),
                    translator.translate(clipboard_mouse_middle_behavior_label_key(behavior)),
                )
                .selected(behavior == clipboard.mouse_middle_behavior)
            })
            .collect::<Vec<_>>();
        let mouse_middle_behavior_selector = settings_dropdown(
            "settings-clipboard-mouse-middle-behavior-select",
            clipboard.mouse_middle_behavior.code(),
            mouse_middle_behavior_items,
            {
                let view = view.clone();
                let save_error = save_error.clone();
                move |action_id, cx| {
                    let Some(behavior) = ClipboardMouseMiddleBehavior::ALL
                        .into_iter()
                        .find(|behavior| behavior.code() == action_id.as_str())
                    else {
                        return;
                    };
                    persist_clipboard_change(
                        view.clone(),
                        move |clipboard| clipboard.mouse_middle_behavior = behavior,
                        save_error.clone(),
                        cx,
                    );
                }
            },
            cx,
        );
        let window_position_items = ClipboardWindowPosition::ALL
            .into_iter()
            .map(|position| {
                DropdownItem::option(
                    DropdownItemId::new(position.code()),
                    translator.translate(clipboard_window_position_label_key(position)),
                )
                .selected(position == clipboard.window_position)
            })
            .collect::<Vec<_>>();
        let window_position_selector = settings_dropdown(
            "settings-clipboard-window-position-select",
            clipboard.window_position.code(),
            window_position_items,
            {
                let view = view.clone();
                let save_error = save_error.clone();
                move |action_id, cx| {
                    let Some(position) = ClipboardWindowPosition::ALL
                        .into_iter()
                        .find(|position| position.code() == action_id.as_str())
                    else {
                        return;
                    };
                    persist_clipboard_change(
                        view.clone(),
                        move |clipboard| clipboard.window_position = position,
                        save_error.clone(),
                        cx,
                    );
                }
            },
            cx,
        );
        let priority_selector = settings_dropdown(
            "settings-clipboard-display-priority-select",
            clipboard.display_priority.code(),
            priority_items,
            {
                let view = view.clone();
                let save_error = save_error.clone();
                move |action_id, cx| {
                    let Some(priority) = ClipboardDisplayPriority::ALL
                        .into_iter()
                        .find(|priority| priority.code() == action_id.as_str())
                    else {
                        return;
                    };
                    persist_clipboard_change(
                        view.clone(),
                        move |clipboard| clipboard.display_priority = priority,
                        save_error.clone(),
                        cx,
                    );
                }
            },
            cx,
        );
        let preview_priority_items = ClipboardPreviewPriority::ALL
            .into_iter()
            .map(|priority| {
                DropdownItem::option(
                    DropdownItemId::new(priority.code()),
                    translator.translate(clipboard_preview_priority_label_key(priority)),
                )
                .selected(priority == clipboard.preview_priority)
            })
            .collect::<Vec<_>>();
        let preview_priority_selector = settings_dropdown(
            "settings-clipboard-preview-priority-select",
            clipboard.preview_priority.code(),
            preview_priority_items,
            {
                let view = view.clone();
                let save_error = save_error.clone();
                move |action_id, cx| {
                    let Some(priority) = ClipboardPreviewPriority::ALL
                        .into_iter()
                        .find(|priority| priority.code() == action_id.as_str())
                    else {
                        return;
                    };
                    persist_clipboard_change(
                        view.clone(),
                        move |clipboard| clipboard.preview_priority = priority,
                        save_error.clone(),
                        cx,
                    );
                }
            },
            cx,
        );
        let edge_hide_switch = Switch::new("settings-clipboard-edge-hide")
            .large()
            .checked(clipboard.edge_hide)
            .accessibility_label(translator.translate("settings.clipboard.edge_hide"))
            .on_click({
                let view = view.clone();
                let save_error = save_error.clone();
                move |checked, _, cx| {
                    persist_clipboard_change(
                        view.clone(),
                        move |clipboard| clipboard.edge_hide = *checked,
                        save_error.clone(),
                        cx,
                    );
                }
            });
        let edge_hide_show_edge_switch = Switch::new("settings-clipboard-edge-hide-show-edge")
            .large()
            .checked(clipboard.edge_hide_show_edge)
            .accessibility_label(translator.translate("settings.clipboard.edge_hide_show_edge"))
            .on_click({
                let view = view.clone();
                let save_error = save_error.clone();
                move |checked, _, cx| {
                    persist_clipboard_change(
                        view.clone(),
                        move |clipboard| clipboard.edge_hide_show_edge = *checked,
                        save_error.clone(),
                        cx,
                    );
                }
            });
        let page_hover_switch = Switch::new("settings-clipboard-page-hover-switch")
            .large()
            .checked(clipboard.page_hover_switch)
            .accessibility_label(translator.translate("settings.clipboard.page_hover_switch"))
            .on_click({
                let view = view.clone();
                let save_error = save_error.clone();
                move |checked, _, cx| {
                    persist_clipboard_change(
                        view.clone(),
                        move |clipboard| clipboard.page_hover_switch = *checked,
                        save_error.clone(),
                        cx,
                    );
                }
            });
        let filter_hover_switch = Switch::new("settings-clipboard-filter-hover-switch")
            .large()
            .checked(clipboard.filter_hover_switch)
            .accessibility_label(translator.translate("settings.clipboard.filter_hover_switch"))
            .on_click({
                let view = view.clone();
                let save_error = save_error.clone();
                move |checked, _, cx| {
                    persist_clipboard_change(
                        view.clone(),
                        move |clipboard| clipboard.filter_hover_switch = *checked,
                        save_error.clone(),
                        cx,
                    );
                }
            });
        let hover_preview_switch = Switch::new("settings-clipboard-hover-preview")
            .large()
            .checked(clipboard.hover_preview_enabled)
            .accessibility_label(translator.translate("settings.clipboard.hover_preview"))
            .on_click({
                let view = view.clone();
                let save_error = save_error.clone();
                move |checked, _, cx| {
                    persist_clipboard_change(
                        view.clone(),
                        move |clipboard| clipboard.hover_preview_enabled = *checked,
                        save_error.clone(),
                        cx,
                    );
                }
            });
        let move_pasted_to_first_in_clipboard_switch =
            Switch::new("settings-clipboard-move-pasted-to-first-in-clipboard")
                .large()
                .checked(clipboard.move_pasted_to_first_in_clipboard)
                .accessibility_label(
                    translator.translate("settings.clipboard.move_pasted_to_first_in_clipboard"),
                )
                .on_click({
                    let view = view.clone();
                    let save_error = save_error.clone();
                    move |checked, _, cx| {
                        persist_clipboard_change(
                            view.clone(),
                            move |clipboard| clipboard.move_pasted_to_first_in_clipboard = *checked,
                            save_error.clone(),
                            cx,
                        );
                    }
                });
        let move_pasted_to_first_in_favorites_switch =
            Switch::new("settings-clipboard-move-pasted-to-first-in-favorites")
                .large()
                .checked(clipboard.move_pasted_to_first_in_favorites)
                .accessibility_label(
                    translator.translate("settings.clipboard.move_pasted_to_first_in_favorites"),
                )
                .on_click({
                    let view = view.clone();
                    let save_error = save_error.clone();
                    move |checked, _, cx| {
                        persist_clipboard_change(
                            view.clone(),
                            move |clipboard| clipboard.move_pasted_to_first_in_favorites = *checked,
                            save_error.clone(),
                            cx,
                        );
                    }
                });
        let auto_scroll_to_top_switch = Switch::new("settings-clipboard-auto-scroll-to-top")
            .large()
            .checked(clipboard.auto_scroll_to_top)
            .accessibility_label(translator.translate("settings.clipboard.auto_scroll_to_top"))
            .on_click({
                let view = view.clone();
                let save_error = save_error.clone();
                move |checked, _, cx| {
                    persist_clipboard_change(
                        view.clone(),
                        move |clipboard| clipboard.auto_scroll_to_top = *checked,
                        save_error.clone(),
                        cx,
                    );
                }
            });
        let auto_focus_search_switch = Switch::new("settings-clipboard-auto-focus-search")
            .large()
            .checked(clipboard.auto_focus_search)
            .accessibility_label(translator.translate("settings.clipboard.auto_focus_search"))
            .on_click({
                let view = view.clone();
                let save_error = save_error.clone();
                move |checked, _, cx| {
                    persist_clipboard_change(
                        view.clone(),
                        move |clipboard| clipboard.auto_focus_search = *checked,
                        save_error.clone(),
                        cx,
                    );
                }
            });
        let auto_clear_search_switch = Switch::new("settings-clipboard-auto-clear-search")
            .large()
            .checked(clipboard.auto_clear_search)
            .accessibility_label(translator.translate("settings.clipboard.auto_clear_search"))
            .on_click({
                let view = view.clone();
                let save_error = save_error.clone();
                move |checked, _, cx| {
                    persist_clipboard_change(
                        view.clone(),
                        move |clipboard| clipboard.auto_clear_search = *checked,
                        save_error.clone(),
                        cx,
                    );
                }
            });
        let confirm_favorite_removal_switch =
            Switch::new("settings-clipboard-confirm-favorite-removal")
                .large()
                .checked(clipboard.confirm_favorite_removal)
                .accessibility_label(
                    translator.translate("settings.clipboard.confirm_favorite_removal"),
                )
                .on_click({
                    let view = view.clone();
                    let save_error = save_error.clone();
                    move |checked, _, cx| {
                        persist_clipboard_change(
                            view.clone(),
                            move |clipboard| clipboard.confirm_favorite_removal = *checked,
                            save_error.clone(),
                            cx,
                        );
                    }
                });
        let save_images_as_files_switch = Switch::new("settings-clipboard-save-images-as-files")
            .large()
            .checked(clipboard.save_images_as_files)
            .accessibility_label(translator.translate("settings.clipboard.save_images_as_files"))
            .on_click({
                let view = view.clone();
                let save_error = save_error.clone();
                move |checked, _, cx| {
                    persist_clipboard_change(
                        view.clone(),
                        move |clipboard| clipboard.save_images_as_files = *checked,
                        save_error.clone(),
                        cx,
                    );
                }
            });

        let mut section = v_flex()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .overflow_hidden()
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .child(settings_page_header(
                theme,
                translator.translate("settings.page.clipboard"),
                translator.translate("settings.clipboard.description"),
                reset_button,
            ))
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.clipboard.mouse_left_behavior"),
                    Some(
                        translator
                            .translate("settings.clipboard.mouse_left_behavior_description")
                            .into(),
                    ),
                    mouse_left_behavior_selector,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.clipboard.mouse_middle_behavior"),
                    Some(
                        translator
                            .translate("settings.clipboard.mouse_middle_behavior_description")
                            .into(),
                    ),
                    mouse_middle_behavior_selector,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.clipboard.edge_hide"),
                    Some(
                        translator
                            .translate("settings.clipboard.edge_hide_description")
                            .into(),
                    ),
                    edge_hide_switch,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.clipboard.edge_hide_show_edge"),
                    Some(
                        translator
                            .translate("settings.clipboard.edge_hide_show_edge_description")
                            .into(),
                    ),
                    edge_hide_show_edge_switch,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.clipboard.window_position"),
                    Some(
                        translator
                            .translate("settings.clipboard.window_position_description")
                            .into(),
                    ),
                    window_position_selector,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.clipboard.display_priority"),
                    Some(
                        translator
                            .translate("settings.clipboard.display_priority_description")
                            .into(),
                    ),
                    priority_selector,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.clipboard.preview_priority"),
                    Some(
                        translator
                            .translate("settings.clipboard.preview_priority_description")
                            .into(),
                    ),
                    preview_priority_selector,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.clipboard.hover_preview"),
                    Some(
                        translator
                            .translate("settings.clipboard.hover_preview_description")
                            .into(),
                    ),
                    hover_preview_switch,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.clipboard.page_hover_switch"),
                    Some(
                        translator
                            .translate("settings.clipboard.page_hover_switch_description")
                            .into(),
                    ),
                    page_hover_switch,
                ),
            )
            .child(self.hover_delay_row(theme, HoverBar::Page, &clipboard, &translator))
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.clipboard.filter_hover_switch"),
                    Some(
                        translator
                            .translate("settings.clipboard.filter_hover_switch_description")
                            .into(),
                    ),
                    filter_hover_switch,
                ),
            )
            .child(self.hover_delay_row(theme, HoverBar::Filter, &clipboard, &translator))
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.clipboard.move_pasted_to_first_in_clipboard"),
                    Some(
                        translator
                            .translate(
                                "settings.clipboard.move_pasted_to_first_in_clipboard_description",
                            )
                            .into(),
                    ),
                    move_pasted_to_first_in_clipboard_switch,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.clipboard.move_pasted_to_first_in_favorites"),
                    Some(
                        translator
                            .translate(
                                "settings.clipboard.move_pasted_to_first_in_favorites_description",
                            )
                            .into(),
                    ),
                    move_pasted_to_first_in_favorites_switch,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.clipboard.auto_scroll_to_top"),
                    Some(
                        translator
                            .translate("settings.clipboard.auto_scroll_to_top_description")
                            .into(),
                    ),
                    auto_scroll_to_top_switch,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.clipboard.auto_focus_search"),
                    Some(
                        translator
                            .translate("settings.clipboard.auto_focus_search_description")
                            .into(),
                    ),
                    auto_focus_search_switch,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.clipboard.auto_clear_search"),
                    Some(
                        translator
                            .translate("settings.clipboard.auto_clear_search_description")
                            .into(),
                    ),
                    auto_clear_search_switch,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.clipboard.confirm_favorite_removal"),
                    Some(
                        translator
                            .translate("settings.clipboard.confirm_favorite_removal_description")
                            .into(),
                    ),
                    confirm_favorite_removal_switch,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.clipboard.save_images_as_files"),
                    Some(
                        translator
                            .translate("settings.clipboard.save_images_as_files_description")
                            .into(),
                    ),
                    save_images_as_files_switch,
                ),
            )
            .child(self.render_search_engines(&translator, &save_error, cx));
        if let Some(error) = &self.clipboard_error {
            section = section.child(settings_error_row(theme, error));
        }
        section.into_any_element()
    }

    /// 渲染“搜索引擎”管理区：已有引擎列表 + 新增表单。
    fn render_search_engines(
        &self,
        translator: &crate::i18n::Translator,
        save_error: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let view = cx.entity().clone();
        let engines = config_runtime::clipboard_config(cx).search_engines_or_default();
        let mut list = v_flex().w_full().min_w_0().flex_shrink_0();
        for (index, engine) in engines.iter().enumerate() {
            let remove_view = view.clone();
            let remove_error = save_error.to_owned();
            list = list.child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_2()
                    .when(index > 0, |row| row.border_t_1().border_color(theme.border))
                    .child(search_engine_icon(engine, theme))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_sm()
                            .text_color(theme.foreground)
                            .child(engine.name.clone()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(engine.url_template.clone()),
                    )
                    .child(
                        Button::new(("search-engine-remove", index as u64))
                            .ghost()
                            .small()
                            .label(translator.translate("settings.clipboard.remove_search_engine"))
                            .on_click(move |_, _, cx| {
                                persist_clipboard_change(
                                    remove_view.clone(),
                                    move |clipboard| {
                                        let mut engines = clipboard.search_engines_or_default();
                                        if index < engines.len() {
                                            engines.remove(index);
                                        }
                                        clipboard.search_engines = engines;
                                    },
                                    remove_error.clone(),
                                    cx,
                                );
                            }),
                    ),
            );
        }

        let name_input = self.search_engine_name_input.clone();
        let url_input = self.search_engine_url_input.clone();
        let add_view = view.clone();
        let add_error = save_error.to_owned();
        let add_button = Button::new("search-engine-add")
            .primary()
            .small()
            .label(translator.translate("settings.clipboard.add_search_engine"))
            .on_click(move |_, window, cx| {
                let name = name_input.read(cx).value().trim().to_owned();
                let url_template = url_input.read(cx).value().trim().to_owned();
                if name.is_empty() || url_template.is_empty() {
                    return;
                }
                let icon_path = add_view.read(cx).pending_search_engine_icon.clone();
                let engine = SearchEngine {
                    name,
                    url_template,
                    icon_path,
                };
                persist_clipboard_change(
                    add_view.clone(),
                    move |clipboard| {
                        let mut engines = clipboard.search_engines_or_default();
                        engines.push(engine);
                        clipboard.search_engines = engines;
                    },
                    add_error.clone(),
                    cx,
                );
                // 清空输入框，方便继续添加下一个。
                name_input.update(cx, |state, cx| {
                    state.set_value("", window, cx);
                });
                url_input.update(cx, |state, cx| {
                    state.set_value("", window, cx);
                });
            });

        // 整行可点击的卡片式标题行：图标 + 标题/描述 + 计数徽标 + chevron，
        // 展开后引擎列表与新增表单整体作为 Collapsible content 显隐。
        let expanded = self.search_engines_expanded;
        let toggle_view = view.clone();
        let chevron_name = if expanded {
            "chevron-up"
        } else {
            "chevron-down"
        };
        let engine_count = engines.len();
        let header = h_flex()
            .id("search-engines-header")
            .group("search-engines-header")
            .w_full()
            .min_w_0()
            .items_center()
            .gap_3()
            .px_4()
            .py_3()
            .cursor_pointer()
            .rounded(px(6.0))
            .hover(|row| row.bg(theme.accent.opacity(0.5)))
            .on_click(move |_, _, cx| {
                toggle_view.update(cx, |view, cx| {
                    view.search_engines_expanded = !view.search_engines_expanded;
                    cx.notify();
                });
            })
            .child(
                div()
                    .w(ui_rem(32.0))
                    .h(ui_rem(32.0))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.0))
                    .bg(theme.accent.opacity(0.6))
                    .child(hover_icon(
                        "search-engines-header",
                        "search",
                        16.0,
                        theme.accent_foreground,
                    )),
            )
            .child(
                v_flex()
                    .min_w_0()
                    .flex_1()
                    .gap_1()
                    .child(
                        div()
                            .min_w_0()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.foreground)
                            .child(translator.translate("settings.clipboard.search_engines")),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(
                                translator
                                    .translate("settings.clipboard.search_engines_description"),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .px_2()
                    .py_0p5()
                    .rounded_full()
                    .bg(theme.accent.opacity(0.6))
                    .text_xs()
                    .text_color(theme.accent_foreground)
                    .child(translator.translate_with_args(
                        "settings.clipboard.search_engine_count",
                        &[("count", engine_count.to_string())],
                    )),
            )
            .child(div().flex_shrink_0().child(hover_icon(
                "search-engines-header",
                chevron_name,
                14.0,
                theme.muted_foreground,
            )));

        let form_row = h_flex()
            .w_full()
            .min_w_0()
            .items_center()
            .gap_2()
            .px_4()
            .py_2()
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .w(ui_rem(140.0))
                    .flex_shrink_0()
                    .child(Input::new(&self.search_engine_name_input).small().w_full()),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .child(Input::new(&self.search_engine_url_input).small().w_full()),
            )
            .when_some(
                self.pending_search_engine_icon
                    .as_ref()
                    .filter(|p| !p.is_empty()),
                |row, icon_path| {
                    row.child(
                        div()
                            .w(ui_rem(20.0))
                            .h(ui_rem(20.0))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .overflow_hidden()
                            .rounded(px(4.0))
                            .border_1()
                            .border_color(theme.border)
                            .child(
                                img(std::path::PathBuf::from(icon_path))
                                    .w(ui_rem(14.0))
                                    .h(ui_rem(14.0))
                                    .flex_none()
                                    .object_fit(gpui::ObjectFit::Contain),
                            ),
                    )
                },
            )
            .child(
                Button::new("search-engine-pick-icon")
                    .ghost()
                    .small()
                    .label(translator.translate("settings.clipboard.choose_search_engine_icon"))
                    .on_click({
                        let icon_view = view.clone();
                        move |_, _, cx| choose_search_engine_icon(icon_view.clone(), cx)
                    }),
            )
            .child(add_button);

        Collapsible::new()
            .open(expanded)
            .w_full()
            .child(self.highlight_element(
                theme,
                translator.translate("settings.clipboard.search_engines"),
                header,
            ))
            .content(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .border_t_1()
                    .border_color(theme.border)
                    .bg(theme.title_bar.opacity(0.5))
                    .child(list)
                    .child(form_row),
            )
            .into_any_element()
    }

    fn hover_delay_row(
        &self,
        theme: gpui_component::ThemeColor,
        bar: HoverBar,
        clipboard: &ClipboardConfig,
        translator: &i18n::Translator,
    ) -> AnyElement {
        let (title_key, delay_ms, enabled, slider) = match bar {
            HoverBar::Page => (
                "settings.clipboard.page_hover_delay",
                clipboard.page_hover_delay_ms,
                clipboard.page_hover_switch,
                &self.page_hover_delay_slider,
            ),
            HoverBar::Filter => (
                "settings.clipboard.filter_hover_delay",
                clipboard.filter_hover_delay_ms,
                clipboard.filter_hover_switch,
                &self.filter_hover_delay_slider,
            ),
        };
        let title = translator.translate(title_key);
        let description = translator.translate("settings.clipboard.hover_delay_description");
        self.highlight_element(
            theme,
            title.clone(),
            h_flex()
                .w_full()
                .min_w_0()
                .min_h(ui_rem(36.0))
                .flex_shrink_0()
                .flex_wrap()
                .justify_between()
                .gap_4()
                .px_4()
                .py_2()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    h_flex()
                        .id(format!("settings-clipboard-hover-delay-{title}"))
                        .min_w(ui_rem(180.0))
                        .flex_1()
                        .pl_2()
                        .gap_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(title)
                        .child(div().opacity(0.7).child("0–1000 ms"))
                        .tooltip(move |window, cx| {
                            Tooltip::new(description.clone()).build(window, cx)
                        }),
                )
                .child(
                    h_flex()
                        .w(ui_rem(248.0))
                        .flex_shrink_0()
                        .gap_3()
                        .when(!enabled, |this| this.opacity(0.5))
                        .child(Slider::new(slider).disabled(!enabled).flex_1())
                        .child(
                            div()
                                .w(ui_rem(62.0))
                                .text_xs()
                                .text_right()
                                .text_color(theme.muted_foreground)
                                .child(format!("{delay_ms} ms")),
                        ),
                ),
        )
    }
}

fn persist_clipboard_change<F>(
    view: Entity<SettingsWindowView>,
    update: F,
    save_error: String,
    cx: &mut App,
) where
    F: FnOnce(&mut ClipboardConfig),
{
    let mut clipboard = config_runtime::clipboard_config(cx);
    update(&mut clipboard);
    let error_message = match config_runtime::set_clipboard_config(clipboard, cx) {
        Ok(_) => None,
        Err(error) => {
            tracing::error!("保存剪贴板设置失败：{error}");
            Some(save_error)
        }
    };
    view.update(cx, |view, cx| {
        view.clipboard_error = notify_settings_error(SettingsPage::Clipboard, error_message, cx);
        cx.notify();
    });
}

const fn clipboard_display_priority_label_key(priority: ClipboardDisplayPriority) -> &'static str {
    match priority {
        ClipboardDisplayPriority::TextHtmlImage => "settings.clipboard.priority.text_html_image",
        ClipboardDisplayPriority::TextImageHtml => "settings.clipboard.priority.text_image_html",
        ClipboardDisplayPriority::HtmlTextImage => "settings.clipboard.priority.html_text_image",
        ClipboardDisplayPriority::HtmlImageText => "settings.clipboard.priority.html_image_text",
        ClipboardDisplayPriority::ImageTextHtml => "settings.clipboard.priority.image_text_html",
        ClipboardDisplayPriority::ImageHtmlText => "settings.clipboard.priority.image_html_text",
    }
}

/// 返回搜索引擎行的图标元素。
fn search_engine_icon(engine: &SearchEngine, theme: gpui_component::ThemeColor) -> AnyElement {
    // 图标优先级：用户上传图标（本地文件）> 内置品牌图标 > 通用 search 图标。
    if let Some(path) = engine.icon_path.as_ref().filter(|p| !p.is_empty()) {
        return img(std::path::PathBuf::from(path))
            .size(ui_rem(14.0))
            .object_fit(gpui::ObjectFit::Contain)
            .into_any_element();
    }
    let name = engine.builtin_icon_name().unwrap_or("search");
    icon(name, 14.0, theme.muted_foreground).into_any_element()
}

/// 允许作为搜索引擎图标的图片扩展名，与 rfd 的 add_filter 保持一致。
const SEARCH_ENGINE_ICON_EXTENSIONS: &[&str] =
    &["png", "jpg", "jpeg", "svg", "ico", "webp", "bmp", "gif"];

/// 弹出文件选择框让用户为新引擎挑选图标，限定图片扩展名，暂存到 pending_search_engine_icon。
fn choose_search_engine_icon(view: Entity<SettingsWindowView>, cx: &mut App) {
    let dialog = rfd::AsyncFileDialog::new()
        .set_title("选择搜索引擎图标")
        .add_filter("图片文件", SEARCH_ENGINE_ICON_EXTENSIONS);
    cx.spawn(async move |cx| {
        let Some(file) = dialog.pick_file().await else {
            return;
        };
        let path = file.path().to_path_buf();
        let is_image = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| {
                SEARCH_ENGINE_ICON_EXTENSIONS
                    .iter()
                    .any(|allowed| allowed.eq_ignore_ascii_case(ext))
            })
            .unwrap_or(false);
        let path = path.to_string_lossy().into_owned();
        cx.update(|cx| {
            view.update(cx, |view, cx| {
                if is_image {
                    view.pending_search_engine_icon = Some(path);
                    view.clipboard_error = None;
                } else {
                    let message = crate::i18n::translator(cx)
                        .translate("settings.clipboard.invalid_search_engine_icon");
                    tracing::error!("搜索引擎图标类型不支持");
                    view.clipboard_error =
                        notify_settings_error(SettingsPage::Clipboard, Some(message), cx);
                }
                cx.notify();
            });
        });
    })
    .detach();
}

const fn clipboard_preview_priority_label_key(priority: ClipboardPreviewPriority) -> &'static str {
    match priority {
        ClipboardPreviewPriority::FollowDisplay => {
            "settings.clipboard.preview_priority.follow_display"
        }
        ClipboardPreviewPriority::TextHtmlImage => "settings.clipboard.priority.text_html_image",
        ClipboardPreviewPriority::TextImageHtml => "settings.clipboard.priority.text_image_html",
        ClipboardPreviewPriority::HtmlTextImage => "settings.clipboard.priority.html_text_image",
        ClipboardPreviewPriority::HtmlImageText => "settings.clipboard.priority.html_image_text",
        ClipboardPreviewPriority::ImageTextHtml => "settings.clipboard.priority.image_text_html",
        ClipboardPreviewPriority::ImageHtmlText => "settings.clipboard.priority.image_html_text",
    }
}

const fn clipboard_mouse_left_behavior_label_key(
    behavior: ClipboardMouseLeftBehavior,
) -> &'static str {
    match behavior {
        ClipboardMouseLeftBehavior::SinglePaste => {
            "settings.clipboard.mouse_left_behavior.single_paste"
        }
        ClipboardMouseLeftBehavior::SinglePlainTextPaste => {
            "settings.clipboard.mouse_left_behavior.single_plain_text_paste"
        }
        ClipboardMouseLeftBehavior::SingleCopy => {
            "settings.clipboard.mouse_left_behavior.single_copy"
        }
        ClipboardMouseLeftBehavior::DoublePaste => {
            "settings.clipboard.mouse_left_behavior.double_paste"
        }
        ClipboardMouseLeftBehavior::DoublePlainTextPaste => {
            "settings.clipboard.mouse_left_behavior.double_plain_text_paste"
        }
        ClipboardMouseLeftBehavior::DoubleCopy => {
            "settings.clipboard.mouse_left_behavior.double_copy"
        }
    }
}

const fn clipboard_mouse_middle_behavior_label_key(
    behavior: ClipboardMouseMiddleBehavior,
) -> &'static str {
    match behavior {
        ClipboardMouseMiddleBehavior::PlainTextPaste => {
            "settings.clipboard.mouse_middle_behavior.plain_text_paste"
        }
        ClipboardMouseMiddleBehavior::SelectItem => {
            "settings.clipboard.mouse_middle_behavior.select_item"
        }
        ClipboardMouseMiddleBehavior::PinItem => {
            "settings.clipboard.mouse_middle_behavior.pin_item"
        }
        ClipboardMouseMiddleBehavior::FavoriteItem => {
            "settings.clipboard.mouse_middle_behavior.favorite_item"
        }
    }
}

const fn clipboard_window_position_label_key(position: ClipboardWindowPosition) -> &'static str {
    match position {
        ClipboardWindowPosition::MouseCenter => "settings.clipboard.window_position.mouse_center",
        ClipboardWindowPosition::MouseNearby => "settings.clipboard.window_position.mouse_nearby",
        ClipboardWindowPosition::Remember => "settings.clipboard.window_position.remember",
        ClipboardWindowPosition::ScreenCenter => "settings.clipboard.window_position.screen_center",
        ClipboardWindowPosition::ScreenLeftCenter => {
            "settings.clipboard.window_position.screen_left_center"
        }
        ClipboardWindowPosition::ScreenRightCenter => {
            "settings.clipboard.window_position.screen_right_center"
        }
        ClipboardWindowPosition::ScreenTopCenter => {
            "settings.clipboard.window_position.screen_top_center"
        }
        ClipboardWindowPosition::ScreenBottomCenter => {
            "settings.clipboard.window_position.screen_bottom_center"
        }
    }
}
