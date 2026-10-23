use crate::ui::scale::ui_rem;

use super::super::{SettingsPage, notify_settings_error};
use gpui::prelude::*;
use gpui::{AnyElement, App, Context, ScrollAnchor};

use super::super::{
    SettingsWindowView, ShortcutCategory, ShortcutRecordingTarget,
    components::{compact_setting_row, settings_error_row, settings_fused_dropdown},
};
use crate::{
    config::{
        QuickPasteShortcutConfig, QuickPasteShortcutKey, QuickPasteShortcutModifier,
        SessionShortcutConfig, SessionShortcutId,
    },
    config_runtime, hotkey, i18n,
    windows::menu::{DropdownItem, DropdownItemId},
};
use gpui::{Entity, FocusHandle, KeyDownEvent, SharedString, div, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::slider::Slider;
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconNamed, InteractiveElementExt as _, Sizable as _,
    checkbox::Checkbox,
    h_flex,
    kbd::Kbd,
    switch::Switch,
    tab::{Tab, TabBar},
    tooltip::Tooltip,
    v_flex,
};

impl ShortcutRecordingTarget {
    fn is_main_window(self) -> bool {
        self == Self::ToggleMainWindow
    }

    fn default_binding(self) -> Option<crate::config::ShortcutBinding> {
        use crate::config::{ShortcutBinding, ShortcutsConfig};

        match self {
            Self::ToggleMainWindow => Some(ShortcutBinding::toggle_main_window_default()),
            Self::OpenSettings => ShortcutsConfig::default().open_settings,
            Self::ToggleClipboardMonitor => ShortcutsConfig::default().toggle_clipboard_monitor,
            Self::PlainTextPaste => ShortcutsConfig::default().plain_text_paste,
            Self::QuickPaste => ShortcutsConfig::default().quick_paste,
            Self::AddFavorite => ShortcutsConfig::default().add_favorite,
            Self::Session(id) => ShortcutsConfig::default().session(id).binding.clone(),
        }
    }

    fn code(self) -> &'static str {
        match self {
            Self::ToggleMainWindow => "toggleMainWindow",
            Self::OpenSettings => "openSettings",
            Self::ToggleClipboardMonitor => "toggleClipboardMonitor",
            Self::PlainTextPaste => "plainTextPaste",
            Self::QuickPaste => "quickPaste",
            Self::AddFavorite => "addFavorite",
            Self::Session(id) => id.code(),
        }
    }

    fn set_binding(
        self,
        binding: Option<crate::config::ShortcutBinding>,
        cx: &mut App,
    ) -> Result<(), String> {
        match self {
            Self::ToggleMainWindow => hotkey::set_toggle_main_window_shortcut(binding, cx),
            Self::OpenSettings => hotkey::set_open_settings_shortcut(binding, cx),
            Self::ToggleClipboardMonitor => {
                hotkey::set_toggle_clipboard_monitor_shortcut(binding, cx)
            }
            Self::PlainTextPaste => hotkey::set_plain_text_paste_shortcut(binding, cx),
            Self::QuickPaste => hotkey::set_quick_paste_shortcut(binding, cx),
            Self::AddFavorite => hotkey::set_add_favorite_shortcut(binding, cx),
            Self::Session(id) => hotkey::set_session_shortcut_binding(id, binding, cx),
        }
    }

    fn reset(self, cx: &mut App) -> Result<(), String> {
        match self {
            Self::ToggleMainWindow => hotkey::reset_toggle_main_window_shortcut(cx),
            Self::OpenSettings => hotkey::reset_open_settings_shortcut(cx),
            Self::ToggleClipboardMonitor => hotkey::reset_toggle_clipboard_monitor_shortcut(cx),
            Self::PlainTextPaste => hotkey::reset_plain_text_paste_shortcut(cx),
            Self::QuickPaste => hotkey::reset_quick_paste_shortcut(cx),
            Self::AddFavorite => hotkey::reset_add_favorite_shortcut(cx),
            Self::Session(id) => hotkey::reset_session_shortcut_binding(id, cx),
        }
    }

    fn reject(self, cx: &mut App) {
        match self {
            Self::ToggleMainWindow => hotkey::reject_shortcut_recording(true, cx),
            Self::OpenSettings => hotkey::reject_open_settings_recording(cx),
            Self::ToggleClipboardMonitor => hotkey::reject_toggle_clipboard_monitor_recording(cx),
            Self::PlainTextPaste => hotkey::reject_plain_text_paste_recording(cx),
            Self::QuickPaste => hotkey::reject_quick_paste_recording(cx),
            Self::AddFavorite => hotkey::reject_add_favorite_recording(cx),
            Self::Session(_) => hotkey::reject_shortcut_recording(false, cx),
        }
    }
}

