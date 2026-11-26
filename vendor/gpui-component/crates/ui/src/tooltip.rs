use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

use gpui::{
    Action, AnyElement, AnyView, App, AppContext, Bounds, Context, ElementId, Entity, EntityId,
    Global, IntoElement, MouseButton, ParentElement, Pixels, Render, SharedString,
    StatefulInteractiveElement, StyleRefinement, Styled, WeakEntity, Window, WindowId, div,
    prelude::FluentBuilder, px,
};
use gpui_base::{
    Tooltip as BaseTooltip, TooltipOverlay as BaseTooltipOverlay,
    TooltipRequest as BaseTooltipRequest, TooltipTransition as BaseTooltipTransition,
};

use crate::{
    ActiveTheme, Placement, StyledExt,
    animation::{EffectTransition, ease_in_out_cubic, ease_out_cubic},
    kbd::Kbd,
    root::Root,
    text::Text,
};

pub(crate) fn init(_cx: &mut App) {
    // No app-level init needed — TooltipOverlay is per-window via Root.
}

/// 允许应用把所有托管提示交给独立窗口，同时保留组件原有的内容构建方式。
pub struct WindowTooltipHost {
    pub dispatch: Rc<dyn Fn(WindowTooltipEvent, &mut Window, &mut App)>,
    pub window_id: Option<WindowId>,
}

impl Global for WindowTooltipHost {}

pub enum WindowTooltipEvent {
    Show(WeakEntity<WindowTooltipTrigger>),
    Hide(EntityId),
    Refresh(EntityId),
}

pub struct WindowTooltipTrigger {
    pub bounds: Bounds<Pixels>,
    pub placement: Option<Placement>,
    pub build: Rc<dyn Fn(&mut Window, &mut App) -> AnyView>,
}

fn dispatch_window_tooltip(event: WindowTooltipEvent, window: &mut Window, cx: &mut App) {
    if let Some(dispatch) = cx
        .try_global::<WindowTooltipHost>()
        .map(|host| host.dispatch.clone())
    {
        dispatch(event, window, cx);
    }
}

// ── Tooltip view (unchanged API) ────────────────────────────────────────────

enum TooltipContext {
    Text(Text),
    Element(Box<dyn Fn(&mut Window, &mut App) -> AnyElement>),
}

/// A Tooltip element that can display text or custom content,
/// with optional key binding information.
pub struct Tooltip {
    style: StyleRefinement,
    content: TooltipContext,
    key_binding: Option<Kbd>,
    action: Option<(Box<dyn Action>, Option<SharedString>)>,
}

impl Tooltip {
    /// 判断普通文字提示是否保持相同的内容、样式和快捷键；自定义内容始终重新渲染。
    pub fn has_same_content(&self, other: &Self) -> bool {
        matches!((&self.content, &other.content),
            (TooltipContext::Text(Text::String(left)), TooltipContext::Text(Text::String(right)))
            if left == right)
            && self.style == other.style
            && self.key_binding == other.key_binding
    }

    /// Create a Tooltip with a text content.
    pub fn new(text: impl Into<Text>) -> Self {
        Self {
            style: StyleRefinement::default(),
            content: TooltipContext::Text(text.into()),
            key_binding: None,
            action: None,
        }
    }

    /// Create a Tooltip with a custom element.
    pub fn element<E, F>(builder: F) -> Self
    where
        E: IntoElement,
        F: Fn(&mut Window, &mut App) -> E + 'static,
    {
        Self {
            style: StyleRefinement::default(),
            key_binding: None,
            action: None,
            content: TooltipContext::Element(Box::new(move |window, cx| {
                builder(window, cx).into_any_element()
            })),
        }
    }

    /// Set Action to display key binding information for the tooltip if it exists.
    pub fn action(mut self, action: &dyn Action, context: Option<&str>) -> Self {
        self.action = Some((action.boxed_clone(), context.map(SharedString::new)));
        self
    }

    /// Set KeyBinding information for the tooltip.
    pub fn key_binding(mut self, key_binding: Option<Kbd>) -> Self {
        self.key_binding = key_binding;
        self
    }

    /// Build the tooltip and return it as an `AnyView`.
    pub fn build(mut self, window: &mut Window, cx: &mut App) -> AnyView {
        // 独立窗口没有来源窗口的按键上下文，须在构建内容时解析快捷键。
        if self.key_binding.is_none() {
            if let Some((action, context)) = &self.action {
                self.key_binding =
                    Kbd::binding_for_action(action.as_ref(), context.as_deref(), window);
            }
        }
        cx.new(|_| self).into()
    }
}

impl FluentBuilder for Tooltip {}
impl Styled for Tooltip {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}
impl Render for Tooltip {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 悬停提示覆盖其他内容，背景不继承窗口材质的透明度。
        let mut background = cx.theme().popover;
        background.alpha = 1.0;

