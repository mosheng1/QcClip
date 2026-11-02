use gpui::{
    AnyElement, App, AppContext, ColorExt as _, Context, Entity, FontWeight, Subscription, Window,
    div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement as _,
    switch::Switch,
    v_flex,
};

use super::super::{
    SettingsWindowView,
    components::{setting_row, settings_dropdown, settings_page_header},
};
use crate::{
    i18n,
    services::sync::{
        SyncService,
        config::{PairingCode, Policy, clean_name},
        pairing::{Candidate, InvitationStatus},
    },
    ui::scale::ui_rem,
    windows::{
        menu::{DropdownItem, DropdownItemId},
        notification::{NotificationLevel, NotificationService},
    },
};

#[derive(Clone, PartialEq)]
enum SyncPanel {
    Nearby,
    Remote,
    Manual,
    Lan,
    WebDav,
    Device(String),
}

pub(super) fn open_webdav(view: Entity<SettingsWindowView>, window: &mut Window, cx: &mut App) {
    open_sync_panel(view, SyncPanel::WebDav, window, cx);
}

pub(in crate::windows::settings) struct SyncForm {
    name: Entity<InputState>,
    address: Entity<InputState>,
    port: Entity<InputState>,
    code: Entity<InputState>,
    url: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    poll: Entity<InputState>,
    cache_entries: Entity<InputState>,
    cache_mb: Entity<InputState>,
    peer_address: Entity<InputState>,
    exclusions: Entity<InputState>,
    pending: Option<Candidate>,
    short_code: Entity<InputState>,
    invitation: Option<InvitationStatus>,
    verification: Option<(String, String)>,
    panel: Option<SyncPanel>,
    dav_return_remote: bool,
    local_pin: Entity<InputState>,
    busy: bool,
    _url_subscription: Subscription,
}

impl SyncForm {
    pub(in crate::windows::settings) fn new(
        window: &mut Window,
        cx: &mut Context<SettingsWindowView>,
    ) -> Self {
        let config = SyncService::snapshot(cx).config;
        let t = i18n::translator(cx);
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .default_value(
                    config
                        .as_ref()
                        .map(|c| c.dav.password.clone())
                        .unwrap_or_default(),
                )
                .placeholder(t.translate("sync.password_placeholder"))
        });
        let mut input = |value: String, placeholder: &str, cx: &mut Context<SettingsWindowView>| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(value)
                    .placeholder(placeholder.to_owned())
            })
        };
        let invitation = SyncService::get(cx)
            .ok()
            .and_then(|r| r.invitation_status());
        let url = input(
            config
                .as_ref()
                .map(|c| c.dav.url.clone())
                .unwrap_or_default(),
            "https://dav.example.com/remote.php/dav/files/user",
            cx,
        );
        let url_subscription = subscribe_sync_url_changes(&url, cx);
        Self {
            name: input(
                config
                    .as_ref()
                    .map(|c| c.profile.name.clone())
                    .unwrap_or_default(),
                &t.translate("sync.name_placeholder"),
                cx,
            ),
            address: input(String::new(), "192.168.1.10:39827", cx),
            port: input(
                config
                    .as_ref()
                    .map(|c| c.port.to_string())
                    .unwrap_or_else(|| "39827".into()),
                "39827",
                cx,
            ),
            code: input(String::new(), "qcclip1:…", cx),
            url,
            username: input(
                config
                    .as_ref()
                    .map(|c| c.dav.username.clone())
                    .unwrap_or_default(),
                &t.translate("sync.username_placeholder"),
                cx,
            ),
            password,
            poll: input(
                config
                    .as_ref()
                    .map(|c| c.dav.poll_seconds.to_string())
                    .unwrap_or_else(|| "60".into()),
                "1–86400",
                cx,
            ),
            cache_entries: input(
                config
                    .as_ref()
                    .map(|c| c.dav.cache_entries.to_string())
                    .unwrap_or_else(|| "100".into()),
                "100",
                cx,
            ),
            cache_mb: input(
                config
                    .as_ref()
                    .map(|c| c.dav.cache_mb.to_string())
                    .unwrap_or_else(|| "100".into()),
                "100",
                cx,
            ),
            peer_address: input(String::new(), "192.168.1.20:39827", cx),
            exclusions: input(
                String::new(),
                &t.translate("sync.exclusions_placeholder"),
                cx,
            ),
            pending: None,
            short_code: input(String::new(), "ABCD-EFGH-2345-6789", cx),
            invitation,
            verification: None,
            panel: None,
            dav_return_remote: false,
            local_pin: input(String::new(), "000000", cx),
            busy: false,
            _url_subscription: url_subscription,
        }
    }
}

fn subscribe_sync_url_changes<T: 'static>(
    url: &Entity<InputState>,
    cx: &mut Context<T>,
) -> Subscription {
    cx.subscribe(url, |_, _, event: &InputEvent, cx| {
        if matches!(event, InputEvent::Change) {
            cx.notify();
        }
    })
}

fn feedback(
    view: Entity<SettingsWindowView>,
    result: crate::services::sync::Result<()>,
    cx: &mut App,
) {
    feedback_as(view, result, "sync.saved", cx);
}

fn feedback_as(
    view: Entity<SettingsWindowView>,
    result: crate::services::sync::Result<()>,
    success: &str,
    cx: &mut App,
) {
    let t = i18n::translator(cx);
    let (message, level) = match result {
        Ok(()) => (
            t.translate(success),
            if matches!(
                success,
                "sync.invite_waiting"
                    | "sync.device_waiting"
                    | "sync.request_ready"
                    | "sync.review_ready"
                    | "sync.local_requested"
                    | "sync.local_verified"
                    | "sync.local_started"
            ) {
                NotificationLevel::Info
            } else {
                NotificationLevel::Success
            },
        ),
        Err(error) => (error.to_string(), NotificationLevel::Error),
    };
    view.update(cx, |_, cx| cx.notify());
    NotificationService::show_message(t.translate("settings.page.sync"), message, level, cx);
}

enum SyncAction {
    Invited(InvitationStatus),
    Reviewed(Candidate),
    Added {
        verification: Option<(String, String)>,
        hosting: bool,
    },
    Cancelled,
    Tested,
    Pulled(usize),
    LocalRequested,
    LocalVerified,
    LocalAccepted,
    LocalCancelled,
}

fn run_action(
    view: Entity<SettingsWindowView>,
    action: impl FnOnce(
        std::sync::Arc<crate::services::sync::Runtime>,
    ) -> crate::services::sync::Result<SyncAction>
    + Send
    + 'static,
    cx: &mut App,
) {
    if view.read(cx).sync_form.busy {
        return;
    }
    let runtime = match SyncService::get(cx) {
        Ok(runtime) => runtime,
        Err(error) => {
            feedback(view, Err(error), cx);
            return;
        }
    };
    view.update(cx, |view, cx| {
        view.sync_form.busy = true;
        cx.notify();
    });
    let task = cx.background_spawn(async move { action(runtime) });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            view.update(cx, |view, cx| {
                view.sync_form.busy = false;
                cx.notify();
            });
            let success = match result {
                Ok(SyncAction::Invited(status)) => {
                    let ready = status.candidate.is_some();
                    view.update(cx, |view, cx| {
                        view.sync_form.pending = status.candidate.clone();
                        view.sync_form.invitation = Some(status);
                        cx.notify();
                    });
                    if ready {
                        "sync.request_ready"
                    } else {
                        "sync.invite_waiting"
                    }
                }
                Ok(SyncAction::Reviewed(candidate)) => {
                    view.update(cx, |view, cx| {
                        view.sync_form.pending = Some(candidate);
                        cx.notify();
                    });
                    "sync.review_ready"
                }
                Ok(SyncAction::Added {
                    verification,
                    hosting,
                }) => {
                    view.update(cx, |view, cx| {
                        view.sync_form.pending = None;
                        view.sync_form.verification = verification;
                        if hosting {
                            view.sync_form.invitation = None;
                        }
                        cx.notify();
                    });
                    if hosting {
                        "sync.device_added"
                    } else {
                        "sync.device_waiting"
                    }
                }
                Ok(SyncAction::Cancelled) => {
                    view.update(cx, |view, cx| {
                        view.sync_form.invitation = None;
                        view.sync_form.pending = None;
                        cx.notify();
                    });
                    "sync.invite_cancelled"
                }
                Ok(SyncAction::Tested) => "sync.connection_ok",
                Ok(SyncAction::Pulled(count)) => {
                    if count == 0 {
                        "sync.pull_empty"
                    } else {
                        "sync.pull_queued"
                    }
                }
                Ok(SyncAction::LocalRequested) => "sync.local_requested",
                Ok(SyncAction::LocalVerified) => "sync.local_verified",
                Ok(SyncAction::LocalAccepted) => "sync.device_added",
                Ok(SyncAction::LocalCancelled) => "sync.local_rejected",
                Err(error) => {
                    feedback(view, Err(error), cx);
                    return;
                }
            };
            feedback_as(view, Ok(()), success, cx);
        });
    })
    .detach();
}

