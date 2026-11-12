//! 首次安装后显示的分步引导窗口。
//!
//! 引导只覆盖第一印象相关的少量选项：语言、外观、呼出快捷键、点击习惯与开机自启，
//! 最后一步让用户实际按一次快捷键确认能呼出主窗口。每一步的修改都立即写入配置，
//! 中途关闭窗口也会保留已做的选择，并视为引导已完成。

use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Context, FocusHandle, FontWeight, Global, Hsla,
    KeyDownEvent, Keystroke, ModifiersChangedEvent, Render, ScrollHandle, SharedString,
    SpringAnimation, SpringConfig, Task, TitlebarOptions, Window, WindowBackgroundAppearance,
    WindowBounds, WindowControlArea, WindowDecorations, WindowHandle, WindowKind, WindowOptions,
    div, img, linear_color_stop, linear_gradient, px, relative, size,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::kbd::Kbd;
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme as _, Colorize as _, Root, Sizable as _, ThemeColor, ThemeMode};
use gpui_component::{h_flex, v_flex};

use crate::{
    config::{AppearanceMode, ClipboardMouseBehavior, InterfaceLanguage, ShortcutBinding},
    config_runtime, hotkey, i18n,
    ui::{icon::icon, scale::ui_rem, theme::available_theme_options},
    windows::notification::{NotificationLevel, NotificationService},
};

const WINDOW_WIDTH: f32 = 780.0;
const WINDOW_HEIGHT: f32 = 560.0;
const SIDEBAR_WIDTH: f32 = 236.0;
const TITLE_BAR_HEIGHT: f32 = 36.0;
const MAIN_WINDOW_POLL_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Welcome,
    Appearance,
    Shortcut,
    Habits,
    TryIt,
}

impl Step {
    const ALL: [Self; 5] = [
        Self::Welcome,
        Self::Appearance,
        Self::Shortcut,
        Self::Habits,
        Self::TryIt,
    ];

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|step| *step == self)
            .unwrap_or_default()
    }

    fn from_index(index: usize) -> Self {
        Self::ALL[index.min(Self::ALL.len() - 1)]
    }

    const fn key(self) -> &'static str {
        match self {
            Self::Welcome => "welcome",
            Self::Appearance => "appearance",
            Self::Shortcut => "shortcut",
            Self::Habits => "habits",
            Self::TryIt => "try_it",
        }
    }

    const fn icon_name(self) -> &'static str {
        match self {
            Self::Welcome => "language",
            Self::Appearance => "palette",
            Self::Shortcut => "keyboard",
            Self::Habits => "mouse",
            Self::TryIt => "rocket",
        }
    }
}

fn step_text(step: Step, field: &str, cx: &App) -> String {
    i18n::translator(cx).translate(&format!("onboarding.step.{}.{field}", step.key()))
}

#[derive(Default)]
pub(crate) struct OnboardingWindowService {
    window: Option<WindowHandle<Root>>,
}

impl Global for OnboardingWindowService {}

impl OnboardingWindowService {
    pub(crate) fn install(cx: &mut App) {
        cx.set_global(Self::default());
        cx.on_window_closed(|cx, _| {
            cx.defer(|cx| {
                let open_windows = cx.windows();
                let closed = cx.read_global::<Self, _>(|service, _| {
                    service.window.is_some_and(|window| {
                        !open_windows
                            .iter()
                            .any(|handle| handle.window_id() == window.window_id())
                    })
                });
                if closed {
                    cx.update_global::<Self, _>(|service, _| service.window = None);
                    finish_onboarding(cx);
                }
            });
        })
        .detach();
    }
}

/// 启动时调用：首次安装时打开引导页并返回 true；否则不做任何事。
pub(crate) fn open_if_pending(cx: &mut App) -> bool {
    if !crate::state_runtime::state(cx).onboarding_pending {
        return false;
    }
    open_onboarding_window(cx);
    true
}

pub(crate) fn open_onboarding_window(cx: &mut App) {
    let existing = cx.read_global::<OnboardingWindowService, _>(|service, _| service.window);
    if let Some(window) = existing
        && window
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        cx.activate(true);
        return;
    }

    let title = i18n::translator(cx).translate("onboarding.window.title");
    let result = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::centered(
                size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)),
                cx,
            )),
            titlebar: Some(TitlebarOptions {
                title: Some(title.into()),
                appears_transparent: true,
                ..Default::default()
            }),
            kind: WindowKind::Normal,
            focus: true,
            is_movable: true,
            is_resizable: false,
            is_minimizable: true,
            window_background: WindowBackgroundAppearance::Transparent,
            window_decorations: Some(WindowDecorations::Client),
            ..Default::default()
        },
        |window, cx| {
            crate::platform::window::configure_frameless_window(window, true);
            window.bounds_changed(cx);
            crate::window_material::register_window(window, cx);
            let view = cx.new(|cx| OnboardingView::new(window, cx));
            cx.new(|cx| crate::window_material::new_root(view, window, cx))
        },
    );
    match result {
        Ok(window) => {
            cx.update_global::<OnboardingWindowService, _>(|service, _| {
                service.window = Some(window);
            });
            if let Err(error) = window.update(cx, |_, window, _| window.activate_window()) {
                tracing::error!("激活引导窗口失败：{error}");
            }
            cx.activate(true);
        }
        Err(error) => {
            tracing::error!("打开引导窗口失败：{error}");
            finish_onboarding(cx);
        }
    }
}

