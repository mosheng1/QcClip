use crate::ui::scale::ui_rem;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnimationExt, Context, InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels,
    SpringAnimation, SpringConfig, StatefulInteractiveElement, Styled, div, px,
};
use gpui_component::{ActiveTheme as _, VirtualListScrollHandle, tooltip::Tooltip};

use crate::{
    i18n,
    ui::hover_icon,
    windows::main::state::{FloatingToolbarDrag, MainWindow},
};

/// 工具栏距容器底部的最小距离。
const MIN_BOTTOM: f32 = 16.0;
/// 工具栏距容器右侧的距离。
const RIGHT_INSET: f32 = 16.0;
/// 滚动偏移超过该值时显示"返回顶部"按钮。
const SCROLL_TOP_THRESHOLD: f32 = 48.0;
/// 按钮区域高度。
const BUTTON_HEIGHT: f32 = 24.0;
const TOOLBAR_VERTICAL_PADDING: f32 = 8.0;
const TOOLBAR_GAP: f32 = 4.0;
const DRAG_HANDLE_HEIGHT: f32 = 10.0;

fn toolbar_height(show_scroll_top: bool, show_new_favorite: bool) -> f32 {
    let button_count = usize::from(show_scroll_top) + usize::from(show_new_favorite);
    if button_count == 0 {
        return 0.0;
    }
    TOOLBAR_VERTICAL_PADDING
        + BUTTON_HEIGHT * button_count as f32
        + TOOLBAR_GAP * button_count as f32
        + DRAG_HANDLE_HEIGHT
}

fn spring<T: gpui::SpringTarget>(target: T) -> SpringAnimation<T> {
    SpringAnimation::new(SpringConfig::new(160.0, 18.0, 1.0)).to(target)
}