fn policy_switch(
    id: &str,
    value: bool,
    peer: &str,
    field: fn(&mut Policy) -> &mut bool,
    view: Entity<SettingsWindowView>,
) -> Switch {
    let peer = peer.to_owned();
    Switch::new(id.to_owned())
        .checked(value)
        .on_click(move |checked, _, cx| {
            let result = SyncService::get(cx).and_then(|r| {
                r.change(
                    |c| {
                        let p = c
                            .peers
                            .iter_mut()
                            .find(|p| p.profile.id == peer)
                            .ok_or_else(|| {
                                crate::services::sync::Error::Message("设备已被撤销".into())
                            })?;
                        *field(&mut p.policy) = *checked;
                        Ok(())
                    },
                    true,
                )
            });
            feedback(view.clone(), result, cx);
        })
}

fn card(cx: &App) -> gpui::Div {
    v_flex()
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .rounded(px(10.0))
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().background)
        .overflow_hidden()
}

fn heading(text: String, description: String, cx: &App) -> gpui::Div {
    v_flex()
        .gap_1()
        .px_4()
        .py_3()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(text),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(description),
        )
}

impl SettingsWindowView {
    pub(in crate::windows::settings) fn render_sync(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let t = i18n::translator(cx);
        let view = cx.entity();
        let snapshot = SyncService::snapshot(cx);
        let section = v_flex()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .overflow_hidden()
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.title_bar);
        let Some(config) = snapshot.config.as_ref() else {
            return section
                .child(settings_page_header(
                    theme,
                    t.translate("settings.page.sync"),
                    t.translate("sync.description"),
                    div(),
                ))
                .child(
                    div()
                        .px_4()
                        .py_3()
                        .text_sm()
                        .text_color(theme.danger)
                        .child(snapshot.message),
                )
                .into_any_element();
        };
        let online = config
            .peers
            .iter()
            .filter(|p| {
                snapshot
                    .statuses
                    .get(&p.profile.id)
                    .is_some_and(|s| s.channel(config, p) == Some("LAN"))
            })
            .count();
        let enabled = config.enabled;
        let header = settings_page_header(
            theme,
            t.translate("settings.page.sync"),
            t.translate("sync.description"),
            Switch::new("sync-enabled").checked(enabled).on_click({
                let view = view.clone();
                move |checked, _, cx| {
                    feedback(
                        view.clone(),
                        SyncService::get(cx).and_then(|r| {
                            r.change(
                                |c| {
                                    c.enabled = *checked;
                                    Ok(())
                                },
                                true,
                            )
                        }),
                        cx,
                    );
                }
            }),
        );
        let status =
            h_flex()
                .w_full()
                .items_center()
                .gap_3()
                .p_4()
                .rounded(px(10.0))
                .bg(if enabled {
                    theme.primary.opacity(0.08)
                } else {
                    theme.muted.opacity(0.3)
                })
                .child(crate::ui::icon::icon(
                    "shield-check",
                    24.0,
                    if enabled {
                        theme.primary
                    } else {
                        theme.muted_foreground
                    },
                ))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(
                            t.translate(if enabled {
                                "sync.active"
                            } else {
                                "sync.paused"
                            }),
                        ))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(t.translate("sync.online_only")),
                        ),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "{online} / {} {}",
                            config.peers.len(),
                            t.translate("sync.online_count")
                        )),
                );

        let local = &snapshot.local_pairing;
        let adding =
            local.active || self.sync_form.invitation.is_some() || self.sync_form.pending.is_some();
        let add_help = local
            .request
            .as_ref()
            .filter(|request| !request.added && !request.rejected)
            .map(|request| {
                format!(
                    "{} · {}",
                    request.profile.name,
                    t.translate("sync.pairing_in_progress")
                )
            })
            .unwrap_or_else(|| t.translate("sync.add_overview_help"));
        let channels = format!(
            "{} · {}",
            t.translate(if config.lan_enabled {
                "sync.lan_on"
            } else {
                "sync.lan_off"
            }),
            t.translate(if config.dav.enabled {
                "sync.dav_on"
            } else {
                "sync.dav_off"
            })
        );
        let actions = card(cx)
            .child(setting_row(
                theme,
                t.translate("sync.add_device"),
                Some(add_help.into()),
                Button::new("sync-add-device")
                    .small()
                    .primary()
                    .label(t.translate(if adding {
                        "sync.continue_adding"
                    } else {
                        "sync.add_device"
                    }))
                    .on_click({
                        let view = view.clone();
                        move |_, window, cx| {
                            let local_pending = SyncService::snapshot(cx)
                                .local_pairing
                                .request
                                .is_some_and(|request| !request.added && !request.rejected);
                            let form = &view.read(cx).sync_form;
                            let panel = if local_pending {
                                SyncPanel::Nearby
                            } else if form.pending.as_ref().is_some_and(|p| !p.remote()) {
                                SyncPanel::Manual
                            } else if form.invitation.is_some()
                                || form.pending.is_some()
                                || form.verification.is_some()
                            {
                                SyncPanel::Remote
                            } else {
                                SyncPanel::Nearby
                            };
                            open_sync_panel(view.clone(), panel, window, cx);
                        }
                    }),
            ))
            .child(setting_row(
                theme,
                t.translate("sync.channels"),
                Some(channels.into()),
                Button::new("sync-connection-settings")
                    .small()
                    .ghost()
                    .label(t.translate("sync.configure_connections"))
                    .on_click({
                        let view = view.clone();
                        move |_, window, cx| {
                            open_sync_panel(view.clone(), SyncPanel::Lan, window, cx)
                        }
                    }),
            ))
            .when(config.dav.enabled, |card| {
                card.child(setting_row(
                    theme,
                    t.translate("sync.pull_now"),
                    Some(t.translate("sync.pull_now_help").into()),
                    Button::new("sync-pull-now")
                        .small()
                        .primary()
                        .disabled(!config.enabled || self.sync_form.busy)
                        .label(t.translate(if self.sync_form.busy {
                            "sync.working"
                        } else {
                            "sync.pull_now"
                        }))
                        .on_click({
                            let view = view.clone();
                            move |_, _, cx| {
                                run_action(
                                    view.clone(),
                                    |runtime| Ok(SyncAction::Pulled(runtime.pull_dav()?)),
                                    cx,
                                );
                            }
                        }),
                ))
            })
            .when(config.dav.enabled, |card| {
                card.child(
                    div()
                        .px_4()
                        .pb_3()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(t.translate_with_args(
                            "sync.request_count",
                            &[("count", snapshot.dav_requests.to_string())],
                        )),
                )
            });
        section
            .child(header)
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_4()
                    .p_4()
                    .child(status)
                    .when(!snapshot.message.is_empty(), |v| {
                        v.child(
                            div()
                                .text_xs()
                                .text_color(theme.danger)
                                .child(snapshot.message.clone()),
                        )
                    })
                    .child(self.render_sync_identity(cx))
                    .child(actions)
                    .child(
                        card(cx).child(setting_row(
                            theme,
                            t.translate("sync.copy_on_receive"),
                            Some(t.translate("sync.copy_on_receive_help").into()),
                            Switch::new("sync-copy-on-receive")
                                .checked(config.copy_on_receive)
                                .on_click({
                                    let view = view.clone();
                                    move |checked, _, cx| {
                                        feedback(
                                            view.clone(),
                                            SyncService::get(cx).and_then(|runtime| {
                                                runtime.change(
                                                    |config| {
                                                        config.copy_on_receive = *checked;
                                                        Ok(())
                                                    },
                                                    false,
                                                )
                                            }),
                                            cx,
                                        );
                                    }
                                }),
                        )),
                    )
                    .child(self.render_sync_devices(config, &snapshot, cx))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(t.translate("sync.limits_help")),
                    ),
            )
            .into_any_element()
    }
}