/// 引导窗口关闭后执行：清除首次引导标记，并补上启动时推迟的旧版迁移提示。
fn finish_onboarding(cx: &mut App) {
    // 录制快捷键期间关闭窗口时，全局快捷键仍处于暂停状态，需要恢复。
    if let Err(error) = hotkey::cancel_toggle_main_window_recording(cx) {
        tracing::warn!("关闭引导窗口后恢复全局快捷键失败：{error}");
    }
    let mut state = crate::state_runtime::state(cx);
    if !state.onboarding_pending {
        return;
    }
    state.onboarding_pending = false;
    if let Err(error) = crate::state_runtime::set_state(state, cx) {
        tracing::warn!("保存引导完成状态失败：{error}");
    }
    crate::windows::settings::offer_legacy_migration(cx);
}

fn close_window(window: &mut Window) {
    window.remove_window();
}

fn report_error(error: impl std::fmt::Display, cx: &mut App) {
    let translator = i18n::translator(cx);
    tracing::error!("引导页保存设置失败：{error}");
    NotificationService::show_message(
        translator.translate("onboarding.window.title"),
        translator.translate("onboarding.save_error"),
        NotificationLevel::Error,
        cx,
    );
}

fn spring(target: f32) -> SpringAnimation<f32> {
    SpringAnimation::new(SpringConfig::new(320.0, 32.0, 1.0)).to(target)
}

struct OnboardingView {
    focus_handle: FocusHandle,
    step: Step,
    /// 最近一次切换步骤的方向，决定内容从左侧还是右侧滑入。
    forward: bool,
    recording: bool,
    recording_modifiers: gpui::Modifiers,
    shortcut_hint: Option<String>,
    main_window_shown: bool,
    scroll_handle: ScrollHandle,
    _poll_task: Option<Task<()>>,
}

impl OnboardingView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        Self {
            focus_handle,
            step: Step::Welcome,
            forward: true,
            recording: false,
            recording_modifiers: gpui::Modifiers::default(),
            shortcut_hint: None,
            main_window_shown: false,
            scroll_handle: ScrollHandle::new(),
            _poll_task: None,
        }
    }

    fn go_to(&mut self, step: Step, cx: &mut Context<Self>) {
        if step == self.step {
            return;
        }
        self.stop_recording(cx);
        self.forward = step.index() > self.step.index();
        self.step = step;
        self.shortcut_hint = None;
        self.scroll_handle.set_offset(gpui::point(px(0.0), px(0.0)));
        if step == Step::TryIt {
            self.start_watching_main_window(cx);
        } else {
            self._poll_task = None;
        }
        cx.notify();
    }

    /// 在“试一试”步骤轮询主窗口是否被呼出；呼出后停止轮询并显示成功状态。
    fn start_watching_main_window(&mut self, cx: &mut Context<Self>) {
        self.main_window_shown = false;
        crate::windows::main::hide_main_window(cx);
        self._poll_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(MAIN_WINDOW_POLL_INTERVAL)
                    .await;
                let shown = cx
                    .update(|cx| crate::windows::main::MainWindowService::is_logically_visible(cx));
                let Ok(()) = this.update(cx, |view, cx| {
                    if shown && !view.main_window_shown {
                        view.main_window_shown = true;
                        cx.notify();
                    }
                }) else {
                    return;
                };
                if shown {
                    return;
                }
            }
        }));
    }

    fn start_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(error) = hotkey::begin_toggle_main_window_recording(cx) {
            report_error(error, cx);
            return;
        }
        self.recording = true;
        self.recording_modifiers = gpui::Modifiers::default();
        self.shortcut_hint = None;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn stop_recording(&mut self, cx: &mut Context<Self>) {
        if !self.recording {
            return;
        }
        self.recording = false;
        if let Err(error) = hotkey::cancel_toggle_main_window_recording(cx) {
            report_error(error, cx);
        }
        cx.notify();
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.recording || event.is_held {
            return;
        }
        window.prevent_default();
        cx.stop_propagation();
        let keystroke = &event.keystroke;
        if keystroke.key == "escape" && !keystroke.modifiers.modified() {
            self.stop_recording(cx);
            return;
        }
        if hotkey::is_modifier_key(&keystroke.key) {
            return;
        }
        let translator = i18n::translator(cx);
        let modifiers = keystroke.modifiers;
        if !(modifiers.control || modifiers.alt || modifiers.platform) {
            self.shortcut_hint = Some(translator.translate("onboarding.shortcut.need_modifier"));
            cx.notify();
            return;
        }
        let binding = match hotkey::shortcut_binding_from_keystroke(keystroke) {
            Ok(binding) => binding,
            Err(error) => {
                self.shortcut_hint = Some(error);
                cx.notify();
                return;
            }
        };
        // 替换系统 Win+V 需要修改注册表并提示风险，交给设置页处理。
        if hotkey::is_win_v_binding(&binding) {
            self.shortcut_hint = Some(translator.translate("onboarding.shortcut.win_v"));
            cx.notify();
            return;
        }
        self.recording = false;
        match hotkey::set_toggle_main_window_shortcut(Some(binding), cx) {
            Ok(()) => self.shortcut_hint = None,
            Err(error) => {
                tracing::warn!("引导页保存呼出快捷键失败：{error}");
                hotkey::reject_shortcut_recording(true, cx);
                self.shortcut_hint = Some(translator.translate("onboarding.shortcut.conflict"));
            }
        }
        cx.notify();
    }
}

