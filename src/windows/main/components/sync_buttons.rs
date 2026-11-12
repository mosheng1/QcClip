use gpui::{App, Context, Div, MouseButton, Rems, div, prelude::*, px};
use gpui_component::{ActiveTheme as _, tooltip::Tooltip};

use crate::{
    i18n,
    services::sync::SyncService,
    ui::{hover_icon, scale::ui_rem},
    windows::{
        main::state::MainWindow,
        notification::{NotificationLevel, NotificationService},
    },
};

#[derive(Clone, Copy)]
pub(crate) enum Action {
    Upload,
    Pull,
}

impl Action {
    fn label(self) -> &'static str {
        match self {
            Self::Upload => "sync.upload_latest",
            Self::Pull => "sync.pull_now",
        }
    }

    fn busy(self, main: &mut MainWindow) -> &mut bool {
        match self {
            Self::Upload => &mut main.dav_upload_busy,
            Self::Pull => &mut main.dav_pull_busy,
        }
    }
}

pub(super) fn render(
    main: &MainWindow,
    cx: &mut Context<MainWindow>,
    is_vertical: bool,
    titlebar_size: Rems,
) -> Option<Div> {
    let config = SyncService::get(cx).ok()?.config();
    if !config.dav.enabled {
        return None;
    }
    let theme = cx.theme().colors;
    let t = i18n::translator(cx);
    let mut buttons = div()
        .flex_none()
        .w(ui_rem(56.0))
        .when(!is_vertical, |buttons| buttons.max_w_full())
        .h(titlebar_size)
        .flex()
        .flex_row()
        .when(is_vertical, |buttons| {
            buttons.w(titlebar_size).h(ui_rem(56.0)).flex_col()
        })
        .rounded_lg()
        .overflow_hidden();
    for (action, name, busy) in [
        (Action::Upload, "cloud-upload", main.dav_upload_busy),
        (Action::Pull, "cloud-download", main.dav_pull_busy),
    ] {
        let enabled = config.enabled && !busy;
        let label = t.translate(action.label());
        let tooltip = if !config.enabled {
            format!("{label} · {}", t.translate("sync.enable_first"))
        } else if busy {
            format!("{label} · {}", t.translate("sync.working"))
        } else {
            label.clone()
        };
        let group = format!("titlebar-webdav-{name}");
        let button = div()
            .id(group.clone())
            .group(group.clone())
            .flex_1()
            .min_w_0()
            .min_h_0()
            .when(is_vertical, |button| button.w_full())
            .when(!is_vertical, |button| button.h_full())
            .flex()
            .items_center()
            .justify_center()
            .aria_label(label)
            .tooltip(move |window, cx| {
                let text = if matches!(action, Action::Upload) && enabled {
                    SyncService::get(cx)
                        .and_then(|runtime| runtime.latest_upload())
                        .ok()
                        .flatten()
                        .map(|latest| {
                            let t = i18n::translator(cx);
                            let title = t.translate(if latest.mode.favorite() {
                                "sync.upload_latest_favorite"
                            } else {
                                "sync.upload_latest_history"
                            });
                            if latest.preview.is_empty() {
                                title
                            } else {
                                format!("{title}\n{}", latest.preview)
                            }
                        })
                        .unwrap_or_else(|| tooltip.clone())
                } else {
                    tooltip.clone()
                };
                Tooltip::new(text).build(window, cx)
            })
            .when(enabled, |button| {
                button
                    .cursor_pointer()
                    .hover(|button| button.bg(theme.secondary_hover))
            })
            .when(!enabled, |button| button.opacity(0.4).cursor_default())
            .when(matches!(action, Action::Pull), |button| {
                button
                    .when(is_vertical, |button| button.border_t(px(0.5)))
                    .when(!is_vertical, |button| button.border_l(px(0.5)))
                    .border_color(theme.title_bar_border)
            })
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |main, _, _, cx| {
                cx.stop_propagation();
                if enabled {
                    run(main, action, cx);
                }
            }))
            .child(hover_icon(group, name, 16.0, theme.foreground));
        buttons = buttons.child(button);
    }
    Some(buttons)
}

/// 全局快捷键入口，与标题栏按钮行为一致；同步未就绪时给出提示。
pub(crate) fn run_shortcut(main: &mut MainWindow, action: Action, cx: &mut Context<MainWindow>) {
    let config = match SyncService::get(cx) {
        Ok(runtime) => runtime.config(),
        Err(error) => {
            show_result(action, Err(error), cx);
            return;
        }
    };
    let t = i18n::translator(cx);
    let blocked = if !config.enabled {
        Some(t.translate("sync.enable_first"))
    } else if !config.dav.enabled {
        Some(t.translate("sync.dav_off"))
    } else {
        None
    };
    if let Some(message) = blocked {
        NotificationService::show_message(
            t.translate(action.label()),
            message,
            NotificationLevel::Info,
            cx,
        );
        return;
    }
    run(main, action, cx);
}

fn run(main: &mut MainWindow, action: Action, cx: &mut Context<MainWindow>) {
    if *action.busy(main) {
        return;
    }
    let runtime = match SyncService::get(cx) {
        Ok(runtime) => runtime,
        Err(error) => {
            show_result(action, Err(error), cx);
            return;
        }
    };
    *action.busy(main) = true;
    cx.notify();
    let task = cx.background_spawn(async move {
        match action {
            Action::Upload => runtime.upload_latest(),
            Action::Pull => runtime.pull_dav().map(Some),
        }
    });
    cx.spawn(async move |this, cx| {
        let result = task.await;
        let _ = this.update(cx, |main, cx| {
            *action.busy(main) = false;
            cx.notify();
        });
        cx.update(|cx| show_result(action, result, cx));
    })
    .detach();
}

fn show_result(action: Action, result: crate::services::sync::Result<Option<usize>>, cx: &mut App) {
    let t = i18n::translator(cx);
    let (message, level) = match result {
        Err(error) => (error.to_string(), NotificationLevel::Error),
        Ok(count) => {
            let message = match (action, count) {
                (Action::Upload, None) => t.translate("sync.upload_latest_empty"),
                (Action::Upload, Some(count)) => {
                    t.translate_with_args("sync.sending_count", &[("count", count.to_string())])
                }
                (Action::Pull, Some(0) | None) => t.translate("sync.pull_empty"),
                (Action::Pull, Some(_)) => t.translate("sync.pull_queued"),
            };
            (message, NotificationLevel::Info)
        }
    };
    NotificationService::show_message(t.translate(action.label()), message, level, cx);
}