impl SettingsWindowView {
    fn render_sync_identity(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let t = i18n::translator(cx);
        let view = cx.entity();
        let identity = card(cx).child(setting_row(
            theme,
            t.translate("sync.device_name"),
            Some(t.translate("sync.device_name_help").into()),
            h_flex()
                .gap_2()
                .child(Input::new(&self.sync_form.name).small().w(ui_rem(180.0)))
                .child(
                    Button::new("sync-name-save")
                        .small()
                        .label(t.translate("sync.save"))
                        .on_click({
                            let view = view.clone();
                            let input = self.sync_form.name.clone();
                            move |_, _, cx| {
                                let name = clean_name(&input.read(cx).value());
                                feedback(
                                    view.clone(),
                                    SyncService::get(cx).and_then(|r| {
                                        r.change(
                                            |c| {
                                                c.profile.name = name;
                                                Ok(())
                                            },
                                            false,
                                        )
                                    }),
                                    cx,
                                );
                            }
                        }),
                ),
        ));

        identity.into_any_element()
    }
    fn render_sync_lan(
        &self,
        config: &crate::services::sync::config::Config,
        snapshot: &crate::services::sync::Snapshot,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let t = i18n::translator(cx);
        let view = cx.entity();
        let mut lan = card(cx)
            .child(heading(
                t.translate("sync.lan_title"),
                t.translate("sync.lan_description"),
                cx,
            ))
            .child(setting_row(
                theme,
                t.translate("sync.lan_enabled"),
                None,
                Switch::new("sync-lan-enabled")
                    .checked(config.lan_enabled)
                    .on_click({
                        let view = view.clone();
                        move |checked, _, cx| {
                            feedback(
                                view.clone(),
                                SyncService::get(cx).and_then(|r| {
                                    r.change(
                                        |c| {
                                            c.lan_enabled = *checked;
                                            Ok(())
                                        },
                                        true,
                                    )
                                }),
                                cx,
                            );
                        }
                    }),
            ))
            .child(setting_row(
                theme,
                t.translate("sync.listen_port"),
                Some(t.translate("sync.port_help").into()),
                h_flex()
                    .gap_2()
                    .child(Input::new(&self.sync_form.port).small().w(ui_rem(80.0)))
                    .child(
                        Button::new("sync-port-save")
                            .small()
                            .label(t.translate("sync.save"))
                            .on_click({
                                let view = view.clone();
                                let input = self.sync_form.port.clone();
                                move |_, _, cx| {
                                    let port = input.read(cx).value().parse::<u16>();
                                    let result = port
                                        .map_err(|_| {
                                            crate::services::sync::Error::Message(
                                                "监听端口必须是 1–65535 的整数".into(),
                                            )
                                        })
                                        .and_then(|port| {
                                            SyncService::get(cx)?.change(
                                                |c| {
                                                    c.port = port;
                                                    Ok(())
                                                },
                                                true,
                                            )
                                        });
                                    feedback(view.clone(), result, cx);
                                }
                            }),
                    ),
            ))
            .child(
                v_flex()
                    .gap_2()
                    .p_4()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(t.translate("sync.lan_address_help")),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Input::new(&self.sync_form.address).small().flex_1())
                            .child(
                                Button::new("sync-copy-lan-code")
                                    .small()
                                    .label(t.translate("sync.copy_lan_code"))
                                    .on_click({
                                        let view = view.clone();
                                        let input = self.sync_form.address.clone();
                                        move |_, _, cx| {
                                            let result = SyncService::get(cx).and_then(|r| {
                                                let config = r.config();
                                                let address =
                                                    input.read(cx).value().trim().to_owned();
                                                crate::services::sync::config::validate_address(
                                                    &address,
                                                )?;
                                                let code = PairingCode {
                                                    profile: config.profile,
                                                    address,
                                                }
                                                .encode()?;
                                                cx.write_to_clipboard(
                                                    gpui::ClipboardItem::new_string(code),
                                                );
                                                Ok(())
                                            });
                                            feedback_as(view.clone(), result, "sync.copied", cx);
                                        }
                                    }),
                            ),
                    ),
            );

        if config.lan_enabled && !snapshot.discovered.is_empty() {
            lan = lan.child(
                div()
                    .px_4()
                    .pb_3()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!(
                        "{} {}",
                        t.translate("sync.discovered"),
                        snapshot.discovered.join(" · ")
                    )),
            );
        }

        lan.into_any_element()
    }
    fn render_remote_pairing(
        &self,
        config: &crate::services::sync::config::Config,
        snapshot: &crate::services::sync::Snapshot,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let t = i18n::translator(cx);
        let view = cx.entity();
        let busy = self.sync_form.busy;
        let manual = self.sync_form.panel == Some(SyncPanel::Manual);
        let mut pairing = card(cx).child(heading(
            t.translate(if manual {
                "sync.manual_mode"
            } else {
                "sync.pair_device"
            }),
            t.translate(if manual {
                "sync.pair_help"
            } else {
                "sync.add_help"
            }),
            cx,
        ));
        if manual {
            pairing = pairing
                .child(
                    v_flex()
                        .gap_2()
                        .p_4()
                        .border_t_1()
                        .border_color(theme.border)
                        .child(
                            Button::new("sync-copy-code")
                                .small()
                                .primary()
                                .label(t.translate("sync.copy_code"))
                                .on_click({
                                    let view = view.clone();
                                    move |_, _, cx| {
                                        let result = SyncService::get(cx).and_then(|r| {
                                            let code = PairingCode {
                                                profile: r.config().profile,
                                                address: String::new(),
                                            }
                                            .encode()?;
                                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                                code,
                                            ));
                                            Ok(())
                                        });
                                        feedback_as(view.clone(), result, "sync.copied", cx);
                                    }
                                }),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!(
                                    "{} {}",
                                    t.translate("sync.fingerprint"),
                                    PairingCode {
                                        profile: config.profile.clone(),
                                        address: String::new()
                                    }
                                    .fingerprint()
                                )),
                        ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .p_4()
                        .child(Input::new(&self.sync_form.code).small().flex_1())
                        .child(
                            Button::new("sync-review-code")
                                .small()
                                .label(t.translate("sync.review"))
                                .disabled(busy)
                                .on_click({
                                    let view = view.clone();
                                    let input = self.sync_form.code.clone();
                                    move |_, _, cx| {
                                        let decoded = PairingCode::decode(&input.read(cx).value());
                                        match decoded {
                                            Ok(code) => {
                                                view.update(cx, |view, cx| {
                                                    view.sync_form.pending =
                                                        Some(Candidate::manual(code));
                                                    cx.notify();
                                                });
                                            }
                                            Err(error) => feedback(view.clone(), Err(error), cx),
                                        }
                                    }
                                }),
                        ),
                );
        } else {
            pairing = pairing.child(
                v_flex()
                    .gap_3()
                    .p_4()
                    .child(div().text_sm().child(t.translate("sync.short_help")))
                    .child(
                        Button::new("sync-create-invite")
                            .small()
                            .primary()
                            .disabled(
                                busy || config.dav.url.is_empty()
                                    || self.sync_form.invitation.is_some(),
                            )
                            .label(t.translate("sync.create_invite"))
                            .on_click({
                                let view = view.clone();
                                move |_, _, cx| {
                                    if let Err(error) = save_dav(&view, None, cx) {
                                        feedback(view.clone(), Err(error), cx);
                                        return;
                                    }
                                    run_action(
                                        view.clone(),
                                        |runtime| {
                                            runtime.create_invitation().map(SyncAction::Invited)
                                        },
                                        cx,
                                    );
                                }
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(t.translate("sync.enter_invite")),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Input::new(&self.sync_form.short_code).small().flex_1())
                            .child(
                                Button::new("sync-join-invite")
                                    .small()
                                    .label(t.translate("sync.find_device"))
                                    .disabled(busy || config.dav.url.is_empty())
                                    .on_click({
                                        let view = view.clone();
                                        let input = self.sync_form.short_code.clone();
                                        move |_, _, cx| {
                                            let code = input.read(cx).value().trim().to_owned();
                                            if let Err(error) = save_dav(&view, None, cx) {
                                                feedback(view.clone(), Err(error), cx);
                                                return;
                                            }
                                            run_action(
                                                view.clone(),
                                                move |runtime| {
                                                    runtime
                                                        .lookup_invitation(&code)
                                                        .map(SyncAction::Reviewed)
                                                },
                                                cx,
                                            );
                                        }
                                    }),
                            ),
                    ),
            );
            if let Some(invitation) = &self.sync_form.invitation {
                let remaining = invitation
                    .expires
                    .saturating_sub(crate::services::sync::protocol::now());
                pairing = pairing.child(
                    v_flex()
                        .gap_2()
                        .p_4()
                        .bg(theme.primary.opacity(0.06))
                        .child(
                            div()
                                .text_lg()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(invitation.code.clone()),
                        )
                        .child(div().text_xs().child(if remaining > 0 {
                            t.translate("sync.invite_expiry")
                        } else {
                            t.translate("sync.invite_expired")
                        }))
                        .child(
                            h_flex()
                                .flex_wrap()
                                .gap_2()
                                .child(
                                    Button::new("sync-copy-invite")
                                        .small()
                                        .label(t.translate("sync.copy_invite"))
                                        .disabled(remaining == 0)
                                        .on_click({
                                            let view = view.clone();
                                            let code = invitation.code.clone();
                                            move |_, _, cx| {
                                                cx.write_to_clipboard(
                                                    gpui::ClipboardItem::new_string(code.clone()),
                                                );
                                                feedback_as(
                                                    view.clone(),
                                                    Ok(()),
                                                    "sync.copied",
                                                    cx,
                                                );
                                            }
                                        }),
                                )
                                .child(
                                    Button::new("sync-check-invite")
                                        .small()
                                        .primary()
                                        .label(t.translate("sync.check_request"))
                                        .disabled(busy || remaining == 0)
                                        .on_click({
                                            let view = view.clone();
                                            move |_, _, cx| {
                                                run_action(
                                                    view.clone(),
                                                    |runtime| {
                                                        runtime
                                                            .check_invitation()
                                                            .map(SyncAction::Invited)
                                                    },
                                                    cx,
                                                );
                                            }
                                        }),
                                )
                                .child(
                                    Button::new("sync-cancel-invite")
                                        .small()
                                        .ghost()
                                        .label(t.translate("sync.cancel_invite"))
                                        .disabled(busy)
                                        .on_click({
                                            let view = view.clone();
                                            move |_, _, cx| {
                                                run_action(
                                                    view.clone(),
                                                    |runtime| {
                                                        runtime.cancel_invitation()?;
                                                        Ok(SyncAction::Cancelled)
                                                    },
                                                    cx,
                                                );
                                            }
                                        }),
                                ),
                        ),
                );
            }
        }
        if let Some((peer_id, verification)) = &self.sync_form.verification
            && config.peers.iter().any(|peer| {
                &peer.profile.id == peer_id
                    && snapshot
                        .statuses
                        .get(peer_id)
                        .is_none_or(|status| status.channel(config, peer).is_none())
            })
        {
            pairing = pairing.child(heading(
                t.translate("sync.waiting_confirmation"),
                format!("{} {verification}", t.translate("sync.verification")),
                cx,
            ));
        }
        if let Some(pending) = &self.sync_form.pending {
            let verification = pending.verification(&config.profile);
            pairing =
                pairing.child(
                    v_flex()
                        .gap_2()
                        .p_4()
                        .border_t_1()
                        .border_color(theme.border)
                        .bg(theme.primary.opacity(0.05))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(pending.device.profile.label()),
                        )
                        .child(div().text_lg().child(format!(
                            "{} {verification}",
                            t.translate("sync.verification")
                        )))
                        .child(div().text_xs().text_color(theme.muted_foreground).child(
                            t.translate(if pending.remote() {
                                "sync.short_verify_help"
                            } else {
                                "sync.verify_help"
                            }),
                        ))
                        .when(!pending.remote(), |v| {
                            v.child(div().text_xs().child(pending.device.fingerprint()))
                        })
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("sync-confirm-pair")
                                        .small()
                                        .primary()
                                        .disabled(busy)
                                        .label(t.translate("sync.trust"))
                                        .on_click({
                                            let view = view.clone();
                                            let candidate = pending.clone();
                                            let local = config.profile.clone();
                                            move |_, _, cx| {
                                                let candidate = candidate.clone();
                                                let hosting =
                                                    !candidate.remote() || candidate.hosting();
                                                let verification = (!hosting).then(|| {
                                                    (
                                                        candidate.device.profile.id.clone(),
                                                        candidate.verification(&local),
                                                    )
                                                });
                                                run_action(
                                                    view.clone(),
                                                    move |runtime| {
                                                        runtime.confirm_candidate(candidate)?;
                                                        Ok(SyncAction::Added {
                                                            verification,
                                                            hosting,
                                                        })
                                                    },
                                                    cx,
                                                );
                                            }
                                        }),
                                )
                                .child(
                                    Button::new("sync-dismiss-candidate")
                                        .small()
                                        .ghost()
                                        .label(t.translate("sync.cancel"))
                                        .disabled(busy)
                                        .on_click({
                                            let view = view.clone();
                                            move |_, _, cx| {
                                                view.update(cx, |view, cx| {
                                                    view.sync_form.pending = None;
                                                    cx.notify();
                                                });
                                            }
                                        }),
                                ),
                        ),
                );
        }
        pairing = pairing.child(
            Button::new("sync-pairing-alternative")
                .small()
                .ghost()
                .label(t.translate(if manual {
                    "sync.use_remote_code"
                } else {
                    "sync.manual_advanced"
                }))
                .disabled(busy)
                .on_click({
                    let view = view.clone();
                    move |_, _, cx| {
                        view.update(cx, |view, cx| {
                            view.sync_form.panel = Some(if manual {
                                SyncPanel::Remote
                            } else {
                                SyncPanel::Manual
                            });
                            cx.notify();
                        })
                    }
                }),
        );
        pairing.into_any_element()
    }
    fn render_sync_dav(
        &self,
        config: &crate::services::sync::config::Config,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let t = i18n::translator(cx);
        let view = cx.entity();
        let busy = self.sync_form.busy;
        let uses_http = self
            .sync_form
            .url
            .read(cx)
            .value()
            .trim()
            .to_ascii_lowercase()
            .starts_with("http://");
        let dav = card(cx)
            .child(heading(
                t.translate("sync.webdav"),
                t.translate("sync.dav_help"),
                cx,
            ))
            .child(setting_row(
                theme,
                t.translate("sync.dav_enabled"),
                None,
                Switch::new("sync-dav-enabled")
                    .checked(config.dav.enabled)
                    .disabled(busy)
                    .on_click({
                        let view = view.clone();
                        move |checked, _, cx| {
                            let checked = *checked;
                            let result = if checked {
                                save_dav(&view, Some(true), cx)
                            } else {
                                SyncService::get(cx).and_then(|r| {
                                    r.change(
                                        |c| {
                                            c.dav.enabled = false;
                                            Ok(())
                                        },
                                        true,
                                    )
                                })
                            };
                            feedback(view.clone(), result, cx);
                        }
                    }),
            ))
            .child(setting_row(
                theme,
                t.translate("sync.auto_upload"),
                Some(t.translate("sync.auto_upload_help").into()),
                Switch::new("sync-dav-auto-upload")
                    .checked(config.dav.auto_upload)
                    .disabled(busy)
                    .on_click({
                        let view = view.clone();
                        move |checked, _, cx| {
                            feedback(
                                view.clone(),
                                SyncService::get(cx).and_then(|runtime| {
                                    runtime.change(
                                        |c| {
                                            c.dav.auto_upload = *checked;
                                            Ok(())
                                        },
                                        true,
                                    )
                                }),
                                cx,
                            );
                        }
                    }),
            ))
            .child(setting_row(
                theme,
                t.translate("sync.auto_pull"),
                Some(t.translate("sync.auto_pull_help").into()),
                Switch::new("sync-dav-auto-pull")
                    .checked(config.dav.auto_pull)
                    .disabled(busy)
                    .on_click({
                        let view = view.clone();
                        move |checked, _, cx| {
                            feedback(
                                view.clone(),
                                SyncService::get(cx).and_then(|runtime| {
                                    runtime.change(
                                        |c| {
                                            c.dav.auto_pull = *checked;
                                            Ok(())
                                        },
                                        true,
                                    )
                                }),
                                cx,
                            );
                        }
                    }),
            ))
            .child(
                v_flex()
                    .gap_3()
                    .p_4()
                    .child(div().text_xs().child(t.translate("sync.server_address")))
                    .child(Input::new(&self.sync_form.url).small().w_full())
                    .when(uses_http, |section| {
                        section.child(
                            div()
                                .text_xs()
                                .text_color(theme.warning)
                                .child(t.translate("sync.dav_http_warning")),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(t.translate("sync.dav_url_help")),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Input::new(&self.sync_form.username).small().flex_1())
                            .child(
                                Input::new(&self.sync_form.password)
                                    .small()
                                    .mask_toggle()
                                    .flex_1(),
                            ),
                    )
                    .child(div().text_xs().child(t.translate("sync.pull_presets")))
                    .child(
                        h_flex().gap_2().children(
                            [
                                ("sync-economical", "sync.preset_economical", Some(60u64)),
                                ("sync-fast", "sync.preset_fast", Some(2u64)),
                                ("sync-manual-pull", "sync.preset_manual", None),
                            ]
                            .into_iter()
                            .map(|(id, label, seconds)| {
                                let view = view.clone();
                                Button::new(id)
                                    .small()
                                    .disabled(busy)
                                    .label(t.translate(label))
                                    .on_click(move |_, window, cx| {
                                        if let Some(seconds) = seconds {
                                            let input = view.read(cx).sync_form.poll.clone();
                                            input.update(cx, |input, cx| {
                                                input.set_value(seconds.to_string(), window, cx)
                                            });
                                        }
                                        feedback(
                                            view.clone(),
                                            SyncService::get(cx).and_then(|runtime| {
                                                runtime.change(
                                                    |c| {
                                                        c.dav.auto_pull = seconds.is_some();
                                                        if let Some(seconds) = seconds {
                                                            c.dav.poll_seconds = seconds;
                                                        }
                                                        Ok(())
                                                    },
                                                    true,
                                                )
                                            }),
                                            cx,
                                        );
                                    })
                            }),
                        ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().text_xs().child(t.translate("sync.cache_entries")))
                            .child(
                                Input::new(&self.sync_form.cache_entries)
                                    .small()
                                    .w(ui_rem(70.0)),
                            )
                            .child(div().text_xs().child(t.translate("sync.cache_mb")))
                            .child(Input::new(&self.sync_form.cache_mb).small().w(ui_rem(70.0))),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(t.translate("sync.cache_help")),
                    )
                    .child(div().text_xs().text_color(theme.muted_foreground).child(
                        if config.dav.auto_pull {
                            let peers = config
                                .peers
                                .iter()
                                .filter(|peer| config.dav_allowed(peer))
                                .count();
                            t.translate_with_args(
                                "sync.request_estimate",
                                &[(
                                    "count",
                                    ((1800 * peers as u64)
                                        .div_ceil(config.dav.poll_seconds.max(1)))
                                    .to_string(),
                                )],
                            )
                        } else {
                            t.translate("sync.manual_pull_help")
                        },
                    ))
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().text_xs().child(t.translate("sync.poll")))
                            .child(Input::new(&self.sync_form.poll).small().w(ui_rem(65.0)))
                            .child(
                                Button::new("sync-save-dav")
                                    .small()
                                    .disabled(busy)
                                    .primary()
                                    .label(t.translate("sync.save"))
                                    .on_click({
                                        let view = view.clone();
                                        move |_, _, cx| {
                                            let result = save_dav(&view, None, cx);
                                            feedback(view.clone(), result, cx);
                                        }
                                    }),
                            )
                            .child(
                                Button::new("sync-test-dav")
                                    .small()
                                    .disabled(busy)
                                    .label(t.translate("sync.test"))
                                    .on_click({
                                        let view = view.clone();
                                        move |_, _, cx| {
                                            let result = save_dav(&view, None, cx);
                                            if let Err(e) = result {
                                                feedback(view.clone(), Err(e), cx);
                                                return;
                                            }
                                            run_action(
                                                view.clone(),
                                                |runtime| {
                                                    runtime.test_dav()?;
                                                    Ok(SyncAction::Tested)
                                                },
                                                cx,
                                            );
                                        }
                                    }),
                            ),
                    ),
            );

        dav.into_any_element()
    }
    fn render_sync_devices(
        &self,
        config: &crate::services::sync::config::Config,
        snapshot: &crate::services::sync::Snapshot,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let t = i18n::translator(cx);
        let view = cx.entity();
        let mut devices = v_flex().w_full().gap_3().child(heading(
            t.translate("sync.devices"),
            t.translate("sync.devices_help"),
            cx,
        ));
        if config.peers.is_empty() {
            devices = devices.child(card(cx).child(heading(
                t.translate("sync.no_devices"),
                t.translate("sync.no_devices_help"),
                cx,
            )));
        }
        for peer in &config.peers {
            let status = snapshot.statuses.get(&peer.profile.id);
            let channel = if config.enabled {
                status.cloned().unwrap_or_default().channel(config, peer)
            } else {
                None
            };
            let state = if !peer.policy.enabled {
                t.translate("sync.blocked")
            } else {
                channel
                    .map(str::to_owned)
                    .unwrap_or_else(|| t.translate("sync.offline"))
            };
            let mut device = card(cx).child(
                h_flex()
                    .gap_3()
                    .p_4()
                    .items_center()
                    .child(div().size(px(8.0)).rounded_full().bg(if channel.is_some() {
                        theme.success
                    } else {
                        theme.muted_foreground
                    }))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(peer.profile.label()),
                            )
                            .child(div().text_xs().text_color(theme.muted_foreground).child(
                                if config.lan_enabled && !peer.address.is_empty() {
                                    format!("{state} · {}", peer.address)
                                } else {
                                    state.clone()
                                },
                            )),
                    )
                    .child(policy_switch(
                        &format!("peer-enable-{}", peer.profile.id),
                        peer.policy.enabled,
                        &peer.profile.id,
                        |p| &mut p.enabled,
                        view.clone(),
                    ))
                    .child(
                        Button::new(format!("peer-details-{}", peer.profile.id))
                            .small()
                            .ghost()
                            .label(t.translate("sync.device_settings"))
                            .on_click({
                                let view = view.clone();
                                let id = peer.profile.id.clone();
                                move |_, window, cx| {
                                    open_sync_panel(
                                        view.clone(),
                                        SyncPanel::Device(id.clone()),
                                        window,
                                        cx,
                                    )
                                }
                            }),
                    ),
            );
            if let Some(status) = status.filter(|s| !s.result.is_empty()) {
                device = device.child(
                    div()
                        .px_4()
                        .pb_3()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(status.result.clone()),
                );
            }
            devices = devices.child(device);
        }
        devices.into_any_element()
    }
    fn render_sync_device_settings(
        &self,
        config: &crate::services::sync::config::Config,
        peer: &crate::services::sync::config::Peer,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let t = i18n::translator(cx);
        let view = cx.entity();
        let mut device = card(cx).child(heading(
            t.translate("sync.verification"),
            Candidate::manual(PairingCode {
                profile: peer.profile.clone(),
                address: String::new(),
            })
            .verification(&config.profile),
            cx,
        ));
        type Toggle = (&'static str, bool, fn(&mut Policy) -> &mut bool);
        let toggles: [Toggle; 12] = [
            ("history", peer.policy.history, |p| &mut p.history),
            ("favorites", peer.policy.favorites, |p| &mut p.favorites),
            ("touches", peer.policy.touches, |p| &mut p.touches),
            ("lan", peer.policy.lan, |p| &mut p.lan),
            ("dav_channel", peer.policy.webdav, |p| &mut p.webdav),
            ("text", peer.policy.text, |p| &mut p.text),
            ("rich_text", peer.policy.rich_text, |p| &mut p.rich_text),
            ("images", peer.policy.images, |p| &mut p.images),
            ("custom", peer.policy.custom, |p| &mut p.custom),
            ("file_paths", peer.policy.file_paths, |p| &mut p.file_paths),
            ("file_contents", peer.policy.file_contents, |p| {
                &mut p.file_contents
            }),
            ("common_only", peer.policy.common_only, |p| {
                &mut p.common_only
            }),
        ];
        for (key, value, field) in toggles {
            device = device.child(setting_row(
                theme,
                t.translate(&format!("sync.{key}")),
                Some(t.translate(&format!("sync.{key}_help")).into()),
                policy_switch(
                    &format!("peer-{}-{key}", peer.profile.id),
                    value,
                    &peer.profile.id,
                    field,
                    view.clone(),
                ),
            ));
        }
        for (key, value) in [
            ("image_limit", peer.policy.image_limit_mb),
            ("file_limit", peer.policy.file_limit_mb),
            ("files_limit", peer.policy.files_limit_mb),
        ] {
            let choices = [8u32, 16, 32, 64, 128]
                .into_iter()
                .map(|n| {
                    DropdownItem::option(DropdownItemId::new(n.to_string()), format!("{n} MB"))
                })
                .collect();
            let id = peer.profile.id.clone();
            let view = view.clone();
            device = device.child(setting_row(
                theme,
                t.translate(&format!("sync.{key}")),
                None,
                settings_dropdown(
                    key,
                    value.to_string(),
                    choices,
                    move |selected, cx| {
                        let value = selected.as_str().parse::<u32>().unwrap_or(32);
                        let result = SyncService::get(cx).and_then(|r| {
                            r.change(
                                |c| {
                                    if let Some(p) = c.peers.iter_mut().find(|p| p.profile.id == id)
                                    {
                                        match key {
                                            "image_limit" => p.policy.image_limit_mb = value,
                                            "file_limit" => p.policy.file_limit_mb = value,
                                            _ => p.policy.files_limit_mb = value,
                                        }
                                    }
                                    Ok(())
                                },
                                true,
                            )
                        });
                        feedback(view.clone(), result, cx);
                    },
                    cx,
                ),
            ));
        }
        device = device.child(
            v_flex()
                .gap_2()
                .p_4()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(t.translate("sync.address_and_exclusions")),
                )
                .child(Input::new(&self.sync_form.peer_address).small().w_full())
                .child(Input::new(&self.sync_form.exclusions).small().w_full())
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("sync-peer-save")
                                .small()
                                .label(t.translate("sync.save"))
                                .on_click({
                                    let view = view.clone();
                                    let id = peer.profile.id.clone();
                                    move |_, _, cx| {
                                        let form = &view.read(cx).sync_form;
                                        let address =
                                            form.peer_address.read(cx).value().trim().to_owned();
                                        let exclusions = form
                                            .exclusions
                                            .read(cx)
                                            .value()
                                            .split('|')
                                            .map(str::trim)
                                            .filter(|s| !s.is_empty())
                                            .map(str::to_owned)
                                            .collect();
                                        let result = SyncService::get(cx).and_then(|r| {
                                            r.change(
                                                |c| {
                                                    if let Some(p) = c
                                                        .peers
                                                        .iter_mut()
                                                        .find(|p| p.profile.id == id)
                                                    {
                                                        p.address = address;
                                                        p.policy.excluded_formats = exclusions;
                                                    }
                                                    Ok(())
                                                },
                                                true,
                                            )
                                        });
                                        feedback(view.clone(), result, cx);
                                    }
                                }),
                        )
                        .child(
                            Button::new("sync-revoke")
                                .small()
                                .danger()
                                .label(t.translate("sync.revoke"))
                                .on_click({
                                    let view = view.clone();
                                    let id = peer.profile.id.clone();
                                    move |_, window, cx| {
                                        let t = i18n::translator(cx);
                                        let answer = window.prompt(
                                            gpui::PromptLevel::Warning,
                                            &t.translate("sync.revoke"),
                                            Some(&t.translate("sync.revoke_help")),
                                            &[
                                                gpui::PromptButton::cancel(
                                                    t.translate("sync.cancel"),
                                                ),
                                                gpui::PromptButton::ok(t.translate("sync.revoke")),
                                            ],
                                            cx,
                                        );
                                        let view = view.clone();
                                        let id = id.clone();
                                        cx.spawn(async move |cx| {
                                            if answer.await.unwrap_or(0) == 1 {
                                                cx.update(|cx| {
                                                    let result = SyncService::get(cx)
                                                        .and_then(|r| r.revoke(&id));
                                                    feedback_as(
                                                        view,
                                                        result,
                                                        "sync.device_removed",
                                                        cx,
                                                    );
                                                });
                                            }
                                        })
                                        .detach();
                                    }
                                }),
                        ),
                ),
        );
        device.into_any_element()
    }
}

