use super::*;
use crate::windows::main::state::{HeatBucket, HeatScrollbarState};
use crate::windows::main::window_drag::exclude_from_window_drag;
use gpui::{Along, Axis};

fn item_ratio(index: usize, total: usize, item_offset: &impl Fn(usize) -> Option<Pixels>) -> f32 {
    if let (Some(offset), Some(height)) = (item_offset(index), item_offset(total))
        && height > px(0.0)
    {
        return (f32::from(offset) / f32::from(height)).clamp(0.0, 1.0);
    }
    index as f32 / total.max(1) as f32
}

fn bucket_at(
    buckets: &[HeatBucket],
    bounds: gpui::Bounds<Pixels>,
    position: Pixels,
    axis: Axis,
    item_offset: impl Fn(usize) -> Option<Pixels>,
) -> Option<usize> {
    let total = buckets.last()?.range.end;
    let start = bounds.origin.along(axis);
    let length = bounds.size.along(axis);
    let length = f32::from(length);
    if length <= 0.0 {
        return None;
    }
    let ratio = (f32::from(position - start) / length).clamp(0.0, 1.0);
    Some(
        buckets
            .partition_point(|bucket| item_ratio(bucket.range.end, total, &item_offset) <= ratio)
            .min(buckets.len() - 1),
    )
}

fn heat_level(count: u64) -> u8 {
    match count {
        0 => 0,
        1..=9 => 1,
        10..=49 => 2,
        50..=99 => 3,
        100..=199 => 4,
        200..=499 => 5,
        _ => 6,
    }
}

fn heat_color(level: u8, dark: bool) -> gpui::Hsla {
    gpui::rgb_to_hsla(gpui::rgb(match level {
        0 if dark => 0x29352f,
        1 if dark => 0x42664d,
        2 if dark => 0x4c8a5a,
        3 if dark => 0x9b873f,
        4 if dark => 0xac7042,
        5 if dark => 0xa8524b,
        6 if dark => 0x8e4143,
        0 => 0xd5e6da,
        1 => 0x8fba9c,
        2 => 0x4d9671,
        3 => 0xd5b75f,
        4 => 0xdd965b,
        5 => 0xcc7466,
        _ => 0xbd5c5b,
    }))
}

fn heat_runs(buckets: &[HeatBucket]) -> Vec<(Range<usize>, u8)> {
    let mut runs: Vec<(Range<usize>, u8)> = Vec::new();
    for bucket in buckets {
        let level = heat_level(bucket.max_count);
        if let Some((range, previous)) = runs.last_mut()
            && *previous == level
            && range.end == bucket.range.start
        {
            range.end = bucket.range.end;
        } else {
            runs.push((bucket.range.clone(), level));
        }
    }
    runs
}

fn overview_buckets(
    buckets: Rc<Vec<HeatBucket>>,
    length: Pixels,
    scale: f32,
) -> Rc<Vec<HeatBucket>> {
    let count = if length <= px(0.0) {
        256
    } else {
        ((f32::from(length) / (3.0 * scale)).floor() as usize).clamp(1, 256)
    };
    if buckets.len() <= count {
        return buckets;
    }
    // 只合并已有统计；概览和悬停共用同一分段，孤立热点仍按最大值保留。
    Rc::new(
        (0..count)
            .map(|slot| {
                let start = slot * buckets.len() / count;
                let end = (slot + 1) * buckets.len() / count;
                let mut merged = buckets[start].clone();
                for bucket in &buckets[start + 1..end] {
                    merged.range.end = bucket.range.end;
                    merged.total_count = merged.total_count.saturating_add(bucket.total_count);
                    if bucket.max_count > merged.max_count {
                        merged.max_count = bucket.max_count;
                        merged.peak_index = bucket.peak_index;
                    }
                }
                merged
            })
            .collect(),
    )
}

fn drag_delta(
    start: Point<Pixels>,
    current: Point<Pixels>,
    dragging: bool,
    scale: f32,
    axis: Axis,
) -> Option<Pixels> {
    let delta = current - start;
    let distance_squared = f32::from(delta.x).powi(2) + f32::from(delta.y).powi(2);
    (dragging || distance_squared >= (4.0 * scale).powi(2)).then_some(delta.along(axis))
}

