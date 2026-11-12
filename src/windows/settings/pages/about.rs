use crate::ui::scale::ui_rem;

use super::super::{SettingsPage, notify_settings_error};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, FocusHandle, FontWeight, Global, KeyDownEvent,
    ObjectFit, Render, TitlebarOptions, Window, WindowBackgroundAppearance, WindowBounds,
    WindowDecorations, WindowHandle, WindowKind, WindowOptions, div, img, px, size,
};
use gpui_component::Disableable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::Input;
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme as _, Root, Sizable as _, h_flex, v_flex};

use super::super::SettingsWindowView;
use super::super::components::settings_dropdown;
use crate::windows::menu::{DropdownItem, DropdownItemId};
use crate::windows::notification::{NotificationLevel, NotificationService};
use crate::{
    config::{UpdateFrequency, UpdateProxyMode},
    config_runtime,
    services::updates::{DownloadState, UpdateService},
};
use crate::{
    i18n,
    ui::{common::window_title_bar, icon},
};

const GITHUB_URL: &str = "https://github.com/mosheng1/QcClip/issues";
const BILIBILI_URL: &str = "https://space.bilibili.com/438982697";
const COMMUNITY_WINDOW_WIDTH: f32 = 560.0;
const COMMUNITY_WINDOW_HEIGHT: f32 = 336.0;

struct Community {
    title_key: &'static str,
    number: &'static str,
    url: &'static str,
    image: &'static str,
}

const COMMUNITIES: [Community; 3] = [
    Community {
        title_key: "settings.about.channel",
        number: "pd80680380",
        url: "https://pd.qq.com/s/blp3j847c",
        image: "assets/pD_1.png",
    },
    Community {
        title_key: "settings.about.group1",
        number: "725313287",
        url: "https://qm.qq.com/q/nUCO76MX9C",
        image: "assets/qG_1.png",
    },
    Community {
        title_key: "settings.about.group2",
        number: "1033556729",
        url: "https://qm.qq.com/q/O5zOi3uTuy",
        image: "assets/qG_2.png",
    },
];