impl SettingsWindowView {
    fn render_nearby_pairing(
        &self,
        local: &crate::services::sync::local_pairing::Snapshot,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let t = i18n::translator(cx);
        let view = cx.entity();
        let busy = self.sync_form.busy;

        let mut nearby = card(cx)
            .child(heading(
                t.translate("sync.nearby_tab"),
                t.translate("sync.nearby_help"),
                cx,
            ))
            .child(
                h_flex().gap_2().px_4().pb_3().child(
                    Button::new("sync-add-nearby")
                        .small()
                        .primary()
                        .label(t.translate(if local.active {
                            "sync.stop_search"
                        } else {
                            "sync.start_search"
                        }))
                        .disabled(
                            busy || (local.active
                                && local
                                    .request
                                    .as_ref()
                                    .is_some_and(|r| !r.added && !r.rejected)),
                        )
                        .on_click({
                            let view = view.clone();
                            let active = local.active;
                            move |_, _, cx| {
                                let result = SyncService::get(cx).and_then(|runtime| {
                                    if active {
                                        runtime.close_local_pairing();
                                        Ok(())
                                    } else {
                                        runtime.begin_local_pairing()
                                    }
                                });
                                feedback_as(
                                    view.clone(),
                                    result,
                                    if active {
                                        "sync.search_stopped"
                                    } else {
                                        "sync.local_started"
                                    },
                                    cx,
                                );
                            }
                        }),
                ),
            );
        if local.active
            && local
                .request
                .as_ref()
                .is_none_or(|request| request.added || request.rejected)
        {
            nearby = nearby.child(
                div()
                    .px_4()
                    .pb_3()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t.translate(if local.nearby.is_empty() {
                        "sync.searching"
                    } else {
                        "sync.nearby_found"
                    })),
            );
            for target in &local.nearby {
                nearby = nearby.child(setting_row(
                    theme,
                    target.profile.label(),
                    Some(t.translate("sync.nearby_unverified").into()),
                    Button::new(format!("pair-{}", target.profile.id))
                        .small()
                        .label(t.translate("sync.request_pair"))
                        .disabled(
                            busy || local
                                .request
                                .as_ref()
                                .is_some_and(|r| !r.added && !r.rejected),
                        )
                        .on_click({
                            let view = view.clone();
                            let target = target.clone();
                            move |_, window, cx| {
                                view.update(cx, |view, cx| {
                                    view.sync_form
                                        .local_pin
                                        .update(cx, |input, cx| input.set_value("", window, cx))
                                });
                                let target = target.clone();
                                run_action(
                                    view.clone(),
                                    move |runtime| {
                                        runtime.request_local_pairing(target)?;
                                        Ok(SyncAction::LocalRequested)
                                    },
                                    cx,
                                );
                            }
                        }),
                ));
            }
        }
        if let Some(request) = &local.request {
            let mut detail = v_flex()
                .gap_3()
                .p_4()
                .border_t_1()
                .border_color(theme.border)
                .bg(theme.primary.opacity(0.05))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(request.profile.label()),
                );
            if request.added || request.rejected {
                detail = detail.child(div().text_sm().child(t.translate(if request.added {
                    "sync.device_added"
                } else {
                    "sync.local_rejected"
                })));
            } else if request.incoming {
                if let Some(pin) = &request.pin {
                    detail = detail
                        .child(div().text_sm().child(t.translate("sync.show_local_pin")))
                        .child(div().text_lg().font_weight(FontWeight::BOLD).child(format!(
                            "{} {}",
                            &pin[..3],
                            &pin[3..]
                        )))
                        .child(div().text_xs().child(t.translate("sync.pin_private")));
                } else {
                    detail = detail.child(
                        div()
                            .text_sm()
                            .child(t.translate("sync.local_authenticated")),
                    );
                }
                detail = detail.child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("sync-local-accept")
                                .small()
                                .primary()
                                .label(t.translate("sync.accept_pair"))
                                .disabled(busy || !request.authenticated)
                                .on_click({
                                    let view = view.clone();
                                    move |_, _, cx| {
                                        run_action(
                                            view.clone(),
                                            |runtime| {
                                                runtime.accept_local_pairing()?;
                                                Ok(SyncAction::LocalAccepted)
                                            },
                                            cx,
                                        );
                                    }
                                }),
                        )
                        .child(
                            Button::new("sync-local-reject")
                                .small()
                                .ghost()
                                .label(t.translate("sync.reject_pair"))
                                .disabled(busy)
                                .on_click({
                                    let view = view.clone();
                                    move |_, _, cx| {
                                        run_action(
                                            view.clone(),
                                            |runtime| {
                                                runtime.cancel_local_request()?;
                                                Ok(SyncAction::LocalCancelled)
                                            },
                                            cx,
                                        );
                                    }
                                }),
                        ),
                );
            } else if !request.authenticated {
                detail = detail
                    .child(div().text_sm().child(t.translate("sync.enter_local_pin")))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Input::new(&self.sync_form.local_pin)
                                    .small()
                                    .w(ui_rem(130.0)),
                            )
                            .child(
                                Button::new("sync-local-verify")
                                    .small()
                                    .primary()
                                    .label(t.translate("sync.verify_pin"))
                                    .disabled(busy)
                                    .on_click({
                                        let view = view.clone();
                                        let input = self.sync_form.local_pin.clone();
                                        move |_, _, cx| {
                                            let pin =
                                                input.read(cx).value().trim().replace(' ', "");
                                            run_action(
                                                view.clone(),
                                                move |runtime| {
                                                    runtime.authenticate_local_pairing(&pin)?;
                                                    Ok(SyncAction::LocalVerified)
                                                },
                                                cx,
                                            );
                                        }
                                    }),
                            ),
                    );
            } else {
                detail = detail.child(div().text_sm().child(t.translate("sync.wait_local_accept")));
            }
            if !request.incoming && !request.added && !request.rejected {
                detail = detail.child(
                    Button::new("sync-cancel-local")
                        .small()
                        .ghost()
                        .label(t.translate("sync.cancel"))
                        .disabled(busy)
                        .on_click({
                            let view = view.clone();
                            move |_, _, cx| {
                                run_action(
                                    view.clone(),
                                    |runtime| {
                                        runtime.cancel_local_request()?;
                                        Ok(SyncAction::LocalCancelled)
                                    },
                                    cx,
                                );
                            }
                        }),
                );
            }
            nearby = nearby.child(detail);
        }
        if !local.message.is_empty() {
            nearby = nearby.child(
                div()
                    .p_4()
                    .text_xs()
                    .text_color(theme.danger)
                    .child(local.message.clone()),
            );
        }

        nearby.into_any_element()
    }
}

