//! CapsLock 修饰键的独立低级键盘钩子与纯状态机。
//!
//! `global-hotkey` 不支持 CapsLock 修饰键，`raw-input` 只监听不能阻断，因此这里
//! 自建 `WH_KEYBOARD_LL` 钩子：回调只做状态判断并 `try_send` 命令，剪贴板 IO 全部
//! 留给后台工作线程。

use std::time::{Duration, Instant};

/// 轻按 CapsLock 的判定窗口；超过后松手不再切换大小写。
pub(crate) const TAP_TIMEOUT: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CapslockSlot {
    Primary,
    Alt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CapslockCommand {
    Copy(CapslockSlot),
    Cut(CapslockSlot),
    Paste(CapslockSlot),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HookKey {
    CapsLock,
    Alt,
    Copy,
    Cut,
    Paste,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HookPhase {
    Down,
    Up,
}

/// 钩子对单个按键事件的处理结果。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HookDecision {
    /// 交给系统继续处理。
    Pass,
    /// 吞掉事件，不产生动作。
    Swallow,
    /// 吞掉事件并合成一次大小写切换。
    ToggleCapsLock,
    /// 吞掉事件并投递剪贴板命令。
    Dispatch(CapslockCommand),
}

#[derive(Debug)]
pub(crate) struct CapslockKeyMachine {
    caps_down: bool,
    caps_down_at: Option<Instant>,
    combo_used: bool,
    alt_down: bool,
    alt_swallowed: bool,
    /// CapsLock 按下之前就已经按住的 C/X/V；其自动重复不应触发新命令。
    preexisting: [bool; 3],
    /// 已经吞下按下事件的 C/X/V，抬起时同样需要吞掉。
    combo_down: [bool; 3],
    primary_enabled: bool,
    alt_enabled: bool,
}

impl Default for CapslockKeyMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl CapslockKeyMachine {
    pub(crate) fn new() -> Self {
        Self {
            caps_down: false,
            caps_down_at: None,
            combo_used: false,
            alt_down: false,
            alt_swallowed: false,
            preexisting: [false; 3],
            combo_down: [false; 3],
            primary_enabled: true,
            alt_enabled: true,
        }
    }

    pub(crate) fn reset(&mut self) {
        let (primary_enabled, alt_enabled) = (self.primary_enabled, self.alt_enabled);
        *self = Self::new();
        self.primary_enabled = primary_enabled;
        self.alt_enabled = alt_enabled;
    }

    /// 同步两个槽位的开关；关闭的槽位组合键会原样放行，但本次 CapsLock 序列
    /// 仍视为已使用，松手不再切换大小写。
    fn configure(&mut self, primary_enabled: bool, alt_enabled: bool) {
        self.primary_enabled = primary_enabled;
        self.alt_enabled = alt_enabled;
    }

    /// `alt_physically_down` 用于兜底“Alt 先于 CapsLock 按下”的场景：
    /// 此时钩子没见过 Alt 按下事件，但取 Alt 槽仍应正确。
    pub(crate) fn handle(
        &mut self,
        key: HookKey,
        phase: HookPhase,
        now: Instant,
        alt_physically_down: bool,
    ) -> HookDecision {
        match (key, phase) {
            (HookKey::CapsLock, HookPhase::Down) => {
                // 长按产生的自动重复不再重置计时，也不改变组合键状态。
                if self.caps_down {
                    return HookDecision::Swallow;
                }
                self.caps_down = true;
                self.caps_down_at = Some(now);
                self.combo_used = false;
                self.alt_down = false;
                self.alt_swallowed = false;
                HookDecision::Swallow
            }
            (HookKey::CapsLock, HookPhase::Up) => {
                if !self.caps_down {
                    return HookDecision::Pass;
                }
                let tapped = !self.combo_used
                    && self
                        .caps_down_at
                        .is_some_and(|start| now.saturating_duration_since(start) <= TAP_TIMEOUT);
                self.reset();
                if tapped {
                    HookDecision::ToggleCapsLock
                } else {
                    HookDecision::Swallow
                }
            }
            (HookKey::Alt, HookPhase::Down) => {
                if !self.caps_down {
                    return HookDecision::Pass;
                }
                // CapsLock 按住期间始终吞掉 Alt：即使 B 槽已关闭，也不能把
                // Alt+C 泄漏给前台触发菜单或快捷键。
                self.alt_down = true;
                self.alt_swallowed = true;
                HookDecision::Swallow
            }
            (HookKey::Alt, HookPhase::Up) => {
                if !self.alt_swallowed {
                    return HookDecision::Pass;
                }
                self.alt_down = false;
                self.alt_swallowed = false;
                HookDecision::Swallow
            }
            (HookKey::Copy | HookKey::Cut | HookKey::Paste, HookPhase::Down) => {
                let index = combo_index(key);
                if !self.caps_down {
                    // CapsLock 按下之前就在按的键，记下来供后续忽略其自动重复。
                    self.preexisting[index] = true;
                    return HookDecision::Pass;
                }
                if self.preexisting[index] {
                    // 按住 CapsLock 前就已按下的键，其自动重复不触发。
                    self.combo_down[index] = true;
                    return HookDecision::Swallow;
                }
                let slot = if self.alt_down || alt_physically_down {
                    CapslockSlot::Alt
                } else {
                    CapslockSlot::Primary
                };
                let enabled = match slot {
                    CapslockSlot::Primary => self.primary_enabled,
                    CapslockSlot::Alt => self.alt_enabled,
                };
                if !enabled {
                    // 该槽位已在设置页关闭：组合键原样交给前台程序，但本次
                    // CapsLock 序列已用于组合键，松手不能再切换大小写。
                    self.combo_used = true;
                    return HookDecision::Pass;
                }
                // 与 capslock-plus 一致：按住不放时每次自动重复都触发一次。
                self.combo_used = true;
                self.combo_down[index] = true;
                HookDecision::Dispatch(match key {
                    HookKey::Copy => CapslockCommand::Copy(slot),
                    HookKey::Cut => CapslockCommand::Cut(slot),
                    HookKey::Paste => CapslockCommand::Paste(slot),
                    _ => unreachable!("组合键分支只处理 C/X/V"),
                })
            }
            (HookKey::Copy | HookKey::Cut | HookKey::Paste, HookPhase::Up) => {
                let index = combo_index(key);
                self.preexisting[index] = false;
                if self.combo_down[index] {
                    self.combo_down[index] = false;
                    HookDecision::Swallow
                } else {
                    HookDecision::Pass
                }
            }
        }
    }
}

fn combo_index(key: HookKey) -> usize {
    match key {
        HookKey::Copy => 0,
        HookKey::Cut => 1,
        HookKey::Paste => 2,
        _ => unreachable!("只有 C/X/V 有组合键槽位"),
    }
}

#[cfg(target_os = "windows")]
mod windows_hook {
    use std::{
        cell::RefCell,
        sync::{
            Mutex,
            atomic::{AtomicBool, AtomicU64, Ordering},
            mpsc::{self, SyncSender},
        },
        thread,
        time::Instant,
    };

    use raw_input::Simulate;
    use windows::Win32::{
        Foundation::{LPARAM, LRESULT, WPARAM},
        System::Threading::GetCurrentThreadId,
        UI::{
            Input::KeyboardAndMouse::{GetAsyncKeyState, VK_MENU},
            WindowsAndMessaging::{
                CallNextHookEx, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG,
                PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SetWindowsHookExW,
                UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN,
                WM_SYSKEYUP,
            },
        },
    };

    use super::{CapslockCommand, CapslockKeyMachine, HookDecision, HookKey, HookPhase};

    static ENABLED: AtomicBool = AtomicBool::new(false);
    static PRIMARY_SLOT_ENABLED: AtomicBool = AtomicBool::new(true);
    static ALT_SLOT_ENABLED: AtomicBool = AtomicBool::new(true);
    /// 钩子回调内只累加计数，不做任何可能落盘的日志。
    static SWALLOWED_ALT_EVENTS: AtomicU64 = AtomicU64::new(0);
    static DROPPED_COMMANDS: AtomicU64 = AtomicU64::new(0);
    static HOOK_LIFECYCLE: Mutex<()> = Mutex::new(());

    thread_local! {
        static HOOK_CONTEXT: RefCell<Option<HookContext>> = const { RefCell::new(None) };
    }

    struct HookContext {
        machine: CapslockKeyMachine,
        sender: SyncSender<CapslockCommand>,
    }

    /// 运行中的 CapsLock 钩子；析构时停止钩子线程。
    pub(crate) struct CapslockHook {
        thread_id: u32,
        handle: Option<thread::JoinHandle<Result<(), String>>>,
    }

    impl CapslockHook {
        pub(crate) fn install(sender: SyncSender<CapslockCommand>) -> Result<Self, String> {
            let _lifecycle = match HOOK_LIFECYCLE.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            let (ready_sender, ready_receiver) = mpsc::channel::<Result<u32, String>>();
            let handle = thread::Builder::new()
                .name("qcclip-capslock-hook".to_owned())
                .spawn(move || {
                    let mut message = MSG::default();
                    // SAFETY: 预先创建线程消息队列，确保之后能收到 PostThreadMessageW 的 WM_QUIT。
                    unsafe {
                        let _ = PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE);
                    }
                    HOOK_CONTEXT.with(|context| {
                        *context.borrow_mut() = Some(HookContext {
                            machine: CapslockKeyMachine::new(),
                            sender,
                        });
                    });
                    // SAFETY: 钩子回调为静态函数，只处理键盘事件；线程消息循环负责驱动回调。
                    let hook = match unsafe {
                        SetWindowsHookExW(WH_KEYBOARD_LL, Some(capslock_hook), None, 0)
                    } {
                        Ok(hook) => hook,
                        Err(error) => {
                            let error = format!("安装 CapsLock 键盘钩子失败：{error}");
                            let _ = ready_sender.send(Err(error.clone()));
                            return Err(error);
                        }
                    };
                    ENABLED.store(true, Ordering::SeqCst);
                    // SAFETY: 仅查询当前线程 ID。
                    let _ = ready_sender.send(Ok(unsafe { GetCurrentThreadId() }));

                    // SAFETY: message 为本线程栈上的有效 MSG。
                    while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {}

                    ENABLED.store(false, Ordering::SeqCst);
                    // SAFETY: hook 由本线程创建，且只在此处卸载一次。
                    unsafe { UnhookWindowsHookEx(hook) }
                        .map_err(|error| format!("卸载 CapsLock 键盘钩子失败：{error}"))
                })
                .map_err(|error| format!("无法启动 CapsLock 键盘钩子线程：{error}"))?;

            match ready_receiver.recv() {
                Ok(Ok(thread_id)) => Ok(Self {
                    thread_id,
                    handle: Some(handle),
                }),
                Ok(Err(error)) => {
                    let _ = handle.join();
                    Err(error)
                }
                Err(_) => {
                    let _ = handle.join();
                    Err("CapsLock 键盘钩子线程启动前异常退出".to_owned())
                }
            }
        }
    }

    impl Drop for CapslockHook {
        fn drop(&mut self) {
            ENABLED.store(false, Ordering::SeqCst);
            // SAFETY: thread_id 来自仍在运行的钩子线程，且该线程已创建消息队列。
            if let Err(error) =
                unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) }
            {
                tracing::warn!(%error, "通知 CapsLock 钩子线程退出失败");
            }
            if let Some(handle) = self.handle.take()
                && handle.join().is_err()
            {
                tracing::error!("CapsLock 钩子线程异常退出");
            }
            // 钩子回调内不写日志，退出时统一汇报计数，便于排查“偶发不响应”。
            let dropped = DROPPED_COMMANDS.swap(0, Ordering::Relaxed);
            let swallowed_alt = SWALLOWED_ALT_EVENTS.swap(0, Ordering::Relaxed);
            if dropped > 0 || swallowed_alt > 0 {
                tracing::debug!(
                    dropped_commands = dropped,
                    swallowed_alt_events = swallowed_alt,
                    "CapsLock 钩子统计"
                );
            }
        }
    }

    /// 暂停或恢复 CapsLock 组合键处理，不销毁钩子线程。
    pub(crate) fn set_enabled(enabled: bool) {
        ENABLED.store(enabled, Ordering::SeqCst);
    }

    /// 独立开关两套剪贴板；关闭的组合键会原样放行给系统。
    pub(crate) fn set_slots_enabled(primary: bool, alt: bool) {
        PRIMARY_SLOT_ENABLED.store(primary, Ordering::SeqCst);
        ALT_SLOT_ENABLED.store(alt, Ordering::SeqCst);
    }

    unsafe extern "system" fn capslock_hook(code: i32, message: WPARAM, data: LPARAM) -> LRESULT {
        if code != HC_ACTION as i32 || data.0 == 0 {
            return unsafe { CallNextHookEx(None, code, message, data) };
        }
        // SAFETY: WH_KEYBOARD_LL 在 HC_ACTION 时提供有效的 KBDLLHOOKSTRUCT 指针。
        let event = unsafe { &*(data.0 as *const KBDLLHOOKSTRUCT) };
        // 自身合成的大小写切换等注入事件必须原样放行，否则会形成递归。
        if event.flags.0 & LLKHF_INJECTED.0 != 0 {
            return unsafe { CallNextHookEx(None, code, message, data) };
        }
        let phase = match message.0 as u32 {
            WM_KEYDOWN | WM_SYSKEYDOWN => HookPhase::Down,
            WM_KEYUP | WM_SYSKEYUP => HookPhase::Up,
            _ => return unsafe { CallNextHookEx(None, code, message, data) },
        };
        let Some(key) = hook_key(event.vkCode) else {
            return unsafe { CallNextHookEx(None, code, message, data) };
        };

        if !ENABLED.load(Ordering::SeqCst) {
            HOOK_CONTEXT.with(|context| {
                if let Some(context) = context.borrow_mut().as_mut() {
                    context.machine.reset();
                }
            });
            return unsafe { CallNextHookEx(None, code, message, data) };
        }

        let alt_physically_down = matches!(key, HookKey::Copy | HookKey::Cut | HookKey::Paste)
            && is_alt_physically_down();
        let (decision, sender) = HOOK_CONTEXT.with(|context| {
            let mut context = context.borrow_mut();
            let Some(context) = context.as_mut() else {
                return (HookDecision::Pass, None);
            };
            context.machine.configure(
                PRIMARY_SLOT_ENABLED.load(Ordering::Relaxed),
                ALT_SLOT_ENABLED.load(Ordering::Relaxed),
            );
            let decision = context
                .machine
                .handle(key, phase, Instant::now(), alt_physically_down);
            let sender =
                matches!(decision, HookDecision::Dispatch(_)).then(|| context.sender.clone());
            (decision, sender)
        });

        match decision {
            HookDecision::Pass => unsafe { CallNextHookEx(None, code, message, data) },
            HookDecision::Swallow => {
                if matches!(key, HookKey::Alt) {
                    SWALLOWED_ALT_EVENTS.fetch_add(1, Ordering::Relaxed);
                }
                LRESULT(1)
            }
            HookDecision::Dispatch(command) => {
                if let Some(sender) = sender {
                    // 钩子回调不能阻塞：队列满或接收端退出时直接丢弃该次命令。
                    if sender.try_send(command).is_err() {
                        DROPPED_COMMANDS.fetch_add(1, Ordering::Relaxed);
                    }
                }
                LRESULT(1)
            }
            HookDecision::ToggleCapsLock => {
                ctx_toggle_caps_lock();
                LRESULT(1)
            }
        }
    }

    fn ctx_toggle_caps_lock() {
        Simulate::keyboard(raw_input::Key::CapsLock, true);
        Simulate::keyboard(raw_input::Key::CapsLock, false);
    }

    fn hook_key(vk_code: u32) -> Option<HookKey> {
        Some(match vk_code {
            0x14 => HookKey::CapsLock,
            0x12 | 0xA4 | 0xA5 => HookKey::Alt,
            0x43 => HookKey::Copy,
            0x58 => HookKey::Cut,
            0x56 => HookKey::Paste,
            _ => return None,
        })
    }

    fn is_alt_physically_down() -> bool {
        // SAFETY: GetAsyncKeyState 不需要指针，仅查询虚拟键状态。
        (unsafe { GetAsyncKeyState(VK_MENU.0 as i32) } as u16 & 0x8000) != 0
    }
}