impl Render for OnboardingView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let animations = config_runtime::animation_enabled(
            cx,
            crate::config::AnimationKind::SettingsPageTransition,
        );
        h_flex()
            .id("onboarding-root")
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(theme.background)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::handle_key_down))
            .on_modifiers_changed(cx.listener(|view, event: &ModifiersChangedEvent, _, cx| {
                if view.recording {
                    view.recording_modifiers = event.modifiers;
                    cx.notify();
                }
            }))
            .child(self.render_sidebar(theme, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.render_top_bar(theme, animations, window, cx))
                    .child(self.render_content(theme, animations, window, cx))
                    .child(self.render_footer(theme, cx)),
            )
    }
}

// ---------- 布局骨架 ----------

impl OnboardingView {
    fn render_sidebar(&self, theme: ThemeColor, cx: &mut Context<Self>) -> AnyElement {
        let translator = i18n::translator(cx);
        let foreground = theme.primary_foreground;
        let top = theme.primary;
        let bottom = theme.primary.darken(0.3);
        let current = self.step.index();

        let steps = Step::ALL.into_iter().enumerate().map(|(index, step)| {
            let done = index < current;
            let active = index == current;
            let last = index + 1 == Step::ALL.len();
            let view = cx.entity().clone();
            let marker = div()
                .size(ui_rem(26.0))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .when(active, |this| {
                    this.bg(foreground).text_color(top).shadow_md().child(icon(
                        step.icon_name(),
                        14.0,
                        top,
                    ))
                })
                .when(done, |this| {
                    this.bg(foreground.opacity(0.22))
                        .child(icon("check", 13.0, foreground))
                })
                .when(!active && !done, |this| {
                    this.border_1()
                        .border_color(foreground.opacity(0.45))
                        .text_color(foreground.opacity(0.75))
                        .child(format!("{}", index + 1))
                });
            v_flex()
                .child(
                    h_flex()
                        .id(SharedString::from(format!("onboarding-nav-{}", step.key())))
                        .gap_3()
                        .items_center()
                        .px_2()
                        .py_1()
                        .rounded(px(8.0))
                        .cursor_pointer()
                        .when(active, |this| this.bg(foreground.opacity(0.12)))
                        .when(!active, |this| {
                            this.hover(|style| style.bg(foreground.opacity(0.08)))
                        })
                        .on_click(move |_, _, cx| {
                            view.update(cx, |view, cx| view.go_to(step, cx));
                        })
                        .child(marker)
                        .child(
                            div()
                                .text_sm()
                                .text_color(if active {
                                    foreground
                                } else {
                                    foreground.opacity(0.78)
                                })
                                .when(active, |this| this.font_weight(FontWeight::SEMIBOLD))
                                .child(step_text(step, "nav", cx)),
                        ),
                )
                .when(!last, |this| {
                    this.child(
                        div()
                            .ml(ui_rem(8.0 + 12.0))
                            .w(px(2.0))
                            .h(ui_rem(14.0))
                            .rounded_full()
                            .bg(foreground.opacity(if done { 0.6 } else { 0.22 })),
                    )
                })
        });

        v_flex()
            .id("onboarding-sidebar")
            .w(ui_rem(SIDEBAR_WIDTH))
            .h_full()
            .flex_shrink_0()
            .relative()
            .overflow_hidden()
            .bg(linear_gradient(
                160.0,
                linear_color_stop(top, 0.0),
                linear_color_stop(bottom, 1.0),
            ))
            .window_control_area(WindowControlArea::Drag)
            // 装饰性的半透明光斑，让渐变不显得单调。
            .child(
                div()
                    .absolute()
                    .top(ui_rem(-70.0))
                    .right(ui_rem(-80.0))
                    .size(ui_rem(220.0))
                    .rounded_full()
                    .bg(foreground.opacity(0.08)),
            )
            .child(
                div()
                    .absolute()
                    .bottom(ui_rem(-90.0))
                    .left(ui_rem(-60.0))
                    .size(ui_rem(200.0))
                    .rounded_full()
                    .bg(foreground.opacity(0.06)),
            )
            .child(
                v_flex()
                    .size_full()
                    .px_5()
                    .pt(ui_rem(44.0))
                    .pb_5()
                    .gap_6()
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(
                                div()
                                    .size(ui_rem(44.0))
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(12.0))
                                    .bg(foreground.opacity(0.95))
                                    .shadow_lg()
                                    .child(img("assets/icon128.png").size(ui_rem(32.0))),
                            )
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .text_lg()
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(foreground)
                                            .child(env!("CARGO_PKG_NAME")),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(foreground.opacity(0.75))
                                            .child(translator.translate("onboarding.tagline")),
                                    ),
                            ),
                    )
                    .child(v_flex().children(steps))
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_xs()
                            .text_color(foreground.opacity(0.6))
                            .child(format!("v{}", env!("CARGO_PKG_VERSION"))),
                    ),
            )
            .into_any_element()
    }

    fn render_top_bar(
        &self,
        theme: ThemeColor,
        animations: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> AnyElement {
        let progress = (self.step.index() + 1) as f32 / Step::ALL.len() as f32;
        let bar = div()
            .absolute()
            .top_0()
            .left_0()
            .h_full()
            .rounded_full()
            .bg(theme.primary);
        let bar = if animations {
            bar.with_spring(
                "onboarding-progress-bar",
                spring(progress),
                |this, value| this.w(relative(value.clamp(0.0, 1.0))),
            )
            .into_any_element()
        } else {
            bar.w(relative(progress)).into_any_element()
        };
        v_flex()
            .w_full()
            .flex_shrink_0()
            .child(
                h_flex()
                    .w_full()
                    .h(ui_rem(TITLE_BAR_HEIGHT))
                    .child(
                        div()
                            .id("onboarding-drag")
                            .flex_1()
                            .h_full()
                            .window_control_area(WindowControlArea::Drag),
                    )
                    .child(title_button(
                        "onboarding-minimize",
                        "window-minimize",
                        WindowControlArea::Min,
                        false,
                        theme,
                        |window| window.minimize_window(),
                    ))
                    .child(title_button(
                        "onboarding-close",
                        "window-close",
                        WindowControlArea::Close,
                        true,
                        theme,
                        close_window,
                    )),
            )
            .child(
                div()
                    .mx_8()
                    .h(px(4.0))
                    .relative()
                    .rounded_full()
                    .bg(theme.muted)
                    .overflow_hidden()
                    .child(bar),
            )
            .into_any_element()
    }

    fn render_content(
        &self,
        theme: ThemeColor,
        animations: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let translator = i18n::translator(cx);
        let step = self.step;
        let body = match step {
            Step::Welcome => self.render_welcome(theme, cx),
            Step::Appearance => self.render_appearance(theme, cx),
            Step::Shortcut => self.render_shortcut(theme, window, cx),
            Step::Habits => self.render_habits(theme, cx),
            Step::TryIt => self.render_try_it(theme, animations, cx),
        };
        let content = v_flex()
            .w_full()
            .gap_5()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.primary)
                            .child(translator.translate_with_args(
                                "onboarding.step_counter",
                                &[
                                    ("current", (step.index() + 1).to_string()),
                                    ("total", Step::ALL.len().to_string()),
                                ],
                            )),
                    )
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.foreground)
                            .child(step_text(step, "title", cx)),
                    )
                    .child(
                        div()
                            .text_sm()
                            .line_height(relative(1.6))
                            .text_color(theme.muted_foreground)
                            .child(step_text(step, "description", cx)),
                    ),
            )
            .child(body);
        let offset = if self.forward { 28.0 } else { -28.0 };
        let content = div().w_full().relative().child(content);
        let content = if animations {
            content
                .with_animation(
                    SharedString::from(format!("onboarding-step-{}", step.key())),
                    Animation::new(Duration::from_millis(260)).with_easing(gpui::ease_out_quint()),
                    move |this, progress| {
                        this.left(px(offset * (1.0 - progress))).opacity(progress)
                    },
                )
                .into_any_element()
        } else {
            content.into_any_element()
        };
        // 文案较长的语言或界面缩放较大时内容可能超出可视区域，允许纵向滚动。
        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .relative()
            .overflow_hidden()
            .child(
                div()
                    .id("onboarding-content-scroll")
                    .size_full()
                    .px_8()
                    .pt_6()
                    .pb_6()
                    .track_scroll(&self.scroll_handle)
                    .overflow_y_scroll()
                    .child(content),
            )
            .vertical_scrollbar(&self.scroll_handle)
            .into_any_element()
    }

    fn render_footer(&self, theme: ThemeColor, cx: &mut Context<Self>) -> AnyElement {
        let translator = i18n::translator(cx);
        let index = self.step.index();
        let last = index + 1 == Step::ALL.len();
        let view = cx.entity().clone();
        let back_view = view.clone();
        let dots = Step::ALL.into_iter().enumerate().map(|(dot, _)| {
            let active = dot == index;
            div()
                .h(px(6.0))
                .w(px(if active { 18.0 } else { 6.0 }))
                .rounded_full()
                .bg(if dot <= index {
                    theme.primary
                } else {
                    theme.muted_foreground.opacity(0.3)
                })
        });
        h_flex()
            .w_full()
            .flex_shrink_0()
            .px_8()
            .py_4()
            .gap_3()
            .items_center()
            .border_t_1()
            .border_color(theme.border.opacity(0.6))
            .child(
                Button::new("onboarding-skip")
                    .ghost()
                    .small()
                    .label(translator.translate(if last {
                        "onboarding.action.close"
                    } else {
                        "onboarding.action.skip"
                    }))
                    .on_click(|_, window, _| close_window(window)),
            )
            .child(h_flex().flex_1().justify_center().gap_1p5().children(dots))
            .when(index > 0, |this| {
                this.child(
                    Button::new("onboarding-back")
                        .outline()
                        .icon(icon("arrow-left", 14.0, theme.foreground))
                        .label(translator.translate("onboarding.action.back"))
                        .on_click(move |_, _, cx| {
                            back_view.update(cx, |view, cx| {
                                let step = Step::from_index(view.step.index().saturating_sub(1));
                                view.go_to(step, cx);
                            });
                        }),
                )
            })
            .child(
                Button::new("onboarding-next")
                    .primary()
                    .label(translator.translate(if last {
                        "onboarding.action.finish"
                    } else {
                        "onboarding.action.next"
                    }))
                    .on_click(move |_, window, cx| {
                        if last {
                            close_window(window);
                            return;
                        }
                        view.update(cx, |view, cx| {
                            let step = Step::from_index(view.step.index() + 1);
                            view.go_to(step, cx);
                        });
                    }),
            )
            .into_any_element()
    }
}