impl SettingsWindowView {
    pub(in crate::windows::settings) fn render_shortcuts(
        &self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let translator = i18n::translator(cx);
        let shortcuts = config_runtime::shortcuts_config(cx);
        let binding = shortcuts.toggle_main_window.clone();
        let settings_binding = shortcuts.open_settings.clone();
        let clipboard_monitor_binding = shortcuts.toggle_clipboard_monitor.clone();
        let plain_text_paste_binding = shortcuts.plain_text_paste.clone();
        let quick_paste_binding = shortcuts.quick_paste.clone();
        let add_favorite_binding = shortcuts.add_favorite.clone();
        let add_favorite_open_editor_on_empty = shortcuts.add_favorite_open_editor_on_empty;
        let quick_paste_1_to_9 = shortcuts.quick_paste_1_to_9;
        let recording_target = self.recording_target;
        let recording = recording_target.is_some();
        let is_recording = |target| recording_target == Some(target);
        let selected_category = self.selected_shortcut_category;
        let view = cx.entity().clone();
        let view_for_key = view.clone();
        let view_for_page_reset = view.clone();
        let reset_error = translator.translate("settings.shortcuts.reset_error");
        let page_reset_button = Button::new("settings-shortcuts-reset-page")
            .warning()
            .small()
            .label(translator.translate("settings.action.reset_page"))
            .on_click(move |_, _, cx| {
                let error = hotkey::reset_shortcuts(cx).err().map(|error| {
                    tracing::error!("重置快捷键设置失败：{error}");
                    reset_error.clone()
                });
                view_for_page_reset.update(cx, |view, cx| {
                    view.shortcut_error = notify_settings_error(SettingsPage::Shortcuts, error, cx);
                    view.recording_target = None;
                    cx.notify();
                });
            });

        let global_anchor = ScrollAnchor::for_handle(self.shortcut_scroll_handle.clone());
        let temporary_anchor = ScrollAnchor::for_handle(self.shortcut_scroll_handle.clone());
        let category_tabs = shortcut_category_tabs(
            view.clone(),
            selected_category,
            global_anchor.clone(),
            temporary_anchor.clone(),
            translator.clone(),
        );

        let status = hotkey::registration_status(cx);
        let global_recording = is_recording(ShortcutRecordingTarget::ToggleMainWindow);
        let settings_recording = is_recording(ShortcutRecordingTarget::OpenSettings);
        let clipboard_monitor_recording =
            is_recording(ShortcutRecordingTarget::ToggleClipboardMonitor);
        let plain_text_paste_recording = is_recording(ShortcutRecordingTarget::PlainTextPaste);
        let quick_paste_recording = is_recording(ShortcutRecordingTarget::QuickPaste);
        let add_favorite_recording = is_recording(ShortcutRecordingTarget::AddFavorite);
        let quick_paste_1_to_9_status = if recording {
            hotkey::RegistrationStatus::Suspended
        } else {
            hotkey::quick_paste_index_registration_status(cx)
        };
        let quick_paste_1_to_9_failures = hotkey::quick_paste_index_registration_failures(cx);
        let global_status = if global_recording {
            hotkey::RegistrationStatus::Suspended
        } else {
            status
        };
        let save_error = translator.translate("settings.shortcuts.save_error");
        let save_error_for_key = save_error.clone();
        let win_v_replace_error = translator.translate("settings.shortcuts.win_v_replace.error");
        let translator_for_key = translator.clone();
        let save_error_for_session_rows = save_error.clone();
        let translator_for_session_rows = translator.clone();
        let view_for_session_rows = view.clone();
        let all_only_in_window = SessionShortcutId::ALL
            .into_iter()
            .all(|id| shortcuts.session(id).only_in_window);
        let select_all_scope = Checkbox::new("settings-session-shortcuts-select-all-scope")
            .checked(all_only_in_window)
            .label(translator.translate(if all_only_in_window {
                "settings.shortcuts.deselect_all"
            } else {
                "settings.shortcuts.select_all"
            }))
            .tooltip(translator.translate("settings.shortcuts.only_in_window"))
            .on_click({
                let view = view.clone();
                let save_error = save_error.clone();
                move |checked, _, cx| {
                    cx.stop_propagation();
                    let error = hotkey::set_all_session_shortcuts_only_in_window(*checked, cx)
                        .err()
                        .map(|error| {
                            tracing::error!("批量更新快捷键窗口范围失败：{error}");
                            save_error.clone()
                        });
                    view.update(cx, |view, cx| {
                        view.shortcut_error =
                            notify_settings_error(SettingsPage::Shortcuts, error, cx);
                        cx.notify();
                    });
                }
            });
        let shortcut_id = view_for_key.clone();
        let middle_mouse_controls = self.middle_mouse_controls(cx);
        let shortcut_section = v_flex()
            .id("settings-shortcuts-page")
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .gap_4()
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                let Some(target) = shortcut_id.read(cx).recording_target else {
                    return;
                };
                if event.is_held {
                    return;
                }
                window.prevent_default();
                cx.stop_propagation();
                if event.keystroke.key == "escape" && !event.keystroke.modifiers.modified() {
                    if let Err(error) = hotkey::cancel_toggle_main_window_recording(cx) {
                        tracing::error!("恢复显示/隐藏窗口快捷键失败：{error}");
                        let _ = notify_settings_error(SettingsPage::Shortcuts, Some(save_error_for_key.clone()), cx);
                    }
                    let _ = shortcut_id.update(cx, |view, cx| {
                        view.recording_target = None;
                        cx.notify();
                    });
                    return;
                }
                let binding = match hotkey::shortcut_binding_from_keystroke(&event.keystroke) {
                    Ok(binding) => binding,
                    Err(error) => {
                        tracing::error!("保存快捷键失败：{error}");
                        target.reject(cx);
                        let _ = shortcut_id.update(cx, |view, cx| {
                            view.shortcut_error = notify_settings_error(SettingsPage::Shortcuts, Some(save_error_for_key.clone()), cx);
                            view.recording_target = None;
                            cx.notify();
                        });
                        return;
                    }
                };
                #[cfg(target_os = "windows")]
                if target.is_main_window()
                    && hotkey::is_win_v_binding(&binding)
                    && !crate::services::system::win_v_hotkey::is_win_v_hotkey_disabled()
                {
                    let translator = translator_for_key.clone();
                    let title =
                        translator.translate("settings.shortcuts.win_v_replace.title");
                    let detail =
                        translator.translate("settings.shortcuts.win_v_replace.detail");
                    let cancel =
                        gpui::PromptButton::cancel(translator.translate(
                            "settings.shortcuts.win_v_replace.cancel",
                        ));
                    let confirm =
                        gpui::PromptButton::ok(translator.translate(
                            "settings.shortcuts.win_v_replace.confirm",
                        ));
                    let answer = window.prompt(
                        gpui::PromptLevel::Warning,
                        &title,
                        Some(&detail),
                        &[cancel, confirm],
                        cx,
                    );
                    let view = shortcut_id.clone();
                    let save_error = save_error_for_key.clone();
                    let replace_error = win_v_replace_error.clone();
                    cx.spawn(async move |cx| {
                        let accepted = answer.await.unwrap_or(0) == 1;
                        if !accepted {
                            cx.update(|cx| {
                                if let Err(error) =
                                    hotkey::cancel_toggle_main_window_recording(cx)
                                {
                                    tracing::error!("恢复显示/隐藏窗口快捷键失败：{error}");
                                    let _ = notify_settings_error(SettingsPage::Shortcuts, Some(save_error.clone()), cx);
                                }
                                let _ = view.update(cx, |view, cx| {
                                    view.recording_target = None;
                                    cx.notify();
                                });
                            });
                            return;
                        }

                        let replacement = cx
                            .background_spawn(async {
                                crate::services::system::win_v_hotkey::disable_win_v_hotkey()
                            })
                            .await;
                        cx.update(|cx| {
                            let error = match replacement {
                                Ok(()) => target
                                    .set_binding(Some(binding), cx)
                                    .err()
                                    .map(|error| {
                                        tracing::error!("保存快捷键失败：{error}");
                                        target.reject(cx);
                                        save_error.clone()
                                    }),
                                Err(error) => {
                                    tracing::error!("替换系统 Win+V 失败：{error}");
                                    target.reject(cx);
                                    Some(replace_error.clone())
                                }
                            };
                            let _ = view.update(cx, |view, cx| {
                                view.shortcut_error = notify_settings_error(SettingsPage::Shortcuts, error, cx);
                                view.recording_target = None;
                                cx.notify();
                            });
                        });
                    })
                    .detach();
                    return;
                }
                let result = target.set_binding(Some(binding), cx);
                let error = result.err().map(|error| {
                    tracing::error!("保存快捷键失败：{error}");
                    target.reject(cx);
                    save_error_for_key.clone()
                });
                let _ = shortcut_id.update(cx, |view, cx| {
                    view.shortcut_error = notify_settings_error(SettingsPage::Shortcuts, error, cx);
                    view.recording_target = None;
                    cx.notify();
                });
            })
            .child(
                h_flex()
                    .w_full()
                    .min_h(ui_rem(48.0))
                    .flex_shrink_0()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(theme.border)
                    .px_4()
                    .child(category_tabs)
                    .child(page_reset_button),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(
                        v_flex()
                            .id("settings-shortcuts-scroll-content")
                            .size_full()
                            .min_h_0()
                            .track_scroll(&self.shortcut_scroll_handle)
                            .overflow_y_scroll()
                            .lock_scroll_axis()
                            .child(
                                v_flex()
                                    .w_full()
                                    .min_w_0()
                                    .flex_shrink_0()
                                    .overflow_hidden()
                                    .rounded(px(8.0))
                                    .border_1()
                                    .border_color(theme.border)
                                    .bg(theme.title_bar)
                                    .child(self.highlight_element(
                theme,
                translator.translate("settings.shortcuts.global_section"),
                shortcut_category(
                ShortcutCategory::Global,
                translator.translate("settings.shortcuts.global_section"),
                translator.translate("settings.shortcuts.global_description"),
                v_flex()
                    .w_full()
                    .min_w_0()
                    .child(self.compact_setting_row(
                        theme,
                        translator.translate("settings.shortcuts.toggle_main_window"),
                        shortcut_recorder(
                            binding,
                            global_status,
                            global_recording,
                            view.clone(),
                            self.recording_focus_handle.clone(),
                            save_error.clone(),
                            translator.clone(),
                            theme,
                            ShortcutRecordingTarget::ToggleMainWindow,
                        ),
                    ))
                    .when(cfg!(target_os = "windows"), |this| this.child(middle_mouse_controls))
                    .child(self.compact_setting_row(
                        theme,
                        translator.translate("settings.shortcuts.open_settings"),
                        shortcut_recorder(
                            settings_binding,
                            if settings_recording {
                                hotkey::RegistrationStatus::Suspended
                            } else {
                                hotkey::settings_registration_status(cx)
                            },
                            settings_recording,
                            view.clone(),
                            self.recording_focus_handle.clone(),
                            save_error.clone(),
                            translator.clone(),
                            theme,
                            ShortcutRecordingTarget::OpenSettings,
                        ),
                    ))
                    .child(self.compact_setting_row(
                        theme,
                        translator.translate("settings.shortcuts.toggle_clipboard_monitor"),
                        shortcut_recorder(
                            clipboard_monitor_binding,
                            if clipboard_monitor_recording {
                                hotkey::RegistrationStatus::Suspended
                            } else {
                                hotkey::clipboard_monitor_registration_status(cx)
                            },
                            clipboard_monitor_recording,
                            view.clone(),
                            self.recording_focus_handle.clone(),
                            save_error.clone(),
                            translator.clone(),
                            theme,
                            ShortcutRecordingTarget::ToggleClipboardMonitor,
                        ),
                    ))
                    .child(self.compact_setting_row(
                        theme,
                        translator.translate("settings.shortcuts.plain_text_paste"),
                        shortcut_recorder(
                            plain_text_paste_binding,
                            if plain_text_paste_recording {
                                hotkey::RegistrationStatus::Suspended
                            } else {
                                hotkey::plain_text_paste_registration_status(cx)
                            },
                            plain_text_paste_recording,
                            view.clone(),
                            self.recording_focus_handle.clone(),
                            save_error.clone(),
                            translator.clone(),
                            theme,
                            ShortcutRecordingTarget::PlainTextPaste,
                        ),
                    ))
                    .child(self.compact_setting_row(
                        theme,
                        translator.translate("settings.shortcuts.quick_paste"),
                        shortcut_recorder(
                            quick_paste_binding,
                            if quick_paste_recording {
                                hotkey::RegistrationStatus::Suspended
                            } else {
                                hotkey::quick_paste_registration_status(cx)
                            },
                            quick_paste_recording,
                            view.clone(),
                            self.recording_focus_handle.clone(),
                            save_error.clone(),
                            translator.clone(),
                            theme,
                            ShortcutRecordingTarget::QuickPaste,
                        ),
                    ))
                    .child(self.compact_setting_row(
                        theme,
                        translator.translate("settings.shortcuts.add_favorite"),
                        shortcut_recorder(
                            add_favorite_binding,
                            if add_favorite_recording {
                                hotkey::RegistrationStatus::Suspended
                            } else {
                                hotkey::add_favorite_registration_status(cx)
                            },
                            add_favorite_recording,
                            view.clone(),
                            self.recording_focus_handle.clone(),
                            save_error.clone(),
                            translator.clone(),
                            theme,
                            ShortcutRecordingTarget::AddFavorite,
                        ),
                    ))
                    .child(self.compact_setting_row(
                        theme,
                        translator.translate("settings.shortcuts.add_favorite_open_editor_on_empty"),
                        Checkbox::new("settings-shortcut-add-favorite-open-editor-on-empty")
                            .checked(add_favorite_open_editor_on_empty)
                            .accessibility_label(
                                translator.translate("settings.shortcuts.add_favorite_open_editor_on_empty"),
                            )
                            .tooltip(
                                translator.translate(
                                    "settings.shortcuts.add_favorite_open_editor_on_empty.tooltip",
                                ),
                            )
                            .on_click({
                                let view = view.clone();
                                let save_error = save_error.clone();
                                move |checked, _, cx| {
                                    cx.stop_propagation();
                                    let error = hotkey::set_add_favorite_open_editor_on_empty(*checked, cx)
                                        .err()
                                        .map(|error| {
                                            tracing::error!(
                                                "更新快捷收藏编辑器开关失败：{error}"
                                            );
                                            save_error.clone()
                                        });
                                    view.update(cx, |view, cx| {
                                        view.shortcut_error = notify_settings_error(
                                            SettingsPage::Shortcuts,
                                            error,
                                            cx,
                                        );
                                        cx.notify();
                                    });
                                }
                            }),
                    ))
                    .child(self.highlight_element(
                        theme,
                        translator.translate("settings.shortcuts.quick_paste_1_to_9"),
                        quick_paste_1_to_9_row(
                            quick_paste_1_to_9,
                            quick_paste_1_to_9_status,
                            quick_paste_1_to_9_failures,
                            view.clone(),
                            translator.clone(),
                            theme,
                            save_error.clone(),
                            cx,
                        ),
                    )),
                theme,
                global_anchor,
                None,
            )))
                                    .child(div().h(px(1.0)).w_full().bg(theme.border))
                                    .child(self.highlight_element(
                theme,
                translator.translate("settings.shortcuts.session_section"),
                shortcut_category(
                ShortcutCategory::Temporary,
                translator.translate("settings.shortcuts.session_section"),
                translator.translate("settings.shortcuts.session_description"),
                v_flex()
                    .w_full()
                    .min_w_0()
                    .children(SessionShortcutId::ALL.into_iter().map(|id| {
                        let session_recording =
                            recording_target == Some(ShortcutRecordingTarget::Session(id));
                        let session_status = if session_recording {
                            hotkey::RegistrationStatus::Suspended
                        } else {
                            hotkey::session_registration_status(id, cx)
                        };
                        self.highlight_element(
                            theme,
                            translator_for_session_rows.translate(id.label_key()),
                            session_shortcut_row(
                                id,
                                shortcuts.session(id).clone(),
                                session_status,
                                session_recording,
                                self.recording_focus_handle.clone(),
                                save_error_for_session_rows.clone(),
                                translator_for_session_rows.clone(),
                                theme,
                                view_for_session_rows.clone(),
                            ),
                        )
                    })),
                theme,
                temporary_anchor,
                Some(select_all_scope.into_any_element()),
            )),
                            )),
                    )
                    .vertical_scrollbar(&self.shortcut_scroll_handle),
            );
        if let Some(error) = &self.shortcut_error {
            shortcut_section.child(settings_error_row(theme, error))
        } else {
            shortcut_section
        }
        .into_any_element()
    }
}

