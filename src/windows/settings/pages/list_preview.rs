use crate::ui::scale::ui_rem;

use super::super::{SETTINGS_LINE_HEIGHT_SELECTOR_WIDTH, SettingsPage, notify_settings_error};
use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, Window, div, px};

use super::super::{
    SettingsWindowView,
    components::{
        clickable_setting_row, compact_clickable_setting_row, compact_setting_row,
        settings_dropdown, settings_error_row, settings_page_header, settings_section_title,
    },
};
use super::main_window::persist_clipboard_change;
use super::personalization::{
    list_item_height_label_key, list_item_style_label_key, persist_appearance_change,
};
use crate::{
    config::{
        AppearanceConfig, CardItemGap, ClipboardConfig, ClipboardDisplayPriority,
        ClipboardPreviewPriority, ClipboardPreviewType, ListItemHeight, ListItemStyle,
        ListTimeFormat, PreviewInformationField,
    },
    config_runtime, i18n,
    ui::{SingleSelectOption, single_select, single_select_grid},
    windows::menu::{DropdownItem, DropdownItemId},
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::slider::Slider;
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};

impl SettingsWindowView {
    pub(in crate::windows::settings) fn render_list_preview(
        &self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let translator = i18n::translator(cx);
        let clipboard = config_runtime::clipboard_config(cx);
        let appearance = config_runtime::appearance_config(cx);
        let single_select_animation = config_runtime::animation_enabled(
            cx,
            crate::config::AnimationKind::SingleSelectIndicator,
        );
        let view = cx.entity().clone();
        let clipboard_save_error = translator.translate("settings.clipboard.save_error");
        let appearance_save_error = translator.translate("settings.personalization.save_error");

        let reset_error = translator.translate("settings.list_preview.reset_error");
        let reset_view = view.clone();
        let reset_hover_preview_delay_slider = self.hover_preview_delay_slider.clone();
        let reset_button = Button::new("settings-list-preview-reset")
            .warning()
            .small()
            .label(translator.translate("settings.action.reset_page"))
            .on_click(move |_, window, cx| {
                // 本页字段分属剪贴板与外观两份配置，分别只恢复本页展示的部分。
                let mut clipboard = config_runtime::clipboard_config(cx);
                reset_list_preview_clipboard_fields(&mut clipboard);
                let mut appearance = config_runtime::appearance_config(cx);
                reset_list_preview_appearance_fields(&mut appearance);
                let result = config_runtime::set_clipboard_config(clipboard, cx)
                    .and_then(|_| config_runtime::set_appearance_config(appearance, cx));
                let error_message = result.as_ref().err().map(|error| {
                    tracing::error!("重置列表与预览设置失败：{error}");
                    reset_error.clone()
                });
                if result.is_ok() {
                    let default_delay = f32::from(ClipboardConfig::HOVER_PREVIEW_DELAY_DEFAULT_MS);
                    reset_hover_preview_delay_slider.update(cx, |slider, cx| {
                        slider.set_value(default_delay, window, cx);
                    });
                }
                reset_view.update(cx, |view, cx| {
                    view.appearance_error =
                        notify_settings_error(SettingsPage::ListPreview, error_message, cx);
                    cx.notify();
                });
            });

        let list_style_selector = single_select(
            "settings-list-item-style-select",
            ListItemStyle::ALL
                .into_iter()
                .map(|style| {
                    SingleSelectOption::new(
                        style,
                        translator.translate(list_item_style_label_key(style)),
                    )
                })
                .collect::<Vec<_>>(),
            appearance.list_item_style,
            {
                let view = view.clone();
                let save_error = appearance_save_error.clone();
                move |style, cx| {
                    persist_appearance_change(
                        view.clone(),
                        "列表项风格",
                        move |appearance| appearance.list_item_style = style,
                        save_error.clone(),
                        cx,
                    );
                }
            },
            theme,
            single_select_animation,
        );
        let card_gap_selector = single_select_grid(
            "settings-card-item-gap-select",
            CardItemGap::ALL
                .into_iter()
                .map(|gap| SingleSelectOption::new(gap, gap.code()))
                .collect::<Vec<_>>(),
            appearance.card_item_gap,
            3,
            {
                let view = view.clone();
                let save_error = appearance_save_error.clone();
                move |gap, cx| {
                    persist_appearance_change(
                        view.clone(),
                        "卡片间距",
                        move |appearance| appearance.card_item_gap = gap,
                        save_error.clone(),
                        cx,
                    );
                }
            },
            theme,
            single_select_animation,
        );
        let list_item_height_selector = single_select(
            "settings-list-item-height-select",
            ListItemHeight::ALL.into_iter().map(|height| {
                SingleSelectOption::new(
                    height,
                    translator.translate(list_item_height_label_key(height)),
                )
            }),
            appearance.list_item_height,
            {
                let view = view.clone();
                let save_error = appearance_save_error.clone();
                move |height, cx| {
                    persist_appearance_change(
                        view.clone(),
                        "列表项行高",
                        move |appearance| appearance.list_item_height = height,
                        save_error.clone(),
                        cx,
                    );
                }
            },
            theme,
            single_select_animation,
        );
        let list_time_format_selector = single_select(
            "settings-list-time-format-select",
            ListTimeFormat::ALL.into_iter().map(|format| {
                SingleSelectOption::new(
                    format,
                    translator.translate(match format {
                        ListTimeFormat::Absolute => "settings.list_preview.time_format.absolute",
                        ListTimeFormat::Relative => "settings.list_preview.time_format.relative",
                    }),
                )
            }),
            appearance.list_time_format,
            {
                let view = view.clone();
                let save_error = appearance_save_error.clone();
                move |format, cx| {
                    persist_appearance_change(
                        view.clone(),
                        "列表时间显示",
                        move |appearance| appearance.list_time_format = format,
                        save_error.clone(),
                        cx,
                    );
                }
            },
            theme,
            single_select_animation,
        );
        let adaptive_max_lines_selector = h_flex()
            .w(ui_rem(SETTINGS_LINE_HEIGHT_SELECTOR_WIDTH))
            .gap_3()
            .child(Slider::new(&self.adaptive_max_lines_slider).flex_1())
            .child(
                div()
                    .w(ui_rem(36.0))
                    .text_sm()
                    .text_center()
                    .text_color(theme.foreground)
                    .child(appearance.adaptive_max_lines.to_string()),
            );
        let only_show_notes_switch = Switch::new("settings-only-show-notes")
            .checked(appearance.only_show_notes)
            .accessibility_label(translator.translate("settings.personalization.only_show_notes"))
            .on_click({
                let view = view.clone();
                let save_error = appearance_save_error.clone();
                move |checked, _, cx| {
                    persist_appearance_change(
                        view.clone(),
                        "仅显示备注",
                        move |appearance| appearance.only_show_notes = *checked,
                        save_error.clone(),
                        cx,
                    )
                }
            });
        let show_tag_names_switch = Switch::new("settings-show-tag-names")
            .checked(appearance.show_tag_names)
            .accessibility_label(translator.translate("settings.list_preview.show_tag_names"))
            .on_click({
                let view = view.clone();
                let save_error = appearance_save_error.clone();
                move |checked, _, cx| {
                    persist_appearance_change(
                        view.clone(),
                        "完整显示标签文本",
                        move |appearance| appearance.show_tag_names = *checked,
                        save_error.clone(),
                        cx,
                    );
                }
            });
        let gallery_gif_autoplay_switch = Switch::new("settings-gallery-gif-autoplay")
            .checked(appearance.gallery_gif_autoplay)
            .accessibility_label(
                translator.translate("settings.personalization.gallery_gif_autoplay"),
            )
            .on_click({
                let view = view.clone();
                let save_error = appearance_save_error.clone();
                move |checked, _, cx| {
                    persist_appearance_change(
                        view.clone(),
                        "图库 GIF 自动播放",
                        move |appearance| appearance.gallery_gif_autoplay = *checked,
                        save_error.clone(),
                        cx,
                    )
                }
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
        let priority_selector = settings_dropdown(
            "settings-clipboard-display-priority-select",
            clipboard.display_priority.code(),
            priority_items,
            {
                let view = view.clone();
                let save_error = clipboard_save_error.clone();
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
                let save_error = clipboard_save_error.clone();
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
        let hover_preview_switch = Switch::new("settings-clipboard-hover-preview")
            .large()
            .checked(clipboard.hover_preview_enabled)
            .accessibility_label(translator.translate("settings.clipboard.hover_preview"))
            .on_click({
                let view = view.clone();
                let save_error = clipboard_save_error.clone();
                move |checked, _, cx| {
                    persist_clipboard_change(
                        view.clone(),
                        move |clipboard| clipboard.hover_preview_enabled = *checked,
                        save_error.clone(),
                        cx,
                    );
                }
            });
        let mut preview_scale_presets = crate::config::UI_SCALE_PRESETS.to_vec();
        if !preview_scale_presets.contains(&appearance.preview_scale_percent) {
            preview_scale_presets.push(appearance.preview_scale_percent);
            preview_scale_presets.sort_unstable();
        }
        let preview_scale_items = preview_scale_presets
            .into_iter()
            .map(|percent| {
                DropdownItem::option(
                    DropdownItemId::new(percent.to_string()),
                    format!("{percent}%"),
                )
                .selected(percent == appearance.preview_scale_percent)
            })
            .collect::<Vec<_>>();
        let preview_scale_selector = settings_dropdown(
            "settings-preview-scale-select",
            appearance.preview_scale_percent.to_string(),
            preview_scale_items,
            {
                let view = view.clone();
                let save_error = appearance_save_error.clone();
                move |id, cx| {
                    let Ok(percent) = id.as_str().parse::<u16>() else {
                        return;
                    };
                    persist_appearance_change(
                        view.clone(),
                        "预览窗口缩放",
                        move |appearance| appearance.preview_scale_percent = percent,
                        save_error.clone(),
                        cx,
                    );
                }
            },
            cx,
        );
        let hover_preview_delay_selector = h_flex()
            .w(ui_rem(SETTINGS_LINE_HEIGHT_SELECTOR_WIDTH))
            .gap_3()
            .when(!clipboard.hover_preview_enabled, |this| this.opacity(0.5))
            .child(
                Slider::new(&self.hover_preview_delay_slider)
                    .disabled(!clipboard.hover_preview_enabled)
                    .flex_1(),
            )
            .child(
                div()
                    .w(ui_rem(62.0))
                    .text_sm()
                    .text_right()
                    .text_color(theme.foreground)
                    .child(format!("{} ms", clipboard.hover_preview_delay_ms)),
            );
        let heat_scrollbar_switch = Switch::new("settings-clipboard-heat-scrollbar")
            .large()
            .checked(clipboard.heat_scrollbar_enabled)
            .accessibility_label(translator.translate("settings.clipboard.heat_scrollbar"))
            .on_click({
                let view = view.clone();
                let save_error = clipboard_save_error.clone();
                move |checked, _, cx| {
                    persist_clipboard_change(
                        view.clone(),
                        move |clipboard| clipboard.heat_scrollbar_enabled = *checked,
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
                translator.translate("settings.page.list_preview"),
                translator.translate("settings.list_preview.description"),
                reset_button,
            ))
            .child(settings_section_title(
                theme,
                translator.translate("settings.section.list"),
            ))
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.personalization.list_item_style"),
                    Some(
                        translator
                            .translate("settings.personalization.list_item_style_description")
                            .into(),
                    ),
                    list_style_selector,
                ),
            );
        if appearance.list_item_style == ListItemStyle::Card {
            section = section.child(
                self.setting_row(
                    theme,
                    translator.translate("settings.personalization.card_item_gap"),
                    Some(
                        translator
                            .translate("settings.personalization.card_item_gap_description")
                            .into(),
                    ),
                    card_gap_selector,
                ),
            );
        }
        section = section.child(
            self.setting_row(
                theme,
                translator.translate("settings.personalization.list_item_height"),
                Some(
                    translator
                        .translate("settings.personalization.list_item_height_description")
                        .into(),
                ),
                list_item_height_selector,
            ),
        );
        if appearance.list_item_height == ListItemHeight::Adaptive {
            section = section.child(
                self.setting_row(
                    theme,
                    translator.translate("settings.personalization.adaptive_max_lines"),
                    Some(
                        translator
                            .translate("settings.personalization.adaptive_max_lines_description")
                            .into(),
                    ),
                    adaptive_max_lines_selector,
                ),
            );
        }
        section = section
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.personalization.only_show_notes"),
                    Some(
                        translator
                            .translate("settings.personalization.only_show_notes_description")
                            .into(),
                    ),
                    only_show_notes_switch,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.list_preview.time_format"),
                    Some(
                        translator
                            .translate("settings.list_preview.time_format_description")
                            .into(),
                    ),
                    list_time_format_selector,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.list_preview.show_tag_names"),
                    Some(
                        translator
                            .translate("settings.list_preview.show_tag_names_description")
                            .into(),
                    ),
                    show_tag_names_switch,
                ),
            )
            .child(self.highlight_element(
                theme,
                translator.translate("settings.item_display.title"),
                compact_clickable_setting_row(
                    "settings-item-display",
                    theme,
                    translator.translate("settings.item_display.title"),
                    {
                        let view = view.clone();
                        move |_, window, cx| {
                            super::item_display_editor::open_item_display_sheet(
                                view.clone(),
                                window,
                                cx,
                            )
                        }
                    },
                ),
            ))
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
                    translator.translate("settings.clipboard.heat_scrollbar"),
                    Some(
                        translator
                            .translate("settings.clipboard.heat_scrollbar_description")
                            .into(),
                    ),
                    heat_scrollbar_switch,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.personalization.gallery_gif_autoplay"),
                    Some(
                        translator
                            .translate("settings.personalization.gallery_gif_autoplay_description")
                            .into(),
                    ),
                    gallery_gif_autoplay_switch,
                ),
            )
            .child(settings_section_title(
                theme,
                translator.translate("settings.section.hover_preview"),
            ))
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
                    translator.translate("settings.clipboard.hover_preview_delay"),
                    Some(
                        translator
                            .translate("settings.clipboard.hover_preview_delay_description")
                            .into(),
                    ),
                    hover_preview_delay_selector,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.personalization.preview_scale"),
                    Some(
                        translator
                            .translate("settings.personalization.preview_scale_description")
                            .into(),
                    ),
                    preview_scale_selector,
                ),
            )
            .child(
                self.highlight_element(
                    theme,
                    translator.translate("settings.clipboard.preview_types"),
                    clickable_setting_row(
                        "settings-clipboard-preview-types",
                        theme,
                        translator.translate("settings.clipboard.preview_types"),
                        Some(
                            translator
                                .translate("settings.clipboard.preview_types_description")
                                .into(),
                        ),
                        {
                            let view = view.clone();
                            move |_, window, cx| open_preview_types_sheet(view.clone(), window, cx)
                        },
                    ),
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
            .child(self.highlight_element(
                theme,
                translator.translate("settings.personalization.preview_information"),
                compact_clickable_setting_row(
                    "settings-preview-information",
                    theme,
                    translator.translate("settings.personalization.preview_information"),
                    {
                        let view = view.clone();
                        move |_, window, cx| {
                            open_preview_information_sheet(view.clone(), window, cx)
                        }
                    },
                ),
            ));
        // 本页同时写入剪贴板与外观配置，两类保存错误都在这里展示。
        for error in [&self.clipboard_error, &self.appearance_error]
            .into_iter()
            .flatten()
        {
            section = section.child(settings_error_row(theme, error));
        }
        section.into_any_element()
    }
}

