use crate::ui::scale::ui_rem;

use super::super::{SettingsPage, notify_settings_error};
use gpui::prelude::*;
use gpui::{AnyElement, App, ColorExt as _, Context, Entity, div, img, px};

use super::super::{
    SettingsWindowView,
    components::{
        settings_dropdown, settings_error_row, settings_page_header, settings_section_title,
    },
};
use crate::ui::{hover_icon, icon};
use crate::{
    config::{
        ClipboardConfig, ClipboardListLayout, ClipboardMouseBehavior, ClipboardWindowPosition,
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
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, collapsible::Collapsible, h_flex, v_flex,
};

#[derive(Clone, Copy)]
enum HoverBar {
    Page,
    Filter,
}

impl SettingsWindowView {
    pub(in crate::windows::settings) fn render_main_window(
        &self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let translator = i18n::translator(cx);
        let clipboard = config_runtime::clipboard_config(cx);
        let view = cx.entity().clone();
        let reset_error = translator.translate("settings.main_window.reset_error");
        let reset_view = view.clone();
        let reset_page_hover_delay_slider = self.page_hover_delay_slider.clone();
        let reset_filter_hover_delay_slider = self.filter_hover_delay_slider.clone();
        let reset_button = Button::new("settings-main-window-reset")
            .warning()
            .small()
            .label(translator.translate("settings.action.reset_page"))
            .on_click(move |_, window, cx| {
                let mut clipboard = config_runtime::clipboard_config(cx);
                reset_main_window_fields(&mut clipboard);
                let result = config_runtime::set_clipboard_config(clipboard, cx);
                let error_message = result.as_ref().err().map(|error| {
                    tracing::error!("重置主窗口设置失败：{error}");
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
                        notify_settings_error(SettingsPage::MainWindow, error_message, cx);
                    cx.notify();
                });
            });

        let save_error = translator.translate("settings.clipboard.save_error");
        let mouse_left_behavior_items = ClipboardMouseBehavior::ALL
            .into_iter()
            .map(|behavior| {
                DropdownItem::option(
                    DropdownItemId::new(behavior.code()),
                    translator.translate(clipboard_mouse_behavior_label_key(behavior)),
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
                    let Some(behavior) = ClipboardMouseBehavior::ALL
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
        let mouse_middle_behavior_items = ClipboardMouseBehavior::ALL
            .into_iter()
            .map(|behavior| {
                DropdownItem::option(
                    DropdownItemId::new(behavior.code()),
                    translator.translate(clipboard_mouse_behavior_label_key(behavior)),
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
                    let Some(behavior) = ClipboardMouseBehavior::ALL
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
        let list_layout_selector = settings_dropdown(
            "settings-clipboard-list-layout-select",
            clipboard.effective_list_layout().code(),
            ClipboardListLayout::ALL
                .into_iter()
                .map(|layout| {
                    DropdownItem::option(
                        DropdownItemId::new(layout.code()),
                        translator.translate(layout.label_key()),
                    )
                    .selected(layout == clipboard.effective_list_layout())
                })
                .collect(),
            {
                let view = view.clone();
                let save_error = save_error.clone();
                move |action_id, cx| {
                    if let Some(layout) = ClipboardListLayout::ALL
                        .into_iter()
                        .find(|layout| layout.code() == action_id.as_str())
                    {
                        persist_clipboard_change(
                            view.clone(),
                            move |clipboard| clipboard.set_list_layout(layout),
                            save_error.clone(),
                            cx,
                        );
                    }
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
        let edge_hide_switch = Switch::new("settings-clipboard-edge-hide")
            .large()
            .disabled(clipboard.window_position.is_screen_edge())
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
            .disabled(clipboard.window_position.is_screen_edge())
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
                translator.translate("settings.page.main_window"),
                translator.translate("settings.main_window.description"),
                reset_button,
            ))
            .child(settings_section_title(
                theme,
                translator.translate("settings.section.window_behavior"),
            ))
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
                    translator.translate("settings.clipboard.list_layout"),
                    Some(
                        translator
                            .translate("settings.clipboard.list_layout.description")
                            .into(),
                    ),
                    list_layout_selector,
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
            .child(settings_section_title(
                theme,
                translator.translate("settings.section.mouse_and_actions"),
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
                    translator.translate("settings.clipboard.confirm_favorite_removal"),
                    Some(
                        translator
                            .translate("settings.clipboard.confirm_favorite_removal_description")
                            .into(),
                    ),
                    confirm_favorite_removal_switch,
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

pub(super) fn persist_clipboard_change<F>(
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
        view.clipboard_error = notify_settings_error(view.active_page, error_message, cx);
        cx.notify();
    });
}

/// 把“主窗口”页展示的字段恢复为默认值，其余剪贴板字段保持不变。
fn reset_main_window_fields(clipboard: &mut ClipboardConfig) {
    let default = ClipboardConfig::default();
    clipboard.window_position = default.window_position;
    clipboard.list_layout = default.list_layout;
    clipboard.edge_list_layouts = default.edge_list_layouts;
    clipboard.edge_hide = default.edge_hide;
    clipboard.edge_hide_show_edge = default.edge_hide_show_edge;
    clipboard.auto_scroll_to_top = default.auto_scroll_to_top;
    clipboard.auto_focus_search = default.auto_focus_search;
    clipboard.auto_clear_search = default.auto_clear_search;
    clipboard.mouse_left_behavior = default.mouse_left_behavior;
    clipboard.mouse_middle_behavior = default.mouse_middle_behavior;
    clipboard.page_hover_switch = default.page_hover_switch;
    clipboard.filter_hover_switch = default.filter_hover_switch;
    clipboard.page_hover_delay_ms = default.page_hover_delay_ms;
    clipboard.filter_hover_delay_ms = default.filter_hover_delay_ms;
    clipboard.move_pasted_to_first_in_clipboard = default.move_pasted_to_first_in_clipboard;
    clipboard.move_pasted_to_first_in_favorites = default.move_pasted_to_first_in_favorites;
    clipboard.confirm_favorite_removal = default.confirm_favorite_removal;
    clipboard.search_engines = default.search_engines;
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
                        notify_settings_error(SettingsPage::MainWindow, Some(message), cx);
                }
                cx.notify();
            });
        });
    })
    .detach();
}

const fn clipboard_mouse_behavior_label_key(behavior: ClipboardMouseBehavior) -> &'static str {
    match behavior {
        ClipboardMouseBehavior::SinglePaste => "settings.clipboard.mouse_behavior.single_paste",
        ClipboardMouseBehavior::SinglePlainTextPaste => {
            "settings.clipboard.mouse_behavior.single_plain_text_paste"
        }
        ClipboardMouseBehavior::SingleCopy => "settings.clipboard.mouse_behavior.single_copy",
        ClipboardMouseBehavior::SingleSelectItem => {
            "settings.clipboard.mouse_behavior.single_select_item"
        }
        ClipboardMouseBehavior::SinglePinItem => {
            "settings.clipboard.mouse_behavior.single_pin_item"
        }
        ClipboardMouseBehavior::SingleFavoriteItem => {
            "settings.clipboard.mouse_behavior.single_favorite_item"
        }
        ClipboardMouseBehavior::DoublePaste => "settings.clipboard.mouse_behavior.double_paste",
        ClipboardMouseBehavior::DoublePlainTextPaste => {
            "settings.clipboard.mouse_behavior.double_plain_text_paste"
        }
        ClipboardMouseBehavior::DoubleCopy => "settings.clipboard.mouse_behavior.double_copy",
        ClipboardMouseBehavior::DoubleSelectItem => {
            "settings.clipboard.mouse_behavior.double_select_item"
        }
        ClipboardMouseBehavior::DoublePinItem => {
            "settings.clipboard.mouse_behavior.double_pin_item"
        }
        ClipboardMouseBehavior::DoubleFavoriteItem => {
            "settings.clipboard.mouse_behavior.double_favorite_item"
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
