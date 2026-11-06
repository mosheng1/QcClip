use super::super::{SettingsPage, notify_settings_error};
use crate::ui::scale::ui_rem;
use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, PromptButton, PromptLevel, Window};

use super::super::{
    SettingsWindowView,
    components::{settings_dropdown, settings_page_header},
};
use std::{cell::Cell, rc::Rc};

use super::super::{SETTINGS_DROPDOWN_HEIGHT, SETTINGS_DROPDOWN_WIDTH};
use crate::{
    config::{InterfaceLanguage, PasteKeyMode, TrayClickAction},
    config_runtime, i18n,
    platform::elevation,
    platform::window as platform_window,
    ui::hover_icon,
    windows::menu::{DropdownAnchor, DropdownItem, DropdownItemId, DropdownModel, MenuService},
};
use gpui::{Bounds, Pixels, div, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme as _, Sizable as _, v_flex};

impl SettingsWindowView {
    pub(in crate::windows::settings) fn render_general(
        &self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let translator = i18n::translator(cx);
        let language = config_runtime::interface_language(cx);
        let launch_at_login = config_runtime::launch_at_login(cx);
        #[cfg(target_os = "windows")]
        let inactive_window_scroll_compatibility =
            config_runtime::inactive_window_scroll_compatibility(cx);
        let run_as_administrator = config_runtime::run_as_administrator(cx);
        let language_label = translator.translate(language_label_key(language));
        let view = cx.entity().clone();
        let reset_error = translator.translate("settings.general.reset_error");
        let reset_view = view.clone();
        let reset_button = Button::new("settings-general-reset")
            .warning()
            .small()
            .label(translator.translate("settings.action.reset_page"))
            .on_click(move |_, _, cx| {
                let result = config_runtime::reset_general_config(cx);
                let error_message = result.as_ref().err().map(|error| {
                    tracing::error!("重置常规设置失败：{error}");
                    reset_error.clone()
                });
                reset_view.update(cx, |view, cx| {
                    view.language_error =
                        notify_settings_error(SettingsPage::General, error_message, cx);
                    cx.notify();
                });
            });
        let language_options = InterfaceLanguage::ALL
            .into_iter()
            .map(|candidate| {
                (
                    candidate,
                    translator.translate(language_label_key(candidate)),
                )
            })
            .collect::<Vec<_>>();
        let language_menu = DropdownModel::new(
            language_options
                .iter()
                .map(|(candidate, label)| {
                    DropdownItem::option(DropdownItemId::new(candidate.code()), label.clone())
                        .selected(*candidate == language)
                })
                .collect(),
        );
        let launch_save_error = translator.translate("settings.general.launch_save_error");
        let launch_view = view.clone();
        let launch_switch = Switch::new("settings-general-launch-at-login")
            .large()
            .checked(launch_at_login)
            .accessibility_label(translator.translate("settings.general.launch_at_login"))
            .on_click(move |checked, _, cx| {
                let error = config_runtime::set_launch_at_login(*checked, cx)
                    .err()
                    .map(|error| {
                        tracing::error!("保存开机自启设置失败：{error}");
                        launch_save_error.clone()
                    });
                launch_view.update(cx, |view, cx| {
                    view.language_error = notify_settings_error(SettingsPage::General, error, cx);
                    cx.notify();
                });
            });
        #[cfg(target_os = "windows")]
        let scroll_compatibility_save_error =
            translator.translate("settings.general.scroll_compatibility_save_error");
        #[cfg(target_os = "windows")]
        let scroll_compatibility_view = view.clone();
        #[cfg(target_os = "windows")]
        let scroll_compatibility_switch =
            Switch::new("settings-general-inactive-window-scroll-compatibility")
                .large()
                .checked(inactive_window_scroll_compatibility)
                .accessibility_label(
                    translator.translate("settings.general.inactive_window_scroll_compatibility"),
                )
                .on_click(move |checked, _, cx| {
                    let error =
                        config_runtime::set_inactive_window_scroll_compatibility(*checked, cx)
                            .err()
                            .map(|error| {
                                tracing::error!("保存非焦点窗口滚轮兼容设置失败：{error}");
                                scroll_compatibility_save_error.clone()
                            });
                    scroll_compatibility_view.update(cx, |view, cx| {
                        view.language_error =
                            notify_settings_error(SettingsPage::General, error, cx);
                        cx.notify();
                    });
                });
        let administrator_save_error =
            translator.translate("settings.general.administrator_save_error");
        let administrator_view = view.clone();
        let administrator_switch = Switch::new("settings-general-run-as-administrator")
            .large()
            .checked(run_as_administrator)
            .accessibility_label(translator.translate("settings.general.run_as_administrator"))
            .on_click(move |checked, window, cx| {
                handle_run_as_administrator_switch(
                    *checked,
                    administrator_view.clone(),
                    administrator_save_error.clone(),
                    window,
                    cx,
                );
            });
        let tray_save_error = translator.translate("settings.general.tray_save_error");
        let hide_tray_view = view.clone();
        let hide_tray_save_error = tray_save_error.clone();
        let hide_tray_switch = Switch::new("settings-general-hide-tray-icon")
            .large()
            .checked(config_runtime::hide_tray_icon(cx))
            .accessibility_label(translator.translate("settings.general.hide_tray_icon"))
            .on_click(move |checked, _, cx| {
                let error = config_runtime::set_hide_tray_icon(*checked, cx)
                    .err()
                    .map(|error| {
                        tracing::error!("保存隐藏托盘图标设置失败：{error}");
                        hide_tray_save_error.clone()
                    });
                hide_tray_view.update(cx, |view, cx| {
                    view.language_error = notify_settings_error(SettingsPage::General, error, cx);
                    cx.notify();
                });
            });
        let (tray_left_action, tray_right_action) = config_runtime::tray_click_actions(cx);
        let tray_left_selector = tray_click_action_dropdown(
            "settings-general-tray-left-click-select",
            false,
            tray_left_action,
            view.clone(),
            tray_save_error.clone(),
            cx,
        );
        let tray_right_selector = tray_click_action_dropdown(
            "settings-general-tray-right-click-select",
            true,
            tray_right_action,
            view.clone(),
            tray_save_error,
            cx,
        );
        let paste_key_selector = paste_key_mode_dropdown(
            config_runtime::paste_key_mode(cx),
            view.clone(),
            translator.translate("settings.general.save_error"),
            cx,
        );
        let save_error = translator.translate("settings.general.save_error");
        let trigger_bounds = Rc::new(Cell::new(None::<Bounds<Pixels>>));
        let trigger_bounds_for_paint = trigger_bounds.clone();
        let trigger_bounds_for_click = trigger_bounds.clone();
        let selector = div()
            .w(ui_rem(SETTINGS_DROPDOWN_WIDTH))
            .on_children_prepainted(move |bounds, _, _| {
                if let Some(bounds) = bounds.first().copied() {
                    trigger_bounds_for_paint.set(Some(bounds));
                }
            })
            .child(
                div()
                    .id("settings-language-select")
                    .group("settings-language-select")
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(ui_rem(SETTINGS_DROPDOWN_HEIGHT))
                    .w_full()
                    .gap_2()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(theme.input)
                    .bg(theme.background)
                    .px_3()
                    .text_sm()
                    .cursor_pointer()
                    .hover(|style| style.border_color(theme.primary).bg(theme.accent))
                    .active(|style| style.border_color(theme.primary_active))
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
                        let view = view.clone();
                        let save_error = save_error.clone();
                        MenuService::open_dropdown_menu(
                            language_menu.clone(),
                            anchor,
                            move |action_id, cx| {
                                let Some(candidate) = InterfaceLanguage::ALL
                                    .into_iter()
                                    .find(|candidate| candidate.code() == action_id.as_str())
                                else {
                                    return;
                                };
                                let result = config_runtime::set_interface_language(candidate, cx);
                                let error_message = result.as_ref().err().map(|error| {
                                    tracing::error!("保存界面语言失败：{error}");
                                    save_error.clone()
                                });
                                view.update(cx, |view, cx| {
                                    view.language_error = notify_settings_error(
                                        SettingsPage::General,
                                        error_message,
                                        cx,
                                    );
                                    cx.notify();
                                });
                            },
                            cx,
                        );
                    })
                    .child(div().flex_1().truncate().child(language_label))
                    .child(hover_icon(
                        "settings-language-select",
                        "chevron-down",
                        12.0,
                        theme.muted_foreground,
                    )),
            );

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
                translator.translate("settings.page.general"),
                translator.translate("settings.general.description"),
                reset_button,
            ))
            .child(self.setting_row(
                theme,
                translator.translate("settings.general.interface_language"),
                None,
                selector,
            ))
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.general.launch_at_login"),
                    Some(
                        translator
                            .translate("settings.general.launch_at_login_description")
                            .into(),
                    ),
                    launch_switch,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.general.run_as_administrator"),
                    Some(
                        translator
                            .translate("settings.general.run_as_administrator_description")
                            .into(),
                    ),
                    administrator_switch,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.general.paste_key_mode"),
                    Some(
                        translator
                            .translate("settings.general.paste_key_mode_description")
                            .into(),
                    ),
                    paste_key_selector,
                ),
            )
            .child(
                self.setting_row(
                    theme,
                    translator.translate("settings.general.hide_tray_icon"),
                    Some(
                        translator
                            .translate("settings.general.hide_tray_icon_description")
                            .into(),
                    ),
                    hide_tray_switch,
                ),
            )
            .child(self.setting_row(
                theme,
                translator.translate("settings.general.tray_left_click"),
                None,
                tray_left_selector,
            ))
            .child(self.setting_row(
                theme,
                translator.translate("settings.general.tray_right_click"),
                None,
                tray_right_selector,
            ));
        #[cfg(target_os = "windows")]
        {
            section = section.child(
                self.setting_row(
                    theme,
                    translator.translate("settings.general.inactive_window_scroll_compatibility"),
                    Some(
                        translator
                            .translate(
                                "settings.general.inactive_window_scroll_compatibility_description",
                            )
                            .into(),
                    ),
                    scroll_compatibility_switch,
                ),
            );
        }
        if let Some(error) = &self.language_error {
            section = section.child(
                div()
                    .border_t_1()
                    .border_color(theme.border)
                    .px_4()
                    .py_2()
                    .text_sm()
                    .text_color(theme.danger)
                    .child(error.clone()),
            );
        }
        section.into_any_element()
    }
}