#[cfg(target_os = "windows")]
pub(crate) use windows_hook::{CapslockHook, set_enabled, set_slots_enabled};

#[cfg(not(target_os = "windows"))]
pub(crate) use unsupported_hook::{CapslockHook, set_enabled, set_slots_enabled};

#[cfg(not(target_os = "windows"))]
mod unsupported_hook {
    use std::sync::mpsc::SyncSender;

    use super::CapslockCommand;

    pub(crate) struct CapslockHook;

    impl CapslockHook {
        pub(crate) fn install(_sender: SyncSender<CapslockCommand>) -> Result<Self, String> {
            Err("当前平台暂不支持 CapsLock 组合键".to_owned())
        }
    }

    pub(crate) fn set_enabled(_enabled: bool) {}

    pub(crate) fn set_slots_enabled(_primary: bool, _alt: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, millis: u64) -> Instant {
        base + Duration::from_millis(millis)
    }

    #[test]
    fn short_tap_toggles_caps_lock() {
        let mut machine = CapslockKeyMachine::new();
        let base = Instant::now();
        assert_eq!(
            machine.handle(HookKey::CapsLock, HookPhase::Down, base, false),
            HookDecision::Swallow
        );
        assert_eq!(
            machine.handle(HookKey::CapsLock, HookPhase::Up, at(base, 120), false),
            HookDecision::ToggleCapsLock
        );
    }