        let key_binding = if let Some(key_binding) = &self.key_binding {
            Some(key_binding.clone())
        } else {
            if let Some((action, context)) = &self.action {
                Kbd::binding_for_action(
                    action.as_ref(),
                    context.as_ref().map(|s| s.as_ref()),
                    window,
                )
            } else {
                None
            }
        };

        if cx
            .try_global::<WindowTooltipHost>()
            .is_some_and(|host| host.window_id == Some(window.window_handle().window_id()))
        {
            return div()
                .flex()
                .items_center()
                .font_family(cx.theme().font_family.clone())
                .text_color(cx.theme().popover_foreground)
                .text_size(gpui::rems(0.75))
                .line_height(gpui::rems(1.125))
                .gap_3()
                .refine_style(&self.style)
                .child(div().min_w_0().map(|this| match self.content {
                    TooltipContext::Text(ref text) => this.child(text.clone()),
                    TooltipContext::Element(ref builder) => this.child(builder(window, cx)),
                }))
                .when_some(key_binding, |this, kbd| {
                    this.child(
                        div()
                            .text_xs()
                            .flex_shrink_0()
                            .text_color(cx.theme().muted_foreground)
                            .child(kbd.appearance(false)),
                    )
                })
                .into_any_element();
        }

        div()
            .child(
                // Wrap in a child, to ensure the left margin is applied to the tooltip
                BaseTooltip::new("tooltip-popup")
                    .h_flex()
                    .font_family(cx.theme().font_family.clone())
                    .m_3()
                    .bg(background)
                    .text_color(cx.theme().popover_foreground)
                    .border_1()
                    .border_color(cx.theme().border)
                    .shadow_md()
                    .rounded(cx.theme().radius)
                    .justify_between()
                    .py_0p5()
                    .px_2()
                    .text_sm()
                    .gap_3()
                    .refine_style(&self.style)
                    .map(|this| {
                        this.child(div().map(|this| match self.content {
                            TooltipContext::Text(ref text) => this.child(text.clone()),
                            TooltipContext::Element(ref builder) => this.child(builder(window, cx)),
                        }))
                    })
                    .when_some(key_binding, |this, kbd| {
                        this.child(
                            div()
                                .text_xs()
                                .flex_shrink_0()
                                .text_color(cx.theme().muted_foreground)
                                .child(kbd.appearance(false)),
                        )
                    }),
            )
            .into_any_element()
    }
}

// ── Managed tooltip system ──────────────────────────────────────────────────

/// Duration of the slide-down enter animation.
const ENTER_DURATION: Duration = Duration::from_millis(150);
/// Duration of the position-slide animation when switching tooltips.
const SLIDE_DURATION: Duration = Duration::from_millis(200);
pub(crate) fn render_tooltip(
    content_view: AnyView,
    transition: BaseTooltipTransition,
    _: &mut Window,
    _: &mut App,
) -> AnyElement {
    div().child(content_view).map(|element| match transition {
        BaseTooltipTransition::Switch {
            epoch,
            previous,
            current,
        } => {
            let same_row = (current.origin.y - previous.origin.y).abs() < px(10.);
            if !same_row {
                return element.into_any_element();
            }
            let dx = current.center().x - previous.center().x;
            EffectTransition::new(SLIDE_DURATION)
                .ease(ease_in_out_cubic)
                .slide_x(-dx, px(0.))
                .apply(
                    element,
                    ElementId::NamedInteger("tooltip-slide".into(), epoch as u64),
                )
                .into_any_element()
        }
        BaseTooltipTransition::Enter { epoch } => EffectTransition::new(ENTER_DURATION)
            .ease(ease_out_cubic)
            .slide_y(px(4.), px(0.))
            .fade(0.0, 1.0)
            .apply(
                element,
                ElementId::NamedInteger("tooltip-enter".into(), epoch as u64),
            )
            .into_any_element(),
    })
}

// ── Extension trait for managed tooltips ─────────────────────────────────────

// ── Shared tooltip state for components ─────────────────────────────────────

/// Shared tooltip state that components (Button, Switch, Checkbox, Radio, etc.)
/// can embed to get `.tooltip()` support with minimal boilerplate.
#[derive(Default)]
pub(crate) struct ComponentTooltip {
    pub text: Option<(
        SharedString,
        Option<(Rc<Box<dyn Action>>, Option<SharedString>)>,
    )>,
    pub builder: Option<Rc<dyn Fn(&mut Window, &mut App) -> AnyView>>,
}

impl ComponentTooltip {
    /// Apply this tooltip to a `Stateful<Div>` (or any `ManagedTooltipExt` element).
    pub fn apply<E: ManagedTooltipExt>(self, el: E) -> E {
        if let Some(builder) = self.builder {
            el.managed_tooltip(move |window, cx| builder(window, cx))
        } else if let Some((text, action)) = self.text {
            el.managed_tooltip(move |window, cx| {
                Tooltip::new(text.clone())
                    .when_some(action.clone(), |this, (action, context)| {
                        this.action(
                            action.boxed_clone().as_ref(),
                            context.as_ref().map(|c| c.as_ref()),
                        )
                    })
                    .build(window, cx)
            })
        } else {
            el
        }
    }
}