impl SettingsWindowView {
    pub(in crate::windows::settings) fn render_about(
        &self,
        cx: &mut Context<SettingsWindowView>,
    ) -> AnyElement {
        let header = self.render_about_header(cx);
        let links = self.render_about_links(cx);
        let updates = self.render_about_updates(cx);
        let donation = self.render_about_donation(cx);
        let theme = cx.theme().colors;

        v_flex()
            .w_full()
            .min_w_0()
            .flex_none()
            .text_color(theme.foreground)
            .child(header)
            .child(links)
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_start()
                    .gap(ui_rem(20.0))
                    .border_t_1()
                    .border_color(theme.border)
                    .pt(ui_rem(20.0))
                    .pb_2()
                    .child(updates)
                    .child(donation),
            )
            .into_any_element()
    }

    fn render_about_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let translator = i18n::translator(cx);
        let latest = UpdateService::available(cx);
        let version_text = latest.as_ref().map_or_else(
            || format!("v{}", env!("CARGO_PKG_VERSION")),
            |release| {
                format!(
                    "v{} — {}",
                    env!("CARGO_PKG_VERSION"),
                    release.version_label()
                )
            },
        );

        let version = h_flex()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(version_text),
            )
            .when(latest.is_some(), |view| {
                view.child(
                    Button::new("about-open-release")
                        .ghost()
                        .small()
                        .icon(icon("external-link", 14.0, theme.success))
                        .tooltip(translator.translate("updates.open_release"))
                        .on_click(|_, _, cx| UpdateService::open_release(cx)),
                )
            })
            .child(
                Button::new("about-copy-version")
                    .ghost()
                    .small()
                    .icon(icon("copy", 14.0, theme.muted_foreground))
                    .tooltip(translator.translate("settings.about.copy_version"))
                    .on_click(|_, window, cx| copy_text(env!("CARGO_PKG_VERSION"), window, cx)),
            )
            .into_any_element();

        v_flex()
            .w_full()
            .min_w_0()
            .items_center()
            .gap_3()
            .py_4()
            .child(
                img("assets/icon1024.png")
                    .size(ui_rem(72.0))
                    .object_fit(ObjectFit::Contain),
            )
            .child(
                div()
                    .text_size(ui_rem(24.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("QcClip"),
            )
            .child(version)
            .child(
                div()
                    .w_full()
                    .max_w(ui_rem(440.0))
                    .text_sm()
                    .text_center()
                    .text_color(theme.muted_foreground)
                    .child(translator.translate("settings.about.description")),
            )
            .into_any_element()
    }

    fn render_about_links(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let translator = i18n::translator(cx);

        h_flex()
            .w_full()
            .justify_center()
            .flex_wrap()
            .gap_2()
            .pb_5()
            .child(
                Button::new("about-github")
                    .small()
                    .icon(icon("brand-github", 16.0, theme.foreground))
                    .label(translator.translate("settings.about.github"))
                    .on_click(|_, _, cx| cx.open_url(GITHUB_URL)),
            )
            .child(
                Button::new("about-bilibili")
                    .small()
                    .icon(icon("brand-bilibili", 16.0, theme.foreground))
                    .label(translator.translate("settings.about.bilibili"))
                    .on_click(|_, _, cx| cx.open_url(BILIBILI_URL)),
            )
            .child(
                Button::new("about-community")
                    .primary()
                    .small()
                    .icon(icon("users-group", 16.0, theme.primary_foreground))
                    .label(translator.translate("settings.about.community"))
                    .on_click(|_, window, cx| open_community(window, cx)),
            )
            .child(
                Button::new("about-copy-log")
                    .small()
                    .icon(icon("copy", 16.0, theme.foreground))
                    .label(translator.translate("settings.about.copy_log"))
                    .on_click(|_, _, cx| copy_latest_log(cx)),
            )
            .child(
                Button::new("about-open-onboarding")
                    .small()
                    .icon(icon("sparkles", 16.0, theme.foreground))
                    .label(translator.translate("settings.about.onboarding"))
                    .on_click(|_, _, cx| crate::windows::onboarding::open_onboarding_window(cx)),
            )
            .into_any_element()
    }

    fn render_about_updates(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let translator = i18n::translator(cx);
        let updates = config_runtime::update_config(cx);
        let service = cx.try_global::<UpdateService>();
        let checking = service.is_some_and(|service| service.busy());
        let has_error = service.is_some_and(|service| service.error.is_some());
        let status = service
            .map(|service| {
                service
                    .error
                    .clone()
                    .unwrap_or_else(|| translator.translate(service.status_key))
            })
            .unwrap_or_default();
        let installing = service.is_some_and(|service| service.installing);
        let cancelling = service.is_some_and(|service| service.cancelling());
        let downloading = service
            .is_some_and(|service| matches!(service.download, DownloadState::Downloading(_)));
        let latest = UpdateService::available(cx);
        let ready = service.is_some_and(|service| matches!(&service.download,
            DownloadState::Ready(cached) if latest.as_ref().is_some_and(|r| r.tag_name == cached.tag)));
        let status = if installing {
            translator.translate("updates.installing")
        } else if ready {
            translator.translate("updates.ready")
        } else {
            status
        };
        let download_error = service.and_then(|service| match &service.download {
            DownloadState::Failed(error) => Some(error.clone()),
            _ => None,
        });
        let progress = service.and_then(|service| match &service.download {
            DownloadState::Downloading(progress) => Some(progress.clone()),
            _ => None,
        });
        let auto_download = Switch::new("about-auto-download")
            .small()
            .checked(updates.auto_download)
            .accessibility_label(translator.translate("updates.auto_download"))
            .on_click({
                let view = cx.entity();
                move |checked, _, cx| {
                    let mut config = config_runtime::update_config(cx);
                    config.auto_download = *checked;
                    save_update_settings(&view, config, cx);
                }
            });
        let frequency_menu = settings_dropdown(
            "about-update-frequency",
            updates.frequency.label_key(),
            UpdateFrequency::ALL
                .into_iter()
                .map(|frequency| {
                    DropdownItem::option(
                        DropdownItemId::new(frequency.label_key()),
                        translator.translate(frequency.label_key()),
                    )
                    .selected(frequency == updates.frequency)
                })
                .collect(),
            {
                let view = cx.entity();
                move |id, cx| {
                    let Some(frequency) = UpdateFrequency::ALL
                        .into_iter()
                        .find(|f| f.label_key() == id.as_str())
                    else {
                        return;
                    };
                    let mut config = config_runtime::update_config(cx);
                    config.frequency = frequency;
                    save_update_settings(&view, config, cx);
                }
            },
            cx,
        );
        let modes = [
            ("updates.proxy.system", UpdateProxyMode::System),
            ("updates.proxy.direct", UpdateProxyMode::Direct),
            ("updates.proxy.custom", UpdateProxyMode::Custom),
        ];
        let proxy_menu = settings_dropdown(
            "about-update-proxy",
            modes
                .into_iter()
                .find(|(_, mode)| *mode == self.update_proxy_mode)
                .unwrap()
                .0,
            modes
                .into_iter()
                .map(|(key, mode)| {
                    DropdownItem::option(DropdownItemId::new(key), translator.translate(key))
                        .selected(mode == self.update_proxy_mode)
                })
                .collect(),
            {
                let view = cx.entity();
                move |id, cx| {
                    let Some((_, mode)) = modes.into_iter().find(|(key, _)| *key == id.as_str())
                    else {
                        return;
                    };
                    if mode == UpdateProxyMode::Custom {
                        view.update(cx, |view, cx| {
                            view.update_proxy_mode = mode;
                            view.update_error = None;
                            cx.notify();
                        });
                    } else {
                        let mut config = config_runtime::update_config(cx);
                        config.proxy_mode = mode;
                        if save_update_settings(&view, config, cx) {
                            view.update(cx, |view, cx| {
                                view.update_proxy_mode = mode;
                                cx.notify();
                            });
                        }
                    }
                }
            },
            cx,
        );

        let heading = h_flex()
            .w_full()
            .min_w_0()
            .justify_between()
            .gap_2()
            .mb_2()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(translator.translate("updates.settings")),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("about-check-updates")
                            .small()
                            .disabled(checking)
                            .icon(icon("refresh", 14.0, theme.foreground))
                            .label(translator.translate("updates.check"))
                            .on_click(|_, _, cx| UpdateService::check(true, cx)),
                    )
                    .when(latest.is_some() && !downloading, |view| {
                        view.child(
                            Button::new("about-download-install-update")
                                .small()
                                .primary()
                                .disabled(checking)
                                .label(translator.translate(if installing {
                                    "updates.installing"
                                } else if ready {
                                    "updates.install"
                                } else {
                                    "updates.download"
                                }))
                                .on_click(move |_, _, cx| {
                                    if ready {
                                        UpdateService::install_downloaded(None, cx);
                                    } else {
                                        UpdateService::start_download(true, cx);
                                    }
                                }),
                        )
                    })
                    .when(downloading && !installing, |view| {
                        view.child(
                            Button::new("about-cancel-update-download")
                                .small()
                                .disabled(cancelling)
                                .label(translator.translate(if cancelling {
                                    "updates.cancelling"
                                } else {
                                    "updates.cancel_download"
                                }))
                                .on_click(|_, _, cx| UpdateService::cancel_download(cx)),
                        )
                    }),
            )
            .into_any_element();
        let frequency = about_update_row(
            translator.translate("updates.frequency"),
            frequency_menu,
            cx,
        );
        let proxy = about_update_row(translator.translate("updates.proxy"), proxy_menu, cx);
        let proxy_editor = (self.update_proxy_mode == UpdateProxyMode::Custom)
            .then(|| self.render_about_proxy_editor(cx));

        v_flex()
            .flex_1()
            .min_w_0()
            .child(heading)
            .child(
                div()
                    .w_full()
                    .pt_3()
                    .text_xs()
                    .text_color(if has_error {
                        theme.danger
                    } else {
                        theme.muted_foreground
                    })
                    .child(status),
            )
            .when_some(progress, |view, progress| {
                let fraction = (!progress.probing && progress.total != 0)
                    .then(|| (progress.downloaded as f32 / progress.total as f32).clamp(0.0, 1.0));
                let text = if progress.probing {
                    translator.translate("updates.probing")
                } else if let Some(fraction) = fraction {
                    format!(
                        "{} · {:.0}% · {:.1} / {:.1} MB",
                        progress.source,
                        fraction * 100.0,
                        progress.downloaded as f64 / 1048576.0,
                        progress.total as f64 / 1048576.0
                    )
                } else {
                    format!(
                        "{} · {} · {:.1} MB",
                        progress.source,
                        translator.translate("updates.downloading"),
                        progress.downloaded as f64 / 1048576.0
                    )
                };
                view.child(
                    v_flex()
                        .w_full()
                        .gap_2()
                        .pt_3()
                        .child(div().text_xs().child(text))
                        .when_some(fraction, |view, fraction| {
                            view.child(
                                div()
                                    .w_full()
                                    .h(ui_rem(5.0))
                                    .rounded_md()
                                    .bg(theme.border)
                                    .child(
                                        div()
                                            .h_full()
                                            .w(gpui::relative(fraction))
                                            .rounded_md()
                                            .bg(theme.primary),
                                    ),
                            )
                        })
                        .when(fraction.is_none(), |view| {
                            view.child(gpui_component::spinner::Spinner::new())
                        }),
                )
            })
            .when_some(download_error, |view, error| {
                view.child(div().pt_2().text_xs().text_color(theme.danger).child(error))
            })
            .child(frequency)
            .child(about_update_row(
                translator.translate("updates.auto_download"),
                auto_download,
                cx,
            ))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .pt_2()
                    .child(translator.translate("updates.auto_download_detail")),
            )
            .child(proxy)
            .when_some(proxy_editor, |view, editor| view.child(editor))
            .when_some(self.update_error.clone(), |view, error| {
                view.child(
                    div()
                        .w_full()
                        .pt_2()
                        .text_xs()
                        .text_color(theme.danger)
                        .child(error),
                )
            })
            .into_any_element()
    }

    fn render_about_proxy_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let translator = i18n::translator(cx);
        h_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .pt_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.update_proxy_input).small().w_full()),
            )
            .child(
                Button::new("about-save-proxy")
                    .small()
                    .icon(icon("check", 14.0, theme.foreground))
                    .tooltip(translator.translate("updates.proxy.save"))
                    .on_click({
                        let view = cx.entity();
                        let input = self.update_proxy_input.clone();
                        move |_, _, cx| {
                            let mut config = config_runtime::update_config(cx);
                            config.proxy_mode = UpdateProxyMode::Custom;
                            config.proxy_url = input.read(cx).value().to_string();
                            save_update_settings(&view, config, cx);
                        }
                    }),
            )
            .into_any_element()
    }

    fn render_about_donation(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let translator = i18n::translator(cx);
        v_flex()
            .w(ui_rem(164.0))
            .flex_shrink_0()
            .items_center()
            .gap_3()
            .border_l_1()
            .border_color(theme.border)
            .pl_4()
            .pt_1()
            .child(
                h_flex()
                    .gap_2()
                    .child(icon(
                        "brand-wechat",
                        18.0,
                        gpui::rgb_to_hsla(gpui::rgb(0x07C160)),
                    ))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(translator.translate("settings.about.donation")),
                    ),
            )
            .child(
                img("assets/wxzsm.png")
                    .size(ui_rem(144.0))
                    .object_fit(ObjectFit::Contain),
            )
            .into_any_element()
    }
}