    #[test]
    fn long_press_does_not_toggle() {
        let mut machine = CapslockKeyMachine::new();
        let base = Instant::now();
        machine.handle(HookKey::CapsLock, HookPhase::Down, base, false);
        assert_eq!(
            machine.handle(
                HookKey::CapsLock,
                HookPhase::Up,
                at(base, TAP_TIMEOUT.as_millis() as u64 + 1),
                false
            ),
            HookDecision::Swallow
        );
    }

    #[test]
    fn used_combination_suppresses_toggle_and_repeats_dispatch() {
        let mut machine = CapslockKeyMachine::new();
        let base = Instant::now();
        machine.handle(HookKey::CapsLock, HookPhase::Down, base, false);
        assert_eq!(
            machine.handle(HookKey::Copy, HookPhase::Down, at(base, 40), false),
            HookDecision::Dispatch(CapslockCommand::Copy(CapslockSlot::Primary))
        );
        // 按住不放时每次自动重复都触发（与 capslock-plus 一致）。
        assert_eq!(
            machine.handle(HookKey::Copy, HookPhase::Down, at(base, 90), false),
            HookDecision::Dispatch(CapslockCommand::Copy(CapslockSlot::Primary))
        );
        assert_eq!(
            machine.handle(HookKey::Copy, HookPhase::Up, at(base, 100), false),
            HookDecision::Swallow
        );
        // 使用过组合键后松手不切换大小写。
        assert_eq!(
            machine.handle(HookKey::CapsLock, HookPhase::Up, at(base, 150), false),
            HookDecision::Swallow
        );
    }