impl SettingsWindowView {
    fn middle_mouse_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let translator = i18n::translator(cx);
        let config = config_runtime::shortcuts_config(cx).middle_mouse;
        let mut items = vec![
            DropdownItem::option(
                DropdownItemId::new("none"),
                translator.translate("settings.shortcuts.middle_mouse.none"),
            )
            .selected(config.modifier.is_none()),
        ];
        items.extend(QuickPasteShortcutModifier::ALL.into_iter().map(|modifier| {
            DropdownItem::option(
                DropdownItemId::new(modifier.code()),
                translator.translate(modifier.label_key()),
            )
            .selected(config.modifier == Some(modifier))
        }));
        let view = cx.entity();
        let modifier_view = view.clone();
        let selector = settings_fused_dropdown(
            "settings-shortcuts-middle-mouse",
            config
                .modifier
                .map(|modifier| modifier.code())
                .unwrap_or("none"),
            items,
            translator.translate("settings.shortcuts.middle_mouse.button"),
            Vec::new(),
            move |id, cx| {
                let modifier = if id.as_str() == "none" {
                    None
                } else {
                    let Some(modifier) = QuickPasteShortcutModifier::from_code(id.as_str()) else {
                        return;
                    };
                    Some(modifier)
                };
                let mut config = config_runtime::shortcuts_config(cx).middle_mouse;
                config.modifier = modifier;
                save_middle_mouse_config(&modifier_view, config, cx);
            },
            |_, _| {},
            !config.enabled,
            cx,
        );
        let enabled = Switch::new("settings-shortcuts-middle-mouse-enabled")
            .large()
            .checked(config.enabled)
            .accessibility_label(translator.translate("settings.shortcuts.middle_mouse.enabled"))
            .on_click(move |checked, _, cx| {
                let mut config = config_runtime::shortcuts_config(cx).middle_mouse;
                config.enabled = *checked;
                save_middle_mouse_config(&view, config, cx);
            });
        let enabled_title = translator.translate("settings.shortcuts.middle_mouse.enabled");
        let threshold_title = translator.translate("settings.shortcuts.middle_mouse.threshold");
        let description = translator.translate("settings.shortcuts.middle_mouse.description");
        v_flex()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .gap_2()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                self.highlight_element(
                    theme,
                    enabled_title.clone(),
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .min_h(ui_rem(36.0))
                        .flex_wrap()
                        .justify_between()
                        .gap_4()
                        .px_4()
                        .child(
                            div()
                                .min_w(ui_rem(180.0))
                                .flex_1()
                                .pl_2()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(enabled_title),
                        )
                        .child(
                            h_flex()
                                .flex_shrink_0()
                                .gap_2()
                                .child(selector)
                                .child(enabled),
                        ),
                ),
            )
            .child(
                self.highlight_element(
                    theme,
                    threshold_title.clone(),
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .min_h(ui_rem(28.0))
                        .flex_wrap()
                        .justify_between()
                        .gap_4()
                        .px_4()
                        .child(
                            h_flex()
                                .id("settings-shortcuts-middle-mouse-threshold-label")
                                .min_w(ui_rem(180.0))
                                .flex_1()
                                .pl_2()
                                .gap_2()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(threshold_title)
                                .child(div().opacity(0.7).child("10–1000 ms"))
                                .tooltip(move |window, cx| {
                                    Tooltip::new(description.clone()).build(window, cx)
                                }),
                        )
                        .child(
                            h_flex()
                                .w(ui_rem(248.0))
                                .flex_shrink_0()
                                .gap_3()
                                .when(!config.enabled, |this| this.opacity(0.5))
                                .child(
                                    Slider::new(&self.middle_mouse_threshold_slider)
                                        .disabled(!config.enabled)
                                        .flex_1(),
                                )
                                .child(
                                    div()
                                        .w(ui_rem(62.0))
                                        .text_xs()
                                        .text_right()
                                        .text_color(theme.muted_foreground)
                                        .child(format!("{} ms", config.hold_threshold_ms)),
                                ),
                        ),
                ),
            )
            .into_any_element()
    }
}