fn select_sync_panel(
    view: &Entity<SettingsWindowView>,
    panel: SyncPanel,
    window: &mut Window,
    cx: &mut App,
) {
    if panel == SyncPanel::Nearby
        && let Err(error) = SyncService::get(cx).and_then(|runtime| runtime.begin_local_pairing())
    {
        feedback(view.clone(), Err(error), cx);
    }
    view.update(cx, |view, cx| {
        if let SyncPanel::Device(id) = &panel
            && let Some(peer) = SyncService::snapshot(cx)
                .config
                .and_then(|c| c.peers.into_iter().find(|p| &p.profile.id == id))
        {
            view.sync_form
                .peer_address
                .update(cx, |input, cx| input.set_value(peer.address, window, cx));
            view.sync_form.exclusions.update(cx, |input, cx| {
                input.set_value(peer.policy.excluded_formats.join(" | "), window, cx)
            });
        }
        view.sync_form.panel = Some(panel);
        cx.notify();
    });
}

fn open_sync_panel(
    view: Entity<SettingsWindowView>,
    panel: SyncPanel,
    window: &mut Window,
    cx: &mut App,
) {
    view.update(cx, |view, _| view.sync_form.dav_return_remote = false);
    select_sync_panel(&view, panel, window, cx);
    window.open_sheet(cx, move |sheet, _, cx| {
        let t = i18n::translator(cx);
        let snapshot = SyncService::snapshot(cx);
        let title = match view.read(cx).sync_form.panel.as_ref() {
            Some(SyncPanel::Device(id)) => snapshot
                .config
                .as_ref()
                .and_then(|c| c.peers.iter().find(|p| &p.profile.id == id))
                .map(|peer| peer.profile.name.clone())
                .unwrap_or_else(|| t.translate("sync.device_settings")),
            Some(SyncPanel::Lan | SyncPanel::WebDav) => t.translate("sync.channels"),
            _ => t.translate("sync.add_device"),
        };
        let content = view.update(cx, |view, cx| view.render_sync_panel(&snapshot, cx));
        sheet
            .title(title)
            .size(ui_rem(520.0))
            .resizable(false)
            .shadow_none()
            .map(|this| crate::window_material::sheet_surface(this, cx))
            .on_close({
                let view = view.clone();
                move |_, _, cx| {
                    view.update(cx, |view, cx| {
                        view.sync_form.panel = None;
                        view.sync_form.dav_return_remote = false;
                        cx.notify();
                    })
                }
            })
            .child(content)
    });
}