/// 渲染右下角浮动工具栏：收藏页永久显示"新增收藏"，其他页面按需显示"返回顶部"。
///
/// `scroll_handle` 用于判断当前滚动偏移并执行返回顶部；
/// 容器 bounds 由列表容器上的 canvas 记录，用于约束拖拽范围。
/// 没有可显示的功能按钮时工具栏整体隐藏。
pub fn render(
    bottom: Pixels,
    is_dragging: bool,
    show_new_favorite: bool,
    scroll_handle: &VirtualListScrollHandle,
    cx: &mut Context<MainWindow>,
) -> gpui::AnyElement {
    let theme = cx.theme().colors;
    let animations_enabled =
        crate::config_runtime::animation_enabled(cx, crate::config::AnimationKind::FloatingToolbar);
    let translator = i18n::translator(cx);
    let horizontal = crate::config_runtime::clipboard_config(cx)
        .window_position
        .uses_horizontal_list();
    let scroll_offset = if horizontal {
        scroll_handle.offset().x
    } else {
        scroll_handle.offset().y
    };
    let show_scroll_top = scroll_offset < px(-SCROLL_TOP_THRESHOLD);
    let mut toolbar_background = theme.popover;
    toolbar_background.alpha = 1.0;
    let toolbar_background = crate::window_material::overlay_surface_color(cx, toolbar_background);
    let ui_scale = crate::ui::scale::theme_scale(cx);
    let toolbar_height = toolbar_height(show_scroll_top, show_new_favorite) * ui_scale;
    // 没有任何功能按钮需要显示时整体隐藏（含拖拽手柄）。
    if !show_scroll_top && !show_new_favorite {
        return div().into_any_element();
    }
    let entity = cx.entity().clone();

    // 返回顶部按钮（按需显示，带缩放/透明度过渡）
    let scroll_top_button = div()
        .id("floating-toolbar-scroll-top")
        .group("floating-toolbar-scroll-top")
        .size(ui_rem(24.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .bg(theme.secondary)
        .cursor_pointer()
        .hover(|this| this.bg(theme.secondary_hover))
        .tooltip({
            let label = translator.translate("main.floating_toolbar.scroll_to_top");
            move |window, cx| Tooltip::new(label.clone()).build(window, cx)
        })
        .child(hover_icon(
            "floating-toolbar-scroll-top",
            if horizontal {
                "arrow-bar-to-left"
            } else {
                "arrow-bar-to-up"
            },
            14.0,
            theme.foreground,
        ))
        .on_click(cx.listener(|this, _, _, cx| {
            let mut offset = this.list_scroll_handle.offset();
            offset.x = px(0.0);
            offset.y = px(0.0);
            this.list_scroll_handle.set_offset(offset);
            cx.notify();
        }));

    let new_favorite_button = div()
        .id("floating-toolbar-new-favorite")
        .group("floating-toolbar-new-favorite")
        .size(ui_rem(24.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .bg(theme.secondary)
        .cursor_pointer()
        .hover(|this| this.bg(theme.secondary_hover))
        .tooltip({
            let label = translator.translate("main.floating_toolbar.new_favorite");
            move |window, cx| Tooltip::new(label.clone()).build(window, cx)
        })
        .child(hover_icon(
            "floating-toolbar-new-favorite",
            "plus",
            14.0,
            theme.foreground,
        ))
        .on_click(cx.listener(|this, _, _, cx| {
            this.new_favorite_item(cx);
        }));

    // 拖拽手柄
    let drag_handle = div()
        .id("floating-toolbar-drag-handle")
        .group("floating-toolbar-drag-handle")
        .w_full()
        .h(ui_rem(10.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_ns_resize()
        .tooltip({
            let label = translator.translate("main.floating_toolbar.drag_to_move");
            move |window, cx| Tooltip::new(label.clone()).build(window, cx)
        })
        .child(hover_icon(
            "floating-toolbar-drag-handle",
            "grip-horizontal",
            14.0,
            theme.muted_foreground,
        ))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                cx.stop_propagation();
                this.floating_toolbar_drag = Some(FloatingToolbarDrag {
                    start_mouse_y: event.position.y,
                    start_bottom: this.floating_toolbar_bottom,
                });
                cx.notify();
            }),
        );

    // 工具栏容器：绝对定位于右下角，bottom 可拖拽调整
    let toolbar = div()
        .absolute()
        .right(ui_rem(RIGHT_INSET))
        .bottom(bottom)
        .flex()
        .flex_col()
        .items_center()
        .p_1()
        .rounded_md()
        .bg(toolbar_background)
        .map(|this| crate::window_material::with_backdrop_blur(this, cx))
        .border_1()
        .border_color(theme.border)
        .shadow_lg()
        // 阻止 hover/鼠标事件穿透到下面的列表
        .occlude()
        // 拖拽移动：即使鼠标移出工具栏也持续跟踪
        .on_mouse_move_all({
            let entity = entity.clone();
            move |event: &gpui::MouseMoveEvent, phase, _hitbox, _window, cx| {
                if phase != gpui::DispatchPhase::Bubble {
                    return;
                }
                entity.update(cx, |this, cx| {
                    if !event.dragging() {
                        if this.floating_toolbar_drag.is_some() {
                            this.floating_toolbar_drag = None;
                            cx.notify();
                        }
                        return;
                    }
                    let Some(drag) = this.floating_toolbar_drag else {
                        return;
                    };
                    let delta_y = drag.start_mouse_y - event.position.y;
                    let mut new_bottom = drag.start_bottom + delta_y;
                    let container_height = this.floating_toolbar_container_bounds.size.height;
                    let max_bottom = (container_height - px(toolbar_height) - px(MIN_BOTTOM))
                        .max(px(MIN_BOTTOM));
                    new_bottom = new_bottom.clamp(px(MIN_BOTTOM), max_bottom);
                    if new_bottom != this.floating_toolbar_bottom {
                        this.floating_toolbar_bottom = new_bottom;
                        cx.notify();
                    }
                });
            }
        })
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                if this.floating_toolbar_drag.take().is_some() {
                    cx.notify();
                }
            }),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                if this.floating_toolbar_drag.take().is_some() {
                    cx.notify();
                }
            }),
        )
        // 阻止工具栏上的鼠标事件穿透到列表
        .on_mouse_down(MouseButton::Left, |_, _, cx| {
            cx.stop_propagation();
        })
        .on_scroll_wheel(|_, _, cx| {
            cx.stop_propagation();
        })
        .child({
            let mut buttons = div().flex().flex_col().items_center().gap_1();
            if show_new_favorite {
                buttons = buttons.child(new_favorite_button);
            }
            if show_scroll_top {
                let scroll_top_container = div()
                    .h(ui_rem(BUTTON_HEIGHT))
                    .overflow_hidden()
                    .child(scroll_top_button);
                let scroll_top_container = if animations_enabled {
                    scroll_top_container
                        .with_spring(
                            "floating-toolbar-scroll-top-height",
                            spring(px(BUTTON_HEIGHT * ui_scale)).from(px(0.0)),
                            |this, height| this.h(height),
                        )
                        .into_any_element()
                } else {
                    scroll_top_container.into_any_element()
                };
                buttons = buttons.child(scroll_top_container);
            }
            buttons.child(drag_handle)
        });
    let toolbar = crate::windows::main::window_drag::exclude_from_window_drag(toolbar, entity);
    let toolbar = if animations_enabled {
        toolbar
            .with_spring(
                "floating-toolbar-opacity",
                spring(1.0_f32).from(0.0),
                |this, opacity| this.opacity(opacity),
            )
            .into_any_element()
    } else {
        toolbar.into_any_element()
    };
    toolbar
}