fn save_middle_mouse_config(
    view: &Entity<SettingsWindowView>,
    config: crate::config::MiddleMouseShortcutConfig,
    cx: &mut gpui::App,
) {
    let error = hotkey::set_middle_mouse_config(config, cx)
        .err()
        .map(|error| {
            tracing::error!("保存鼠标中键呼出设置失败：{error}");
            i18n::translator(cx).translate("settings.shortcuts.save_error")
        });
    view.update(cx, |view, cx| {
        view.shortcut_error = notify_settings_error(SettingsPage::Shortcuts, error, cx);
        cx.notify();
    });
}

fn quick_paste_1_to_9_row(
    config: QuickPasteShortcutConfig,
    status: hotkey::RegistrationStatus,
    failures: Vec<String>,
    view: Entity<SettingsWindowView>,
    translator: i18n::Translator,
    theme: gpui_component::ThemeColor,
    save_error: String,
    cx: &mut Context<SettingsWindowView>,
) -> AnyElement {
    let modifier_items = QuickPasteShortcutModifier::ALL
        .into_iter()
        .map(|modifier| {
            DropdownItem::option(
                DropdownItemId::new(modifier.code()),
                translator.translate(modifier.label_key()),
            )
            .selected(modifier == config.modifier)
        })
        .collect();
    let key_items = QuickPasteShortcutKey::ALL
        .into_iter()
        .map(|key| {
            DropdownItem::option(
                DropdownItemId::new(key.code()),
                translator.translate(key.label_key()),
            )
            .selected(key == config.key)
        })
        .collect();
    let modifier_error = save_error.clone();
    let modifier_view = view.clone();
    let modifier_handler = {
        let modifier_error = modifier_error.clone();
        let modifier_view = modifier_view.clone();
        move |id: DropdownItemId, cx: &mut gpui::App| {
            let Some(modifier) = QuickPasteShortcutModifier::from_code(id.as_str()) else {
                return;
            };
            let mut next = config;
            next.modifier = modifier;
            let error = hotkey::set_quick_paste_index_config(next, cx)
                .err()
                .map(|error| {
                    tracing::error!("保存快捷粘贴 1~9 修饰键失败：{error}");
                    modifier_error.clone()
                });
            modifier_view.update(cx, |view, cx| {
                view.shortcut_error = notify_settings_error(SettingsPage::Shortcuts, error, cx);
                cx.notify();
            });
        }
    };
    let key_handler = {
        let key_error = save_error.clone();
        let key_view = view.clone();
        move |id: DropdownItemId, cx: &mut gpui::App| {
            let Some(key) = QuickPasteShortcutKey::from_code(id.as_str()) else {
                return;
            };
            let mut next = config;
            next.key = key;
            let error = hotkey::set_quick_paste_index_config(next, cx)
                .err()
                .map(|error| {
                    tracing::error!("保存快捷粘贴 1~9 按键组失败：{error}");
                    key_error.clone()
                });
            key_view.update(cx, |view, cx| {
                view.shortcut_error = notify_settings_error(SettingsPage::Shortcuts, error, cx);
                cx.notify();
            });
        }
    };
    let selector = settings_fused_dropdown(
        "settings-shortcuts-quick-paste-1-to-9",
        config.modifier.code(),
        modifier_items,
        config.key.code(),
        key_items,
        modifier_handler,
        key_handler,
        false,
        cx,
    );
    let enabled_switch_error = save_error.clone();
    let enabled_switch_view = view.clone();
    let enabled_switch = Switch::new("settings-shortcuts-quick-paste-1-to-9-enabled")
        .large()
        .checked(config.enabled)
        .accessibility_label(translator.translate("settings.shortcuts.quick_paste_1_to_9.enabled"))
        .tooltip(translator.translate("settings.shortcuts.quick_paste_1_to_9.enabled.tooltip"))
        .on_click(move |checked, _, cx| {
            let mut next = config;
            next.enabled = *checked;
            let error = hotkey::set_quick_paste_index_config(next, cx)
                .err()
                .map(|error| {
                    tracing::error!("保存快捷粘贴 1~9 开关失败：{error}");
                    enabled_switch_error.clone()
                });
            enabled_switch_view.update(cx, |view, cx| {
                view.shortcut_error = notify_settings_error(SettingsPage::Shortcuts, error, cx);
                cx.notify();
            });
        });
    let include_pinned_error = save_error;
    let include_pinned_view = view.clone();
    let include_pinned_checkbox =
        Checkbox::new("settings-shortcuts-quick-paste-1-to-9-include-pinned")
            .checked(config.include_pinned)
            .accessibility_label(
                translator.translate("settings.shortcuts.quick_paste_1_to_9.include_pinned"),
            )
            .tooltip(
                translator
                    .translate("settings.shortcuts.quick_paste_1_to_9.include_pinned.tooltip"),
            )
            .on_click(move |checked, _, cx| {
                let mut next = config;
                next.include_pinned = *checked;
                let error = hotkey::set_quick_paste_index_config(next, cx)
                    .err()
                    .map(|error| {
                        tracing::error!("保存快捷粘贴 1~9 置顶项选项失败：{error}");
                        include_pinned_error.clone()
                    });
                include_pinned_view.update(cx, |view, cx| {
                    view.shortcut_error = notify_settings_error(SettingsPage::Shortcuts, error, cx);
                    cx.notify();
                });
            });
    let switches = h_flex()
        .gap_2()
        .items_center()
        .child(enabled_switch)
        .child(include_pinned_checkbox);
    let failure_icon = if status == hotkey::RegistrationStatus::Failed {
        let tooltip = format!(
            "{}\n{}",
            translator.translate("settings.shortcuts.quick_paste_1_to_9.failed.tooltip"),
            failures.join("、")
        );
        Button::new("settings-shortcuts-quick-paste-1-to-9-failure")
            .ghost()
            .xsmall()
            .icon(ShortcutStatusIcon::Failed)
            .text_color(theme.danger)
            .tooltip(tooltip)
            .into_any_element()
    } else {
        div().into_any_element()
    };
    compact_setting_row(
        theme,
        translator.translate("settings.shortcuts.quick_paste_1_to_9"),
        h_flex()
            .gap_2()
            .items_center()
            .child(failure_icon)
            .child(selector)
            .child(switches),
    )
}

