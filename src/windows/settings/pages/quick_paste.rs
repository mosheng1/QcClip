use super::super::{SettingsPage, notify_settings_error};
use gpui::prelude::*;
use gpui::{AnyElement, App, Context, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme as _, Sizable as _, v_flex};

use super::super::{
    SettingsWindowView,
    components::{
        settings_dropdown, settings_error_row, settings_page_header, settings_section_title,
    },
};
use crate::{
    config::{QuickPasteConfig, QuickPasteMode},
    config_runtime, i18n,
    ui::{SingleSelectOption, single_select},
    windows::menu::{DropdownItem, DropdownItemId},
};

impl SettingsWindowView {
    pub(in crate::windows::settings) fn render_quick_paste(
        &self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let translator = i18n::translator(cx);
        let config = config_runtime::quick_paste_config(cx);
        let single_select_animation = config_runtime::animation_enabled(
            cx,
            crate::config::AnimationKind::SingleSelectIndicator,
        );
        let view = cx.entity().clone();
        let reset_error = translator.translate("settings.quick_paste.save_error");
        let reset_view = view.clone();
        let reset_button = Button::new("settings-quick-paste-reset")
            .warning()
            .small()
            .label(translator.translate("settings.action.reset_page"))
            .on_click(move |_, _, cx| {
                let result = config_runtime::reset_quick_paste_config(cx);
                let error = result.as_ref().err().map(|error| {
                    tracing::error!("重置便捷粘贴设置失败：{error}");
                    reset_error.clone()
                });
                reset_view.update(cx, |view, cx| {
                    view.quick_paste_error =
                        notify_settings_error(SettingsPage::QuickPaste, error, cx);
                    cx.notify();
                });
            });
        let items = QuickPasteConfig::VISIBLE_ITEM_COUNTS
            .into_iter()
            .map(|count| {
                let count_text = count.to_string();
                DropdownItem::option(
                    DropdownItemId::new(count_text.clone()),
                    translator.translate_with_args(
                        "settings.quick_paste.visible_items_option",
                        &[("count", count_text)],
                    ),
                )
                .selected(count == config.visible_items)
            })
            .collect::<Vec<_>>();
        let save_error = translator.translate("settings.quick_paste.save_error");
        let mode_save_error = save_error.clone();
        let mode_selector = single_select(
            "settings-quick-paste-mode-select",
            QuickPasteMode::ALL.into_iter().map(|mode| {
                SingleSelectOption::new(
                    mode,
                    translator.translate(match mode {
                        QuickPasteMode::ReleaseToPaste => "settings.quick_paste.mode.release",
                        QuickPasteMode::HoldToScroll => "settings.quick_paste.mode.hold",
                    }),
                )
            }),
            config.mode,
            {
                let view = view.clone();
                move |mode, cx| {
                    persist_quick_paste_toggle_change(
                        view.clone(),
                        move |config| config.mode = mode,
                        mode_save_error.clone(),
                        cx,
                    );
                }
            },
            cx.theme().colors,
            single_select_animation,
        );
        let visible_items_save_error = save_error.clone();
        let selector = settings_dropdown(
            "settings-quick-paste-visible-items",
            config.visible_items.to_string(),
            items,
            {
                let view = view.clone();
                move |action_id, cx| {
                    let Ok(visible_items) = action_id.as_str().parse::<u8>() else {
                        return;
                    };
                    if !QuickPasteConfig::VISIBLE_ITEM_COUNTS.contains(&visible_items) {
                        return;
                    }
                    persist_quick_paste_change(
                        view.clone(),
                        visible_items,
                        visible_items_save_error.clone(),
                        cx,
                    );
                }
            },
            cx,
        );
        let window_width_items = QuickPasteConfig::WINDOW_WIDTHS
            .into_iter()
            .map(|width| {
                let width_text = width.to_string();
                DropdownItem::option(
                    DropdownItemId::new(width_text.clone()),
                    translator.translate_with_args(
                        "settings.quick_paste.window_width_option",
                        &[("width", width_text)],
                    ),
                )
                .selected(width == config.window_width)
            })
            .collect::<Vec<_>>();
        let window_width_selector = settings_dropdown(
            "settings-quick-paste-window-width",
            config.window_width.to_string(),
            window_width_items,
            {
                let view = view.clone();
                let window_width_save_error = save_error.clone();
                move |action_id, cx| {
                    let Ok(window_width) = action_id.as_str().parse::<u16>() else {
                        return;
                    };
                    if !QuickPasteConfig::WINDOW_WIDTHS.contains(&window_width) {
                        return;
                    }
                    persist_quick_paste_width_change(
                        view.clone(),
                        window_width,
                        window_width_save_error.clone(),
                        cx,
                    );
                }
            },
            cx,
        );
        let select_with_up_down_switch = Switch::new("settings-quick-paste-select-with-up-down")
            .large()
            .checked(config.select_with_up_down)
            .accessibility_label(translator.translate("settings.quick_paste.select_with_up_down"))
            .on_click({
                let view = view.clone();
                let save_error = save_error.clone();
                move |checked, _, cx| {
                    persist_quick_paste_toggle_change(
                        view.clone(),
                        move |config| config.select_with_up_down = *checked,
                        save_error.clone(),
                        cx,
                    );
                }
            });
        let keep_last_position_save_error = save_error.clone();
        let switch_list_with_left_right_switch =
            Switch::new("settings-quick-paste-switch-list-with-left-right")
                .large()
                .checked(config.switch_list_with_left_right)
                .accessibility_label(
                    translator.translate("settings.quick_paste.switch_list_with_left_right"),
                )
                .on_click({
                    let view = view.clone();
                    move |checked, _, cx| {
                        persist_quick_paste_toggle_change(
                            view.clone(),
                            move |config| config.switch_list_with_left_right = *checked,
                            save_error.clone(),
                            cx,
                        );
                    }
                });

        let keep_last_position_switch = Switch::new("settings-quick-paste-keep-last-position")
            .large()
            .checked(config.keep_last_position)
            .accessibility_label(translator.translate("settings.quick_paste.keep_last_position"))
            .on_click({
                let view = view.clone();
                move |checked, _, cx| {
                    persist_quick_paste_toggle_change(
                        view.clone(),
                        move |config| config.keep_last_position = *checked,
                        keep_last_position_save_error.clone(),
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
                translator.translate("settings.page.quick_paste"),
                translator.translate("settings.quick_paste.description"),
                reset_button,
            ))
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.quick_paste.mode"),
                    Some(
                        translator
                            .translate(match config.mode {
                                QuickPasteMode::ReleaseToPaste => {
                                    "settings.quick_paste.mode.release_description"
                                }
                                QuickPasteMode::HoldToScroll => {
                                    "settings.quick_paste.mode.hold_description"
                                }
                            })
                            .into(),
                    ),
                    mode_selector,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.quick_paste.visible_items"),
                    Some(
                        translator
                            .translate("settings.quick_paste.visible_items_description")
                            .into(),
                    ),
                    selector,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.quick_paste.window_width"),
                    Some(
                        translator
                            .translate("settings.quick_paste.window_width_description")
                            .into(),
                    ),
                    window_width_selector,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.quick_paste.select_with_up_down"),
                    Some(
                        translator
                            .translate("settings.quick_paste.select_with_up_down_description")
                            .into(),
                    ),
                    select_with_up_down_switch,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.quick_paste.switch_list_with_left_right"),
                    Some(
                        translator
                            .translate(
                                "settings.quick_paste.switch_list_with_left_right_description",
                            )
                            .into(),
                    ),
                    switch_list_with_left_right_switch,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.quick_paste.keep_last_position"),
                    Some(
                        translator
                            .translate("settings.quick_paste.keep_last_position_description")
                            .into(),
                    ),
                    keep_last_position_switch,
                ),
            )
            .child(settings_section_title(
                theme,
                translator.translate("settings.section.related_settings"),
            ))
            .child(self.page_link_row(
                theme,
                SettingsPage::Shortcuts,
                translator.translate("settings.quick_paste.shortcuts_link"),
                cx,
            ))
            .child(self.page_link_row(
                theme,
                SettingsPage::Notifications,
                translator.translate("settings.quick_paste.sound_link"),
                cx,
            ));
        if let Some(error) = &self.quick_paste_error {
            section = section.child(settings_error_row(theme, error));
        }
        section.into_any_element()
    }
}