fn thumb_bounds(
    track: gpui::Bounds<Pixels>,
    viewport_length: Pixels,
    max_offset: Pixels,
    offset: Pixels,
    scale: f32,
    axis: Axis,
) -> gpui::Bounds<Pixels> {
    let track_length = track.size.along(axis).max(px(0.0));
    let content_length = viewport_length + max_offset;
    let fraction = if content_length > px(0.0) {
        (f32::from(viewport_length) / f32::from(content_length)).clamp(0.0, 1.0)
    } else {
        1.0
    };
    let thumb_length = (track_length * fraction)
        .max(px(24.0 * scale))
        .min(track_length);
    let progress = if max_offset > px(0.0) {
        (-f32::from(offset) / f32::from(max_offset)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    gpui::Bounds::new(
        track
            .origin
            .apply_along(axis, |start| {
                start + (track_length - thumb_length) * progress
            })
            .apply_along(axis.invert(), |start| {
                start + track.size.along(axis.invert()) / 12.0
            }),
        track
            .size
            .apply_along(axis, |_| thumb_length)
            .apply_along(axis.invert(), |thickness| thickness * (5.0 / 6.0)),
    )
}

fn pointer_offset(
    track: gpui::Bounds<Pixels>,
    thumb_length: Pixels,
    grip: f32,
    pointer_position: Pixels,
    max_offset: Pixels,
    axis: Axis,
) -> Pixels {
    let start = track.origin.along(axis);
    let track_length = track.size.along(axis);
    let travel = track_length - thumb_length;
    if travel <= px(0.0) || max_offset <= px(0.0) {
        return px(0.0);
    }
    (-max_offset * (f32::from(pointer_position - start - thumb_length * grip) / f32::from(travel)))
        .clamp(-max_offset, px(0.0))
}

fn track_press_offset(
    track: gpui::Bounds<Pixels>,
    thumb: gpui::Bounds<Pixels>,
    position: Pixels,
    max_offset: Pixels,
    axis: Axis,
) -> Option<Pixels> {
    // 滑块的抓取范围覆盖整条交互区，保留鼠标在滑块内的相对位置。
    let thumb_start = thumb.origin.along(axis);
    let thumb_length = thumb.size.along(axis);
    if thumb_length > px(0.0) && position >= thumb_start && position <= thumb_start + thumb_length {
        return None;
    }
    Some(pointer_offset(
        track,
        thumb_length,
        0.5,
        position,
        max_offset,
        axis,
    ))
}

fn is_scrollable(handle: &gpui::ScrollHandle, axis: Axis) -> bool {
    handle.max_offset().along(axis) > px(0.0)
}

pub(super) fn render(
    buckets: Rc<Vec<HeatBucket>>,
    state: &HeatScrollbarState,
    scroll_handle: &VirtualListScrollHandle,
    list_width: Pixels,
    axis: Axis,
    window: &Window,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    let horizontal = axis == Axis::Horizontal;
    if buckets.is_empty() {
        return div().into_any_element();
    }
    let theme = cx.theme().colors;
    let dark = cx.theme().mode.is_dark();
    let scale = crate::ui::scale::window_scale(window);
    let track_length = state.bounds.size.along(axis);
    let buckets = overview_buckets(buckets, track_length, scale);
    let entity = cx.entity();
    let total = buckets.last().expect("热度分段不能为空").range.end;
    let paint_runs = heat_runs(&buckets);
    let paint_buckets = buckets.clone();
    let move_buckets = buckets.clone();
    let press_buckets = buckets.clone();
    let window_hovered = window.is_window_hovered();
    let pressed = state.drag_origin.is_some();
    // 内容不足一屏时没有可滚动距离，热度条不显示也不拦截列表的鼠标事件。
    let scrollable = is_scrollable(scroll_handle.as_ref(), axis);
    let visible = scrollable && state.visible && (window_hovered || pressed);
    let expanded = scrollable && state.expanded() && (window_hovered || pressed);
    let animations_enabled = !cx.reduce_motion()
        && config_runtime::animation_enabled(cx, crate::config::AnimationKind::ListItemHover);
    let motion = Duration::from_millis(180).with_easing(gpui::ease_in_out);
    let mut rail_background = theme.background;
    rail_background.alpha = 0.5;
    let rail_background = crate::window_material::overlay_surface_color(cx, rail_background);
    let thumb_color = gpui::rgb_to_hsla(gpui::rgb(if dark { 0xafc9b5 } else { 0x4f6658 }));

    let rail = div()
        .id("clipboard-heat-scrollbar")
        .absolute()
        .when(horizontal, |this| {
            this.left_0().right_0().bottom_0().h(ui_rem(28.0))
        })
        .when(!horizontal, |this| {
            this.right_0().top_0().bottom_0().w(ui_rem(28.0))
        })
        .when(scrollable, |this| {
            this.cursor_pointer().block_mouse_except_scroll()
        })
        .on_hover(cx.listener(move |this, hovered: &bool, window, cx| {
            let scrollable = is_scrollable(this.list_scroll_handle.as_ref(), axis);
            this.set_heat_scrollbar_hovered(
                *hovered && scrollable && window.is_window_hovered(),
                cx,
            );
        }))
        .on_mouse_move(
            cx.listener(move |this, event: &gpui::MouseMoveEvent, window, cx| {
                if !window.is_window_hovered()
                    || this.heat_scrollbar.drag_origin.is_some()
                    || !this.heat_scrollbar.bounds.contains(&event.position)
                    || !is_scrollable(this.list_scroll_handle.as_ref(), axis)
                {
                    return;
                }
                if !this.heat_scrollbar.expanded() {
                    this.set_heat_scrollbar_hovered(true, cx);
                }
                let slot = bucket_at(
                    &move_buckets,
                    this.heat_scrollbar.bounds,
                    event.position.along(axis),
                    axis,
                    |index| this.list_scroll_handle.item_offset(index),
                );
                if slot != this.heat_scrollbar.hovered_bucket {
                    this.heat_scrollbar.hovered_bucket = slot;
                    cx.notify();
                }
            }),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                if !is_scrollable(this.list_scroll_handle.as_ref(), axis) {
                    return;
                }
                cx.stop_propagation();
                this.set_heat_scrollbar_hovered(true, cx);
                this.list_smooth_scroll.reset();
                let handle = this.list_scroll_handle.as_ref();
                let jump_offset = track_press_offset(
                    this.heat_scrollbar.bounds,
                    this.heat_scrollbar.thumb_bounds,
                    event.position.along(axis),
                    handle.max_offset().along(axis),
                    axis,
                );
                // 比例偏移只用于计算拖动锚点，点击直接定位热点。
                let offset = jump_offset.unwrap_or_else(|| handle.offset().along(axis));
                this.heat_scrollbar.dragging = jump_offset.is_none();
                this.heat_scrollbar.hovered_bucket = None;
                let thumb = thumb_bounds(
                    this.heat_scrollbar.bounds,
                    handle.bounds().size.along(axis),
                    handle.max_offset().along(axis),
                    offset,
                    scale,
                    axis,
                );
                let thumb_start = thumb.origin.along(axis);
                let thumb_length = thumb.size.along(axis);
                let pointer_position = event.position.along(axis);
                let grip = if thumb_length > px(0.0) {
                    (f32::from(pointer_position - thumb_start) / f32::from(thumb_length))
                        .clamp(0.0, 1.0)
                } else {
                    0.5
                };
                this.heat_scrollbar.drag_origin = Some((event.position, grip));
                this.heat_scrollbar.drag_pointer = event.position;
                if jump_offset.is_some()
                    && let Some(slot) = bucket_at(
                        &press_buckets,
                        this.heat_scrollbar.bounds,
                        pointer_position,
                        axis,
                        |index| this.list_scroll_handle.item_offset(index),
                    )
                    && let Some(bucket) = press_buckets.get(slot)
                {
                    this.heat_scrollbar.hovered_bucket = Some(slot);
                    this.navigate_heat_index(bucket.peak_index, window, cx);
                }
                cx.notify();
            }),
        )
        .on_mouse_move_all({
            let entity = entity.clone();
            move |event: &gpui::MouseMoveEvent, phase, _, window, cx| {
                if phase != gpui::DispatchPhase::Bubble {
                    return;
                }
                entity.update(cx, |this, cx| {
                    let Some((start, grip)) = this.heat_scrollbar.drag_origin else {
                        return;
                    };
                    if event.pressed_button != Some(MouseButton::Left) {
                        // 非客户区的移动事件不携带按键状态；窗口外等待捕获后的松键事件。
                        if !window.is_window_hovered() {
                            return;
                        }
                        this.heat_scrollbar.drag_origin = None;
                        this.heat_scrollbar.dragging = false;
                        cx.notify();
                        if !this.heat_scrollbar.bounds.contains(&event.position) {
                            this.set_heat_scrollbar_hovered(false, cx);
                        }
                        return;
                    }
                    if drag_delta(
                        start,
                        event.position,
                        this.heat_scrollbar.dragging,
                        scale,
                        axis,
                    )
                    .is_none()
                    {
                        return;
                    }
                    this.heat_scrollbar.dragging = true;
                    this.heat_scrollbar.hovered_bucket = None;
                    this.heat_scrollbar.drag_pointer = event.position;
                    let handle = this.list_scroll_handle.as_ref();
                    let mut offset = handle.offset();
                    let value = pointer_offset(
                        this.heat_scrollbar.bounds,
                        this.heat_scrollbar.thumb_bounds.size.along(axis),
                        grip,
                        event.position.along(axis),
                        handle.max_offset().along(axis),
                        axis,
                    );
                    offset = offset.apply_along(axis, |_| value);
                    handle.set_offset(offset);
                    cx.stop_propagation();
                    cx.notify();
                });
            }
        })
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, _: &gpui::MouseUpEvent, window, cx| {
                cx.stop_propagation();
                this.heat_scrollbar.drag_origin = None;
                this.heat_scrollbar.dragging = false;
                if !window.is_window_hovered() {
                    this.heat_scrollbar.clear_on_window_exit();
                }
                cx.notify();
            }),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|this, _, window, cx| {
                if this.heat_scrollbar.drag_origin.take().is_some() {
                    cx.stop_propagation();
                    this.heat_scrollbar.dragging = false;
                    this.set_heat_scrollbar_hovered(false, cx);
                    if !window.is_window_hovered() {
                        this.heat_scrollbar.clear_on_window_exit();
                    }
                }
            }),
        )
        .child(
            canvas(
                {
                    let entity = entity.clone();
                    move |bounds, window, cx| {
                        entity.update(cx, |this, cx| {
                            this.heat_scrollbar.bounds = bounds;
                            this.observe_heat_scrollbar_scroll(axis, cx);
                            // Windows 离开事件只刷新窗口状态，不能依赖旧命中框清除悬停。
                            if !window.is_window_hovered()
                                && this.heat_scrollbar.clear_on_window_exit()
                            {
                                cx.notify();
                            }
                        });
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        )
        .child(
            div()
                .id("clipboard-heat-scrollbar-track")
                .absolute()
                .when(horizontal, |this| {
                    this.left_0()
                        .right_0()
                        .bottom_0()
                        .h(ui_rem(if expanded { 24.0 } else { 12.0 }))
                })
                .when(!horizontal, |this| {
                    this.right_0()
                        .top_0()
                        .bottom_0()
                        .w(ui_rem(if expanded { 24.0 } else { 12.0 }))
                })
                .when(expanded, |this| {
                    this.bg(rail_background)
                        .map(|this| crate::window_material::with_backdrop_blur(this, cx))
                        .when(horizontal, |this| this.border_t_1())
                        .when(!horizontal, |this| this.border_l_1())
                        .border_color(gpui::ColorExt::opacity(&theme.muted_foreground, 0.16))
                })
                .opacity(if visible { 1.0 } else { 0.0 })
                .when(animations_enabled, |this| {
                    this.transitions(|transitions| {
                        if horizontal {
                            transitions.h(motion.clone()).opacity(motion)
                        } else {
                            transitions.w(motion.clone()).opacity(motion)
                        }
                    })
                })
                .child(
                    canvas(
                        {
                            let entity = entity.clone();
                            move |bounds, _, cx| {
                                entity.update(cx, |this, cx| {
                                    let handle = this.list_scroll_handle.as_ref();
                                    let mut thumb = thumb_bounds(
                                        bounds,
                                        handle.bounds().size.along(axis),
                                        handle.max_offset().along(axis),
                                        handle.offset().along(axis),
                                        scale,
                                        axis,
                                    );
                                    if this.heat_scrollbar.dragging
                                        && let Some((_, grip)) = this.heat_scrollbar.drag_origin
                                    {
                                        let mut offset = handle.offset();
                                        let value = pointer_offset(
                                            bounds,
                                            thumb.size.along(axis),
                                            grip,
                                            this.heat_scrollbar.drag_pointer.along(axis),
                                            handle.max_offset().along(axis),
                                            axis,
                                        );
                                        if (offset.along(axis)) != value {
                                            offset = offset.apply_along(axis, |_| value);
                                            handle.set_offset(offset);
                                            thumb = thumb_bounds(
                                                bounds,
                                                handle.bounds().size.along(axis),
                                                handle.max_offset().along(axis),
                                                value,
                                                scale,
                                                axis,
                                            );
                                            cx.notify();
                                        }
                                    }
                                    this.heat_scrollbar.thumb_bounds = thumb;
                                });
                            }
                        },
                        move |bounds, _, window, cx| {
                            let this = entity.read(cx);
                            let state = &this.heat_scrollbar;
                            let item_offset = |index| this.list_scroll_handle.item_offset(index);
                            let thickness = bounds.size.along(axis.invert()) / 3.0;
                            let heat_inset = if expanded {
                                thickness
                            } else {
                                bounds.size.along(axis.invert()) - thickness
                            };
                            let heat_bounds = gpui::Bounds::new(
                                bounds
                                    .origin
                                    .apply_along(axis.invert(), |start| start + heat_inset),
                                bounds.size.apply_along(axis.invert(), |_| thickness),
                            );
                            window.paint_quad(
                                gpui::fill(heat_bounds, heat_color(0, dark))
                                    .corner_radii(thickness / 2.0),
                            );
                            // 合并相邻同档热度，避免大量分段边缘产生横纹。
                            for (range, level) in &paint_runs {
                                if *level == 0 {
                                    continue;
                                }
                                let start = bounds.origin.along(axis)
                                    + bounds.size.along(axis)
                                        * item_ratio(range.start, total, &item_offset);
                                let end = bounds.origin.along(axis)
                                    + bounds.size.along(axis)
                                        * item_ratio(range.end, total, &item_offset);
                                window.paint_quad(gpui::fill(
                                    gpui::Bounds::new(
                                        heat_bounds.origin.apply_along(axis, |_| start),
                                        heat_bounds.size.apply_along(axis, |_| {
                                            (end - start - px(0.7 * scale)).max(px(0.0))
                                        }),
                                    ),
                                    gpui::ColorExt::opacity(&heat_color(*level, dark), 0.94),
                                ));
                            }
                            if let Some(bucket) = state
                                .hovered_bucket
                                .and_then(|slot| paint_buckets.get(slot))
                            {
                                let start = bounds.origin.along(axis)
                                    + bounds.size.along(axis)
                                        * item_ratio(bucket.range.start, total, &item_offset);
                                let end = bounds.origin.along(axis)
                                    + bounds.size.along(axis)
                                        * item_ratio(bucket.range.end, total, &item_offset);
                                window.paint_quad(
                                    gpui::outline(
                                        gpui::Bounds::new(
                                            heat_bounds.origin.apply_along(axis, |_| start),
                                            heat_bounds.size.apply_along(axis, |_| end - start),
                                        ),
                                        thumb_color,
                                        gpui::BorderStyle::Solid,
                                    )
                                    .border_widths(px(scale)),
                                );
                            }
                            let mut thumb = state.thumb_bounds;
                            if !expanded {
                                let thumb_thickness = thickness * 1.5;
                                thumb.origin = thumb.origin.apply_along(axis.invert(), |_| {
                                    bounds.origin.along(axis.invert())
                                        + bounds.size.along(axis.invert())
                                        - thumb_thickness
                                });
                                thumb.size =
                                    thumb.size.apply_along(axis.invert(), |_| thumb_thickness);
                            }
                            let border = bounds.size.along(axis.invert()) / 12.0;
                            let radius = if expanded {
                                bounds.size.along(axis.invert()) / 6.0
                            } else {
                                thumb.size.along(axis.invert()) / 2.0
                            };
                            if expanded {
                                window.paint_quad(
                                    gpui::outline(
                                        gpui::Bounds::new(
                                            thumb
                                                .origin
                                                .apply_along(axis.invert(), |start| start - border),
                                            thumb.size.apply_along(axis.invert(), |length| {
                                                length + border * 2.0
                                            }),
                                        ),
                                        rail_background,
                                        gpui::BorderStyle::Solid,
                                    )
                                    .border_widths(border)
                                    .corner_radii(radius + border),
                                );
                            }
                            let thumb_background = if expanded {
                                gpui::ColorExt::opacity(
                                    &theme.background,
                                    if state.dragging { 0.12 } else { 0.28 },
                                )
                            } else {
                                gpui::ColorExt::opacity(&thumb_color, 0.35)
                            };
                            window.paint_quad(
                                gpui::fill(thumb, thumb_background)
                                    .border_widths(border)
                                    .border_color(thumb_color)
                                    .corner_radii(radius),
                            );
                        },
                    )
                    .absolute()
                    .size_full(),
                ),
        );

    let mut overlay = div()
        .absolute()
        .inset_0()
        .child(exclude_from_window_drag(rail, cx.entity()));
    if visible
        && expanded
        && window_hovered
        && !state.dragging
        && let Some(bucket) = state.hovered_bucket.and_then(|slot| buckets.get(slot))
    {
        let translator = i18n::translator(cx);
        let region = translator.translate_with_args(
            "main.heatmap.region",
            &[
                ("start", (bucket.range.start + 1).to_string()),
                ("end", bucket.range.end.to_string()),
            ],
        );
        let summary = if bucket.max_count == 0 {
            translator.translate("main.heatmap.unused")
        } else {
            translator.translate_with_args(
                "main.heatmap.summary",
                &[
                    ("total", bucket.total_count.to_string()),
                    ("peak", bucket.max_count.to_string()),
                ],
            )
        };
        let tooltip_width = (list_width - px(40.0 * scale)).max(px(0.0));
        let tooltip_width = if horizontal {
            tooltip_width.min(px(220.0 * scale))
        } else {
            tooltip_width
        };
        let position = (state.bounds.size.along(axis)
            * item_ratio(bucket.range.start, total, &|index| {
                scroll_handle.item_offset(index)
            }))
        .min(
            (state.bounds.size.along(axis)
                - if horizontal {
                    tooltip_width
                } else {
                    px(72.0 * scale)
                })
            .max(px(0.0)),
        );
        let mut background = theme.popover;
        background.alpha = 1.0;
        let background = crate::window_material::overlay_surface_color(cx, background);
        let foreground = theme.popover_foreground;
        let secondary = gpui::ColorExt::opacity(&foreground, 0.7);
        // 纯展示元素不注册命中区域，鼠标可穿透到下方列表。
        overlay = overlay.child(
            gpui::deferred(
                div()
                    .absolute()
                    .when(horizontal, |this| this.left(position).bottom(ui_rem(30.0)))
                    .when(!horizontal, |this| this.right(ui_rem(30.0)).top(position))
                    .w_auto()
                    .max_w(tooltip_width)
                    .px_3()
                    .py_2()
                    .flex()
                    .flex_col()
                    .gap(ui_rem(3.0))
                    .rounded_md()
                    .border_1()
                    .border_color(gpui::ColorExt::opacity(&foreground, 0.12))
                    .bg(background)
                    .map(|this| crate::window_material::with_backdrop_blur(this, cx))
                    .text_color(foreground)
                    .text_xs()
                    .child(region)
                    .child(div().text_color(secondary).child(summary)),
            )
            .priority(1),
        );
    }
    overlay.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizontal_heat_rail_uses_x_for_thumb_click_drag_and_hover() {
        let axis = Axis::Horizontal;
        let track = gpui::Bounds::new(point(px(30.0), px(700.0)), size(px(400.0), px(24.0)));
        let thumb = thumb_bounds(track, px(400.0), px(1600.0), px(-400.0), 1.0, axis);
        assert_eq!(thumb.size.width, px(80.0));
        assert_eq!(thumb.left(), px(110.0));
        assert!(thumb.top() > track.top());
        assert!(thumb.bottom() < track.bottom());
        assert_eq!(
            track_press_offset(track, thumb, px(120.0), px(1600.0), axis),
            None
        );
        assert_eq!(
            track_press_offset(track, thumb, px(230.0), px(1600.0), axis),
            Some(px(-800.0))
        );
        assert_eq!(
            pointer_offset(track, thumb.size.width, 0.5, px(-100.0), px(1600.0), axis),
            px(0.0)
        );
        assert_eq!(
            pointer_offset(track, thumb.size.width, 0.5, px(900.0), px(1600.0), axis),
            px(-1600.0)
        );
        assert_eq!(
            drag_delta(
                track.origin,
                track.origin + point(px(5.0), px(0.0)),
                false,
                1.0,
                axis
            ),
            Some(px(5.0))
        );
        let moved = pointer_offset(track, thumb.size.width, 0.25, px(230.0), px(1600.0), axis);
        let dragged = thumb_bounds(track, px(400.0), px(1600.0), moved, 1.0, axis);
        assert_eq!(dragged.left() + dragged.size.width * 0.25, px(230.0));

        let buckets = vec![
            HeatBucket {
                range: 0..2,
                total_count: 0,
                max_count: 0,
                peak_index: 0,
            },
            HeatBucket {
                range: 2..4,
                total_count: 80,
                max_count: 80,
                peak_index: 3,
            },
        ];
        let offsets = [0.0, 48.0, 96.0, 288.0, 480.0];
        let item_offset = |index| offsets.get(index).copied().map(px);
        assert_eq!(
            bucket_at(&buckets, track, px(109.0), axis, item_offset),
            Some(0)
        );
        assert_eq!(
            bucket_at(&buckets, track, px(110.0), axis, item_offset),
            Some(1)
        );
        assert_eq!(
            bucket_at(&buckets, track, px(900.0), axis, item_offset),
            Some(1)
        );
    }

    #[test]
    fn heat_overview_limits_density_and_preserves_counts_and_peaks() {
        let buckets = Rc::new(
            (0..256)
                .map(|index| HeatBucket {
                    range: index * 4..(index + 1) * 4,
                    total_count: if index == 127 { 90 } else { 1 },
                    max_count: if index == 127 { 90 } else { 1 },
                    peak_index: index * 4,
                })
                .collect::<Vec<_>>(),
        );
        let overview = overview_buckets(buckets.clone(), px(360.0), 1.0);
        assert_eq!(overview.len(), 120);
        assert_eq!(overview.first().unwrap().range.start, 0);
        assert_eq!(overview.last().unwrap().range.end, 1024);
        assert!(
            overview
                .windows(2)
                .all(|pair| pair[0].range.end == pair[1].range.start)
        );
        assert_eq!(
            overview
                .iter()
                .map(|bucket| bucket.total_count)
                .sum::<u64>(),
            345
        );
        let peak = overview
            .iter()
            .find(|bucket| bucket.range.contains(&508))
            .unwrap();
        assert_eq!((peak.max_count, peak.peak_index), (90, 508));
        assert!(Rc::ptr_eq(
            &overview,
            &overview_buckets(overview.clone(), px(720.0), 1.0)
        ));
        assert_eq!(overview_buckets(buckets.clone(), px(2.0), 1.0).len(), 1);
        assert_eq!(overview_buckets(buckets, px(0.0), 1.0).len(), 256);
    }

    #[test]
    fn heat_press_distinguishes_click_from_drag_anywhere_on_the_rail() {
        let start = point(px(14.0), px(200.0));
        assert_eq!(
            drag_delta(
                start,
                point(px(15.0), px(202.0)),
                false,
                1.0,
                Axis::Vertical
            ),
            None
        );
        assert_eq!(
            drag_delta(
                start,
                point(px(14.0), px(204.0)),
                false,
                1.0,
                Axis::Vertical
            ),
            Some(px(4.0))
        );
        assert_eq!(
            drag_delta(start, start, true, 1.0, Axis::Vertical),
            Some(px(0.0))
        );
        assert_eq!(
            drag_delta(
                start,
                point(px(14.0), px(204.0)),
                false,
                2.0,
                Axis::Vertical
            ),
            None
        );
        assert_eq!(
            drag_delta(
                start,
                point(px(14.0), px(208.0)),
                false,
                2.0,
                Axis::Vertical
            ),
            Some(px(8.0))
        );
    }

    #[test]
    fn heat_runs_merge_similar_neighbors_without_losing_hotspots() {
        let counts = [0, 1, 9, 10, 49, 50, 99, 100, 199, 200, 499, 500, 0];
        let buckets = counts
            .iter()
            .enumerate()
            .map(|(index, count)| HeatBucket {
                range: index..index + 1,
                total_count: *count,
                max_count: *count,
                peak_index: index,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            heat_runs(&buckets),
            vec![
                (0..1, 0),
                (1..3, 1),
                (3..5, 2),
                (5..7, 3),
                (7..9, 4),
                (9..11, 5),
                (11..12, 6),
                (12..13, 0),
            ]
        );
        assert!(heat_runs(&[]).is_empty());
    }

    #[test]
    fn heat_thumb_tracks_pixel_scroll_and_clamps_drag_at_both_ends() {
        let track = gpui::Bounds::new(point(px(88.0), px(10.0)), size(px(12.0), px(400.0)));
        let top = thumb_bounds(track, px(400.0), px(1600.0), px(0.0), 1.0, Axis::Vertical);
        let bottom = thumb_bounds(
            track,
            px(400.0),
            px(1600.0),
            px(-1600.0),
            1.0,
            Axis::Vertical,
        );
        assert_eq!(top.size.height, px(80.0));
        assert_eq!(top.top(), track.top());
        assert_eq!(bottom.bottom(), track.bottom());
        assert!(top.left() > track.left());
        assert!(top.right() < track.right());
        assert_eq!(
            thumb_bounds(track, px(400.0), px(0.0), px(0.0), 1.0, Axis::Vertical)
                .size
                .height,
            px(400.0)
        );
        assert_eq!(
            pointer_offset(
                track,
                top.size.height,
                0.5,
                px(370.0),
                px(1600.0),
                Axis::Vertical
            ),
            px(-1600.0)
        );
        assert_eq!(
            pointer_offset(
                track,
                top.size.height,
                0.5,
                px(-900.0),
                px(1600.0),
                Axis::Vertical
            ),
            px(0.0)
        );
        assert_eq!(
            pointer_offset(
                track,
                top.size.height,
                0.5,
                px(900.0),
                px(1600.0),
                Axis::Vertical
            ),
            px(-1600.0)
        );
        assert_eq!(
            pointer_offset(
                track,
                track.size.height,
                0.5,
                px(900.0),
                px(0.0),
                Axis::Vertical
            ),
            px(0.0)
        );
    }

    #[test]
    fn heat_track_press_centers_thumb_before_drag_and_preserves_grab_position() {
        let track = gpui::Bounds::new(point(px(88.0), px(10.0)), size(px(24.0), px(400.0)));
        let thumb = thumb_bounds(
            track,
            px(400.0),
            px(1600.0),
            px(-400.0),
            1.0,
            Axis::Vertical,
        );
        let start =
            track_press_offset(track, thumb, px(210.0), px(1600.0), Axis::Vertical).unwrap();
        assert_eq!(start, px(-800.0));
        let jumped = thumb_bounds(track, px(400.0), px(1600.0), start, 1.0, Axis::Vertical);
        assert_eq!(jumped.top() + jumped.size.height / 2.0, px(210.0));

        let moved = pointer_offset(
            track,
            jumped.size.height,
            0.5,
            px(250.0),
            px(1600.0),
            Axis::Vertical,
        );
        let dragged = thumb_bounds(track, px(400.0), px(1600.0), moved, 1.0, Axis::Vertical);
        assert_eq!(dragged.top() + dragged.size.height / 2.0, px(250.0));
        assert_eq!(
            track_press_offset(
                track,
                thumb,
                thumb.top() + px(10.0),
                px(1600.0),
                Axis::Vertical
            ),
            None
        );
        let grabbed = thumb_bounds(
            track,
            px(400.0),
            px(1600.0),
            pointer_offset(
                track,
                thumb.size.height,
                0.125,
                thumb.top() + px(50.0),
                px(1600.0),
                Axis::Vertical,
            ),
            1.0,
            Axis::Vertical,
        );
        assert_eq!(grabbed.top() + px(10.0), thumb.top() + px(50.0));

        assert_eq!(
            track_press_offset(track, thumb, track.top(), px(1600.0), Axis::Vertical),
            Some(px(0.0))
        );
        assert_eq!(
            track_press_offset(track, thumb, track.bottom(), px(1600.0), Axis::Vertical),
            Some(px(-1600.0))
        );
        assert_eq!(
            track_press_offset(track, thumb, px(500.0), px(0.0), Axis::Vertical),
            Some(px(0.0))
        );
    }

    #[test]
    fn heat_hit_testing_clamps_edges_and_matches_uneven_regions() {
        let buckets = vec![
            HeatBucket {
                range: 0..1,
                total_count: 0,
                max_count: 0,
                peak_index: 0,
            },
            HeatBucket {
                range: 1..3,
                total_count: 0,
                max_count: 0,
                peak_index: 1,
            },
            HeatBucket {
                range: 3..5,
                total_count: 0,
                max_count: 0,
                peak_index: 3,
            },
        ];
        let bounds = gpui::Bounds::new(point(px(0.0), px(10.0)), size(px(24.0), px(100.0)));
        assert_eq!(
            bucket_at(&buckets, bounds, px(-20.0), Axis::Vertical, |_| None),
            Some(0)
        );
        assert_eq!(
            bucket_at(&buckets, bounds, px(30.0), Axis::Vertical, |_| None),
            Some(1)
        );
        assert_eq!(
            bucket_at(&buckets, bounds, px(70.0), Axis::Vertical, |_| None),
            Some(2)
        );
        assert_eq!(
            bucket_at(&buckets, bounds, px(300.0), Axis::Vertical, |_| None),
            Some(2)
        );
        assert_eq!(
            bucket_at(&[], bounds, px(50.0), Axis::Vertical, |_| None),
            None
        );
        assert_eq!(
            bucket_at(
                &buckets,
                gpui::Bounds::default(),
                px(50.0),
                Axis::Vertical,
                |_| None
            ),
            None
        );
    }

    #[test]
    fn heat_regions_follow_variable_row_heights_for_paint_hover_and_click() {
        let offsets = [0.0, 48.0, 96.0, 288.0, 480.0];
        let item_offset = |index| offsets.get(index).copied().map(px);
        let buckets = vec![
            HeatBucket {
                range: 0..2,
                total_count: 0,
                max_count: 0,
                peak_index: 0,
            },
            HeatBucket {
                range: 2..4,
                total_count: 80,
                max_count: 80,
                peak_index: 3,
            },
        ];
        let bounds = gpui::Bounds::new(point(px(0.0), px(10.0)), size(px(24.0), px(400.0)));
        let boundary = bounds.top() + bounds.size.height * item_ratio(2, 4, &item_offset);
        assert_eq!(boundary, px(90.0));
        assert_eq!(item_ratio(0, 4, &item_offset), 0.0);
        assert_eq!(item_ratio(4, 4, &item_offset), 1.0);
        assert_eq!(
            bucket_at(
                &buckets,
                bounds,
                boundary - px(0.1),
                Axis::Vertical,
                item_offset
            ),
            Some(0)
        );
        assert_eq!(
            bucket_at(&buckets, bounds, boundary, Axis::Vertical, item_offset),
            Some(1)
        );
        assert_eq!(
            bucket_at(&buckets, bounds, px(210.0), Axis::Vertical, item_offset),
            Some(1)
        );
        let thumb = thumb_bounds(bounds, px(96.0), px(384.0), px(-96.0), 1.0, Axis::Vertical);
        assert_eq!(thumb.top(), boundary);
    }

    #[test]
    fn heat_drag_keeps_pointer_anchor_when_content_height_or_track_changes() {
        let track = gpui::Bounds::new(point(px(0.0), px(10.0)), size(px(24.0), px(400.0)));
        for max_offset in [1600.0, 3200.0, 8000.0, 800.0] {
            let thumb = thumb_bounds(
                track,
                px(400.0),
                px(max_offset),
                px(0.0),
                1.0,
                Axis::Vertical,
            );
            for grip in [0.0, 0.25, 0.5, 1.0] {
                let offset = pointer_offset(
                    track,
                    thumb.size.height,
                    grip,
                    px(210.0),
                    px(max_offset),
                    Axis::Vertical,
                );
                let dragged = thumb_bounds(
                    track,
                    px(400.0),
                    px(max_offset),
                    offset,
                    1.0,
                    Axis::Vertical,
                );
                assert!(
                    f32::from(dragged.top() + dragged.size.height * grip - px(210.0)).abs() < 0.001
                );
            }
        }
        let resized = gpui::Bounds::new(point(px(0.0), px(30.0)), size(px(24.0), px(600.0)));
        let thumb = thumb_bounds(resized, px(600.0), px(3200.0), px(0.0), 1.0, Axis::Vertical);
        let offset = pointer_offset(
            resized,
            thumb.size.height,
            0.25,
            px(210.0),
            px(3200.0),
            Axis::Vertical,
        );
        let dragged = thumb_bounds(resized, px(600.0), px(3200.0), offset, 1.0, Axis::Vertical);
        assert!(f32::from(dragged.top() + dragged.size.height * 0.25 - px(210.0)).abs() < 0.001);
    }
}