fn shortcut_category(
    category: ShortcutCategory,
    title: String,
    description: String,
    content: impl IntoElement,
    theme: gpui_component::ThemeColor,
    anchor: ScrollAnchor,
    header_control: Option<AnyElement>,
) -> AnyElement {
    let header = h_flex()
        .w_full()
        .min_w_0()
        .min_h(ui_rem(48.0))
        .items_center()
        .gap_3()
        .px_4()
        .py_3()
        .border_b_1()
        .border_color(theme.border)
        .child(
            h_flex()
                .min_w_0()
                .flex_1()
                .items_baseline()
                .gap_3()
                .child(
                    div()
                        .text_size(ui_rem(15.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .whitespace_nowrap()
                        .truncate()
                        .child(description),
                ),
        )
        .when_some(header_control, |this, control| {
            this.child(h_flex().flex_shrink_0().child(control))
        });

    v_flex()
        .id(match category {
            ShortcutCategory::Global => "settings-global-shortcuts-section",
            ShortcutCategory::Temporary => "settings-session-shortcuts-section",
        })
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .overflow_hidden()
        .anchor_scroll(Some(anchor))
        .child(header)
        .child(content)
        .into_any_element()
}

fn shortcut_category_tabs(
    view: Entity<SettingsWindowView>,
    selected_category: ShortcutCategory,
    global_anchor: ScrollAnchor,
    temporary_anchor: ScrollAnchor,
    translator: i18n::Translator,
) -> AnyElement {
    let global_label = translator.translate("settings.shortcuts.global_section");
    let temporary_label = translator.translate("settings.shortcuts.session_section");
    TabBar::new("settings-shortcuts-category-tabs")
        .underline()
        .small()
        .selected_index(match selected_category {
            ShortcutCategory::Global => 0,
            ShortcutCategory::Temporary => 1,
        })
        .children([
            Tab::new().label(global_label),
            Tab::new().label(temporary_label),
        ])
        .on_click(move |index, window, cx| {
            let (category, anchor) = if *index == 0 {
                (ShortcutCategory::Global, global_anchor.clone())
            } else {
                (ShortcutCategory::Temporary, temporary_anchor.clone())
            };
            view.update(cx, |view, cx| {
                view.selected_shortcut_category = category;
                cx.notify();
            });
            anchor.scroll_to(window, cx);
        })
        .into_any_element()
}

#[derive(Clone, Copy)]
enum ShortcutActionIcon {
    Clear,
    Reset,
}

impl IconNamed for ShortcutActionIcon {
    fn path(self) -> SharedString {
        match self {
            Self::Clear => "assets/icons/x.svg",
            Self::Reset => "assets/icons/refresh.svg",
        }
        .into()
    }
}

#[derive(Clone, Copy)]
enum ShortcutStatusIcon {
    Registered,
    Disabled,
    Suspended,
    Inactive,
    Failed,
}

impl IconNamed for ShortcutStatusIcon {
    fn path(self) -> SharedString {
        match self {
            Self::Registered => "assets/icons/check.svg",
            Self::Disabled => "assets/icons/x.svg",
            Self::Suspended => "assets/icons/player-pause.svg",
            Self::Inactive => "assets/icons/player-pause.svg",
            Self::Failed => "assets/icons/exclamation-circle.svg",
        }
        .into()
    }
}

fn shortcut_recorder(
    binding: Option<crate::config::ShortcutBinding>,
    status: hotkey::RegistrationStatus,
    recording: bool,
    view: Entity<SettingsWindowView>,
    focus_handle: FocusHandle,
    save_error: String,
    translator: i18n::Translator,
    theme: gpui_component::ThemeColor,
    target: ShortcutRecordingTarget,
) -> AnyElement {
    let registration_failed = status == hotkey::RegistrationStatus::Failed;
    let clear_disabled = recording || binding.is_none();
    let default_binding = target.default_binding();
    let reset_disabled = recording || binding == default_binding;
    let view_for_click = view.clone();
    let view_for_clear = view.clone();
    let view_for_reset = view.clone();
    let status_icon = match status {
        hotkey::RegistrationStatus::Registered => ShortcutStatusIcon::Registered,
        hotkey::RegistrationStatus::Disabled => ShortcutStatusIcon::Disabled,
        hotkey::RegistrationStatus::Inactive => ShortcutStatusIcon::Inactive,
        hotkey::RegistrationStatus::Suspended => ShortcutStatusIcon::Suspended,
        hotkey::RegistrationStatus::Failed => ShortcutStatusIcon::Failed,
    };
    let status_color = match status {
        hotkey::RegistrationStatus::Registered => theme.success,
        hotkey::RegistrationStatus::Disabled => theme.muted_foreground,
        hotkey::RegistrationStatus::Inactive => theme.muted_foreground,
        hotkey::RegistrationStatus::Suspended => theme.info,
        hotkey::RegistrationStatus::Failed => theme.danger,
    };
    let status_tooltip = match status {
        hotkey::RegistrationStatus::Registered => {
            translator.translate("settings.shortcuts.status.registered.tooltip")
        }
        hotkey::RegistrationStatus::Disabled => {
            translator.translate("settings.shortcuts.status.disabled.tooltip")
        }
        hotkey::RegistrationStatus::Inactive => {
            translator.translate("settings.shortcuts.status.inactive.tooltip")
        }
        hotkey::RegistrationStatus::Suspended => {
            translator.translate("settings.shortcuts.status.recording.tooltip")
        }
        hotkey::RegistrationStatus::Failed => {
            translator.translate("settings.shortcuts.status.failed.tooltip")
        }
    };
    let binding_content = if recording {
        div()
            .text_color(theme.primary)
            .child(translator.translate("settings.shortcuts.recorder.recording"))
            .into_any_element()
    } else if let Some(binding) = binding.as_ref() {
        Kbd::new(hotkey::binding_to_keystroke(binding))
            .appearance(false)
            .into_any_element()
    } else {
        div()
            .text_color(theme.muted_foreground)
            .child(translator.translate("settings.shortcuts.recorder.unassigned"))
            .into_any_element()
    };

    let common_presets_label = translator.translate("settings.shortcuts.common_presets");

    let shortcut_code = target.code();
    let clear_error_text = save_error.clone();
    let clear_button = if binding.is_some() {
        Some(
            Button::new(format!("settings-shortcut-{shortcut_code}-clear"))
                .ghost()
                .xsmall()
                .tab_stop(false)
                .icon(ShortcutActionIcon::Clear)
                .tooltip(translator.translate("settings.shortcuts.action.clear.tooltip"))
                .disabled(clear_disabled)
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    let result = target.set_binding(None, cx);
                    let error = result.err().map(|error| {
                        tracing::error!("清空快捷键失败：{error}");
                        clear_error_text.clone()
                    });
                    view_for_clear.update(cx, |view, cx| {
                        view.shortcut_error =
                            notify_settings_error(SettingsPage::Shortcuts, error, cx);
                        cx.notify();
                    });
                })
                .into_any_element(),
        )
    } else {
        None
    };

    let reset_error_text = save_error.clone();
    let reset_button = Button::new(format!("settings-shortcut-{shortcut_code}-reset"))
        .ghost()
        .small()
        .icon(ShortcutActionIcon::Reset)
        .tooltip(translator.translate("settings.shortcuts.action.reset.tooltip"))
        .disabled(reset_disabled)
        .on_click(move |_, _, cx| {
            let result = target.reset(cx);
            let error = result.err().map(|error| {
                tracing::error!("恢复快捷键默认值失败：{error}");
                reset_error_text.clone()
            });
            view_for_reset.update(cx, |view, cx| {
                view.shortcut_error = notify_settings_error(SettingsPage::Shortcuts, error, cx);
                view.recording_target = None;
                cx.notify();
            });
        });

    let recorder = div()
        .id(format!("settings-shortcut-{shortcut_code}-recorder"))
        .track_focus(&focus_handle)
        .flex()
        .items_center()
        .w(ui_rem(200.0))
        .h_8()
        .gap_1()
        .px_3()
        .rounded(px(6.0))
        .border_1()
        .border_color(if recording {
            theme.primary
        } else if registration_failed {
            theme.danger
        } else {
            theme.input
        })
        .bg(theme.background)
        .text_sm()
        .cursor_pointer()
        .hover(|style| style.border_color(theme.primary).bg(theme.accent))
        .child(
            div()
                .id(format!("settings-shortcut-{shortcut_code}-status"))
                .flex()
                .items_center()
                .justify_center()
                .size_4()
                .tooltip(move |window, cx| Tooltip::new(status_tooltip.clone()).build(window, cx))
                .child(Icon::new(status_icon).xsmall().text_color(status_color)),
        )
        .child(
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .justify_center()
                .child(binding_content),
        )
        .when_some(clear_button, |this, button| this.child(button))
        .on_click(move |_, window, cx| {
            if recording {
                return;
            }
            if let Err(error) = hotkey::begin_toggle_main_window_recording(cx) {
                tracing::error!("进入快捷键录制失败：{error}");
                let _ = notify_settings_error(
                    SettingsPage::Shortcuts,
                    Some(i18n::translator(cx).translate("settings.shortcuts.save_error")),
                    cx,
                );
                return;
            }
            let _ = view_for_click.update(cx, |view, cx| {
                view.recording_target = Some(target);
                view.shortcut_error = None;
                cx.notify();
            });
            window.focus(&focus_handle, cx);
        });

    let recorder_row = h_flex().gap_2().child(recorder).child(reset_button);

    if target.is_main_window() {
        let restore_button = if crate::services::system::win_v_hotkey::is_win_v_hotkey_disabled() {
            Some(restore_system_win_v_button(
                view.clone(),
                save_error.clone(),
                translator.clone(),
                recording,
            ))
        } else {
            None
        };
        v_flex()
            .gap_1()
            .items_end()
            .child(recorder_row)
            .child(common_shortcut_presets(
                view,
                theme,
                save_error,
                common_presets_label,
                translator,
                recording,
            ))
            .when_some(restore_button, |this, button| this.child(button))
            .into_any_element()
    } else {
        recorder_row.into_any_element()
    }
}