fn title_button(
    id: &'static str,
    icon_name: &'static str,
    area: WindowControlArea,
    danger: bool,
    theme: ThemeColor,
    on_click: impl Fn(&mut Window) + 'static,
) -> AnyElement {
    let (hover_bg, hover_fg) = if danger {
        (theme.danger, theme.danger_foreground)
    } else {
        (theme.secondary_hover, theme.foreground)
    };
    div()
        .id(id)
        .group(id)
        .h_full()
        .w(ui_rem(44.0))
        .flex()
        .items_center()
        .justify_center()
        .window_control_area(area)
        .hover(|style| style.bg(hover_bg))
        .on_click(move |_, window, _| on_click(window))
        .child(
            div()
                .child(icon(icon_name, 14.0, theme.muted_foreground))
                .group_hover(id, |style| style.text_color(hover_fg)),
        )
        .into_any_element()
}

// ---------- 通用小部件 ----------

/// 可选择的大卡片：选中时描边为主色并带浅色底。
fn choice_card(
    id: impl Into<SharedString>,
    selected: bool,
    theme: ThemeColor,
    content: impl IntoElement,
    on_click: impl Fn(&mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(gpui::ElementId::Name(id.into()))
        .flex_1()
        .min_w_0()
        .relative()
        .p_3()
        .rounded(px(12.0))
        .cursor_pointer()
        .border_2()
        .border_color(if selected {
            theme.primary
        } else {
            theme.border
        })
        .bg(if selected {
            theme.primary.opacity(0.07)
        } else {
            theme.background
        })
        .when(!selected, |this| {
            this.hover(|style| style.border_color(theme.primary.opacity(0.45)))
        })
        .on_click(move |_, _, cx| on_click(cx))
        .child(content)
        .when(selected, |this| {
            this.child(
                div()
                    .absolute()
                    .top(ui_rem(8.0))
                    .right(ui_rem(8.0))
                    .size(ui_rem(18.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(theme.primary)
                    .child(icon("check", 11.0, theme.primary_foreground)),
            )
        })
}

fn icon_badge(name: &str, tint: Hsla) -> AnyElement {
    div()
        .size(ui_rem(36.0))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(10.0))
        .bg(tint.opacity(0.12))
        .child(icon(name, 18.0, tint))
        .into_any_element()
}

fn switch_row(
    icon_name: &str,
    title: String,
    description: String,
    control: impl IntoElement,
    theme: ThemeColor,
) -> AnyElement {
    h_flex()
        .w_full()
        .gap_3()
        .items_center()
        .px_4()
        .py_3()
        .rounded(px(12.0))
        .border_1()
        .border_color(theme.border)
        .bg(theme.secondary.opacity(0.35))
        .child(icon_badge(icon_name, theme.primary))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.foreground)
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(description),
                ),
        )
        .child(control)
        .into_any_element()
}