impl SettingsWindowView {
    fn render_sync_panel(
        &self,
        snapshot: &crate::services::sync::Snapshot,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let t = i18n::translator(cx);
        let view = cx.entity();
        let Some(config) = snapshot.config.as_ref() else {
            return div().child(snapshot.message.clone()).into_any_element();
        };
        let panel = self.sync_form.panel.clone().unwrap_or(SyncPanel::Nearby);
        let mut content = v_flex().size_full().min_h_0().gap_3();
        if matches!(panel, SyncPanel::Nearby | SyncPanel::Remote) {
            content = content.child(self.sync_panel_tabs(
                &panel,
                &[
                    (SyncPanel::Nearby, "sync.nearby_tab"),
                    (SyncPanel::Remote, "sync.remote_tab"),
                ],
                cx,
            ));
        } else if matches!(panel, SyncPanel::Lan | SyncPanel::WebDav) {
            if self.sync_form.dav_return_remote {
                content = content.child(
                    Button::new("sync-back-to-add")
                        .small()
                        .ghost()
                        .label(t.translate("sync.back_to_remote"))
                        .disabled(self.sync_form.busy)
                        .on_click({
                            let view = view.clone();
                            move |_, window, cx| {
                                select_sync_panel(&view, SyncPanel::Remote, window, cx)
                            }
                        }),
                );
            }
            content = content
                .child(self.sync_panel_tabs(
                    &panel,
                    &[
                        (SyncPanel::Lan, "sync.lan_title"),
                        (SyncPanel::WebDav, "sync.webdav"),
                    ],
                    cx,
                ))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(t.translate("sync.channels_help")),
                );
        }
        if self.sync_form.busy {
            content = content.child(
                div()
                    .text_xs()
                    .text_color(theme.primary)
                    .child(t.translate("sync.working")),
            );
        }
        let body = match &panel {
            SyncPanel::Nearby => self.render_nearby_pairing(&snapshot.local_pairing, cx),
            SyncPanel::Remote => self.render_sync_remote_panel(config, snapshot, cx),
            SyncPanel::Manual => self.render_remote_pairing(config, snapshot, cx),
            SyncPanel::Lan => self.render_sync_lan(config, snapshot, cx),
            SyncPanel::WebDav => self.render_sync_dav(config, cx),
            SyncPanel::Device(id) => config
                .peers
                .iter()
                .find(|p| &p.profile.id == id)
                .map(|peer| self.render_sync_device_settings(config, peer, cx))
                .unwrap_or_else(|| {
                    div()
                        .child(t.translate("sync.device_removed"))
                        .into_any_element()
                }),
        };
        content
            .child(
                v_flex()
                    .id(format!(
                        "sync-panel-scroll-{}",
                        match panel {
                            SyncPanel::Nearby => "nearby",
                            SyncPanel::Remote => "remote",
                            SyncPanel::Manual => "manual",
                            SyncPanel::Lan => "lan",
                            SyncPanel::WebDav => "webdav",
                            SyncPanel::Device(_) => "device",
                        }
                    ))
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .gap_3()
                    .overflow_y_scrollbar()
                    .child(body),
            )
            .into_any_element()
    }

    fn sync_panel_tabs(
        &self,
        selected: &SyncPanel,
        tabs: &[(SyncPanel, &str)],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = i18n::translator(cx);
        let view = cx.entity();
        let mut row = h_flex().w_full().flex_shrink_0().gap_2();
        for (panel, label) in tabs {
            row = row.child(
                Button::new(format!("sync-panel-tab-{label}"))
                    .small()
                    .flex_1()
                    .when(selected == panel, |button| button.primary())
                    .when(selected != panel, |button| button.ghost())
                    .label(t.translate(label))
                    .disabled(self.sync_form.busy)
                    .on_click({
                        let view = view.clone();
                        let panel = panel.clone();
                        move |_, window, cx| select_sync_panel(&view, panel.clone(), window, cx)
                    }),
            );
        }
        row.into_any_element()
    }

    fn render_sync_remote_panel(
        &self,
        config: &crate::services::sync::config::Config,
        snapshot: &crate::services::sync::Snapshot,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = i18n::translator(cx);
        let view = cx.entity();
        v_flex()
            .w_full()
            .gap_3()
            .child(
                card(cx).child(setting_row(
                    cx.theme().colors,
                    t.translate("sync.remote_directory"),
                    Some(
                        if config.dav.url.is_empty() {
                            t.translate("sync.remote_setup_help")
                        } else {
                            config.dav.url.clone()
                        }
                        .into(),
                    ),
                    Button::new("sync-remote-configure")
                        .small()
                        .label(t.translate(if config.dav.url.is_empty() {
                            "sync.configure_dav"
                        } else {
                            "sync.edit_connection"
                        }))
                        .disabled(self.sync_form.busy)
                        .on_click(move |_, window, cx| {
                            view.update(cx, |view, _| view.sync_form.dav_return_remote = true);
                            select_sync_panel(&view, SyncPanel::WebDav, window, cx);
                        }),
                )),
            )
            .child(self.render_remote_pairing(config, snapshot, cx))
            .into_any_element()
    }
}

