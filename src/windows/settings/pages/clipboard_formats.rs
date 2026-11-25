use gpui::prelude::*;
use gpui::{AnyElement, Context, div, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::Input;
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};

use super::super::{SettingsPage, SettingsWindowView, notify_settings_error};
use super::main_window::persist_clipboard_change;
use crate::{config::ClipboardFormatConfig, config_runtime, i18n};

impl SettingsWindowView {
    pub(super) fn render_clipboard_formats(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let translator = i18n::translator(cx);
        let clipboard = config_runtime::clipboard_config(cx);
        let formats = clipboard.capture_formats;
        let view = cx.entity().clone();
        let save_error = translator.translate("settings.clipboard.save_error");
        let respect_history_exclusion_switch = Switch::new("clipboard-respect-history-exclusion")
            .checked(clipboard.respect_history_exclusion)
            .accessibility_label(
                translator.translate("settings.clipboard.respect_history_exclusion"),
            )
            .on_click({
                let view = view.clone();
                let save_error = save_error.clone();
                move |checked, _, cx| {
                    persist_clipboard_change(
                        view.clone(),
                        |clipboard| clipboard.respect_history_exclusion = *checked,
                        save_error.clone(),
                        cx,
                    );
                }
            });
        let all_switch = Switch::new("clipboard-capture-all")
            .checked(formats.all)
            .on_click({
                let view = view.clone();
                let save_error = save_error.clone();
                move |checked, _, cx| {
                    persist_clipboard_change(
                        view.clone(),
                        |clipboard| clipboard.capture_formats.all = *checked,
                        save_error.clone(),
                        cx,
                    );
                }
            });
        let mut section =
            v_flex()
                .w_full()
                .min_w_0()
                .flex_shrink_0()
                .child(
                    self.setting_row(
                        theme,
                        translator.translate("settings.clipboard.capture_all_formats"),
                        Some(
                            translator
                                .translate("settings.clipboard.capture_all_formats_description")
                                .into(),
                        ),
                        all_switch,
                    ),
                )
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .items_center()
                        .gap_2()
                        .px_4()
                        .py_3()
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .child(div().text_sm().child(translator.translate_with_args(
                                    "settings.clipboard.capture_formats_count",
                                    &[("count", formats.names.len().to_string())],
                                )))
                                .child(div().text_xs().text_color(theme.muted_foreground).child(
                                    translator.translate(
                                        "settings.clipboard.capture_formats_description",
                                    ),
                                )),
                        )
                        .child(
                            Button::new("clipboard-formats-expand")
                                .small()
                                .ghost()
                                .label(translator.translate(if self.clipboard_formats_expanded {
                                    "settings.clipboard.collapse_formats"
                                } else {
                                    "settings.clipboard.edit_formats"
                                }))
                                .on_click({
                                    let view = view.clone();
                                    move |_, _, cx| {
                                        view.update(cx, |view, cx| {
                                            view.clipboard_formats_expanded =
                                                !view.clipboard_formats_expanded;
                                            cx.notify();
                                        });
                                    }
                                }),
                        ),
                );
        if self.clipboard_formats_expanded {
            let mut list = h_flex().w_full().min_w_0().flex_wrap().gap_2();
            for (index, name) in formats.names.iter().enumerate() {
                let remove_name = name.clone();
                let remove_view = view.clone();
                let remove_error = save_error.clone();
                list = list.child(
                    h_flex()
                        .min_w_0()
                        .items_center()
                        .gap_1()
                        .pl_2()
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(theme.border)
                        .child(div().text_xs().child(name.clone()))
                        .child(
                            Button::new(("clipboard-format-remove", index as u64))
                                .ghost()
                                .small()
                                .label(
                                    translator.translate("settings.clipboard.remove_native_format"),
                                )
                                .disabled(formats.all)
                                .on_click(move |_, _, cx| {
                                    let remove_name = remove_name.clone();
                                    persist_clipboard_change(
                                        remove_view.clone(),
                                        move |clipboard| {
                                            clipboard.capture_formats.names.retain(|name| {
                                                !name.eq_ignore_ascii_case(&remove_name)
                                            });
                                        },
                                        remove_error.clone(),
                                        cx,
                                    );
                                }),
                        ),
                );
            }
            let input = self.clipboard_format_input.clone();
            let add_view = view.clone();
            let add_error = save_error.clone();
            let add_button = Button::new("clipboard-format-add")
                .primary()
                .small()
                .label(translator.translate("settings.clipboard.add_native_format"))
                .disabled(formats.all)
                .on_click(move |_, window, cx| {
                    let name = input.read(cx).value().trim().to_owned();
                    let mut clipboard = config_runtime::clipboard_config(cx);
                    let error = if !ClipboardFormatConfig::valid_name(&name) {
                        Some(
                            i18n::translator(cx)
                                .translate("settings.clipboard.invalid_native_format"),
                        )
                    } else if clipboard
                        .capture_formats
                        .names
                        .iter()
                        .any(|item| item.eq_ignore_ascii_case(&name))
                    {
                        Some(
                            i18n::translator(cx)
                                .translate("settings.clipboard.duplicate_native_format"),
                        )
                    } else {
                        clipboard.capture_formats.names.push(name);
                        match config_runtime::set_clipboard_config(clipboard, cx) {
                            Ok(_) => None,
                            Err(error) => {
                                tracing::error!("添加剪贴板格式失败：{error}");
                                Some(add_error.clone())
                            }
                        }
                    };
                    let succeeded = error.is_none();
                    add_view.update(cx, |view, cx| {
                        view.clipboard_error =
                            notify_settings_error(SettingsPage::Capture, error, cx);
                        cx.notify();
                    });
                    if succeeded {
                        input.update(cx, |input, cx| input.set_value("", window, cx));
                    }
                });
            section =
                section.child(
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .gap_3()
                        .px_4()
                        .pb_3()
                        .when(formats.all, |section| {
                            section.child(div().text_xs().text_color(theme.muted_foreground).child(
                                translator.translate("settings.clipboard.format_list_inactive"),
                            ))
                        })
                        .when(formats.names.is_empty(), |section| {
                            section.child(div().text_xs().text_color(theme.muted_foreground).child(
                                translator.translate("settings.clipboard.no_native_formats"),
                            ))
                        })
                        .child(list)
                        .child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .gap_2()
                                .items_center()
                                .child(
                                    div().min_w_0().flex_1().child(
                                        Input::new(&self.clipboard_format_input)
                                            .small()
                                            .w_full()
                                            .disabled(formats.all),
                                    ),
                                )
                                .child(add_button)
                                .child(
                                    Button::new("clipboard-formats-defaults")
                                        .small()
                                        .ghost()
                                        .label(
                                            translator.translate(
                                                "settings.clipboard.restore_native_formats",
                                            ),
                                        )
                                        .disabled(formats.all)
                                        .on_click(move |_, _, cx| {
                                            persist_clipboard_change(
                                                view.clone(),
                                                |clipboard| {
                                                    clipboard.capture_formats.names =
                                                        ClipboardFormatConfig::default().names;
                                                },
                                                save_error.clone(),
                                                cx,
                                            );
                                        }),
                                ),
                        ),
                );
        }
        section
            .when(cfg!(target_os = "windows"), |section| {
                section.child(
                    self.setting_row(
                        theme,
                        translator.translate("settings.clipboard.respect_history_exclusion"),
                        Some(
                            translator
                                .translate(
                                    "settings.clipboard.respect_history_exclusion_description",
                                )
                                .into(),
                        ),
                        respect_history_exclusion_switch,
                    ),
                )
            })
            .border_b_1()
            .border_color(theme.border)
            .into_any_element()
    }
}