/// 把快捷键拆成单独的键帽，例如 Ctrl、Shift、X。
fn binding_parts(binding: &ShortcutBinding) -> Vec<String> {
    let mut parts = Vec::new();
    if binding.control {
        parts.push("Ctrl".to_owned());
    }
    if binding.alt {
        parts.push("Alt".to_owned());
    }
    if binding.shift {
        parts.push("Shift".to_owned());
    }
    if binding.platform {
        parts.push("Win".to_owned());
    }
    let key = Kbd::format(&Keystroke {
        modifiers: gpui::Modifiers::default(),
        key: binding.key.clone(),
        key_char: None,
    });
    parts.push(if key.is_empty() {
        binding.key.to_uppercase()
    } else {
        key
    });
    parts
}

fn modifier_parts(modifiers: gpui::Modifiers) -> Vec<String> {
    let mut parts = Vec::new();
    if modifiers.control {
        parts.push("Ctrl".to_owned());
    }
    if modifiers.alt {
        parts.push("Alt".to_owned());
    }
    if modifiers.shift {
        parts.push("Shift".to_owned());
    }
    if modifiers.platform {
        parts.push("Win".to_owned());
    }
    parts
}

fn keycap(label: String, theme: ThemeColor, highlighted: bool) -> AnyElement {
    div()
        .min_w(ui_rem(46.0))
        .h(ui_rem(46.0))
        .px_3()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(10.0))
        .border_1()
        .border_b(px(4.0))
        .border_color(if highlighted {
            theme.primary
        } else {
            theme.border
        })
        .bg(theme.background)
        .shadow_sm()
        .text_lg()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(if highlighted {
            theme.primary
        } else {
            theme.foreground
        })
        .child(label)
        .into_any_element()
}