fn common_shortcut_presets(
    view: Entity<SettingsWindowView>,
    theme: gpui_component::ThemeColor,
    save_error: String,
    label: String,
    translator: i18n::Translator,
    recording: bool,
) -> AnyElement {
    let presets = [
        (
            "Ctrl+Shift+X",
            crate::config::ShortcutBinding {
                control: true,
                alt: false,
                shift: true,
                platform: false,
                key: "x".to_owned(),
            },
        ),
        (
            "Ctrl+Alt+V",
            crate::config::ShortcutBinding {
                control: true,
                alt: true,
                shift: false,
                platform: false,
                key: "v".to_owned(),
            },
        ),
        (
            "Win+V",
            crate::config::ShortcutBinding {
                control: false,
                alt: false,
                shift: false,
                platform: true,
                key: "v".to_owned(),
            },
        ),
        (
            "F1",
            crate::config::ShortcutBinding {
                control: false,
                alt: false,
                shift: false,
                platform: false,
                key: "f1".to_owned(),
            },
        ),
    ];

    v_flex()
        .gap_1()
        .items_end()
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .children(presets.into_iter().enumerate().map(
                    |(index, (name, binding))| {
                        let view = view.clone();
                        let save_error = save_error.clone();
                        let translator = translator.clone();
                        Button::new(format!("settings-shortcut-preset-{index}"))
                            .ghost()
                            .xsmall()
                            .compact()
                            .label(name)
                            .disabled(recording)
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                #[cfg(target_os = "windows")]
                                if hotkey::is_win_v_binding(&binding)
                                    && !crate::services::system::win_v_hotkey::is_win_v_hotkey_disabled()
                                {
                                    let title = translator
                                        .translate("settings.shortcuts.win_v_replace.title");
                                    let detail = translator
                                        .translate("settings.shortcuts.win_v_replace.detail");
                                    let cancel = gpui::PromptButton::cancel(translator.translate(
                                        "settings.shortcuts.win_v_replace.cancel",
                                    ));
                                    let confirm = gpui::PromptButton::ok(translator.translate(
                                        "settings.shortcuts.win_v_replace.confirm",
                                    ));
                                    let answer = window.prompt(
                                        gpui::PromptLevel::Warning,
                                        &title,
                                        Some(&detail),
                                        &[cancel, confirm],
                                        cx,
                                    );
                                    let replace_error =
                                        translator.translate("settings.shortcuts.win_v_replace.error");
                                    let binding_for_task = binding.clone();
                                    let view_for_task = view.clone();
                                    let save_error_for_task = save_error.clone();
                                    cx.spawn(async move |cx| {
                                        if answer.await.unwrap_or(0) != 1 {
                                            return;
                                        }
                                        let replacement = cx
                                            .background_spawn(async {
                                                crate::services::system::win_v_hotkey::disable_win_v_hotkey()
                                            })
                                            .await;
                                        cx.update(|cx| {
                                            let error = match replacement {
                                                Ok(()) => hotkey::set_toggle_main_window_shortcut(
                                                    Some(binding_for_task),
                                                    cx,
                                                )
                                                .err()
                                                .map(|error| {
                                                    tracing::error!(
                                                        "保存快捷键预设失败：{error}"
                                                    );
                                                    save_error_for_task.clone()
                                                }),
                                                Err(error) => {
                                                    tracing::error!(
                                                        "替换系统 Win+V 失败：{error}"
                                                    );
                                                    Some(replace_error.clone())
                                                }
                                            };
                                            let _ = view_for_task.update(cx, |view, cx| {
                                                view.shortcut_error = notify_settings_error(SettingsPage::Shortcuts, error, cx);
                                                cx.notify();
                                            });
                                        });
                                    })
                                    .detach();
                                    return;
                                }
                                let error = hotkey::set_toggle_main_window_shortcut(Some(
                                    binding.clone(),
                                ), cx)
                                .err()
                                .map(|error| {
                                    tracing::error!("设置快捷键预设失败：{error}");
                                    hotkey::reject_shortcut_recording(true, cx);
                                    save_error.clone()
                                });
                                let _ = view.update(cx, |view, cx| {
                                    view.shortcut_error = notify_settings_error(SettingsPage::Shortcuts, error, cx);
                                    view.recording_target = None;
                                    cx.notify();
                                });
                            })
                    },
                )),
        )
        .into_any_element()
}

