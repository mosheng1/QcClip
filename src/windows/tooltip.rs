use std::{
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::prelude::*;
use gpui::{
    AnyView, AnyWindowHandle, App, AvailableSpace, Bounds, Context, DispatchPhase, EntityId,
    Global, MouseDownEvent, PathBuilder, Pixels, Point, Render, Role, ScrollWheelEvent, Size, Task,
    WeakEntity, Window, WindowHandle, canvas, div, point, px, size,
};
use gpui_component::{
    ActiveTheme as _, Placement,
    tooltip::{WindowTooltipEvent, WindowTooltipHost, WindowTooltipTrigger},
};

use crate::platform::{input, window as platform};

const SHOW_DELAY: Duration = Duration::from_millis(500);
const SWITCH_GRACE: Duration = Duration::from_millis(240);
const APPEAR_DURATION: Duration = Duration::from_millis(90);
const TRANSITION_DURATION: Duration = Duration::from_millis(140);
const POLL_INTERVAL: Duration = Duration::from_millis(40);
const ARROW: f32 = 8.0;
const RADIUS: f32 = 7.0;
const MARGIN: f32 = 8.0;
const BORDER_WIDTH: f32 = 1.5;

struct Request {
    trigger: WeakEntity<WindowTooltipTrigger>,
    source: AnyWindowHandle,
    show_at: Instant,
    mouse_sequence: Option<u64>,
    was_active: bool,
    dirty: bool,
    switching: bool,
}

#[derive(Default)]
pub(crate) struct TooltipService {
    request: Option<Request>,
    popup: Option<WindowHandle<TooltipWindow>>,
    hide_at: Option<Instant>,
    task: Option<Task<()>>,
}

impl Global for TooltipService {}

impl TooltipService {
    pub(crate) fn install(cx: &mut App) {
        cx.set_global(Self::default());
        cx.set_global(WindowTooltipHost {
            dispatch: Rc::new(Self::dispatch),
            window_id: None,
        });
        cx.observe_keystrokes(|_, _, cx| Self::dismiss(cx)).detach();
    }

    fn dispatch(event: WindowTooltipEvent, window: &mut Window, cx: &mut App) {
        match event {
            WindowTooltipEvent::Show(trigger) => {
                cx.update_global::<Self, _>(|service, cx| {
                    if service
                        .request
                        .as_ref()
                        .is_some_and(|request| request.trigger == trigger)
                    {
                        service.resume(cx);
                        return;
                    }
                    let quick_switch = service.popup.is_some();
                    service.resume(cx);
                    if let Some(popup) = service.popup {
                        let _ = popup.update(cx, |_, window, _| {
                            // 复用窗口时也重新提升层级，避免切换寄主后被其他置顶窗口遮挡。
                            if let Err(error) = platform::raise_window_to_topmost(window) {
                                tracing::warn!("提升提示窗口层级失败：{error}");
                            }
                        });
                    }
                    service.request = Some(Request {
                        trigger,
                        source: window.window_handle(),
                        show_at: Instant::now()
                            + if quick_switch {
                                Duration::ZERO
                            } else {
                                SHOW_DELAY
                            },
                        mouse_sequence: input::latest_mouse_down_sequence(),
                        was_active: window.is_window_active(),
                        dirty: true,
                        switching: quick_switch,
                    });
                    service.task = Some(cx.spawn(async move |cx| {
                        if !quick_switch {
                            cx.background_executor().timer(POLL_INTERVAL).await;
                        }
                        loop {
                            // 切换立即处理，轮询间隔只用于后续状态检查。
                            let keep_running = cx.update(|cx| {
                                cx.update_global::<Self, _>(|service, cx| service.tick(cx))
                            });
                            if !keep_running {
                                break;
                            }
                            cx.background_executor().timer(POLL_INTERVAL).await;
                        }
                    }));
                    // 下一帧安装来源窗口的滚轮和点击关闭处理。
                    window.refresh();
                });
            }
            WindowTooltipEvent::Hide(id) => {
                cx.update_global::<Self, _>(|service, cx| {
                    if service.is_current(id) {
                        service.request_hide(cx);
                    }
                });
            }
            WindowTooltipEvent::Refresh(id) => {
                let current = cx.update_global::<Self, _>(|service, _| {
                    if service.is_current(id) {
                        service.request.as_mut().unwrap().dirty = true;
                        true
                    } else {
                        false
                    }
                });
                if current {
                    window.on_mouse_event(|_: &MouseDownEvent, phase, _, cx| {
                        if phase == DispatchPhase::Capture {
                            Self::dismiss(cx);
                        }
                    });
                    window.on_mouse_event(|_: &ScrollWheelEvent, phase, _, cx| {
                        if phase == DispatchPhase::Capture {
                            Self::dismiss(cx);
                        }
                    });
                }
            }
        }
    }

    fn is_current(&self, id: EntityId) -> bool {
        self.request
            .as_ref()
            .is_some_and(|request| request.trigger.entity_id() == id)
    }

    fn dismiss(cx: &mut App) {
        cx.update_global::<Self, _>(|service, cx| service.clear(cx));
    }

    fn clear(&mut self, cx: &mut App) {
        self.request = None;
        self.hide_at = None;
        if let Some(popup) = self.popup.take() {
            cx.global_mut::<WindowTooltipHost>().window_id = None;
            let _ = popup.update(cx, |_, window, _| window.remove_window());
        }
    }

    fn resume(&mut self, cx: &mut App) {
        if self.hide_at.take().is_some()
            && let Some(popup) = self.popup
        {
            let _ = popup.update(cx, |view, _, cx| {
                view.fade_to(1.0, Instant::now(), APPEAR_DURATION);
                cx.notify();
            });
        }
    }

    fn request_hide(&mut self, cx: &mut App) {
        if let Some(popup) = self.popup {
            if self.hide_at.is_none() {
                let now = Instant::now();
                self.hide_at = Some(now + SWITCH_GRACE);
                let _ = popup.update(cx, |view, _, cx| {
                    view.fade_to(0.0, now, SWITCH_GRACE);
                    cx.notify();
                });
            }
        } else {
            self.clear(cx);
        }
    }

    fn tick(&mut self, cx: &mut App) -> bool {
        if let Some(hide_at) = self.hide_at {
            if Instant::now() >= hide_at {
                self.clear(cx);
                return false;
            }
            return true;
        }
        let Some(request) = self.request.as_mut() else {
            return false;
        };
        let Some(trigger) = request.trigger.upgrade() else {
            self.request_hide(cx);
            return self.popup.is_some();
        };
        let bounds = trigger.read(cx).bounds;
        let placement = trigger.read(cx).placement;
        let build = trigger.read(cx).build.clone();
        let ready = Instant::now() >= request.show_at;
        let rebuild = ready && (request.dirty || self.popup.is_none());
        let source = request
            .source
            .update(cx, |_, window, cx| {
                if (request.was_active && !window.is_window_active())
                    || cx.has_active_drag()
                    || bounds.size.width <= px(0.0)
                    || bounds.size.height <= px(0.0)
                    || input::latest_mouse_down_sequence() != request.mouse_sequence
                {
                    return None;
                }
                let origin = platform::logical_to_physical_desktop(window, bounds.origin)?;
                let scale = window.scale_factor();
                let anchor = Bounds::new(
                    origin,
                    size(
                        f32::from(bounds.size.width) * scale,
                        f32::from(bounds.size.height) * scale,
                    ),
                );
                let pointer = platform::physical_cursor_position().or_else(|| {
                    platform::logical_to_physical_desktop(window, window.mouse_position())
                })?;
                let hovered = anchor.contains(&pointer)
                    && platform::is_window_at_physical_point(window, pointer);
                Some((
                    anchor,
                    window.rem_size(),
                    (hovered && rebuild).then(|| build(window, cx)),
                    hovered,
                ))
            })
            .ok()
            .flatten();
        let Some((anchor, rem_size, content, hovered)) = source else {
            self.clear(cx);
            return false;
        };
        if !hovered {
            self.request_hide(cx);
            return self.request.is_some();
        }
        if !ready {
            return true;
        }
        request.dirty = false;
        let switching = std::mem::take(&mut request.switching);
        if let Some(popup) = self.popup {
            if popup
                .update(cx, |view, _, cx| {
                    let mut changed = view.anchor != anchor
                        || view.rem_size != rem_size
                        || view.placement != placement;
                    view.anchor = anchor;
                    view.rem_size = rem_size;
                    view.placement = placement;
                    if let Some(content) = content {
                        changed |= view.replace_content(content, switching, Instant::now(), cx);
                    }
                    if changed {
                        cx.notify();
                    }
                })
                .is_err()
            {
                self.clear(cx);
                return false;
            }
        } else if let Some(content) = content {
            match TooltipWindow::open(content, anchor, placement, rem_size, cx) {
                Ok(popup) => self.popup = Some(popup),
                Err(error) => {
                    tracing::error!("打开提示窗口失败：{error}");
                    self.clear(cx);
                    return false;
                }
            }
        }
        true
    }
}

struct TooltipWindow {
    content: AnyView,
    previous_content: Option<FadingContent>,
    content_size: Size<Pixels>,
    measurement: Option<(Size<Pixels>, Pixels)>,
    anchor: Bounds<f32>,
    placement: Option<Placement>,
    rem_size: Pixels,
    popup: platform::BorderlessPopup,
    last_bounds: Option<Bounds<f32>>,
    pending_bounds: Option<Bounds<f32>>,
    layout: Option<BubbleLayout>,
    motion: Option<BubbleMotion>,
    fade: Option<TooltipFade>,
}

#[derive(Clone, Copy)]
struct TooltipFade {
    from: f32,
    to: f32,
    started_at: Instant,
    duration: Duration,
}

impl TooltipFade {
    fn value(self, now: Instant) -> f32 {
        let progress = (now.saturating_duration_since(self.started_at).as_secs_f32()
            / self.duration.as_secs_f32())
        .clamp(0.0, 1.0);
        let eased = if self.to < self.from {
            progress * progress
        } else {
            1.0 - (1.0 - progress).powi(3)
        };
        self.from + (self.to - self.from) * eased
    }

    fn done(self, now: Instant) -> bool {
        now.saturating_duration_since(self.started_at) >= self.duration
    }
}

struct FadingContent {
    layers: Vec<(AnyView, Size<Pixels>, f32)>,
    started_at: Instant,
}

impl TooltipWindow {
    fn fade_to(&mut self, to: f32, now: Instant, duration: Duration) {
        let from = self.fade.map(|fade| fade.value(now)).unwrap_or(1.0);
        self.fade = Some(TooltipFade {
            from,
            to,
            started_at: now,
            duration,
        });
    }

    fn replace_content(
        &mut self,
        content: AnyView,
        switching: bool,
        now: Instant,
        cx: &App,
    ) -> bool {
        if let (Ok(current), Ok(next)) = (
            self.content
                .clone()
                .downcast::<gpui_component::tooltip::Tooltip>(),
            content
                .clone()
                .downcast::<gpui_component::tooltip::Tooltip>(),
        ) && current.read(cx).has_same_content(next.read(cx))
        {
            return false;
        }
        if switching && self.layout.is_some() {
            // 中途再次切换时保留当前混合比例，避免尚未完成的内容突然变亮。
            let mut layers = if let Some(previous) = self.previous_content.take() {
                let progress = transition_progress(previous.started_at, now);
                let mut layers = previous.layers;
                for (_, _, opacity) in &mut layers {
                    *opacity *= 1.0 - (progress * 1.6).min(1.0);
                }
                layers.push((self.content.clone(), self.content_size, progress));
                layers
            } else {
                vec![(self.content.clone(), self.content_size, 1.0)]
            };
            layers.retain(|(_, _, opacity)| *opacity > 0.01);
            self.previous_content = Some(FadingContent {
                layers,
                started_at: now,
            });
        }
        self.content = content;
        self.measurement = None;
        true
    }

    fn open(
        content: AnyView,
        anchor: Bounds<f32>,
        placement: Option<Placement>,
        rem_size: Pixels,
        cx: &mut App,
    ) -> Result<WindowHandle<Self>, String> {
        let display = platform::display_for_physical_point(cx, anchor.center());
        let scale = display.map(|display| display.scale_factor).unwrap_or(1.0);
        let initial = Bounds::new(
            point(px(anchor.origin.x / scale), px(anchor.origin.y / scale)),
            size(px(1.0), px(1.0)),
        );
        platform::open_borderless_mouse_passthrough_popup(
            cx,
            initial,
            display.map(|display| display.display_id),
            move |window, cx, popup| {
                cx.global_mut::<WindowTooltipHost>().window_id =
                    Some(window.window_handle().window_id());
                cx.new(|_| Self {
                    content,
                    previous_content: None,
                    content_size: Size::default(),
                    measurement: None,
                    anchor,
                    placement,
                    rem_size,
                    popup,
                    last_bounds: None,
                    pending_bounds: None,
                    layout: None,
                    motion: None,
                    fade: None,
                })
            },
        )
    }

    fn content(&self, max_size: Size<Pixels>, cx: &App) -> gpui::Div {
        Self::content_element(self.content.clone(), max_size, cx)
    }

    fn content_element(content: AnyView, max_size: Size<Pixels>, cx: &App) -> gpui::Div {
        div()
            .max_w(max_size.width)
            .max_h(max_size.height)
            .font_family(cx.theme().font_family.clone())
            .text_size(gpui::rems(0.75))
            .line_height(gpui::rems(1.125))
            .px(gpui::rems(9.0 / 16.0))
            .py(gpui::rems(5.0 / 16.0))
            .overflow_hidden()
            .child(content)
    }

    fn animate_layout(
        &mut self,
        target: BubbleLayout,
        now: Instant,
        animate: bool,
    ) -> BubbleLayout {
        if !animate {
            self.motion = None;
            self.previous_content = None;
            self.layout = Some(target);
            return target;
        }
        let previous_target = self.motion.as_ref().map(|motion| motion.to).or(self.layout);
        // 忽略亚像素布局噪声，避免反复重启同一个目标的动画。
        let target = previous_target
            .filter(|previous| previous.nearly_equal(target))
            .unwrap_or(target);
        if previous_target != Some(target) {
            if let Some(from) = self
                .motion
                .as_ref()
                .map(|motion| motion.frame(now))
                .or(self.layout)
            {
                self.motion = Some(BubbleMotion {
                    from,
                    to: target,
                    started_at: now,
                });
            }
        }
        let current = if let Some(motion) = &self.motion {
            let current = motion.frame(now);
            if now.saturating_duration_since(motion.started_at) >= TRANSITION_DURATION {
                self.motion = None;
            }
            current
        } else {
            target
        };
        self.layout = Some(current);
        current
    }
}

impl Render for TooltipWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(self.rem_size);
        let display = platform::display_for_physical_point(cx, self.anchor.center());
        let scale = window.scale_factor().max(0.1);
        let screen = display
            .map(|display| {
                Bounds::new(
                    point(display.bounds.x, display.bounds.y),
                    size(display.bounds.width, display.bounds.height),
                )
            })
            .unwrap_or_else(|| Bounds::new(point(0.0, 0.0), size(1920.0, 1080.0)));
        let ui_scale = f32::from(self.rem_size) / 16.0;
        let inset = MARGIN * ui_scale;
        let arrow = ARROW * ui_scale;
        let radius = RADIUS * ui_scale;
        let max_size = size(
            px((screen.size.width / scale - (inset + arrow) * 2.0)
                .max(1.0)
                .min(320.0 * ui_scale)),
            px((screen.size.height / scale - (inset + arrow) * 2.0).max(1.0)),
        );
        // 内容、约束或字号变化时用 GPUI 测量，动画帧复用结果。
        let measured = if self.measurement == Some((max_size, self.rem_size)) {
            self.content_size
        } else {
            let measured = self
                .content(max_size, cx)
                .into_any_element()
                .layout_as_root(
                    size(AvailableSpace::MaxContent, AvailableSpace::MaxContent),
                    window,
                    cx,
                );
            self.content_size = measured;
            self.measurement = Some((max_size, self.rem_size));
            measured
        };
        let target = BubbleLayout::new(
            self.anchor,
            measured,
            screen,
            scale,
            ui_scale,
            self.placement,
        );
        let now = Instant::now();
        let layout = self.animate_layout(target, now, !cx.reduce_motion());
        let content_progress = self
            .previous_content
            .as_ref()
            .map(|previous| transition_progress(previous.started_at, now))
            .unwrap_or(1.0);
        if content_progress >= 1.0 {
            self.previous_content = None;
        }
        let fade = *self.fade.get_or_insert(TooltipFade {
            from: 0.0,
            to: 1.0,
            started_at: now,
            duration: APPEAR_DURATION,
        });
        let opacity = if cx.reduce_motion() {
            fade.to
        } else {
            fade.value(now)
        };
        let animating = self.motion.is_some()
            || self.previous_content.is_some()
            || (!cx.reduce_motion() && !fade.done(now));
        if animating {
            window.request_animation_frame();
        }
        // 可见期间只扩展已使用的承载范围，不在动画结束时收缩原生窗口。
        let bounds =
            transition_window_bounds(self.pending_bounds.or(self.last_bounds), target.window);
        if self.last_bounds != Some(bounds) && self.pending_bounds != Some(bounds) {
            self.pending_bounds = Some(bounds);
            let entity = cx.weak_entity();
            let popup = self.popup;
            window.defer(cx, move |window, cx| {
                let Some(entity) = entity.upgrade() else {
                    return;
                };
                if entity.read(cx).pending_bounds != Some(bounds) {
                    return;
                }
                let first_frame = entity.read(cx).last_bounds.is_none();
                let updated = popup.prepare_to_physical_bounds(
                    window,
                    cx,
                    bounds.origin.x,
                    bounds.origin.y,
                    bounds.size.width,
                    bounds.size.height,
                );
                entity.update(cx, |view, _| {
                    view.pending_bounds = None;
                    if updated {
                        view.last_bounds = Some(bounds);
                    }
                });
                if updated {
                    // 原生尺寸就绪后再按新客户区重绘，不在 Render 中触发尺寸消息。
                    window.refresh();
                    window.draw(cx).clear(cx);
                    if first_frame {
                        popup.set_visible(window, true);
                    }
                    window.refresh();
                } else {
                    tracing::error!("调整提示窗口边界失败");
                }
            });
        }
        // 新承载区尚未提交时沿用当前坐标系，避免旧交换链上的气泡提前跳位。
        let bounds = self.last_bounds.unwrap_or(bounds);
        let mut background = cx.theme().popover;
        background.alpha = 1.0;
        let dark = cx.theme().mode.is_dark();
        let border = if dark {
            gpui::hsla(0.0, 0.0, 1.0, 0.2)
        } else {
            gpui::hsla(0.0, 0.0, 0.0, 0.18)
        };
        let layout = layout.in_window(bounds, scale);
        let body = layout.body;
        div()
            .id("tooltip-window")
            .role(Role::Tooltip)
            .size_full()
            .opacity(opacity)
            .font_family(cx.theme().font_family.clone())
            .text_color(cx.theme().popover_foreground)
            .text_sm()
            .child(
                canvas(
                    |_, _, _| {},
                    move |_, _, window, _| {
                        // 使用双层阴影，避免每帧重复细分多条轮廓路径。
                        let (ambient, contact) = if dark { (0.55, 0.45) } else { (0.2, 0.12) };
                        window.paint_drop_shadows(
                            body,
                            gpui::Corners::all(px(radius)),
                            &[
                                gpui::BoxShadow::new(
                                    px(0.0),
                                    px(2.0 * ui_scale),
                                    gpui::hsla(0.0, 0.0, 0.0, ambient),
                                )
                                .blur_radius(px(6.0 * ui_scale)),
                                gpui::BoxShadow::new(
                                    px(0.0),
                                    px(0.0),
                                    gpui::hsla(0.0, 0.0, 0.0, contact),
                                )
                                .blur_radius(px(ui_scale)),
                            ],
                        );
                        // 气泡和尖角使用同一条闭合路径，边缘没有拼接缝。
                        for (stroke, color) in [(false, background), (true, border)] {
                            let mut path = if stroke {
                                PathBuilder::stroke(px(BORDER_WIDTH * ui_scale))
                            } else {
                                PathBuilder::fill()
                            };
                            bubble_path(&mut path, &layout, radius);
                            if let Ok(path) = path.build() {
                                window.paint_path(path, color);
                            }
                        }
                    },
                )
                .absolute()
                .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .left(body.origin.x)
                    .top(body.origin.y)
                    .w(body.size.width)
                    .h(body.size.height)
                    .overflow_hidden()
                    .children(
                        self.previous_content
                            .iter()
                            .flat_map(|previous| &previous.layers)
                            .map(|(view, measured, opacity)| {
                                div()
                                    .absolute()
                                    .left_0()
                                    .top_0()
                                    .w(measured.width)
                                    .h(measured.height)
                                    .opacity(*opacity * (1.0 - (content_progress * 1.6).min(1.0)))
                                    .child(Self::content_element(view.clone(), *measured, cx))
                            }),
                    )
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .top_0()
                            .w(measured.width)
                            .h(measured.height)
                            .opacity(content_progress)
                            .child(self.content(max_size, cx)),
                    ),
            )
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct BubbleLayout {
    window: Bounds<f32>,
    body: Bounds<Pixels>,
    side: Placement,
    tip: Point<Pixels>,
    base: Pixels,
    arrow: Pixels,
}