fn keycap_row(parts: Vec<String>, theme: ThemeColor, highlighted: bool) -> AnyElement {
    let count = parts.len();
    h_flex()
        .gap_2()
        .items_center()
        .children(
            parts
                .into_iter()
                .enumerate()
                .flat_map(move |(index, part)| {
                    let mut items = vec![keycap(part, theme, highlighted)];
                    if index + 1 < count {
                        items.push(
                            div()
                                .text_lg()
                                .text_color(theme.muted_foreground)
                                .child("+")
                                .into_any_element(),
                        );
                    }
                    items
                }),
        )
        .into_any_element()
}

// ---------- 各步骤内容 ----------

impl OnboardingView {
    fn render_welcome(&self, theme: ThemeColor, cx: &mut Context<Self>) -> AnyElement {
        let translator = i18n::translator(cx);
        let language = config_runtime::interface_language(cx);
        let cards = InterfaceLanguage::ALL.into_iter().map(|candidate| {
            let (label, sample) = match candidate {
                InterfaceLanguage::ZhCn => ("简体中文", "你好，欢迎使用"),
                InterfaceLanguage::EnUs => ("English", "Hello, welcome"),
            };
            choice_card(
                format!("onboarding-language-{}", candidate.code()),
                candidate == language,
                theme,
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(icon_badge("language", theme.primary))
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child(label),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(sample),
                            ),
                    ),
                move |cx| {
                    if let Err(error) = config_runtime::set_interface_language(candidate, cx) {
                        report_error(error, cx);
                    }
                    cx.refresh_windows();
                },
            )
        });
        let features = [
            ("clipboard-list", "history"),
            ("search", "search"),
            ("star", "favorites"),
        ]
        .into_iter()
        .map(|(icon_name, key)| {
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .p_3()
                .rounded(px(12.0))
                .bg(theme.secondary.opacity(0.4))
                .child(icon_badge(icon_name, theme.primary))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child(translator.translate(&format!("onboarding.feature.{key}.title"))),
                )
                .child(
                    div()
                        .text_xs()
                        .line_height(relative(1.5))
                        .text_color(theme.muted_foreground)
                        .child(
                            translator.translate(&format!("onboarding.feature.{key}.description")),
                        ),
                )
        });
        v_flex()
            .gap_5()
            .child(h_flex().gap_3().items_stretch().children(cards))
            .child(h_flex().gap_3().items_stretch().children(features))
            .into_any_element()
    }

    fn render_appearance(&self, theme: ThemeColor, cx: &mut Context<Self>) -> AnyElement {
        let translator = i18n::translator(cx);
        let appearance = config_runtime::appearance_config(cx);
        let options = available_theme_options(cx);
        let light = crate::windows::settings::selected_theme_preview(
            &options,
            ThemeMode::Light,
            &appearance.light_theme,
        );
        let dark = crate::windows::settings::selected_theme_preview(
            &options,
            ThemeMode::Dark,
            &appearance.dark_theme,
        );
        let cards = AppearanceMode::ALL.into_iter().map(|mode| {
            let preview = match mode {
                AppearanceMode::Light => {
                    crate::windows::settings::theme_thumbnail(light).into_any_element()
                }
                AppearanceMode::Dark => {
                    crate::windows::settings::theme_thumbnail(dark).into_any_element()
                }
                AppearanceMode::System => div()
                    .size_full()
                    .relative()
                    .child(crate::windows::settings::theme_thumbnail(light))
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .h_full()
                            .w(relative(0.5))
                            .overflow_hidden()
                            .child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .right_0()
                                    .h_full()
                                    .w(relative(2.0))
                                    .child(crate::windows::settings::theme_thumbnail(dark)),
                            ),
                    )
                    .into_any_element(),
            };
            choice_card(
                format!("onboarding-appearance-{}", mode.code()),
                mode == appearance.mode,
                theme,
                v_flex()
                    .gap_3()
                    .child(div().w_full().h(ui_rem(118.0)).child(preview))
                    .child(
                        div()
                            .w_full()
                            .text_center()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.foreground)
                            .child(translator.translate(
                                crate::windows::settings::appearance_mode_label_key(mode),
                            )),
                    ),
                move |cx| {
                    let mut appearance = config_runtime::appearance_config(cx);
                    appearance.mode = mode;
                    if let Err(error) = config_runtime::set_appearance_config(appearance, cx) {
                        report_error(error, cx);
                    }
                    cx.refresh_windows();
                },
            )
        });
        v_flex()
            .gap_4()
            .child(h_flex().gap_3().items_stretch().children(cards))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(icon("bulb", 14.0, theme.muted_foreground))
                    .child(translator.translate("onboarding.appearance.more")),
            )
            .into_any_element()
    }

    fn render_shortcut(
        &self,
        theme: ThemeColor,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let translator = i18n::translator(cx);
        let shortcuts = config_runtime::shortcuts_config(cx);
        let view = cx.entity().clone();
        let keys = if self.recording {
            let parts = modifier_parts(self.recording_modifiers);
            if parts.is_empty() {
                div()
                    .h(ui_rem(50.0))
                    .flex()
                    .items_center()
                    .text_base()
                    .text_color(theme.primary)
                    .child(translator.translate("onboarding.shortcut.recording"))
                    .with_animation(
                        "onboarding-recording-pulse",
                        Animation::new(Duration::from_millis(1200))
                            .repeat()
                            .with_easing(gpui::pulsating_between(0.45, 1.0)),
                        |this, progress| this.opacity(progress),
                    )
                    .into_any_element()
            } else {
                let mut parts = parts;
                parts.push("…".to_owned());
                keycap_row(parts, theme, true)
            }
        } else {
            match shortcuts.toggle_main_window.as_ref() {
                Some(binding) => keycap_row(binding_parts(binding), theme, false),
                None => div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(translator.translate("onboarding.shortcut.unset"))
                    .into_any_element(),
            }
        };
        let record_view = view.clone();
        let stop_view = view.clone();
        let reset_view = view.clone();
        let action_buttons = if self.recording {
            h_flex().gap_2().child(
                Button::new("onboarding-shortcut-cancel")
                    .outline()
                    .small()
                    .label(translator.translate("onboarding.shortcut.cancel"))
                    .on_click(move |_, _, cx| {
                        stop_view.update(cx, |view, cx| view.stop_recording(cx));
                    }),
            )
        } else {
            h_flex()
                .gap_2()
                .child(
                    Button::new("onboarding-shortcut-record")
                        .primary()
                        .small()
                        .icon(icon("keyboard", 14.0, theme.primary_foreground))
                        .label(translator.translate("onboarding.shortcut.change"))
                        .on_click(move |_, window, cx| {
                            record_view.update(cx, |view, cx| view.start_recording(window, cx));
                        }),
                )
                .when(
                    shortcuts.toggle_main_window
                        != Some(ShortcutBinding::toggle_main_window_default()),
                    |this| {
                        this.child(
                            Button::new("onboarding-shortcut-reset")
                                .ghost()
                                .small()
                                .icon(icon("refresh", 14.0, theme.foreground))
                                .label(translator.translate("onboarding.shortcut.reset"))
                                .on_click(move |_, _, cx| {
                                    if let Err(error) =
                                        hotkey::reset_toggle_main_window_shortcut(cx)
                                    {
                                        report_error(error, cx);
                                    }
                                    reset_view.update(cx, |view, cx| {
                                        view.shortcut_hint = None;
                                        cx.notify();
                                    });
                                }),
                        )
                    },
                )
        };

        let stage = v_flex()
            .w_full()
            .items_center()
            .h(ui_rem(190.0))
            .justify_center()
            .gap_4()
            .rounded(px(14.0))
            .border_2()
            .border_color(if self.recording {
                theme.primary
            } else {
                theme.border.opacity(0.7)
            })
            .bg(linear_gradient(
                180.0,
                linear_color_stop(theme.primary.opacity(0.07), 0.0),
                linear_color_stop(theme.background.opacity(0.0), 1.0),
            ))
            .child(keys)
            .child(action_buttons)
            .when_some(self.shortcut_hint.clone(), |this, hint| {
                this.child(
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .text_xs()
                        .text_color(theme.warning)
                        .child(icon("exclamation-circle", 13.0, theme.warning))
                        .child(hint),
                )
            })
            .when(self.recording && self.shortcut_hint.is_none(), |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(translator.translate("onboarding.shortcut.recording_hint")),
                )
            });

        let middle_mouse = shortcuts.middle_mouse.clone();
        let middle_switch = Switch::new("onboarding-middle-mouse")
            .checked(middle_mouse.enabled)
            .on_click(move |checked, _, cx| {
                let mut config = middle_mouse.clone();
                config.enabled = *checked;
                if let Err(error) = hotkey::set_middle_mouse_config(config, cx) {
                    report_error(error, cx);
                }
                cx.refresh_windows();
            });
        v_flex()
            .gap_3()
            .child(stage)
            .child(switch_row(
                "mouse",
                translator.translate("onboarding.shortcut.middle_mouse"),
                translator.translate("onboarding.shortcut.middle_mouse_description"),
                middle_switch,
                theme,
            ))
            .into_any_element()
    }

    fn render_habits(&self, theme: ThemeColor, cx: &mut Context<Self>) -> AnyElement {
        let translator = i18n::translator(cx);
        let clipboard = config_runtime::clipboard_config(cx);
        let click_cards = [
            (ClipboardMouseBehavior::SinglePaste, "hand-click", "single"),
            (ClipboardMouseBehavior::DoublePaste, "mouse", "double"),
        ]
        .into_iter()
        .map(|(behavior, icon_name, key)| {
            choice_card(
                format!("onboarding-click-{key}"),
                clipboard.mouse_left_behavior == behavior,
                theme,
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(icon_badge(icon_name, theme.primary))
                    .child(
                        v_flex()
                            .min_w_0()
                            .pr_4()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child(
                                        translator.translate(&format!("onboarding.click.{key}")),
                                    ),
                            )
                            .child(
                                div().text_xs().text_color(theme.muted_foreground).child(
                                    translator
                                        .translate(&format!("onboarding.click.{key}_description")),
                                ),
                            ),
                    ),
                move |cx| {
                    let mut clipboard = config_runtime::clipboard_config(cx);
                    clipboard.mouse_left_behavior = behavior;
                    if let Err(error) = config_runtime::set_clipboard_config(clipboard, cx) {
                        report_error(error, cx);
                    }
                    cx.refresh_windows();
                },
            )
        });
        let formatted_switch = Switch::new("onboarding-formatted-paste")
            .checked(clipboard.formatted_paste)
            .on_click(|checked, _, cx| {
                let mut clipboard = config_runtime::clipboard_config(cx);
                clipboard.formatted_paste = *checked;
                if let Err(error) = config_runtime::set_clipboard_config(clipboard, cx) {
                    report_error(error, cx);
                }
                crate::services::paste::set_formatted_paste_enabled(*checked);
                cx.refresh_windows();
            });
        let launch_switch = Switch::new("onboarding-launch-at-login")
            .checked(config_runtime::launch_at_login(cx))
            .on_click(|checked, _, cx| {
                if let Err(error) = config_runtime::set_launch_at_login(*checked, cx) {
                    report_error(error, cx);
                }
                cx.refresh_windows();
            });
        v_flex()
            .gap_3()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted_foreground)
                    .child(translator.translate("onboarding.click.title")),
            )
            .child(h_flex().gap_3().items_stretch().children(click_cards))
            .child(switch_row(
                "typography",
                translator.translate("onboarding.habits.formatted_paste"),
                translator.translate("onboarding.habits.formatted_paste_description"),
                formatted_switch,
                theme,
            ))
            .child(switch_row(
                "power",
                translator.translate("onboarding.habits.launch_at_login"),
                translator.translate("onboarding.habits.launch_at_login_description"),
                launch_switch,
                theme,
            ))
            .into_any_element()
    }

    fn render_try_it(
        &self,
        theme: ThemeColor,
        animations: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let translator = i18n::translator(cx);
        let shown = self.main_window_shown;
        let binding = config_runtime::shortcuts_config(cx).toggle_main_window;
        let tint = if shown { theme.success } else { theme.primary };

        let halo = |size: f32, opacity: f32, id: &'static str, delay: f32| {
            let ring = div()
                .absolute()
                .size(ui_rem(size))
                .rounded_full()
                .bg(tint.opacity(opacity));
            if animations && !shown {
                ring.with_animation(
                    id,
                    Animation::new(Duration::from_millis(1800)).repeat(),
                    move |this, progress| {
                        let phase = (progress + delay).fract();
                        let wave = (phase * std::f32::consts::TAU).sin() * 0.5 + 0.5;
                        this.opacity(0.35 + wave * 0.65)
                    },
                )
                .into_any_element()
            } else {
                ring.into_any_element()
            }
        };
        let emblem = div()
            .size(ui_rem(132.0))
            .flex_shrink_0()
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .child(halo(132.0, 0.08, "onboarding-halo-outer", 0.0))
            .child(halo(100.0, 0.12, "onboarding-halo-inner", 0.3))
            .child(
                div()
                    .size(ui_rem(68.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(tint)
                    .shadow_lg()
                    .child(icon(
                        if shown { "confetti" } else { "rocket" },
                        32.0,
                        theme.primary_foreground,
                    )),
            );

        let message = if shown {
            v_flex()
                .gap_1()
                .items_center()
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme.success)
                        .child(translator.translate("onboarding.try.success")),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(translator.translate("onboarding.try.success_description")),
                )
                .into_any_element()
        } else {
            v_flex()
                .gap_3()
                .items_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(translator.translate("onboarding.try.press")),
                )
                .child(match binding.as_ref() {
                    Some(binding) => keycap_row(binding_parts(binding), theme, true),
                    None => div()
                        .text_sm()
                        .text_color(theme.warning)
                        .child(translator.translate("onboarding.shortcut.unset"))
                        .into_any_element(),
                })
                .into_any_element()
        };
        let tray_tip = h_flex()
            .max_w_full()
            .gap_2()
            .items_center()
            .px_3()
            .py_2()
            .rounded(px(8.0))
            .bg(theme.secondary.opacity(0.45))
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(icon("info-circle", 14.0, theme.muted_foreground))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(translator.translate("onboarding.try.tray_tip")),
            );

        let body = v_flex()
            .w_full()
            .items_center()
            .gap_4()
            .child(emblem)
            .child(message)
            .child(tray_tip);
        if animations && shown {
            body.with_animation(
                "onboarding-try-success",
                Animation::new(Duration::from_millis(420)).with_easing(gpui::ease_out_quint()),
                |this, progress| this.opacity(progress).mt(px(12.0 * (1.0 - progress))),
            )
            .into_any_element()
        } else {
            body.into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Step, binding_parts, modifier_parts};
    use crate::config::ShortcutBinding;

    #[test]
    fn steps_round_trip_by_index() {
        for (index, step) in Step::ALL.into_iter().enumerate() {
            assert_eq!(step.index(), index);
            assert_eq!(Step::from_index(index), step);
        }
        assert_eq!(Step::from_index(99), Step::TryIt);
    }

    #[test]
    fn binding_is_split_into_keycaps_in_windows_order() {
        let binding = ShortcutBinding {
            control: true,
            alt: true,
            shift: true,
            platform: true,
            key: "x".to_owned(),
        };
        let parts = binding_parts(&binding);
        assert_eq!(&parts[..4], ["Ctrl", "Alt", "Shift", "Win"]);
        assert_eq!(parts.len(), 5);
        assert!(parts[4].eq_ignore_ascii_case("x"));
    }

    #[test]
    fn modifier_parts_are_empty_without_modifiers() {
        assert!(modifier_parts(gpui::Modifiers::default()).is_empty());
    }
}
