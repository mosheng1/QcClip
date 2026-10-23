use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::App;
use raw_input::{Event, Key, Listen, MouseButton, SubscriptionHandle};

use crate::config::MiddleMouseShortcutConfig;

pub(super) struct MiddleMouseRuntime {
    state: Arc<Mutex<HoldState>>,
    subscription: Option<SubscriptionHandle>,
}

impl MiddleMouseRuntime {
    pub(super) fn install(config: MiddleMouseShortcutConfig, cx: &mut App) -> Self {
        let state = Arc::new(Mutex::new(HoldState::new(config)));
        let (sender, receiver) = async_channel::unbounded();
        let state_for_input = state.clone();
        let subscription = Listen::subscribe(move |event| {
            let mut state = state_for_input
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(press) = state.handle_event(event, Instant::now()) {
                let _ = sender.try_send(press);
            }
        });
        let state_for_events = state.clone();
        cx.spawn(async move |cx| {
            while let Ok((generation, deadline)) = receiver.recv().await {
                let state = state_for_events.clone();
                cx.spawn(async move |cx| {
                    cx.background_executor()
                        .timer(deadline.saturating_duration_since(Instant::now()))
                        .await;
                    cx.update(|cx| {
                        let fire = state
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .complete(generation, Instant::now());
                        if fire {
                            crate::windows::main::toggle_main_window(cx);
                        }
                    });
                })
                .detach();
            }
        })
        .detach();
        Self {
            state,
            subscription: Some(subscription),
        }
    }

    pub(super) fn configure(&self, config: MiddleMouseShortcutConfig, available: bool) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.config = config.normalized();
        state.available = available;
        state.pending = None;
    }
}

impl Drop for MiddleMouseRuntime {
    fn drop(&mut self) {
        if let Some(subscription) = self.subscription.take() {
            subscription.unsubscribe();
        }
    }
}

#[cfg(test)]
pub(super) fn runtime_without_subscription() -> MiddleMouseRuntime {
    MiddleMouseRuntime {
        state: Arc::new(Mutex::new(HoldState::new(
            MiddleMouseShortcutConfig::default(),
        ))),
        subscription: None,
    }
}

struct HoldState {
    config: MiddleMouseShortcutConfig,
    available: bool,
    keys: HashSet<Key>,
    middle_pressed: bool,
    generation: u64,
    pending: Option<(u64, Instant)>,
}

impl HoldState {
    fn new(config: MiddleMouseShortcutConfig) -> Self {
        Self {
            config: config.normalized(),
            available: true,
            keys: HashSet::new(),
            middle_pressed: false,
            generation: 0,
            pending: None,
        }
    }

    fn modifiers_match(&self) -> bool {
        let control =
            self.keys.contains(&Key::ControlLeft) || self.keys.contains(&Key::ControlRight);
        let alt = self.keys.contains(&Key::AltLeft) || self.keys.contains(&Key::AltRight);
        let shift = self.keys.contains(&Key::ShiftLeft) || self.keys.contains(&Key::ShiftRight);
        let platform = self.keys.contains(&Key::MetaLeft) || self.keys.contains(&Key::MetaRight);
        let expected = self
            .config
            .modifier
            .map(|modifier| modifier.binding_flags())
            .unwrap_or((false, false, false));
        !platform && (control, alt, shift) == expected
    }

    fn handle_event(&mut self, event: Event, now: Instant) -> Option<(u64, Instant)> {
        match event {
            Event::KeyDown { key, .. } => {
                self.keys.insert(key);
            }
            Event::KeyUp { key, .. } => {
                self.keys.remove(&key);
            }
            Event::MouseDown {
                button: MouseButton::Middle,
            } => {
                if self.middle_pressed {
                    return None;
                }
                self.middle_pressed = true;
                if self.available && self.config.enabled && self.modifiers_match() {
                    self.generation = self.generation.wrapping_add(1);
                    let deadline =
                        now + Duration::from_millis(u64::from(self.config.hold_threshold_ms));
                    self.pending = Some((self.generation, deadline));
                    return self.pending;
                }
            }
            Event::MouseUp {
                button: MouseButton::Middle,
            } => {
                self.middle_pressed = false;
                self.pending = None;
            }
            _ => return None,
        }
        // 修饰键一旦不匹配，本次长按即取消，重新按下中键才开始计时。
        if !self.modifiers_match() {
            self.pending = None;
        }
        None
    }