    #[test]
    fn keys_held_before_caps_lock_do_not_dispatch_repeats() {
        let mut machine = CapslockKeyMachine::new();
        let base = Instant::now();
        // CapsLock 按下之前 C 已被按住（事件被放行给系统）。
        assert_eq!(
            machine.handle(HookKey::Copy, HookPhase::Down, base, false),
            HookDecision::Pass
        );
        machine.handle(HookKey::CapsLock, HookPhase::Down, at(base, 10), false);
        // 该键的自动重复不应触发新命令。
        assert_eq!(
            machine.handle(HookKey::Copy, HookPhase::Down, at(base, 20), false),
            HookDecision::Swallow
        );
        assert_eq!(
            machine.handle(HookKey::Copy, HookPhase::Up, at(base, 30), false),
            HookDecision::Swallow
        );
        // 预先按住的 C 不算"使用过组合键"，快速轻按 CapsLock 仍切换大小写。
        assert_eq!(
            machine.handle(HookKey::CapsLock, HookPhase::Up, at(base, 40), false),
            HookDecision::ToggleCapsLock
        );
        // 松开 C 后再按，应正常触发。
        machine.handle(HookKey::CapsLock, HookPhase::Down, at(base, 50), false);
        assert_eq!(
            machine.handle(HookKey::Copy, HookPhase::Down, at(base, 60), false),
            HookDecision::Dispatch(CapslockCommand::Copy(CapslockSlot::Primary))
        );
    }