fn save_dav(
    view: &Entity<SettingsWindowView>,
    enabled: Option<bool>,
    cx: &mut App,
) -> crate::services::sync::Result<()> {
    let form = &view.read(cx).sync_form;
    let url = form.url.read(cx).value().trim().to_owned();
    let username = form.username.read(cx).value().to_string();
    let password = form.password.read(cx).value().to_string();
    let poll = form.poll.read(cx).value().parse::<u64>().map_err(|_| {
        crate::services::sync::Error::Message("自动拉取间隔必须是 1–86400 秒的整数".into())
    })?;
    if !(1..=86400).contains(&poll) {
        return crate::services::sync::fail("自动拉取间隔必须是 1–86400 秒的整数");
    }
    let cache_entries = form
        .cache_entries
        .read(cx)
        .value()
        .parse::<usize>()
        .map_err(|_| {
            crate::services::sync::Error::Message("缓存条数必须是 1–1000 的整数".into())
        })?;
    let cache_mb = form
        .cache_mb
        .read(cx)
        .value()
        .parse::<usize>()
        .map_err(|_| {
            crate::services::sync::Error::Message("缓存容量必须是 1–1024 MiB 的整数".into())
        })?;
    if !(1..=1000).contains(&cache_entries) || !(1..=1024).contains(&cache_mb) {
        return crate::services::sync::fail("缓存条数须为 1–1000，缓存容量须为 1–1024 MiB");
    }
    SyncService::get(cx)?.change(
        |c| {
            c.dav.url = url;
            c.dav.username = username;
            c.dav.password = password;
            c.dav.poll_seconds = poll;
            c.dav.cache_entries = cache_entries;
            c.dav.cache_mb = cache_mb;
            if let Some(enabled) = enabled {
                c.dav.enabled = enabled;
            }
            Ok(())
        },
        true,
    )
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use gpui::{
        AppContext, Context, Entity, IntoElement, Render, Subscription, TestAppContext, Window, div,
    };
    use gpui_component::input::{InputEvent, InputState};

    struct UrlRefreshProbe {
        url: Entity<InputState>,
        notifications: Rc<Cell<usize>>,
        _notification_subscription: Subscription,
        _url_subscription: Subscription,
    }

    impl UrlRefreshProbe {
        fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
            let url = cx.new(|cx| InputState::new(window, cx));
            let notifications = Rc::new(Cell::new(0));
            let observed_notifications = notifications.clone();
            let notification_subscription = cx.observe_self(move |_, _| {
                observed_notifications.set(observed_notifications.get() + 1);
            });
            let url_subscription = super::subscribe_sync_url_changes(&url, cx);

            Self {
                url,
                notifications,
                _notification_subscription: notification_subscription,
                _url_subscription: url_subscription,
            }
        }
    }

    impl Render for UrlRefreshProbe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    #[gpui::test]
    fn url_change_notifies_parent_view_to_refresh_http_warning(cx: &mut TestAppContext) {
        cx.skip_drawing();
        let window = cx.add_window(UrlRefreshProbe::new);
        let notifications_before = window
            .read_with(cx, |view, _| view.notifications.get())
            .unwrap();
        let url = window.read_with(cx, |view, _| view.url.clone()).unwrap();

        url.update(cx, |_, cx| cx.emit(InputEvent::Change));
        cx.run_until_parked();

        let notifications_after = window
            .read_with(cx, |view, _| view.notifications.get())
            .unwrap();
        assert_eq!(notifications_after, notifications_before + 1);
    }
}