    fn complete(&mut self, generation: u64, now: Instant) -> bool {
        if !self
            .pending
            .is_some_and(|(pending, deadline)| pending == generation && now >= deadline)
            || !self.available
            || !self.config.enabled
        {
            return false;
        }
        self.pending = None;
        self.middle_pressed && self.modifiers_match()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::QuickPasteShortcutModifier;

    const DOWN: Event = Event::MouseDown {
        button: MouseButton::Middle,
    };
    const UP: Event = Event::MouseUp {
        button: MouseButton::Middle,
    };

    fn state(modifier: Option<QuickPasteShortcutModifier>) -> HoldState {
        HoldState::new(MiddleMouseShortcutConfig {
            enabled: true,
            modifier,
            hold_threshold_ms: 200,
        })
    }

    #[test]
    fn middle_mouse_fires_once_and_requires_a_new_press() {
        let mut state = state(None);
        let now = Instant::now();
        let (generation, deadline) = state.handle_event(DOWN, now).unwrap();
        assert_eq!(deadline - now, Duration::from_millis(200));
        assert!(!state.complete(generation, deadline - Duration::from_millis(1)));
        assert!(state.handle_event(DOWN, deadline).is_none());
        assert!(state.complete(generation, deadline));
        assert!(!state.complete(generation, deadline));
        state.handle_event(UP, deadline);
        let (next, next_deadline) = state.handle_event(DOWN, deadline).unwrap();
        assert!(!state.complete(generation, next_deadline));
        assert!(state.complete(next, next_deadline));
    }

    #[test]
    fn middle_mouse_short_press_and_modifier_change_cancel_the_hold() {
        let mut state = state(Some(QuickPasteShortcutModifier::Control));
        let now = Instant::now();
        assert!(state.handle_event(DOWN, now).is_none());
        state.handle_event(UP, now);
        state.handle_event(
            Event::KeyDown {
                key: Key::ControlRight,
                code: None,
            },
            now,
        );
        let (generation, deadline) = state.handle_event(DOWN, now).unwrap();
        state.handle_event(UP, deadline - Duration::from_millis(1));
        assert!(!state.complete(generation, deadline));
        let (generation, deadline) = state.handle_event(DOWN, now).unwrap();
        state.handle_event(
            Event::KeyUp {
                key: Key::ControlRight,
                code: None,
            },
            now,
        );
        state.handle_event(
            Event::KeyDown {
                key: Key::ControlRight,
                code: None,
            },
            now,
        );
        assert!(!state.complete(generation, deadline));
    }

    #[test]
    fn middle_mouse_none_rejects_modifiers_and_pause_cancels_pending() {
        let mut state = state(None);
        let now = Instant::now();
        state.handle_event(
            Event::KeyDown {
                key: Key::MetaLeft,
                code: None,
            },
            now,
        );
        assert!(state.handle_event(DOWN, now).is_none());
        state.handle_event(UP, now);
        state.handle_event(
            Event::KeyUp {
                key: Key::MetaLeft,
                code: None,
            },
            now,
        );
        let (generation, deadline) = state.handle_event(DOWN, now).unwrap();
        let runtime = MiddleMouseRuntime {
            state: Arc::new(Mutex::new(state)),
            subscription: None,
        };
        let config = MiddleMouseShortcutConfig {
            enabled: true,
            ..Default::default()
        };
        runtime.configure(config, false);
        runtime.configure(config, true);
        assert!(!runtime.state.lock().unwrap().complete(generation, deadline));
    }

    #[test]
    fn middle_mouse_requires_exact_modifiers_and_supports_both_sides() {
        for key in [Key::ControlLeft, Key::ControlRight] {
            let mut state = state(Some(QuickPasteShortcutModifier::Control));
            let now = Instant::now();
            state.handle_event(Event::KeyDown { key, code: None }, now);
            state.handle_event(
                Event::KeyDown {
                    key: Key::ShiftLeft,
                    code: None,
                },
                now,
            );
            assert!(state.handle_event(DOWN, now).is_none());
            state.handle_event(UP, now);
            state.handle_event(
                Event::KeyUp {
                    key: Key::ShiftLeft,
                    code: None,
                },
                now,
            );
            let (generation, deadline) = state.handle_event(DOWN, now).unwrap();
            assert!(state.complete(generation, deadline));
        }
    }

    #[test]
    fn middle_mouse_disabled_and_config_changes_cancel_pending() {
        let mut state = HoldState::new(MiddleMouseShortcutConfig::default());
        let now = Instant::now();
        assert!(state.handle_event(DOWN, now).is_none());
        state.handle_event(UP, now);
        state.config.enabled = true;
        let (generation, deadline) = state.handle_event(DOWN, now).unwrap();
        let runtime = MiddleMouseRuntime {
            state: Arc::new(Mutex::new(state)),
            subscription: None,
        };
        runtime.configure(
            MiddleMouseShortcutConfig {
                enabled: true,
                hold_threshold_ms: 1000,
                ..Default::default()
            },
            true,
        );
        let mut state = runtime.state.lock().unwrap();
        assert!(!state.complete(generation, deadline));
        assert!(state.handle_event(DOWN, deadline).is_none());
    }
}