struct BubbleMotion {
    from: BubbleLayout,
    to: BubbleLayout,
    started_at: Instant,
}

fn transition_window_bounds(current: Option<Bounds<f32>>, target: Bounds<f32>) -> Bounds<f32> {
    if let Some(current) = current {
        let origin = point(
            current.left().min(target.left()),
            current.top().min(target.top()),
        );
        Bounds::new(
            origin,
            size(
                current.right().max(target.right()) - origin.x,
                current.bottom().max(target.bottom()) - origin.y,
            ),
        )
    } else {
        target
    }
}

fn transition_progress(started_at: Instant, now: Instant) -> f32 {
    let progress = (now.saturating_duration_since(started_at).as_secs_f32()
        / TRANSITION_DURATION.as_secs_f32())
    .clamp(0.0, 1.0);
    1.0 - (1.0 - progress).powi(3)
}

impl BubbleMotion {
    fn frame(&self, now: Instant) -> BubbleLayout {
        let progress = transition_progress(self.started_at, now);
        if progress >= 1.0 {
            return self.to;
        }
        let lerp = |from: f32, to: f32| from + (to - from) * progress;
        let pixel = |from: Pixels, to: Pixels| px(lerp(f32::from(from), f32::from(to)));
        let mut frame = BubbleLayout {
            window: Bounds::new(
                point(
                    lerp(self.from.window.left(), self.to.window.left()),
                    lerp(self.from.window.top(), self.to.window.top()),
                ),
                size(
                    lerp(self.from.window.size.width, self.to.window.size.width),
                    lerp(self.from.window.size.height, self.to.window.size.height),
                ),
            ),
            body: Bounds::new(
                point(
                    pixel(self.from.body.left(), self.to.body.left()),
                    pixel(self.from.body.top(), self.to.body.top()),
                ),
                size(
                    pixel(self.from.body.size.width, self.to.body.size.width),
                    pixel(self.from.body.size.height, self.to.body.size.height),
                ),
            ),
            tip: point(
                pixel(self.from.tip.x, self.to.tip.x),
                pixel(self.from.tip.y, self.to.tip.y),
            ),
            base: pixel(self.from.base, self.to.base),
            arrow: pixel(self.from.arrow, self.to.arrow),
            side: self.to.side,
        };
        if self.from.side != self.to.side {
            // 翻转方向时先收起旧尖角、再展开新尖角，避免尖角跨过气泡内部。
            let (reference, extent) = if progress < 0.5 {
                (self.from, 1.0 - progress * 2.0)
            } else {
                (self.to, progress * 2.0 - 1.0)
            };
            frame.side = reference.side;
            frame.arrow = reference.arrow * extent;
            let horizontal = matches!(reference.side, Placement::Left | Placement::Right);
            let (start, length, old_start, old_length, tip) = if horizontal {
                (
                    frame.body.top(),
                    frame.body.size.height,
                    reference.body.top(),
                    reference.body.size.height,
                    reference.tip.y,
                )
            } else {
                (
                    frame.body.left(),
                    frame.body.size.width,
                    reference.body.left(),
                    reference.body.size.width,
                    reference.tip.x,
                )
            };
            let map =
                |value| start + length * f32::from((value - old_start) / old_length.max(px(1.0)));
            frame.base = map(reference.base);
            let tip = map(tip);
            frame.tip = match reference.side {
                Placement::Top => point(tip, frame.body.bottom() + frame.arrow),
                Placement::Bottom => point(tip, frame.body.top() - frame.arrow),
                Placement::Left => point(frame.body.right() + frame.arrow, tip),
                Placement::Right => point(frame.body.left() - frame.arrow, tip),
            };
        }
        frame
    }
}