    #[test]
    fn alt_combination_uses_alt_slot_and_swallows_alt() {
        let mut machine = CapslockKeyMachine::new();
        let base = Instant::now();
        machine.handle(HookKey::CapsLock, HookPhase::Down, base, false);
        assert_eq!(
            machine.handle(HookKey::Alt, HookPhase::Down, at(base, 10), false),
            HookDecision::Swallow
        );
        assert_eq!(
            machine.handle(HookKey::Cut, HookPhase::Down, at(base, 20), false),
            HookDecision::Dispatch(CapslockCommand::Cut(CapslockSlot::Alt))
        );
        assert_eq!(
            machine.handle(HookKey::Alt, HookPhase::Up, at(base, 30), false),
            HookDecision::Swallow
        );
        assert_eq!(
            machine.handle(HookKey::CapsLock, HookPhase::Up, at(base, 40), false),
            HookDecision::Swallow
        );
    }

    #[test]
    fn alt_pressed_before_caps_lock_still_uses_alt_slot() {
        let mut machine = CapslockKeyMachine::new();
        let base = Instant::now();
        machine.handle(HookKey::CapsLock, HookPhase::Down, base, false);
        let decision = machine.handle(HookKey::Paste, HookPhase::Down, at(base, 10), true);
        assert_eq!(
            decision,
            HookDecision::Dispatch(CapslockCommand::Paste(CapslockSlot::Alt))
        );
    }

