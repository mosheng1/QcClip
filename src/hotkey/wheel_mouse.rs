use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::App;
use raw_input::{Event, Listen, MouseButton, SubscriptionHandle};

use crate::config::{WheelMouseButton, WheelShortcutConfig};

/// 发给轮盘控制通道的指令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WheelCommand {
    /// 侧键按下（携带按下代数），到达按住阈值且仍未松开时弹出轮盘。
    PressStarted(u64),
    /// 侧键松开且轮盘已弹出：按当前指向选定并关闭。
    Commit,
    /// 侧键松开但轮盘未弹出（短按），确保没有残留窗口。
    Cancel,
}

pub(super) struct WheelMouseRuntime {
    state: Arc<Mutex<WheelHoldState>>,
    subscription: Option<SubscriptionHandle>,
}

impl WheelMouseRuntime {
    pub(super) fn install(config: WheelShortcutConfig, cx: &mut App) -> Self {
        let state = Arc::new(Mutex::new(WheelHoldState::new(config)));
        let (sender, receiver) = async_channel::unbounded();
        let state_for_input = state.clone();
        let subscription = Listen::subscribe(move |event| {
            let mut state = state_for_input
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(command) = state.handle_event(event) {
                let _ = sender.try_send(command);
            }
        });
        let state_for_events = state.clone();
        cx.spawn(async move |cx| {
            while let Ok(command) = receiver.recv().await {
                match command {
                    WheelCommand::PressStarted(generation) => {
                        let state = state_for_events.clone();
                        cx.spawn(async move |cx| {
                            let threshold = {
                                let state = state.lock().unwrap_or_else(|error| error.into_inner());
                                Duration::from_millis(u64::from(state.config.hold_threshold_ms))
                            };
                            cx.background_executor().timer(threshold).await;
                            cx.update(move |cx| {
                                let mut state =
                                    state.lock().unwrap_or_else(|error| error.into_inner());
                                // 期间已松开或已开始新的一次按下：这枚定时器作废。
                                if state.held && state.generation == generation {
                                    state.open_requested =
                                        crate::windows::wheel::open_wheel_window(cx);
                                }
                            });
                        })
                        .detach();
                    }
                    WheelCommand::Commit => {
                        cx.update(|cx| {
                            crate::windows::wheel::commit_wheel_selection(cx);
                        });
                    }
                    WheelCommand::Cancel => {
                        cx.update(|cx| {
                            crate::windows::wheel::close_wheel_window(cx);
                        });
                    }
                }
            }
        })
        .detach();
        Self {
            state,
            subscription: Some(subscription),
        }
    }

    pub(super) fn configure(&self, config: WheelShortcutConfig, available: bool) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.config = config.normalized();
        state.available = available;
        state.held = false;
        state.open_requested = false;
    }
}

impl Drop for WheelMouseRuntime {
    fn drop(&mut self) {
        if let Some(subscription) = self.subscription.take() {
            subscription.unsubscribe();
        }
    }
}

struct WheelHoldState {
    config: WheelShortcutConfig,
    available: bool,
    held: bool,
    open_requested: bool,
    generation: u64,
}

impl WheelHoldState {
    fn new(config: WheelShortcutConfig) -> Self {
        Self {
            config: config.normalized(),
            available: true,
            held: false,
            open_requested: false,
            generation: 0,
        }
    }

    fn trigger_button(&self) -> MouseButton {
        match self.config.button {
            WheelMouseButton::Back => MouseButton::Back,
            WheelMouseButton::Forward => MouseButton::Forward,
        }
    }