/// 把“列表与预览”页展示的剪贴板字段恢复为默认值。
fn reset_list_preview_clipboard_fields(clipboard: &mut ClipboardConfig) {
    let default = ClipboardConfig::default();
    clipboard.display_priority = default.display_priority;
    clipboard.heat_scrollbar_enabled = default.heat_scrollbar_enabled;
    clipboard.hover_preview_enabled = default.hover_preview_enabled;
    clipboard.hover_preview_delay_ms = default.hover_preview_delay_ms;
    clipboard.preview_types = default.preview_types;
    clipboard.preview_priority = default.preview_priority;
}

/// 把“列表与预览”页展示的外观字段恢复为默认值。
fn reset_list_preview_appearance_fields(appearance: &mut AppearanceConfig) {
    let default = AppearanceConfig::default();
    appearance.list_item_style = default.list_item_style;
    appearance.card_item_gap = default.card_item_gap;
    appearance.list_item_height = default.list_item_height;
    appearance.list_time_format = default.list_time_format;
    appearance.adaptive_max_lines = default.adaptive_max_lines;
    appearance.only_show_notes = default.only_show_notes;
    appearance.show_tag_names = default.show_tag_names;
    appearance.item_display = default.item_display;
    appearance.pinned_item_border = default.pinned_item_border;
    appearance.pasted_item_border = default.pasted_item_border;
    appearance.gallery_gif_autoplay = default.gallery_gif_autoplay;
    appearance.preview_information = default.preview_information;
    appearance.preview_scale_percent = default.preview_scale_percent;
}