    #[test]
    fn keys_without_caps_lock_pass_through() {
        let mut machine = CapslockKeyMachine::new();
        let base = Instant::now();
        assert_eq!(
            machine.handle(HookKey::Copy, HookPhase::Down, base, false),
            HookDecision::Pass
        );
        assert_eq!(
            machine.handle(HookKey::Alt, HookPhase::Down, base, false),
            HookDecision::Pass
        );
        assert_eq!(
            machine.handle(HookKey::CapsLock, HookPhase::Up, base, false),
            HookDecision::Pass
        );
    }

    #[test]
    fn caps_lock_repeat_keeps_tap_start_time() {
        let mut machine = CapslockKeyMachine::new();
        let base = Instant::now();
        machine.handle(HookKey::CapsLock, HookPhase::Down, base, false);
        // 自动重复的按下事件不重置起始时间。
        machine.handle(HookKey::CapsLock, HookPhase::Down, at(base, 250), false);
        assert_eq!(
            machine.handle(HookKey::CapsLock, HookPhase::Up, at(base, 260), false),
            HookDecision::ToggleCapsLock
        );
    }

    #[test]
    fn reset_clears_partial_state() {
        let mut machine = CapslockKeyMachine::new();
        let base = Instant::now();
        machine.handle(HookKey::CapsLock, HookPhase::Down, base, false);
        machine.handle(HookKey::Copy, HookPhase::Down, at(base, 10), false);
        machine.reset();
        assert_eq!(
            machine.handle(HookKey::CapsLock, HookPhase::Up, at(base, 20), false),
            HookDecision::Pass
        );
        // 重置后 C 抬起事件不再拦截。
        let mut machine = CapslockKeyMachine::new();
        machine.handle(HookKey::CapsLock, HookPhase::Down, base, false);
        machine.handle(HookKey::Copy, HookPhase::Down, at(base, 10), false);
        machine.reset();
        assert_eq!(
            machine.handle(HookKey::Copy, HookPhase::Up, at(base, 20), false),
            HookDecision::Pass
        );
    }