impl BubbleLayout {
    fn nearly_equal(self, other: Self) -> bool {
        self.side == other.side
            && [
                self.window.left() - other.window.left(),
                self.window.top() - other.window.top(),
                self.window.size.width - other.window.size.width,
                self.window.size.height - other.window.size.height,
            ]
            .into_iter()
            .all(|delta| delta.abs() <= 0.5)
            && [
                self.body.left() - other.body.left(),
                self.body.top() - other.body.top(),
                self.body.size.width - other.body.size.width,
                self.body.size.height - other.body.size.height,
                self.tip.x - other.tip.x,
                self.tip.y - other.tip.y,
                self.base - other.base,
                self.arrow - other.arrow,
            ]
            .into_iter()
            .all(|delta| delta.abs() <= px(0.5))
    }

    fn in_window(mut self, bounds: Bounds<f32>, scale: f32) -> Self {
        let offset = point(
            px((self.window.left() - bounds.left()) / scale),
            px((self.window.top() - bounds.top()) / scale),
        );
        self.body.origin += offset;
        self.tip += offset;
        self.base += if matches!(self.side, Placement::Left | Placement::Right) {
            offset.y
        } else {
            offset.x
        };
        self
    }

    fn new(
        anchor: Bounds<f32>,
        measured: Size<Pixels>,
        screen: Bounds<f32>,
        scale: f32,
        ui_scale: f32,
        preferred: Option<Placement>,
    ) -> Self {
        let margin = MARGIN * ui_scale * scale;
        let arrow = ARROW * ui_scale * scale;
        // 扣除阴影的透明留白，让尖角与调用控件保持 4px 的实际间距。
        let gap = 4.0 * ui_scale * scale - margin;
        let body_size = size(
            f32::from(measured.width) * scale,
            f32::from(measured.height) * scale,
        );
        let available = |side| match side {
            Placement::Top => anchor.top() - screen.top(),
            Placement::Bottom => screen.bottom() - anchor.bottom(),
            Placement::Left => anchor.left() - screen.left(),
            Placement::Right => screen.right() - anchor.right(),
        };
        let needed = |side| match side {
            Placement::Top | Placement::Bottom => body_size.height + arrow + gap + margin * 2.0,
            Placement::Left | Placement::Right => body_size.width + arrow + gap + margin * 2.0,
        };
        let preferred = preferred.unwrap_or(Placement::Top);
        let opposite = match preferred {
            Placement::Top => Placement::Bottom,
            Placement::Bottom => Placement::Top,
            Placement::Left => Placement::Right,
            Placement::Right => Placement::Left,
        };
        let candidates = [
            preferred,
            opposite,
            Placement::Bottom,
            Placement::Top,
            Placement::Right,
            Placement::Left,
        ];
        let side = candidates
            .iter()
            .copied()
            .find(|side| available(*side) >= needed(*side))
            .unwrap_or_else(|| {
                candidates
                    .into_iter()
                    .max_by(|a, b| {
                        (available(*a) - needed(*a)).total_cmp(&(available(*b) - needed(*b)))
                    })
                    .unwrap_or(preferred)
            });
        let horizontal = matches!(side, Placement::Left | Placement::Right);
        let window_size = size(
            body_size.width + margin * 2.0 + if horizontal { arrow } else { 0.0 },
            body_size.height + margin * 2.0 + if horizontal { 0.0 } else { arrow },
        );
        let center = anchor.center();
        let origin = match side {
            Placement::Bottom => point(center.x - window_size.width / 2.0, anchor.bottom() + gap),
            Placement::Top => point(
                center.x - window_size.width / 2.0,
                anchor.top() - gap - window_size.height,
            ),
            Placement::Right => point(anchor.right() + gap, center.y - window_size.height / 2.0),
            Placement::Left => point(
                anchor.left() - gap - window_size.width,
                center.y - window_size.height / 2.0,
            ),
        };
        let origin = point(
            origin
                .x
                .clamp(
                    screen.left(),
                    (screen.right() - window_size.width).max(screen.left()),
                )
                .round(),
            origin
                .y
                .clamp(
                    screen.top(),
                    (screen.bottom() - window_size.height).max(screen.top()),
                )
                .round(),
        );
        let window = Bounds::new(
            origin,
            size(window_size.width.ceil(), window_size.height.ceil()),
        );
        let body = Bounds::new(
            point(
                px((margin + if side == Placement::Right { arrow } else { 0.0 }) / scale),
                px((margin
                    + if side == Placement::Bottom {
                        arrow
                    } else {
                        0.0
                    })
                    / scale),
            ),
            measured,
        );
        let tip = match side {
            Placement::Bottom => point(
                px((center.x - origin.x) / scale),
                body.top() - px(arrow / scale),
            ),
            Placement::Top => point(
                px((center.x - origin.x) / scale),
                body.bottom() + px(arrow / scale),
            ),
            Placement::Right => point(
                body.left() - px(arrow / scale),
                px((center.y - origin.y) / scale),
            ),
            Placement::Left => point(
                body.right() + px(arrow / scale),
                px((center.y - origin.y) / scale),
            ),
        };
        let (start, end, target) = if horizontal {
            (body.top(), body.bottom(), tip.y)
        } else {
            (body.left(), body.right(), tip.x)
        };
        let clearance = px((RADIUS + ARROW) * ui_scale).min((end - start) / 2.0);
        let base = target.clamp(start + clearance, end - clearance);
        Self {
            window,
            body,
            side,
            tip,
            base,
            arrow: px(ARROW * ui_scale),
        }
    }
}