fn open_preview_types_sheet(
    view: Entity<SettingsWindowView>,
    window: &mut gpui::Window,
    cx: &mut App,
) {
    let save_error = i18n::translator(cx).translate("settings.clipboard.save_error");
    window.open_sheet(cx, move |sheet, _, cx| {
        let theme = cx.theme().colors;
        let translator = i18n::translator(cx);
        let clipboard = config_runtime::clipboard_config(cx);
        let mut content = v_flex().size_full().min_h_0().overflow_y_scrollbar();
        for preview_type in ClipboardPreviewType::ALL {
            let label = translator.translate(preview_type.label_key());
            let checkbox = Checkbox::new(format!(
                "settings-clipboard-preview-type-{}",
                preview_type.code()
            ))
            .checked(clipboard.preview_type_enabled(preview_type))
            .accessibility_label(label)
            .on_click({
                let view = view.clone();
                let save_error = save_error.clone();
                move |checked, _, cx| {
                    persist_clipboard_change(
                        view.clone(),
                        move |clipboard| {
                            clipboard.set_preview_type_enabled(preview_type, *checked);
                        },
                        save_error.clone(),
                        cx,
                    );
                }
            });
            content = content.child(compact_setting_row(
                theme,
                translator.translate(preview_type.label_key()),
                checkbox,
            ));
        }
        sheet
            .title(translator.translate("settings.clipboard.preview_types"))
            .size(ui_rem(420.0))
            .resizable(false)
            .shadow_none()
            .map(|this| crate::window_material::sheet_surface(this, cx))
            .child(content)
    });
}