// ── Internal managed tooltip trait ──────────────────────────────────────────

pub trait ManagedTooltipExt: StatefulInteractiveElement + crate::ElementExt + Sized {
    fn managed_tooltip(
        self,
        build_tooltip: impl Fn(&mut Window, &mut App) -> AnyView + 'static,
    ) -> Self {
        self.managed_tooltip_with_placement(None, build_tooltip)
    }

    fn managed_tooltip_at(
        self,
        placement: Placement,
        build_tooltip: impl Fn(&mut Window, &mut App) -> AnyView + 'static,
    ) -> Self {
        self.managed_tooltip_with_placement(Some(placement), build_tooltip)
    }

    fn managed_tooltip_with_placement(
        self,
        preferred_placement: Option<Placement>,
        build_tooltip: impl Fn(&mut Window, &mut App) -> AnyView + 'static,
    ) -> Self {
        let build_tooltip: Rc<dyn Fn(&mut Window, &mut App) -> AnyView> = Rc::new(build_tooltip);
        let trigger: Rc<RefCell<Option<Entity<WindowTooltipTrigger>>>> = Rc::default();
        let trigger_for_bounds = trigger.clone();
        let trigger_for_paint = trigger.clone();
        let trigger_for_move = trigger.clone();
        let build_for_bounds = build_tooltip.clone();
        let trigger_bounds_cell: Rc<Cell<Bounds<Pixels>>> = Rc::new(Cell::new(Bounds::default()));
        let bounds_writer = trigger_bounds_cell.clone();

        gpui::StatefulInteractiveElement::on_prepaint(self, move |info, window, cx| {
            let bounds = info.bounds;
            bounds_writer.set(bounds);
            if cx.has_global::<WindowTooltipHost>() {
                let bounds = bounds.intersect(&window.content_mask().bounds);
                let state = window.use_keyed_state("window-tooltip-trigger", cx, |_, _| {
                    WindowTooltipTrigger {
                        bounds,
                        placement: preferred_placement,
                        build: build_for_bounds.clone(),
                    }
                });
                state.update(cx, |state, _| {
                    state.bounds = bounds;
                    state.build = build_for_bounds.clone();
                    state.placement = preferred_placement;
                });
                *trigger_for_bounds.borrow_mut() = Some(state);
            }
        })
        .child(
            gpui::canvas(
                |_, _, _| {},
                move |_, _, window, cx| {
                    if let Some(trigger) = trigger_for_paint.borrow().as_ref() {
                        dispatch_window_tooltip(
                            WindowTooltipEvent::Refresh(trigger.entity_id()),
                            window,
                            cx,
                        );
                    }
                },
            )
            .absolute()
            .size_full(),
        )
        .on_mouse_move_all(move |event, phase, hitbox, window, cx| {
            // 动态裁剪中的小按钮可能没有新的 hover 边沿事件，按实际命中恢复跟随。
            if phase == gpui::DispatchPhase::Bubble
                && event.pressed_button.is_none()
                && hitbox.is_hovered(window)
                && !cx.has_active_drag()
                && let Some(trigger) = trigger_for_move.borrow().as_ref()
            {
                dispatch_window_tooltip(WindowTooltipEvent::Show(trigger.downgrade()), window, cx);
            }
        })
        .on_hover({
            let trigger_bounds_cell = trigger_bounds_cell.clone();
            let build_tooltip = build_tooltip.clone();
            move |hovered, window, cx| {
                if cx.has_global::<WindowTooltipHost>() {
                    if let Some(trigger) = trigger.borrow().as_ref() {
                        let event = if *hovered {
                            WindowTooltipEvent::Show(trigger.downgrade())
                        } else {
                            WindowTooltipEvent::Hide(trigger.entity_id())
                        };
                        dispatch_window_tooltip(event, window, cx);
                    }
                    return;
                }
                if let Some(overlay) = Root::tooltip_overlay(window, cx) {
                    if *hovered {
                        let bounds = trigger_bounds_cell.get();
                        overlay.update(cx, |o: &mut BaseTooltipOverlay, cx| {
                            let build = build_tooltip.clone();
                            let request = BaseTooltipRequest::new(bounds, move |window, cx| {
                                build(window, cx)
                            });
                            let request = match preferred_placement {
                                Some(placement) => request.placement(placement),
                                None => request,
                            };
                            o.request_show(request, window, cx);
                        });
                    } else {
                        overlay.update(cx, |o: &mut BaseTooltipOverlay, cx| {
                            o.request_hide(window, cx);
                        });
                    }
                }
            }
        })
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            if let Some(overlay) = Root::tooltip_overlay(window, cx) {
                overlay.update(cx, |overlay, cx| {
                    overlay.hide(cx);
                });
            }
        })
    }
}

impl<E: StatefulInteractiveElement + crate::ElementExt> ManagedTooltipExt for E {}