fn handle_run_as_administrator_switch(
    checked: bool,
    view: Entity<SettingsWindowView>,
    save_error: String,
    window: &mut Window,
    cx: &mut App,
) {
    if !checked {
        let error = config_runtime::set_run_as_administrator(false, cx)
            .err()
            .map(|error| {
                tracing::error!("保存管理员启动设置失败：{error}");
                save_error.clone()
            });
        view.update(cx, |view, cx| {
            view.language_error = notify_settings_error(SettingsPage::General, error, cx);
            cx.notify();
        });
        return;
    }

    match elevation::is_elevated() {
        Ok(true) => {
            let error = config_runtime::set_run_as_administrator(true, cx)
                .err()
                .map(|error| {
                    tracing::error!("保存管理员启动设置失败：{error}");
                    save_error.clone()
                });
            view.update(cx, |view, cx| {
                view.language_error = notify_settings_error(SettingsPage::General, error, cx);
                cx.notify();
            });
        }
        Ok(false) => {
            let translator = i18n::translator(cx);
            let title = translator.translate("settings.general.administrator_confirm_title");
            let detail = translator.translate("settings.general.administrator_confirm_detail");
            let cancel = PromptButton::cancel(
                translator.translate("settings.general.administrator_confirm_cancel"),
            );
            let restart_later = PromptButton::new(
                translator.translate("settings.general.administrator_confirm_restart_later"),
            );
            let restart_now = PromptButton::ok(
                translator.translate("settings.general.administrator_confirm_restart_now"),
            );
            let answer = window.prompt(
                PromptLevel::Warning,
                &title,
                Some(&detail),
                &[cancel, restart_later, restart_now],
                cx,
            );
            cx.spawn(async move |cx| {
                let choice = answer.await.unwrap_or(0);
                cx.update(|cx| {
                    let (error, should_quit) = match choice {
                        1 => (
                            config_runtime::set_run_as_administrator(true, cx)
                                .err()
                                .map(|error| {
                                    tracing::error!("保存管理员启动设置失败：{error}");
                                    save_error.clone()
                                }),
                            false,
                        ),
                        2 => match config_runtime::set_run_as_administrator(true, cx) {
                            Err(error) => (
                                {
                                    tracing::error!("保存管理员启动设置失败：{error}");
                                    Some(save_error.clone())
                                },
                                false,
                            ),
                            Ok(_) => match elevation::relaunch_elevated() {
                                Ok(()) => (None, true),
                                Err(error) => {
                                    tracing::error!("请求管理员权限重启失败：{error}");
                                    if let Err(restore_error) =
                                        config_runtime::set_run_as_administrator(false, cx)
                                    {
                                        tracing::error!("恢复管理员启动设置失败：{restore_error}");
                                    }
                                    (Some(save_error.clone()), false)
                                }
                            },
                        },
                        _ => (None, false),
                    };
                    if should_quit {
                        cx.quit();
                        return;
                    }
                    let _ = view.update(cx, |view, cx| {
                        view.language_error =
                            notify_settings_error(SettingsPage::General, error, cx);
                        cx.notify();
                    });
                });
            })
            .detach();
        }
        Err(error) => {
            tracing::error!("读取管理员权限状态失败：{error}");
            view.update(cx, |view, cx| {
                view.language_error =
                    notify_settings_error(SettingsPage::General, Some(save_error), cx);
                cx.notify();
            });
        }
    }
}