    #[test]
    fn releasing_alt_mid_repeat_routes_back_to_primary() {
        let mut machine = CapslockKeyMachine::new();
        let base = Instant::now();
        machine.handle(HookKey::CapsLock, HookPhase::Down, base, false);
        machine.handle(HookKey::Alt, HookPhase::Down, at(base, 10), false);
        assert_eq!(
            machine.handle(HookKey::Paste, HookPhase::Down, at(base, 20), false),
            HookDecision::Dispatch(CapslockCommand::Paste(CapslockSlot::Alt))
        );
        machine.handle(HookKey::Alt, HookPhase::Up, at(base, 30), false);
        // Alt 松开后继续按住 V，重复事件回到 A 槽。
        assert_eq!(
            machine.handle(HookKey::Paste, HookPhase::Down, at(base, 40), false),
            HookDecision::Dispatch(CapslockCommand::Paste(CapslockSlot::Primary))
        );
    }

    #[test]
    fn disabled_primary_slot_passes_keys_without_toggling() {
        let mut machine = CapslockKeyMachine::new();
        machine.configure(false, true);
        let base = Instant::now();
        machine.handle(HookKey::CapsLock, HookPhase::Down, base, false);
        assert_eq!(
            machine.handle(HookKey::Copy, HookPhase::Down, at(base, 20), false),
            HookDecision::Pass
        );
        assert_eq!(
            machine.handle(HookKey::Copy, HookPhase::Up, at(base, 30), false),
            HookDecision::Pass
        );
        // 槽位关闭也要把本次序列标记为已使用，松手不切换大小写。
        assert_eq!(
            machine.handle(HookKey::CapsLock, HookPhase::Up, at(base, 40), false),
            HookDecision::Swallow
        );
    }

    #[test]
    fn disabled_alt_slot_swallows_alt_without_toggling() {
        let mut machine = CapslockKeyMachine::new();
        machine.configure(true, false);
        let base = Instant::now();
        machine.handle(HookKey::CapsLock, HookPhase::Down, base, false);
        // Alt 仍被吞掉，避免前台看到 Alt+C 弹出菜单。
        assert_eq!(
            machine.handle(HookKey::Alt, HookPhase::Down, at(base, 10), false),
            HookDecision::Swallow
        );
        assert_eq!(
            machine.handle(HookKey::Copy, HookPhase::Down, at(base, 20), false),
            HookDecision::Pass
        );
        assert_eq!(
            machine.handle(HookKey::Alt, HookPhase::Up, at(base, 30), false),
            HookDecision::Swallow
        );
        assert_eq!(
            machine.handle(HookKey::CapsLock, HookPhase::Up, at(base, 40), false),
            HookDecision::Swallow
        );
    }

    #[test]
    fn alt_down_before_caps_lock_tap_does_not_block_toggle() {
        let mut machine = CapslockKeyMachine::new();
        let base = Instant::now();
        // Alt 单独按下（放行），轻按 CapsLock 仍应切换大小写。
        machine.handle(HookKey::Alt, HookPhase::Down, base, false);
        machine.handle(HookKey::Alt, HookPhase::Up, at(base, 5), false);
        machine.handle(HookKey::CapsLock, HookPhase::Down, at(base, 10), false);
        assert_eq!(
            machine.handle(HookKey::CapsLock, HookPhase::Up, at(base, 60), false),
            HookDecision::ToggleCapsLock
        );
    }
}
