use std::{collections::HashMap, time::Instant};

use super::HistoryItemId;

const TRANSITION_SECONDS: f32 = 0.16;

struct Transition {
    index: usize,
    from: f32,
    target: f32,
    started_at: Instant,
}

impl Transition {
    fn progress(&self, now: Instant) -> f32 {
        let elapsed = now.saturating_duration_since(self.started_at).as_secs_f32();
        let progress = gpui::ease_in_out((elapsed / TRANSITION_SECONDS).clamp(0.0, 1.0));
        self.from + (self.target - self.from) * progress
    }

    fn is_animating(&self, now: Instant) -> bool {
        self.from != self.target
            && now.saturating_duration_since(self.started_at).as_secs_f32() < TRANSITION_SECONDS
    }
}

#[derive(Default)]
pub(super) struct InfoRowState {
    pub(super) query_generation: Option<u64>,
    transitions: HashMap<HistoryItemId, Transition>,
}

impl InfoRowState {
    pub(super) fn set_hovered(
        &mut self,
        id: HistoryItemId,
        index: usize,
        hovered: bool,
        animated: bool,
        now: Instant,
    ) -> bool {
        let target = if hovered { 1.0 } else { 0.0 };
        let previous = self.transitions.get(&id);
        if previous.is_none_or(|previous| previous.target == target) && !hovered {
            return false;
        }
        if previous.is_some_and(|previous| previous.target == target) {
            return false;
        }
        let from = if animated {
            previous.map_or(0.0, |previous| previous.progress(now))
        } else {
            target
        };
        if hovered {
            self.collapse(animated, now);
        }
        self.transitions.insert(
            id,
            Transition {
                index,
                from,
                target,
                started_at: now,
            },
        );
        true
    }

    pub(super) fn collapse(&mut self, animated: bool, now: Instant) {
        for transition in self.transitions.values_mut() {
            if transition.target != 0.0 || !animated {
                transition.from = if animated {
                    transition.progress(now)
                } else {
                    0.0
                };
                transition.target = 0.0;
                transition.started_at = now;
            }
        }
    }

    pub(super) fn finish(&mut self) {
        for transition in self.transitions.values_mut() {
            transition.from = transition.target;
        }
    }

    pub(super) fn frame(&mut self, now: Instant) -> (HashMap<HistoryItemId, (usize, f32)>, bool) {
        let progress = self
            .transitions
            .iter()
            .map(|(id, transition)| (*id, (transition.index, transition.progress(now))))
            .collect();
        let animating = self
            .transitions
            .values()
            .any(|transition| transition.is_animating(now));
        self.transitions
            .retain(|_, transition| transition.target != 0.0 || transition.is_animating(now));
        (progress, animating)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn hover_information_reverses_from_current_height_and_finishes_collapsed() {
        let mut state = InfoRowState::default();
        let start = Instant::now();
        assert!(state.set_hovered(1, 3, true, true, start));
        assert_eq!(state.frame(start).0[&1], (3, 0.0));
        let middle = start + Duration::from_millis(80);
        let progress = state.frame(middle).0[&1].1;
        assert!(progress > 0.0 && progress < 1.0);
        assert!(state.set_hovered(1, 3, false, true, middle));
        assert_eq!(state.frame(middle).0[&1].1, progress);
        let (frame, animating) = state.frame(middle + Duration::from_millis(160));
        assert_eq!(frame[&1].1, 0.0);
        assert!(!animating);
        assert!(
            state
                .frame(middle + Duration::from_millis(200))
                .0
                .is_empty()
        );
    }

    #[test]
    fn disabled_animation_and_window_exit_leave_no_reserved_height() {
        let mut state = InfoRowState::default();
        let now = Instant::now();
        state.set_hovered(1, 0, true, false, now);
        assert_eq!(state.frame(now).0[&1].1, 1.0);
        state.collapse(false, now);
        let (frame, animating) = state.frame(now);
        assert_eq!(frame[&1].1, 0.0);
        assert!(!animating);
    }

    #[gpui::test]
    fn hover_information_resizes_virtual_offsets_and_total_height(cx: &mut gpui::TestAppContext) {
        use gpui::{Context, IntoElement, Render, Window, div, prelude::*, px, size};
        use gpui_component::{
            ReorderableVirtualListSizes, VirtualListScrollHandle, v_virtual_list_reorderable,
        };

        struct Rows {
            sizes: ReorderableVirtualListSizes,
            scroll: VirtualListScrollHandle,
            extra: gpui::Pixels,
        }

        impl Render for Rows {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                div().size_full().child(
                    v_virtual_list_reorderable(
                        cx.entity(),
                        "information-row-test",
                        self.sizes.clone(),
                        |this, range, _, _| {
                            range
                                .map(|index| {
                                    div()
                                        .w_full()
                                        .h(px(20.0) + if index == 0 { this.extra } else { px(0.0) })
                                        .into_any_element()
                                })
                                .collect()
                        },
                    )
                    .size_full()
                    .track_scroll(&self.scroll),
                )
            }
        }

        let window = cx.open_window(size(px(320.0), px(200.0)), |_, _| Rows {
            sizes: ReorderableVirtualListSizes::new(vec![size(px(320.0), px(20.0)); 3]),
            scroll: VirtualListScrollHandle::new(),
            extra: px(0.0),
        });
        for extra in [px(0.0), px(8.0), px(16.0), px(8.0), px(0.0)] {
            cx.update_window(window.into(), |view, window, cx| {
                let rows = view.downcast::<Rows>().unwrap();
                rows.update(cx, |rows, cx| {
                    rows.extra = extra;
                    rows.sizes.set_size(0, size(px(320.0), px(20.0) + extra));
                    cx.notify();
                });
                window.refresh();
                window.draw(cx).clear(cx);
            })
            .unwrap();
            window
                .read_with(cx, |rows, _| {
                    assert_eq!(rows.scroll.item_offset(1), Some(px(20.0) + extra));
                    assert_eq!(rows.scroll.item_offset(2), Some(px(40.0) + extra));
                    assert_eq!(rows.scroll.item_offset(3), Some(px(60.0) + extra));
                })
                .unwrap();
        }
    }
}