    fn handle_event(&mut self, event: Event) -> Option<WheelCommand> {
        match event {
            Event::MouseDown { button } if button == self.trigger_button() => {
                if self.held {
                    return None;
                }
                self.held = true;
                self.open_requested = false;
                self.generation = self.generation.wrapping_add(1);
                if self.available && self.config.enabled {
                    return Some(WheelCommand::PressStarted(self.generation));
                }
                None
            }
            Event::MouseUp { button } if button == self.trigger_button() => {
                let was_held = self.held;
                self.held = false;
                if was_held && self.open_requested {
                    self.open_requested = false;
                    return Some(WheelCommand::Commit);
                }
                Some(WheelCommand::Cancel)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WheelMouseButton;

    fn state(button: WheelMouseButton, enabled: bool) -> WheelHoldState {
        WheelHoldState::new(WheelShortcutConfig {
            enabled,
            button,
            hold_threshold_ms: 150,
        })
    }

    fn down(button: WheelMouseButton) -> Event {
        Event::MouseDown {
            button: match button {
                WheelMouseButton::Back => MouseButton::Back,
                WheelMouseButton::Forward => MouseButton::Forward,
            },
        }
    }

    fn up(button: WheelMouseButton) -> Event {
        Event::MouseUp {
            button: match button {
                WheelMouseButton::Back => MouseButton::Back,
                WheelMouseButton::Forward => MouseButton::Forward,
            },
        }
    }

    #[test]
    fn hold_reaches_threshold_then_commit() {
        let mut state = state(WheelMouseButton::Back, true);
        let Some(WheelCommand::PressStarted(generation)) =
            state.handle_event(down(WheelMouseButton::Back))
        else {
            panic!("按下应触发 PressStarted");
        };
        // 定时器到期时仍按住：允许打开，窗口侧回填 open_requested。
        assert!(state.held && state.generation == generation);
        state.open_requested = true;
        assert_eq!(
            state.handle_event(up(WheelMouseButton::Back)),
            Some(WheelCommand::Commit)
        );
    }

    #[test]
    fn quick_click_never_opens_and_cancels() {
        let mut state = state(WheelMouseButton::Back, true);
        assert!(matches!(
            state.handle_event(down(WheelMouseButton::Back)),
            Some(WheelCommand::PressStarted(_))
        ));
        // 在阈值前松开。
        assert_eq!(
            state.handle_event(up(WheelMouseButton::Back)),
            Some(WheelCommand::Cancel)
        );
        // 迟到的定时器发现已松开，不得打开轮盘。
        assert!(!state.held);
    }

    #[test]
    fn disabled_config_ignores_press() {
        let mut state = state(WheelMouseButton::Back, false);
        assert_eq!(state.handle_event(down(WheelMouseButton::Back)), None);
        assert_eq!(
            state.handle_event(up(WheelMouseButton::Back)),
            Some(WheelCommand::Cancel)
        );
    }

    #[test]
    fn forward_button_uses_forward_events_only() {
        let mut state = state(WheelMouseButton::Forward, true);
        assert_eq!(state.handle_event(down(WheelMouseButton::Back)), None);
        assert!(matches!(
            state.handle_event(down(WheelMouseButton::Forward)),
            Some(WheelCommand::PressStarted(_))
        ));
    }

    #[test]
    fn repeated_down_without_up_is_ignored() {
        let mut state = state(WheelMouseButton::Back, true);
        let Some(WheelCommand::PressStarted(first)) =
            state.handle_event(down(WheelMouseButton::Back))
        else {
            panic!("首次按下应触发 PressStarted");
        };
        assert_eq!(state.handle_event(down(WheelMouseButton::Back)), None);
        assert_eq!(state.generation, first);
    }

    #[test]
    fn stale_generation_timer_is_ignored() {
        let mut state = state(WheelMouseButton::Back, true);
        let Some(WheelCommand::PressStarted(first)) =
            state.handle_event(down(WheelMouseButton::Back))
        else {
            panic!("首次按下应触发 PressStarted");
        };
        assert_eq!(
            state.handle_event(up(WheelMouseButton::Back)),
            Some(WheelCommand::Cancel)
        );
        let Some(WheelCommand::PressStarted(second)) =
            state.handle_event(down(WheelMouseButton::Back))
        else {
            panic!("第二次按下应触发新的 PressStarted");
        };
        assert_ne!(first, second);
        // 旧代数定时器不得打开，新代数允许打开。
        assert!(state.held && state.generation == second);
        assert!(state.held && state.generation != first);
    }
}