fn about_update_row(title: String, control: impl IntoElement, cx: &App) -> AnyElement {
    h_flex()
        .w_full()
        .min_w_0()
        .min_h(ui_rem(48.0))
        .flex_shrink_0()
        .justify_between()
        .gap_3()
        .py_2()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(div().flex_1().min_w_0().text_sm().child(title))
        .child(div().flex_shrink_0().child(control))
        .into_any_element()
}

fn save_update_settings(
    view: &gpui::Entity<SettingsWindowView>,
    config: crate::config::UpdateConfig,
    cx: &mut App,
) -> bool {
    let result = config_runtime::set_update_config(config, cx);
    let saved = result.is_ok();
    let error = result.err().map(|error| {
        tracing::error!("保存更新设置失败：{error}");
        error.to_string()
    });
    view.update(cx, |view, cx| {
        view.update_error = notify_settings_error(SettingsPage::About, error, cx);
        cx.notify();
    });
    saved
}

fn copy_text(text: &str, _window: &mut Window, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
}

fn copy_latest_log(cx: &mut App) {
    let task = cx.background_spawn(async move {
        let Some(path) =
            crate::logging::latest_log_for_sharing().map_err(|error| error.to_string())?
        else {
            return Ok(false);
        };
        let payload = crate::platform::clipboard::ClipboardPayload {
            format_kind: crate::domain::capture::FormatKind::FileList
                .as_str()
                .to_owned(),
            format_namespace: "windows".to_owned(),
            format_key: "CF_HDROP".to_owned(),
            format_name: "CF_HDROP".to_owned(),
            native_format_id: Some(15),
            bytes: crate::platform::image_temp_file::hdrop_bytes(&path),
        };
        // 使用独立抑制器，让应用监听器正常收录日志文件并显示全局通知。
        let suppressor = crate::platform::clipboard::ClipboardWriteSuppressor::default();
        crate::platform::clipboard::copy_to_clipboard(&[payload], &suppressor)?;
        Ok::<_, String>(true)
    });
    cx.spawn(async move |cx| {
        let result = task.await;
        if let Err(error) = &result {
            tracing::error!("复制最新日志文件失败：{error}");
        }
        cx.update(|cx| {
            let translator = i18n::translator(cx);
            let (message_key, level) = match result {
                Ok(true) => return,
                Ok(false) => ("settings.about.no_log", NotificationLevel::Warning),
                Err(_) => ("settings.about.copy_log_error", NotificationLevel::Error),
            };
            NotificationService::show_message(
                translator.translate("settings.about.copy_log"),
                translator.translate(message_key),
                level,
                cx,
            );
        });
    })
    .detach();
}