fn tray_click_action_dropdown(
    id: &'static str,
    right: bool,
    current: TrayClickAction,
    view: Entity<SettingsWindowView>,
    save_error: String,
    cx: &mut Context<SettingsWindowView>,
) -> AnyElement {
    let translator = i18n::translator(cx);
    let items = TrayClickAction::ALL
        .into_iter()
        .map(|action| {
            DropdownItem::option(
                DropdownItemId::new(action.code()),
                translator.translate(tray_click_action_label_key(action)),
            )
            .selected(action == current)
        })
        .collect::<Vec<_>>();
    settings_dropdown(
        id,
        current.code(),
        items,
        move |action_id, cx| {
            let Some(action) = TrayClickAction::ALL
                .into_iter()
                .find(|action| action.code() == action_id.as_str())
            else {
                return;
            };
            let error = config_runtime::set_tray_click_action(right, action, cx)
                .err()
                .map(|error| {
                    tracing::error!("保存托盘点击行为失败：{error}");
                    save_error.clone()
                });
            view.update(cx, |view, cx| {
                view.language_error = notify_settings_error(SettingsPage::General, error, cx);
                cx.notify();
            });
        },
        cx,
    )
}

fn paste_key_mode_dropdown(
    current: PasteKeyMode,
    view: Entity<SettingsWindowView>,
    save_error: String,
    cx: &mut Context<SettingsWindowView>,
) -> AnyElement {
    let translator = i18n::translator(cx);
    let items = PasteKeyMode::ALL
        .into_iter()
        .map(|mode| {
            DropdownItem::option(
                DropdownItemId::new(mode.code()),
                translator.translate(paste_key_mode_label_key(mode)),
            )
            .selected(mode == current)
        })
        .collect::<Vec<_>>();
    settings_dropdown(
        "settings-general-paste-key-mode-select",
        current.code(),
        items,
        move |action_id, cx| {
            let Some(mode) = PasteKeyMode::ALL
                .into_iter()
                .find(|mode| mode.code() == action_id.as_str())
            else {
                return;
            };
            let error = config_runtime::set_paste_key_mode(mode, cx)
                .err()
                .map(|error| {
                    tracing::error!("保存模拟粘贴方式失败：{error}");
                    save_error.clone()
                });
            view.update(cx, |view, cx| {
                view.language_error = notify_settings_error(SettingsPage::General, error, cx);
                cx.notify();
            });
        },
        cx,
    )
}