fn persist_quick_paste_change(
    view: gpui::Entity<SettingsWindowView>,
    visible_items: u8,
    save_error: String,
    cx: &mut App,
) {
    let mut config = config_runtime::quick_paste_config(cx);
    config.visible_items = visible_items;
    let error = config_runtime::set_quick_paste_config(config, cx)
        .err()
        .map(|error| {
            tracing::error!("保存便捷粘贴设置失败：{error}");
            save_error
        });
    view.update(cx, |view, cx| {
        view.quick_paste_error = notify_settings_error(SettingsPage::QuickPaste, error, cx);
        cx.notify();
    });
}

fn persist_quick_paste_width_change(
    view: gpui::Entity<SettingsWindowView>,
    window_width: u16,
    save_error: String,
    cx: &mut App,
) {
    let mut config = config_runtime::quick_paste_config(cx);
    config.window_width = window_width;
    let error = config_runtime::set_quick_paste_config(config, cx)
        .err()
        .map(|error| {
            tracing::error!("保存便捷粘贴设置失败：{error}");
            save_error
        });
    view.update(cx, |view, cx| {
        view.quick_paste_error = notify_settings_error(SettingsPage::QuickPaste, error, cx);
        cx.notify();
    });
}

fn persist_quick_paste_toggle_change(
    view: gpui::Entity<SettingsWindowView>,
    update: impl FnOnce(&mut QuickPasteConfig),
    save_error: String,
    cx: &mut App,
) {
    let mut config = config_runtime::quick_paste_config(cx);
    update(&mut config);
    let error = config_runtime::set_quick_paste_config(config, cx)
        .err()
        .map(|error| {
            tracing::error!("保存便捷粘贴设置失败：{error}");
            save_error
        });
    view.update(cx, |view, cx| {
        view.quick_paste_error = notify_settings_error(SettingsPage::QuickPaste, error, cx);
        cx.notify();
    });
}