fn open_community(window: &mut Window, cx: &mut App) {
    if let Some(existing) = cx
        .try_global::<CommunityWindow>()
        .map(|service| service.window)
        && existing
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        cx.activate(true);
        return;
    }

    let title = i18n::translator(cx).translate("settings.about.community_title");
    let window_size = size(px(COMMUNITY_WINDOW_WIDTH), px(COMMUNITY_WINDOW_HEIGHT));
    let display_id = window.display(cx).map(|display| display.id());
    let result = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                display_id,
                window_size,
                cx,
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some(title.into()),
                appears_transparent: true,
                ..Default::default()
            }),
            kind: WindowKind::Normal,
            focus: true,
            is_movable: true,
            is_resizable: true,
            is_minimizable: true,
            window_background: WindowBackgroundAppearance::Transparent,
            window_min_size: Some(window_size),
            window_decorations: Some(WindowDecorations::Client),
            ..Default::default()
        },
        |window, cx| {
            crate::platform::window::configure_frameless_window(window, true);
            window.bounds_changed(cx);
            crate::window_material::register_window(window, cx);
            let view = cx.new(|cx| {
                let focus_handle = cx.focus_handle();
                window.focus(&focus_handle, cx);
                CommunityDialog {
                    selected: 0,
                    focus_handle,
                }
            });
            cx.new(|cx| crate::window_material::new_root(view, window, cx))
        },
    );
    match result {
        Ok(handle) => {
            cx.set_global(CommunityWindow { window: handle });
            if let Err(error) = handle.update(cx, |_, window, _| window.activate_window()) {
                tracing::error!("激活社区交流窗口失败：{error}");
            }
        }
        Err(error) => {
            tracing::error!("打开社区交流窗口失败：{error}");
            let translator = i18n::translator(cx);
            NotificationService::show_message(
                translator.translate("settings.about.community"),
                translator.translate("settings.about.open_error"),
                NotificationLevel::Error,
                cx,
            );
        }
    }
}