const fn paste_key_mode_label_key(mode: PasteKeyMode) -> &'static str {
    match mode {
        PasteKeyMode::Auto => "settings.general.paste_key_mode.auto",
        PasteKeyMode::CtrlV => "settings.general.paste_key_mode.ctrl_v",
        PasteKeyMode::ShiftInsert => "settings.general.paste_key_mode.shift_insert",
    }
}

const fn tray_click_action_label_key(action: TrayClickAction) -> &'static str {
    match action {
        TrayClickAction::None => "settings.general.tray_action.none",
        TrayClickAction::ToggleMainWindow => "settings.general.tray_action.toggle_main_window",
        TrayClickAction::OpenMenu => "settings.general.tray_action.open_menu",
        TrayClickAction::OpenSettings => "settings.general.tray_action.open_settings",
        TrayClickAction::OpenClipboard => "settings.general.tray_action.open_clipboard",
        TrayClickAction::OpenFavorites => "settings.general.tray_action.open_favorites",
        TrayClickAction::OpenEmoji => "settings.general.tray_action.open_emoji",
        TrayClickAction::OpenSymbols => "settings.general.tray_action.open_symbols",
        TrayClickAction::OpenGallery => "settings.general.tray_action.open_gallery",
        TrayClickAction::ToggleClipboardMonitor => {
            "settings.general.tray_action.toggle_clipboard_monitor"
        }
        TrayClickAction::ToggleGlobalHotkeys => {
            "settings.general.tray_action.toggle_global_hotkeys"
        }
        TrayClickAction::ToggleFormattedPaste => {
            "settings.general.tray_action.toggle_formatted_paste"
        }
    }
}

const fn language_label_key(language: InterfaceLanguage) -> &'static str {
    match language {
        InterfaceLanguage::ZhCn => "language.zh_cn",
        InterfaceLanguage::EnUs => "language.en_us",
    }
}