fn restore_system_win_v_button(
    view: Entity<SettingsWindowView>,
    save_error: String,
    translator: i18n::Translator,
    recording: bool,
) -> AnyElement {
    let restore_error = translator.translate("settings.shortcuts.win_v_restore.error");
    let save_error_for_restore = save_error.clone();
    Button::new("settings-shortcut-restore-system-win-v")
        .link()
        .xsmall()
        .compact()
        .label(translator.translate("settings.shortcuts.win_v_restore.button"))
        .disabled(recording)
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            let title = translator.translate("settings.shortcuts.win_v_restore.title");
            let detail = translator.translate("settings.shortcuts.win_v_restore.detail");
            let cancel = gpui::PromptButton::cancel(
                translator.translate("settings.shortcuts.win_v_restore.cancel"),
            );
            let confirm = gpui::PromptButton::ok(
                translator.translate("settings.shortcuts.win_v_restore.confirm"),
            );
            let answer = window.prompt(
                gpui::PromptLevel::Warning,
                &title,
                Some(&detail),
                &[cancel, confirm],
                cx,
            );
            let view = view.clone();
            let save_error = save_error_for_restore.clone();
            let restore_error = restore_error.clone();
            cx.spawn(async move |cx| {
                if answer.await.unwrap_or(0) != 1 {
                    return;
                }
                let reset_result = cx.update(hotkey::reset_toggle_main_window_shortcut);
                if let Err(error) = reset_result {
                    tracing::error!("恢复默认快捷键失败：{error}");
                    let _ = view.update(cx, |view, cx| {
                        view.shortcut_error = notify_settings_error(
                            SettingsPage::Shortcuts,
                            Some(save_error.clone()),
                            cx,
                        );
                        cx.notify();
                    });
                    return;
                }
                let result = cx
                    .background_spawn(async {
                        crate::services::system::win_v_hotkey::enable_win_v_hotkey()
                    })
                    .await;
                cx.update(|cx| {
                    let error = result.err().map(|error| {
                        tracing::error!("恢复系统 Win+V 失败：{error}");
                        restore_error.clone()
                    });
                    let _ = view.update(cx, |view, cx| {
                        view.shortcut_error =
                            notify_settings_error(SettingsPage::Shortcuts, error, cx);
                        view.recording_target = None;
                        cx.notify();
                    });
                });
            })
            .detach();
        })
        .into_any_element()
}