struct CommunityWindow {
    window: WindowHandle<Root>,
}

impl Global for CommunityWindow {}

struct CommunityDialog {
    selected: usize,
    focus_handle: FocusHandle,
}

impl Render for CommunityDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(crate::ui::scale::standalone_rem_size(16.0, cx));
        let theme = cx.theme().colors;
        let translator = i18n::translator(cx);
        let community = &COMMUNITIES[self.selected];

        v_flex()
            .id("about-community-dialog")
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .track_focus(&self.focus_handle)
            .on_key_down(|event: &KeyDownEvent, window, _| {
                if event.keystroke.key == "escape" {
                    window.remove_window();
                }
            })
            .child(window_title_bar(
                translator.translate("settings.about.community_title"),
                window,
                cx,
            ))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p(px(12.0))
                    .child(
                        h_flex()
                            .w_full()
                            .max_w(px(COMMUNITY_WINDOW_WIDTH - 24.0))
                            .items_start()
                            .gap(ui_rem(12.0))
                            .child(
                                v_flex()
                                    .w(ui_rem(224.0))
                                    .flex_none()
                                    .gap(ui_rem(8.0))
                                    .children(COMMUNITIES.iter().enumerate().map(
                                        |(index, entry)| {
                                            let selected = self.selected == index;
                                            v_flex()
                                                .id(format!("community-entry-{index}"))
                                                .w_full()
                                                .h(ui_rem(84.0))
                                                .justify_center()
                                                .gap(ui_rem(6.0))
                                                .p(px(10.0))
                                                .rounded(px(6.0))
                                                .border_1()
                                                .border_color(if selected {
                                                    theme.primary
                                                } else {
                                                    theme.border
                                                })
                                                .bg(if selected {
                                                    theme.accent
                                                } else {
                                                    theme.background
                                                })
                                                .cursor_pointer()
                                                .hover(|style| style.border_color(theme.primary))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.selected = index;
                                                    cx.notify();
                                                }))
                                                .child(
                                                    div()
                                                        .text_size(ui_rem(14.0))
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .child(
                                                            translator.translate(entry.title_key),
                                                        ),
                                                )
                                                .child(
                                                    h_flex()
                                                        .gap_2()
                                                        .child(
                                                            div()
                                                                .flex_1()
                                                                .min_w_0()
                                                                .text_sm()
                                                                .text_color(theme.muted_foreground)
                                                                .child(entry.number),
                                                        )
                                                        .child(
                                                            Button::new(format!(
                                                                "community-copy-number-{index}"
                                                            ))
                                                            .ghost()
                                                            .small()
                                                            .icon(icon(
                                                                "copy",
                                                                16.0,
                                                                theme.muted_foreground,
                                                            ))
                                                            .tooltip(translator.translate(
                                                                "settings.about.copy_number",
                                                            ))
                                                            .on_click(move |_, window, cx| {
                                                                cx.stop_propagation();
                                                                copy_text(entry.number, window, cx);
                                                            }),
                                                        ),
                                                )
                                        },
                                    )),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .items_center()
                                    .gap(ui_rem(8.0))
                                    .child(
                                        img(community.image)
                                            .size(ui_rem(168.0))
                                            .object_fit(ObjectFit::Contain),
                                    )
                                    .child(
                                        div()
                                            .text_size(ui_rem(14.0))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_center()
                                            .child(translator.translate(community.title_key)),
                                    )
                                    .child(
                                        div()
                                            .w_full()
                                            .min_h(ui_rem(32.0))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .px_2()
                                            .rounded(px(6.0))
                                            .bg(theme.muted)
                                            .text_size(ui_rem(12.0))
                                            .text_color(theme.muted_foreground)
                                            .child(community.url),
                                    )
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .gap_2()
                                            .child(
                                                Button::new("community-join")
                                                    .primary()
                                                    .small()
                                                    .flex_1()
                                                    .icon(icon(
                                                        "external-link",
                                                        16.0,
                                                        theme.primary_foreground,
                                                    ))
                                                    .label(
                                                        translator.translate("settings.about.join"),
                                                    )
                                                    .on_click(move |_, _, cx| {
                                                        cx.open_url(community.url)
                                                    }),
                                            )
                                            .child(
                                                Button::new("community-copy-link")
                                                    .small()
                                                    .flex_1()
                                                    .icon(icon("copy", 16.0, theme.foreground))
                                                    .label(
                                                        translator
                                                            .translate("settings.about.copy_link"),
                                                    )
                                                    .on_click(move |_, window, cx| {
                                                        copy_text(community.url, window, cx)
                                                    }),
                                            ),
                                    ),
                            ),
                    ),
            )
    }
}