fn bubble_path(path: &mut PathBuilder, layout: &BubbleLayout, radius: f32) {
    let b = layout.body;
    let r = px(radius).min(b.size.width / 2.0).min(b.size.height / 2.0);
    let base = layout.base;
    let length = if matches!(layout.side, Placement::Left | Placement::Right) {
        b.size.height
    } else {
        b.size.width
    };
    let half = layout.arrow.min((length / 2.0 - r).max(px(0.0)));
    path.move_to(point(b.left() + r, b.top()));
    if layout.side == Placement::Bottom {
        path.line_to(point(base - half, b.top()));
        path.line_to(layout.tip);
        path.line_to(point(base + half, b.top()));
    }
    path.line_to(point(b.right() - r, b.top()));
    path.curve_to(point(b.right(), b.top() + r), point(b.right(), b.top()));
    if layout.side == Placement::Left {
        path.line_to(point(b.right(), base - half));
        path.line_to(layout.tip);
        path.line_to(point(b.right(), base + half));
    }
    path.line_to(point(b.right(), b.bottom() - r));
    path.curve_to(
        point(b.right() - r, b.bottom()),
        point(b.right(), b.bottom()),
    );
    if layout.side == Placement::Top {
        path.line_to(point(base + half, b.bottom()));
        path.line_to(layout.tip);
        path.line_to(point(base - half, b.bottom()));
    }
    path.line_to(point(b.left() + r, b.bottom()));
    path.curve_to(point(b.left(), b.bottom() - r), point(b.left(), b.bottom()));
    if layout.side == Placement::Right {
        path.line_to(point(b.left(), base + half));
        path.line_to(layout.tip);
        path.line_to(point(b.left(), base - half));
    }
    path.line_to(point(b.left(), b.top() + r));
    path.curve_to(point(b.left() + r, b.top()), point(b.left(), b.top()));
    path.close();
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_component::tooltip::{ManagedTooltipExt as _, Tooltip};

    struct CanvasButtons;

    impl Render for CanvasButtons {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().p(px(40.0)).child(
                canvas(
                    |bounds, window, cx| {
                        let mut buttons = div()
                            .flex()
                            .size_full()
                            .children((0..2).map(|id| {
                                div().id(id).size(px(20.0)).border_1().managed_tooltip(
                                    |window, cx| Tooltip::new("按钮提示").build(window, cx),
                                )
                            }))
                            .into_any_element();
                        buttons.layout_as_root(bounds.size.into(), window, cx);
                        buttons.prepaint_at(bounds.origin, window, cx);
                        buttons
                    },
                    |_, mut buttons, window, cx| buttons.paint(window, cx),
                )
                .w(px(40.0))
                .h(px(20.0)),
            )
        }
    }

    #[gpui::test]
    fn canvas_button_tooltips_follow_borders_and_switch_without_gaps(
        cx: &mut gpui::TestAppContext,
    ) {
        use std::cell::RefCell;
        let hovered = Rc::new(RefCell::new(None));
        cx.update(|cx| {
            let hovered = hovered.clone();
            cx.set_global(WindowTooltipHost {
                window_id: None,
                dispatch: Rc::new(move |event, _, cx| match event {
                    WindowTooltipEvent::Show(trigger) => {
                        let entity = trigger.upgrade().unwrap();
                        *hovered.borrow_mut() = Some((entity.entity_id(), entity.read(cx).bounds));
                    }
                    WindowTooltipEvent::Hide(id) => {
                        if hovered
                            .borrow()
                            .as_ref()
                            .is_some_and(|(active, _)| *active == id)
                        {
                            *hovered.borrow_mut() = None;
                        }
                    }
                    _ => {}
                }),
            });
        });
        let window = cx.open_window(size(px(160.0), px(120.0)), |_, _| CanvasButtons);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        for (x, y) in [
            (40.25, 40.25),
            (59.75, 40.25),
            (60.25, 40.25),
            (79.75, 59.75),
            (59.75, 59.75),
        ] {
            visual.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
            visual.simulate_mouse_move(point(px(x), px(y)), None, gpui::Modifiers::default());
            let hovered = hovered.borrow();
            let (_, bounds) = hovered.as_ref().expect("按钮边框内不应中断提示");
            assert!(
                bounds.contains(&point(px(x), px(y))),
                "提示边界必须覆盖按钮实际命中位置：{bounds:?}"
            );
            assert_eq!(bounds.size, size(px(20.0), px(20.0)));
        }
    }

    #[test]
    fn tooltip_fades_resume_without_opacity_jumps() {
        let now = Instant::now();
        let appear = TooltipFade {
            from: 0.0,
            to: 1.0,
            started_at: now,
            duration: APPEAR_DURATION,
        };
        assert_eq!(appear.value(now), 0.0);
        assert_eq!(appear.value(now + APPEAR_DURATION), 1.0);
        let leave = now + APPEAR_DURATION;
        let disappear = TooltipFade {
            from: 1.0,
            to: 0.0,
            started_at: leave,
            duration: SWITCH_GRACE,
        };
        let reenter = leave + SWITCH_GRACE / 2;
        let opacity = disappear.value(reenter);
        assert!(opacity > 0.5 && opacity < 1.0);
        let resume = TooltipFade {
            from: opacity,
            to: 1.0,
            started_at: reenter,
            duration: APPEAR_DURATION,
        };
        assert_eq!(resume.value(reenter), opacity);
        assert_eq!(resume.value(reenter + APPEAR_DURATION), 1.0);
        assert_eq!(disappear.value(leave + SWITCH_GRACE), 0.0);
    }

    struct MeasuredContent(Vec<Size<Pixels>>);

    impl Render for MeasuredContent {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            cx.global_mut::<WindowTooltipHost>().window_id =
                Some(window.window_handle().window_id());
            self.0.clear();
            for text in [
                "提示".to_owned(),
                "内容换行并根据布局决定窗口大小。".repeat(20),
                "第一行\n第二行\n第三行".to_owned(),
            ] {
                let view = TooltipWindow {
                    content: Tooltip::new(text).build(window, cx),
                    previous_content: None,
                    content_size: Size::default(),
                    measurement: None,
                    layout: None,
                    motion: None,
                    fade: None,
                    anchor: Bounds::default(),
                    placement: None,
                    rem_size: px(16.0),
                    popup: platform::BorderlessPopup,
                    last_bounds: None,
                    pending_bounds: None,
                };
                self.0.push(
                    view.content(size(px(180.0), px(600.0)), cx)
                        .into_any_element()
                        .layout_as_root(
                            size(AvailableSpace::MaxContent, AvailableSpace::MaxContent),
                            window,
                            cx,
                        ),
                );
            }
            div()
        }
    }

    #[gpui::test]
    fn content_measurement_wraps_and_resizes_with_gpui(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            TooltipService::install(cx);
        });
        let window = cx.open_window(size(px(600.0), px(600.0)), |_, _| {
            MeasuredContent(Vec::new())
        });
        cx.update_window(window.into(), |_, window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        })
        .unwrap();
        window
            .read_with(cx, |view, _| {
                assert_eq!(view.0.len(), 3);
                let [short, long, multiline] = view.0.as_slice() else {
                    unreachable!()
                };
                assert!(short.width < long.width);
                assert!(long.width <= px(180.0));
                assert!(long.height > short.height);
                assert!(multiline.height > short.height);
            })
            .unwrap();
    }

    struct Triggers(bool);

    impl Render for Triggers {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().when(self.0, |this| {
                this.children((0..2).map(|id| {
                    div()
                        .id(id)
                        .size(px(30.0))
                        .managed_tooltip(|window, cx| Tooltip::new("提示").build(window, cx))
                }))
            })
        }
    }

    #[gpui::test]
    fn trigger_identity_survives_redraw_and_is_released_when_removed(
        cx: &mut gpui::TestAppContext,
    ) {
        use std::cell::RefCell;
        let ids = Rc::new(RefCell::new(Vec::new()));
        let hovered = Rc::new(RefCell::new(None));
        cx.update(|cx| {
            let ids = ids.clone();
            let hovered = hovered.clone();
            cx.set_global(WindowTooltipHost {
                window_id: None,
                dispatch: Rc::new(move |event, _, _| match event {
                    WindowTooltipEvent::Refresh(id) => ids.borrow_mut().push(id),
                    WindowTooltipEvent::Show(trigger) => *hovered.borrow_mut() = Some(trigger),
                    _ => {}
                }),
            });
        });
        let window = cx.open_window(size(px(200.0), px(200.0)), |_, _| Triggers(true));
        for _ in 0..2 {
            cx.update_window(window.into(), |_, window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            })
            .unwrap();
        }
        let ids = ids.borrow();
        assert_ne!(ids[0], ids[1]);
        assert_eq!(ids[0..2], ids[2..4]);
        drop(ids);
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.simulate_mouse_move(point(px(10.0), px(10.0)), None, gpui::Modifiers::default());
        visual.run_until_parked();
        assert!(
            hovered
                .borrow()
                .as_ref()
                .is_some_and(|trigger| trigger.upgrade().is_some())
        );
        window
            .update(&mut visual, |view, _, cx| {
                view.0 = false;
                cx.notify();
            })
            .unwrap();
        for _ in 0..2 {
            visual.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
        }
        visual.run_until_parked();
        assert!(
            hovered
                .borrow()
                .as_ref()
                .is_some_and(|trigger| trigger.upgrade().is_none())
        );
    }

    #[gpui::test]
    fn pending_tooltip_is_cancelled_by_keyboard_click_and_scroll(cx: &mut gpui::TestAppContext) {
        cx.update(TooltipService::install);
        let window = cx.open_window(size(px(200.0), px(200.0)), |_, _| Triggers(true));
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        for event in 0..3 {
            visual.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
            visual.simulate_mouse_move(
                point(px(100.0), px(100.0)),
                None,
                gpui::Modifiers::default(),
            );
            visual.simulate_mouse_move(point(px(10.0), px(10.0)), None, gpui::Modifiers::default());
            visual.update(|window, cx| {
                let request = cx.global::<TooltipService>().request.as_ref().unwrap();
                assert!(request.show_at > Instant::now());
                window.refresh();
                window.draw(cx).clear(cx);
            });
            match event {
                0 => visual.simulate_keystrokes("escape"),
                1 => visual.simulate_mouse_down(
                    point(px(10.0), px(10.0)),
                    gpui::MouseButton::Left,
                    gpui::Modifiers::default(),
                ),
                _ => visual.simulate_event(ScrollWheelEvent {
                    position: point(px(10.0), px(10.0)),
                    delta: gpui::ScrollDelta::Pixels(point(px(0.0), px(20.0))),
                    modifiers: gpui::Modifiers::default(),
                    touch_phase: gpui::TouchPhase::Moved,
                }),
            }
            visual.update(|_, cx| assert!(cx.global::<TooltipService>().request.is_none()));
            if event == 1 {
                visual.simulate_mouse_up(
                    point(px(10.0), px(10.0)),
                    gpui::MouseButton::Left,
                    gpui::Modifiers::default(),
                );
            }
        }
    }

    #[gpui::test]
    fn switching_reuses_popup_and_reentering_cancels_delayed_hide(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            TooltipService::install(cx);
        });
        let source = cx.open_window(size(px(200.0), px(200.0)), |_, _| Triggers(true));
        let popup = cx.open_window(size(px(200.0), px(100.0)), |window, cx| {
            cx.global_mut::<WindowTooltipHost>().window_id =
                Some(window.window_handle().window_id());
            TooltipWindow {
                content: Tooltip::new("提示").build(window, cx),
                previous_content: None,
                content_size: Size::default(),
                measurement: None,
                anchor: Bounds::new(point(300.0, 300.0), size(30.0, 30.0)),
                placement: None,
                rem_size: px(16.0),
                popup: platform::BorderlessPopup,
                last_bounds: None,
                pending_bounds: None,
                layout: None,
                motion: None,
                fade: None,
            }
        });
        cx.update(|cx| cx.global_mut::<TooltipService>().popup = Some(popup));
        let mut visual = gpui::VisualTestContext::from_window(source.into(), cx);
        visual.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        visual.update(|window, cx| window.simulate_mouse_move(point(px(10.0), px(10.0)), cx));
        let first = visual.update(|_, cx| {
            cx.global::<TooltipService>()
                .request
                .as_ref()
                .unwrap()
                .trigger
                .entity_id()
        });
        visual.update(|window, cx| window.simulate_mouse_move(point(px(100.0), px(100.0)), cx));
        visual.update(|_, cx| {
            let service = cx.global::<TooltipService>();
            assert_eq!(service.popup, Some(popup));
            assert!(service.hide_at.is_some());
        });
        visual.update(|window, cx| window.simulate_mouse_move(point(px(10.0), px(10.0)), cx));
        visual.update(|_, cx| assert!(cx.global::<TooltipService>().hide_at.is_none()));
        visual.update(|window, cx| window.simulate_mouse_move(point(px(10.0), px(40.0)), cx));
        visual.update(|_, cx| {
            let service = cx.global::<TooltipService>();
            assert_eq!(service.popup, Some(popup));
            assert!(service.hide_at.is_none());
            let request = service.request.as_ref().unwrap();
            assert_ne!(request.trigger.entity_id(), first);
            assert!(request.switching);
            assert!(request.show_at <= Instant::now());
            cx.update_global::<TooltipService, _>(|service, cx| {
                service.request_hide(cx);
                service.hide_at = Some(Instant::now());
                assert!(!service.tick(cx));
                assert!(service.popup.is_none());
            });
        });
    }

    #[gpui::test]
    fn content_and_geometry_transition_from_the_current_frame(cx: &mut gpui::TestAppContext) {
        let content = cx.new(|_| Tooltip::new("旧提示"));
        let next = cx.new(|_| Tooltip::new("新的更长提示"));
        let mut view = TooltipWindow {
            content: content.clone().into(),
            previous_content: None,
            content_size: size(px(80.0), px(24.0)),
            measurement: None,
            anchor: Bounds::default(),
            placement: None,
            rem_size: px(16.0),
            popup: platform::BorderlessPopup,
            last_bounds: None,
            pending_bounds: None,
            layout: None,
            motion: None,
            fade: None,
        };
        let screen = Bounds::new(point(0.0, 0.0), size(1920.0, 1080.0));
        let from = BubbleLayout::new(
            Bounds::new(point(400.0, 500.0), size(30.0, 30.0)),
            view.content_size,
            screen,
            1.0,
            1.0,
            None,
        );
        let to = BubbleLayout::new(
            Bounds::new(point(900.0, 500.0), size(30.0, 30.0)),
            size(px(240.0), px(60.0)),
            screen,
            1.0,
            1.0,
            None,
        );
        let now = Instant::now();
        assert_eq!(from.side, Placement::Top);
        assert_eq!(view.animate_layout(from, now, true), from);
        let mut subpixel_change = from;
        subpixel_change.body.size.width += px(0.1);
        assert_eq!(view.animate_layout(subpixel_change, now, true), from);
        assert!(view.motion.is_none());
        let measurement = Some((size(px(360.0), px(600.0)), px(16.0)));
        view.measurement = measurement;
        let same = cx.new(|_| Tooltip::new("旧提示"));
        assert!(!cx.update(|cx| view.replace_content(same.into(), true, now, cx)));
        assert_eq!(view.content.entity_id(), content.entity_id());
        assert_eq!(view.measurement, measurement);
        assert!(view.previous_content.is_none());
        cx.update(|cx| view.replace_content(next.clone().into(), true, now, cx));
        assert!(view.measurement.is_none());
        assert_eq!(
            view.previous_content.as_ref().unwrap().layers[0]
                .0
                .entity_id(),
            content.entity_id()
        );
        assert_eq!(view.content.entity_id(), next.entity_id());
        assert_eq!(view.animate_layout(to, now, true), from);
        let middle_at = now + TRANSITION_DURATION / 2;
        let middle = view.animate_layout(to, middle_at, true);
        assert!(
            middle.window.left() > from.window.left() && middle.window.left() < to.window.left()
        );
        assert!(
            middle.body.size.width > from.body.size.width
                && middle.body.size.width < to.body.size.width
        );
        assert!(
            transition_progress(now, middle_at) > 0.0 && transition_progress(now, middle_at) < 1.0
        );
        let retarget = BubbleLayout::new(
            Bounds::new(point(1100.0, 600.0), size(30.0, 30.0)),
            size(px(160.0), px(40.0)),
            screen,
            1.0,
            1.0,
            None,
        );
        let third = cx.new(|_| Tooltip::new("再次切换"));
        cx.update(|cx| view.replace_content(third.into(), true, middle_at, cx));
        let layers = &view.previous_content.as_ref().unwrap().layers;
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].0.entity_id(), next.entity_id());
        assert!((layers[0].2 - transition_progress(now, middle_at)).abs() < 0.001);
        assert_eq!(view.animate_layout(retarget, middle_at, true), middle);
        assert_eq!(
            view.animate_layout(retarget, middle_at + TRANSITION_DURATION, true),
            retarget
        );
        assert!(view.motion.is_none());
        assert_eq!(view.animate_layout(from, middle_at, false), from);
        assert!(view.previous_content.is_none());
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "需要真实 Windows 桌面和 GPU，显式运行原生窗口回归测试"]
    fn native_hidden_tooltip_is_measured_and_shown() {
        use std::cell::Cell;
        use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, IsWindowVisible};

        let directory = tempfile::tempdir().unwrap();
        let succeeded = Rc::new(Cell::new(false));
        let result = succeeded.clone();
        let diagnostics = Rc::new(Cell::new((false, false, false, false, false, false)));
        let test_diagnostics = diagnostics.clone();
        gpui_platform::application().run(move |cx| {
            gpui_component::init(cx);
            crate::config_runtime::ConfigRuntime::install(directory.path().join("config.json"), cx);
            crate::window_material::install(cx);
            crate::ui::theme::install(cx);
            TooltipService::install(cx);
            // SAFETY: 仅读取前台窗口，验证提示没有激活自身。
            let foreground = unsafe { GetForegroundWindow() };
            let content = cx.new(|_| Tooltip::new("独立提示窗口显示回归测试")).into();
            let popup = TooltipWindow::open(
                content,
                Bounds::new(point(120.0, 120.0), size(24.0, 24.0)),
                None,
                px(16.0),
                cx,
            )
            .expect("创建提示窗口失败");
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(600))
                    .await;
                // 每个调用者只停留 10ms，验证切换不再等待 40ms 轮询。
                let (source, builds, triggers) = cx.update(|cx| {
                    let pointer =
                        platform::physical_cursor_position().expect("读取测试光标位置失败");
                    let display = platform::display_for_physical_point(cx, pointer).unwrap();
                    let source = platform::open_borderless_popup_visible(
                        cx,
                        Bounds::new(
                            point(
                                px(pointer.x / display.scale_factor),
                                px(pointer.y / display.scale_factor),
                            ),
                            size(px(100.0), px(100.0)),
                        ),
                        Some(display.display_id),
                        |_, cx, _| cx.new(|_| Triggers(false)),
                    )
                    .unwrap();
                    source
                        .update(cx, |_, window, cx| {
                            platform::BorderlessPopup.resize_to_physical_bounds(
                                window,
                                cx,
                                pointer.x - 40.0,
                                pointer.y - 40.0,
                                160.0,
                                160.0,
                            );
                        })
                        .unwrap();
                    let builds = Rc::new(Cell::new(0));
                    let triggers = (0..3)
                        .map(|index| {
                            let builds = builds.clone();
                            cx.new(|_| WindowTooltipTrigger {
                                bounds: Bounds::new(
                                    point(px(0.0), px(0.0)),
                                    size(px(100.0), px(100.0)),
                                ),
                                placement: None,
                                build: Rc::new(move |window, cx| {
                                    builds.set(builds.get() + 1);
                                    Tooltip::new(format!("快速切换 {index}")).build(window, cx)
                                }),
                            })
                        })
                        .collect::<Vec<_>>();
                    cx.global_mut::<TooltipService>().popup = Some(popup);
                    (source, builds, triggers)
                });
                let mut immediate_switches = true;
                for (index, trigger) in triggers.iter().enumerate() {
                    cx.update(|cx| {
                        source
                            .update(cx, |_, window, cx| {
                                TooltipService::dispatch(
                                    WindowTooltipEvent::Show(trigger.downgrade()),
                                    window,
                                    cx,
                                );
                            })
                            .unwrap()
                    });
                    cx.background_executor()
                        .timer(Duration::from_millis(10))
                        .await;
                    immediate_switches &= builds.get() == index + 1;
                }
                cx.update(|cx| {
                    let service = cx.global_mut::<TooltipService>();
                    service.request = None;
                    service.task = None;
                    source
                        .update(cx, |_, window, _| window.remove_window())
                        .unwrap();
                });
                let (original_hwnd, initially_visible) = cx.update(|cx| {
                    let content = cx
                        .new(|_| {
                            Tooltip::new("切换后内容更长的独立提示窗口\n同时验证位置和大小过渡")
                        })
                        .into();
                    popup
                        .update(cx, |view, window, cx| {
                            let hwnd = platform::BorderlessPopup
                                .native_window_handle(window)
                                .unwrap();
                            // SAFETY: 只查询本测试创建的窗口。
                            let visible = unsafe {
                                IsWindowVisible(windows::Win32::Foundation::HWND(hwnd as *mut _))
                                    .as_bool()
                            };
                            view.replace_content(content, true, Instant::now(), cx);
                            view.anchor = Bounds::new(point(600.0, 400.0), size(24.0, 24.0));
                            cx.notify();
                            (hwnd, visible)
                        })
                        .unwrap()
                });
                let mut remained_visible = initially_visible;
                for _ in 0..4 {
                    cx.background_executor()
                        .timer(Duration::from_millis(80))
                        .await;
                    remained_visible &= cx.update(|cx| {
                        popup
                            .update(cx, |_, window, _| {
                                let hwnd = platform::BorderlessPopup
                                    .native_window_handle(window)
                                    .unwrap();
                                // SAFETY: 验证过渡中继续使用同一个可见窗口。
                                hwnd == original_hwnd
                                    && unsafe {
                                        IsWindowVisible(windows::Win32::Foundation::HWND(
                                            hwnd as *mut _,
                                        ))
                                        .as_bool()
                                    }
                            })
                            .unwrap()
                    });
                }
                // 已覆盖区域内来回切换，原生承载窗口不得在每次结束后收缩或移动。
                let stable_bounds = cx.update(|cx| {
                    popup
                        .update(cx, |view, window, _| {
                            (
                                view.last_bounds,
                                platform::physical_client_bounds(window).unwrap(),
                            )
                        })
                        .unwrap()
                });
                let mut stable_stage = true;
                for index in 0..6 {
                    cx.update(|cx| {
                        popup
                            .update(cx, |view, _, cx| {
                                let x = if index % 2 == 0 { 420.0 } else { 600.0 };
                                view.anchor = Bounds::new(point(x, 400.0), size(24.0, 24.0));
                                let content = cx
                                    .new(|_| {
                                        Tooltip::new(
                                            "切换后内容更长的独立提示窗口\n同时验证位置和大小过渡",
                                        )
                                    })
                                    .into();
                                let unchanged =
                                    !view.replace_content(content, false, Instant::now(), cx);
                                stable_stage &= unchanged;
                                cx.notify();
                            })
                            .unwrap()
                    });
                    cx.background_executor()
                        .timer(Duration::from_millis(60))
                        .await;
                    stable_stage &= cx.update(|cx| {
                        popup
                            .update(cx, |view, window, _| {
                                (
                                    view.last_bounds,
                                    platform::physical_client_bounds(window).unwrap(),
                                ) == stable_bounds
                            })
                            .unwrap()
                    });
                }
                cx.background_executor().timer(TRANSITION_DURATION).await;
                cx.update(|cx| {
                    let shown = popup
                        .update(cx, |view, window, _| {
                            // SAFETY: 仅读取当前前台窗口，确认提示没有激活自身。
                            let focus_unchanged = unsafe { GetForegroundWindow() } == foreground;
                            let settled = view.motion.is_none() && view.previous_content.is_none();
                            let resized = view.last_bounds.is_some_and(|bounds| {
                                bounds.size.width > 1.0
                                    && bounds.size.height > 1.0
                                    && view.layout.is_some_and(|layout| {
                                        bounds.left() <= layout.window.left()
                                            && bounds.top() <= layout.window.top()
                                            && bounds.right() >= layout.window.right()
                                            && bounds.bottom() >= layout.window.bottom()
                                    })
                            });
                            window.remove_window();
                            test_diagnostics.set((
                                immediate_switches,
                                remained_visible,
                                focus_unchanged,
                                settled,
                                stable_stage,
                                resized,
                            ));
                            immediate_switches
                                && remained_visible
                                && focus_unchanged
                                && settled
                                && stable_stage
                                && resized
                        })
                        .unwrap();
                    result.set(shown);
                    cx.quit();
                });
            })
            .detach();
        });
        assert!(
            succeeded.get(),
            "提示窗口回归失败（快速切换、持续可见、焦点、过渡完成、边界稳定、包含内容）：{:?}",
            diagnostics.get()
        );
    }

    #[test]
    fn transition_keeps_native_bounds_stable_and_preserves_screen_coordinates() {
        let screen = Bounds::new(point(-1920.0, 0.0), size(1920.0, 1080.0));
        let from = BubbleLayout::new(
            Bounds::new(point(-1500.0, 500.0), size(30.0, 30.0)),
            size(px(80.0), px(24.0)),
            screen,
            1.5,
            1.0,
            None,
        );
        let to = BubbleLayout::new(
            Bounds::new(point(-500.0, 500.0), size(30.0, 30.0)),
            size(px(240.0), px(60.0)),
            screen,
            1.5,
            1.0,
            None,
        );
        let now = Instant::now();
        let motion = BubbleMotion {
            from,
            to,
            started_at: now,
        };
        let stage = transition_window_bounds(Some(from.window), to.window);
        for elapsed in [0, 35, 70, 105] {
            let frame = motion.frame(now + Duration::from_millis(elapsed));
            assert_eq!(transition_window_bounds(Some(stage), to.window), stage);
            let painted = frame.in_window(stage, 1.5);
            assert!(
                (stage.left() + f32::from(painted.body.left()) * 1.5
                    - (frame.window.left() + f32::from(frame.body.left()) * 1.5))
                    .abs()
                    < 0.01
            );
            assert!(
                (stage.left() + f32::from(painted.tip.x) * 1.5
                    - (frame.window.left() + f32::from(frame.tip.x) * 1.5))
                    .abs()
                    < 0.01
            );
            assert!(
                (painted.base - frame.base - (painted.body.left() - frame.body.left())).abs()
                    < px(0.01)
            );
        }
        assert_eq!(transition_window_bounds(Some(stage), to.window), stage);
    }

    #[test]
    fn bubble_flips_at_screen_edges_and_tip_tracks_the_trigger() {
        let screen = Bounds::new(point(-1920.0, 0.0), size(1920.0, 1080.0));
        let anchor = Bounds::new(point(-35.0, 1040.0), size(30.0, 30.0));
        let layout = BubbleLayout::new(anchor, size(px(200.0), px(40.0)), screen, 1.5, 1.0, None);
        assert_eq!(layout.side, Placement::Top);
        assert!(screen.contains(&layout.window.origin));
        assert!(layout.window.right() <= screen.right());
        assert!(layout.window.bottom() <= screen.bottom());
        assert!(
            (layout.window.left() + f32::from(layout.tip.x) * 1.5 - anchor.center().x).abs() < 0.01
        );
        assert!(layout.base + layout.arrow <= layout.body.right() - px(RADIUS));
        // 显示器底部的任务栏区域也允许显示，边界不再裁到工作区底部。
        let full_screen = Bounds::new(point(0.0, 0.0), size(1920.0, 1080.0));
        let near_taskbar = BubbleLayout::new(
            Bounds::new(point(800.0, 1000.0), size(20.0, 20.0)),
            size(px(80.0), px(24.0)),
            full_screen,
            1.0,
            1.0,
            Some(Placement::Bottom),
        );
        assert_eq!(near_taskbar.side, Placement::Bottom);
        assert!(near_taskbar.window.bottom() > 1040.0);
        assert!(near_taskbar.window.bottom() <= full_screen.bottom());
    }

    #[test]
    fn bubble_respects_side_and_measured_content_size() {
        let screen = Bounds::new(point(0.0, 0.0), size(1920.0, 1080.0));
        let anchor = Bounds::new(point(900.0, 500.0), size(20.0, 20.0));
        for side in [
            Placement::Top,
            Placement::Bottom,
            Placement::Left,
            Placement::Right,
        ] {
            for measured in [size(px(40.0), px(24.0)), size(px(300.0), px(120.0))] {
                let layout = BubbleLayout::new(anchor, measured, screen, 2.0, 1.25, Some(side));
                assert_eq!(layout.side, side);
                assert_eq!(layout.body.size, measured);
                let target = if matches!(side, Placement::Left | Placement::Right) {
                    layout.window.top() + f32::from(layout.tip.y) * 2.0
                } else {
                    layout.window.left() + f32::from(layout.tip.x) * 2.0
                };
                let expected = if matches!(side, Placement::Left | Placement::Right) {
                    anchor.center().y
                } else {
                    anchor.center().x
                };
                assert!((target - expected).abs() < 0.01);
            }
        }
    }
}