fn session_shortcut_row(
    id: SessionShortcutId,
    shortcut: SessionShortcutConfig,
    status: hotkey::RegistrationStatus,
    recording: bool,
    focus_handle: FocusHandle,
    save_error: String,
    translator: i18n::Translator,
    theme: gpui_component::ThemeColor,
    view: Entity<SettingsWindowView>,
) -> AnyElement {
    let label = translator.translate(id.label_key());
    let checkbox_label = translator.translate("settings.shortcuts.only_in_window");
    let checkbox_tooltip = checkbox_label.clone();
    let checkbox_view = view.clone();
    let checkbox_error = save_error.clone();
    let checkbox = Checkbox::new(format!("settings-shortcut-{}-only-in-window", id.code()))
        .checked(shortcut.only_in_window)
        .on_click(move |checked, _, cx| {
            cx.stop_propagation();
            let error = hotkey::set_session_shortcut_only_in_window(id, *checked, cx)
                .err()
                .map(|error| {
                    tracing::error!("更新快捷键窗口范围失败：{error}");
                    checkbox_error.clone()
                });
            checkbox_view.update(cx, |view, cx| {
                view.shortcut_error = notify_settings_error(SettingsPage::Shortcuts, error, cx);
                cx.notify();
            });
        });
    let checkbox = div()
        .id(format!("settings-shortcut-{}-scope-tooltip", id.code()))
        .flex()
        .items_center()
        .justify_center()
        .aria_label(checkbox_label)
        .tooltip(move |window, cx| Tooltip::new(checkbox_tooltip.clone()).build(window, cx))
        .child(checkbox);
    let control = h_flex()
        .gap_2()
        .child(shortcut_recorder(
            shortcut.binding,
            status,
            recording,
            view,
            focus_handle,
            save_error,
            translator,
            theme,
            ShortcutRecordingTarget::Session(id),
        ))
        .child(checkbox);
    compact_setting_row(theme, label, control)
}