fn open_preview_information_sheet(
    view: Entity<SettingsWindowView>,
    window: &mut Window,
    cx: &mut App,
) {
    let save_error = i18n::translator(cx).translate("settings.personalization.save_error");
    window.open_sheet(cx, move |sheet, _, cx| {
        let theme = cx.theme().colors;
        let translator = i18n::translator(cx);
        let information = config_runtime::preview_information_config(cx);
        let mut controls = h_flex().w_full().flex_shrink_0().gap_2().pb_2();
        for (enabled, action) in [(true, "enable_all"), (false, "disable_all")] {
            controls = controls.child(
                Button::new(format!("settings-preview-information-{action}"))
                    .small()
                    .label(translator.translate(&format!(
                        "settings.personalization.preview_information.{action}"
                    )))
                    .on_click({
                        let view = view.clone();
                        let save_error = save_error.clone();
                        move |_, _, cx| {
                            persist_appearance_change(
                                view.clone(),
                                "预览信息展示",
                                move |appearance| {
                                    appearance.preview_information.fields = if enabled {
                                        PreviewInformationField::ALL.to_vec()
                                    } else {
                                        Vec::new()
                                    };
                                },
                                save_error.clone(),
                                cx,
                            );
                        }
                    }),
            );
        }
        let mut content = v_flex()
            .size_full()
            .min_h_0()
            .overflow_y_scrollbar()
            .child(controls);
        for field in PreviewInformationField::ALL {
            let label = translator.translate(field.label_key());
            let switch = Switch::new(format!("settings-preview-information-{}", field.key()))
                .checked(information.enabled(field))
                .accessibility_label(label.clone())
                .on_click({
                    let view = view.clone();
                    let save_error = save_error.clone();
                    move |checked, _, cx| {
                        persist_appearance_change(
                            view.clone(),
                            "预览信息展示",
                            move |appearance| {
                                appearance.preview_information.set_enabled(field, *checked)
                            },
                            save_error.clone(),
                            cx,
                        );
                    }
                });
            content = content.child(compact_setting_row(theme, label, switch));
        }
        sheet
            .title(translator.translate("settings.personalization.preview_information"))
            .size(ui_rem(420.0))
            .resizable(false)
            .shadow_none()
            .map(|this| crate::window_material::sheet_surface(this, cx))
            .child(content)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_list_preview_restores_tag_name_display() {
        let mut appearance = AppearanceConfig::default();
        let mut value = serde_json::to_value(&appearance).unwrap();
        value["showTagNames"] = true.into();
        appearance = serde_json::from_value(value).unwrap();
        reset_list_preview_appearance_fields(&mut appearance);
        let value = serde_json::to_value(&appearance).unwrap();
        assert_eq!(value["showTagNames"], false);
    }
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
