#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PointerButtons {
    pub(crate) left: bool,
    pub(crate) right: bool,
    pub(crate) middle: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PointerSnapshot {
    pub(crate) position: gpui::Point<f32>,
    pub(crate) buttons: PointerButtons,
    pub(crate) mouse_move_sequence: u64,
}

pub(crate) struct PointerRegionWatch {
    region: Option<gpui::Bounds<f32>>,
    was_inside: bool,
}

impl PointerRegionWatch {
    pub(crate) fn new(
        region: Option<gpui::Bounds<f32>>,
        position: Option<gpui::Point<f32>>,
    ) -> Self {
        Self {
            region,
            was_inside: region
                .zip(position)
                .is_some_and(|(region, position)| Self::contains(region, position)),
        }
    }

    pub(crate) fn contains(region: gpui::Bounds<f32>, position: gpui::Point<f32>) -> bool {
        position.x >= region.origin.x
            && position.x < region.origin.x + region.size.width
            && position.y >= region.origin.y
            && position.y < region.origin.y + region.size.height
    }

    pub(crate) fn should_notify(&mut self, position: gpui::Point<f32>) -> bool {
        let Some(region) = self.region else {
            return true;
        };
        let inside = Self::contains(region, position);
        let changed = inside != self.was_inside;
        self.was_inside = inside;
        changed
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum GlobalMouseEvent {
    MouseDown {
        sequence: u64,
        position: gpui::Point<f32>,
        foreground_window: Option<isize>,
    },
    MouseUp {
        position: gpui::Point<f32>,
        left_pressed: bool,
    },
}

/// 轮盘打开期间监听左/右键点击用于翻页的句柄；丢弃即取消监听。
pub(crate) struct WheelPageListener {
    _subscription: Option<raw_input::SubscriptionHandle>,
}

/// 轮盘打开期间吞掉左/右键点击的底层拦截；丢弃即解除拦截。
pub(crate) struct WheelClickSwallowGuard {
    _active: bool,
}

static WHEEL_SWALLOW_CLICKS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

impl WheelClickSwallowGuard {
    /// 安装 WH_MOUSE_LL 钩子：轮盘存续期间左/右键按下不 reaching 前台窗口。
    /// 返回 None 表示钩子安装失败（功能降级为只翻页不拦截）。
    pub(crate) fn install() -> Option<Self> {
        #[cfg(target_os = "windows")]
        {
            if !WHEEL_SWALLOW_CLICKS.swap(true, std::sync::atomic::Ordering::SeqCst) {
                windows_input::install_wheel_click_hook();
            }
            Some(Self { _active: true })
        }
        #[cfg(not(target_os = "windows"))]
        {
            None
        }
    }
}

impl Drop for WheelClickSwallowGuard {
    fn drop(&mut self) {
        #[cfg(target_os = "windows")]
        {
            if WHEEL_SWALLOW_CLICKS.swap(false, std::sync::atomic::Ordering::SeqCst) {
                windows_input::remove_wheel_click_hook();
            }
        }
    }
}

impl WheelPageListener {
    /// 订阅原始输入的左/右键按下事件（true=左键，false=右键）。
    /// 只负责转发事件，消费逻辑由调用方决定。
    pub(crate) fn install() -> (Self, async_channel::Receiver<bool>) {
        #[cfg(target_os = "windows")]
        {
            let (sender, receiver) = async_channel::unbounded::<bool>();
            let subscription = raw_input::Listen::subscribe(move |event| match event {
                raw_input::Event::MouseDown { button } => match button {
                    raw_input::MouseButton::Left => {
                        let _ = sender.try_send(true);
                    }
                    raw_input::MouseButton::Right => {
                        let _ = sender.try_send(false);
                    }
                    _ => {}
                },
                _ => {}
            });
            (
                Self {
                    _subscription: Some(subscription),
                },
                receiver,
            )
        }
        #[cfg(not(target_os = "windows"))]
        {
            let (_sender, receiver) = async_channel::unbounded::<bool>();
            (
                Self {
                    _subscription: None,
                },
                receiver,
            )
        }
    }
}

#[cfg(target_os = "windows")]
mod windows_input {
    use std::{
        sync::{
            Arc, Mutex, OnceLock,
            atomic::{AtomicIsize, Ordering},
        },
        thread,
        time::Duration,
    };

    use raw_input::{Display, Event, Listen, MouseButton, Simulate, SubscriptionHandle};
    use windows::Win32::{
        Foundation::{LPARAM, LRESULT, WPARAM},
        UI::{
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_RCONTROL, VK_RMENU,
                VK_RSHIFT, VK_RWIN,
            },
            WindowsAndMessaging::{
                CallNextHookEx, GetForegroundWindow, GetMessageW, HHOOK, HOOKPROC, LLMHF_INJECTED,
                MSLLHOOKSTRUCT, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx,
                WH_MOUSE_LL, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP,
                WM_XBUTTONDOWN, WM_XBUTTONUP,
            },
        },
    };

    use super::{GlobalMouseEvent, PointerButtons, PointerRegionWatch, PointerSnapshot};

    static INPUT_STATE: OnceLock<Arc<Mutex<InputState>>> = OnceLock::new();
    static CURRENT_TRIGGER_KEY: Mutex<Option<TriggerKey>> = Mutex::new(None);
    static PASTE_SIMULATION_LOCK: Mutex<()> = Mutex::new(());
    const PHYSICAL_KEY_COUNT: usize = raw_input::Key::Unidentified as usize + 1;
    static PHYSICAL_KEY_STATE: OnceLock<Mutex<PhysicalKeySnapshot>> = OnceLock::new();

    // ---- 轮盘打开期间的左/右键点击拦截（WH_MOUSE_LL）----
    static WHEEL_SWALLOW_ACTIVE: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    static WHEEL_MOUSE_HOOK: AtomicIsize = AtomicIsize::new(0);
    static WHEEL_HOOK_THREAD_ID: AtomicIsize = AtomicIsize::new(0);

    /// 轮盘存续期间吞掉左/右键（含 X 键）的真实点击；注入输入放行以防自锁。
    unsafe extern "system" fn wheel_mouse_hook_proc(
        code: i32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if code >= 0
            && WHEEL_SWALLOW_ACTIVE.load(Ordering::Acquire)
            && matches!(
                wparam.0 as u32,
                WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP
            )
        {
            let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
            // LLMHF_INJECTED 标记来自 SendInput 的合成输入，放行避免吞掉自己模拟的点击。
            if info.flags & LLMHF_INJECTED == 0 {
                return LRESULT(1);
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }

    pub(crate) fn install_wheel_click_hook() {
        // 低级钩子必须安装在带消息循环的线程上；复用独立线程运行隐藏消息循环。
        if WHEEL_MOUSE_HOOK.load(Ordering::Acquire) != 0 {
            return;
        }
        let _ = thread::Builder::new()
            .name("qcclip-wheel-hook".to_owned())
            .spawn(|| {
                WHEEL_HOOK_THREAD_ID.store(
                    unsafe { windows::Win32::System::Threading::GetCurrentThreadId() } as isize,
                    Ordering::Release,
                );
                unsafe {
                    // SAFETY: 钩子过程为静态函数，生命周期覆盖整个进程。
                    match SetWindowsHookExW(WH_MOUSE_LL, Some(wheel_mouse_hook_proc), None, 0) {
                        Ok(hook) => {
                            WHEEL_MOUSE_HOOK.store(hook.0 as isize, Ordering::Release);
                            // 钩子线程需要消息循环才能接收回调。
                            let mut msg = windows::Win32::UI::WindowsAndMessaging::MSG::default();
                            while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
                        }
                        Err(error) => {
                            tracing::error!("安装轮盘鼠标钩子失败：{error}");
                        }
                    }
                }
            });
        // 等钩子线程装好再返回，避免第一下点击漏掉。
        for _ in 0..50 {
            if WHEEL_MOUSE_HOOK.load(Ordering::Acquire) != 0 {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        WHEEL_SWALLOW_ACTIVE.store(true, Ordering::Release);
    }

    pub(crate) fn remove_wheel_click_hook() {
        WHEEL_SWALLOW_ACTIVE.store(false, Ordering::Release);
        let hook = WHEEL_MOUSE_HOOK.swap(0, Ordering::Acquire);
        if hook != 0 {
            unsafe {
                // SAFETY: 钩子句柄来自成功的 SetWindowsHookExW。
                let _ = UnhookWindowsHookEx(HHOOK(hook as *mut core::ffi::c_void));
            }
            // 唤醒钩子线程的消息循环让它退出：向自身线程投递 WM_QUIT。
            let thread_id = WHEEL_HOOK_THREAD_ID.swap(0, Ordering::Acquire);
            if thread_id != 0 {
                unsafe {
                    let _ = PostThreadMessageW(thread_id as u32, WM_QUIT, WPARAM(0), LPARAM(0));
                }
            }
        }
    }

    #[derive(Clone, Copy)]
    struct TriggerKey {
        key: raw_input::Key,
    }

    #[derive(Clone, Copy)]
    struct PhysicalKeySnapshot {
        pressed: [bool; PHYSICAL_KEY_COUNT],
        releases: [u64; PHYSICAL_KEY_COUNT],
    }

    impl Default for PhysicalKeySnapshot {
        fn default() -> Self {
            Self {
                pressed: [false; PHYSICAL_KEY_COUNT],
                releases: [0; PHYSICAL_KEY_COUNT],
            }
        }
    }

    struct InputState {
        snapshot: Option<PointerSnapshot>,
        mouse_move_sequence: u64,
        mouse_down_sequence: u64,
        mouse_event_senders: Vec<(u64, async_channel::Sender<GlobalMouseEvent>)>,
        pointer_change_sender: Option<async_channel::Sender<()>>,
        pointer_region_watch: PointerRegionWatch,
        next_mouse_event_subscription: u64,
    }

    impl InputState {
        fn new() -> Self {
            Self {
                snapshot: cursor_position().map(|position| PointerSnapshot {
                    position,
                    buttons: PointerButtons::default(),
                    mouse_move_sequence: 0,
                }),
                mouse_move_sequence: 0,
                mouse_down_sequence: 0,
                mouse_event_senders: Vec::new(),
                pointer_change_sender: None,
                pointer_region_watch: PointerRegionWatch::new(None, None),
                next_mouse_event_subscription: 0,
            }
        }
    }

    pub(crate) fn start() -> Result<(), String> {
        if INPUT_STATE.get().is_some() {
            return Ok(());
        }

        let state = Arc::new(Mutex::new(InputState::new()));
        INPUT_STATE
            .set(state.clone())
            .map_err(|_| "全局鼠标监听状态已初始化".to_owned())?;

        let _subscription: SubscriptionHandle = Listen::subscribe(move |event| {
            handle_event(&state, event);
        });

        // 只监听输入，不拦截或阻断输入。
        Listen::start();
        Listen::mouse_move(true);
        Listen::mouse_button(true);
        Listen::mouse_wheel(false);
        Listen::keyboard(true);

        let result = thread::Builder::new()
            .name("qcclip-raw-input".to_owned())
            .spawn(|| match raw_input::Core::start() {
                Ok(()) => tracing::debug!("全局鼠标事件处理线程已退出"),
                Err(error) => tracing::error!("全局鼠标事件处理线程启动失败：{error:?}"),
            })
            .map(|_| ())
            .map_err(|error| format!("无法启动全局鼠标事件线程：{error}"));
        result
    }

    fn physical_key_state() -> &'static Mutex<PhysicalKeySnapshot> {
        PHYSICAL_KEY_STATE.get_or_init(|| Mutex::new(PhysicalKeySnapshot::default()))
    }

    fn update_physical_key(key: raw_input::Key, pressed: bool) {
        let index = key as usize;
        if index < PHYSICAL_KEY_COUNT {
            let mut state = match physical_key_state().lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            state.pressed[index] = pressed;
            if !pressed {
                state.releases[index] = state.releases[index].wrapping_add(1);
            }
        }
    }

    fn is_physical_key_pressed(key: raw_input::Key) -> bool {
        let index = key as usize;
        match physical_key_state().lock() {
            Ok(state) => state.pressed[index],
            Err(poisoned) => poisoned.into_inner().pressed[index],
        }
    }

    fn modifier_is_physically_pressed(key: raw_input::Key) -> bool {
        let virtual_key = match key {
            raw_input::Key::ControlLeft => VK_LCONTROL,
            raw_input::Key::ControlRight => VK_RCONTROL,
            raw_input::Key::AltLeft => VK_LMENU,
            raw_input::Key::AltRight => VK_RMENU,
            raw_input::Key::ShiftLeft => VK_LSHIFT,
            raw_input::Key::ShiftRight => VK_RSHIFT,
            raw_input::Key::MetaLeft => VK_LWIN,
            raw_input::Key::MetaRight => VK_RWIN,
            _ => return is_physical_key_pressed(key),
        };
        // GetAsyncKeyState 的高位表示当前按下状态，不依赖 Raw Input 是否漏掉了边沿。
        unsafe { GetAsyncKeyState(virtual_key.0 as i32) < 0 }
    }

    pub(crate) fn subscribe_mouse_events() -> (u64, async_channel::Receiver<GlobalMouseEvent>) {
        let (sender, receiver) = async_channel::unbounded();
        let state = INPUT_STATE
            .get()
            .expect("全局鼠标监听状态尚未初始化")
            .clone();
        let mut state = lock_state(&state);
        let subscription_id = state.next_mouse_event_subscription;
        state.next_mouse_event_subscription = state.next_mouse_event_subscription.wrapping_add(1);
        state.mouse_event_senders.push((subscription_id, sender));
        (subscription_id, receiver)
    }

    pub(crate) fn unsubscribe_mouse_events(subscription_id: u64) {
        if let Some(state) = INPUT_STATE.get() {
            lock_state(state)
                .mouse_event_senders
                .retain(|(id, _)| *id != subscription_id);
        }
    }

    pub(crate) fn set_pointer_change_notifier(
        sender: Option<async_channel::Sender<()>>,
        region: Option<gpui::Bounds<f32>>,
    ) {
        if let Some(state) = INPUT_STATE.get() {
            let mut state = lock_state(state);
            state.pointer_change_sender = sender;
            state.pointer_region_watch = PointerRegionWatch::new(region, cursor_position());
        }
    }

    pub(crate) fn pointer_snapshot() -> Option<PointerSnapshot> {
        // 虚拟机、远程桌面等设备可能只上报绝对坐标移动，不能只依赖 Raw Input 的相对移动事件。
        let position = cursor_position();
        let state = INPUT_STATE.get()?.clone();
        let mut state = lock_state(&state);
        if let Some(position) = position {
            if let Some(snapshot) = state.snapshot.as_mut() {
                snapshot.position = position;
            } else {
                state.snapshot = Some(PointerSnapshot {
                    position,
                    buttons: PointerButtons::default(),
                    mouse_move_sequence: state.mouse_move_sequence,
                });
            }
        }
        state.snapshot
    }

    pub(crate) fn latest_mouse_down_sequence() -> Option<u64> {
        let state = INPUT_STATE.get()?.clone();
        let sequence = lock_state(&state).mouse_down_sequence;
        (sequence != 0).then_some(sequence)
    }

    fn handle_event(state: &Arc<Mutex<InputState>>, event: Event) {
        match event {
            Event::KeyDown { key, .. } => {
                update_physical_key(key, true);
                return;
            }
            Event::KeyUp { key, .. } => {
                update_physical_key(key, false);
                return;
            }
            _ => {}
        }

        let mut global_event = None;
        let senders;
        {
            let mut state = lock_state(state);
            if matches!(event, Event::MouseMove { .. }) {
                state.mouse_move_sequence = state.mouse_move_sequence.wrapping_add(1);
                let sequence = state.mouse_move_sequence;
                if let Some(snapshot) = state.snapshot.as_mut() {
                    snapshot.mouse_move_sequence = sequence;
                }
                // 普通隐藏时只维护移动序号，不读取坐标或唤醒界面。
                if state.pointer_change_sender.is_none() {
                    return;
                }
            }
            let position = match event {
                Event::MouseMove { .. } | Event::MouseDown { .. } | Event::MouseUp { .. } => {
                    cursor_position()
                }
                Event::MouseWheel { .. } | Event::KeyDown { .. } | Event::KeyUp { .. } => None,
            };
            if let Some(position) = position {
                if let Some(snapshot) = state.snapshot.as_mut() {
                    snapshot.position = position;
                } else {
                    state.snapshot = Some(PointerSnapshot {
                        position,
                        buttons: PointerButtons::default(),
                        mouse_move_sequence: state.mouse_move_sequence,
                    });
                }
            }

            match event {
                Event::MouseMove { .. } => {}
                Event::MouseDown { button } => {
                    state.mouse_down_sequence = state.mouse_down_sequence.wrapping_add(1);
                    let sequence = state.mouse_down_sequence;
                    let Some(snapshot) = state.snapshot.as_mut() else {
                        return;
                    };
                    set_button(&mut snapshot.buttons, button, true);
                    let event = GlobalMouseEvent::MouseDown {
                        sequence,
                        position: snapshot.position,
                        foreground_window: current_foreground_window(),
                    };
                    global_event = Some(event);
                }
                Event::MouseUp { button } => {
                    let Some(snapshot) = state.snapshot.as_mut() else {
                        return;
                    };
                    set_button(&mut snapshot.buttons, button, false);
                }
                Event::MouseWheel { .. } | Event::KeyDown { .. } | Event::KeyUp { .. } => return,
            }
            global_event = match event {
                Event::MouseUp { .. } => state.snapshot.map(|snapshot| GlobalMouseEvent::MouseUp {
                    position: snapshot.position,
                    left_pressed: snapshot.buttons.left,
                }),
                _ => global_event,
            };
            senders = if global_event.is_some() {
                state.mouse_event_senders.clone()
            } else {
                Vec::new()
            };
            let should_notify =
                position.is_some_and(|position| state.pointer_region_watch.should_notify(position));
            if should_notify && let Some(sender) = &state.pointer_change_sender {
                // 通道容量为 1，合并高频移动，仅在输入变化时唤醒窗口检查。
                let _ = sender.try_send(());
            }
        }

        if let Some(event) = global_event {
            for (_, sender) in senders {
                let _ = sender.try_send(event);
            }
        }
    }

    fn set_button(buttons: &mut PointerButtons, button: MouseButton, pressed: bool) {
        match button {
            MouseButton::Left => buttons.left = pressed,
            MouseButton::Right => buttons.right = pressed,
            MouseButton::Middle => buttons.middle = pressed,
            MouseButton::Back | MouseButton::Forward => {}
        }
    }

    fn cursor_position() -> Option<gpui::Point<f32>> {
        Display::get_cursor_position().map(|(x, y)| gpui::Point {
            x: x as f32,
            y: y as f32,
        })
    }

    fn current_foreground_window() -> Option<isize> {
        // SAFETY: GetForegroundWindow 只读取系统当前前台窗口句柄。
        let hwnd = unsafe { GetForegroundWindow() };
        (!hwnd.0.is_null()).then_some(hwnd.0 as isize)
    }

    fn lock_state(state: &Arc<Mutex<InputState>>) -> std::sync::MutexGuard<'_, InputState> {
        match state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    pub(crate) fn set_trigger_key_code(code: global_hotkey::hotkey::Code) {
        *lock_trigger_key() = raw_input_key(code).map(|key| TriggerKey { key });
    }

    pub(crate) fn clear_trigger_key() {
        lock_trigger_key().take();
    }

    pub(crate) fn trigger_key_is_physically_pressed() -> bool {
        current_trigger_key().is_some_and(|trigger_key| is_physical_key_pressed(trigger_key.key))
    }

    pub(crate) fn trigger_shortcut_is_physically_pressed(
        modifiers: global_hotkey::hotkey::Modifiers,
    ) -> bool {
        if !trigger_key_is_physically_pressed() {
            return false;
        }
        (!modifiers.contains(global_hotkey::hotkey::Modifiers::CONTROL)
            || modifier_is_physically_pressed(raw_input::Key::ControlLeft)
            || modifier_is_physically_pressed(raw_input::Key::ControlRight))
            && (!modifiers.contains(global_hotkey::hotkey::Modifiers::ALT)
                || modifier_is_physically_pressed(raw_input::Key::AltLeft)
                || modifier_is_physically_pressed(raw_input::Key::AltRight))
            && (!modifiers.contains(global_hotkey::hotkey::Modifiers::SHIFT)
                || modifier_is_physically_pressed(raw_input::Key::ShiftLeft)
                || modifier_is_physically_pressed(raw_input::Key::ShiftRight))
            && (!modifiers.contains(global_hotkey::hotkey::Modifiers::SUPER)
                || modifier_is_physically_pressed(raw_input::Key::MetaLeft)
                || modifier_is_physically_pressed(raw_input::Key::MetaRight))
    }

    pub(crate) fn simulate_ctrl_v() {
        let _paste_guard = match PASTE_SIMULATION_LOCK.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        let modifiers = ModifierState::record();
        // Ctrl、Shift、Win 不能参与下面的 Ctrl+V；Alt 必须等触发键释放后再释放，
        // 否则前台程序可能把 Alt 解释成菜单导航。
        modifiers.release_conflicting();
        if let Some(trigger_key) = current_trigger_key() {
            Simulate::keyboard(trigger_key.key, false);
        }
        modifiers.release_alt();

        Simulate::keyboard(raw_input::Key::ControlLeft, true);
        Simulate::keyboard(raw_input::Key::KeyV, true);
        thread::sleep(Duration::from_millis(8));
        Simulate::keyboard(raw_input::Key::KeyV, false);
        Simulate::keyboard(raw_input::Key::ControlLeft, false);

        // 不重新按下触发键：触发键若先于 Ctrl 等修饰键恢复，会被前台程序
        // 当作普通字符；若与修饰键同时恢复，又可能再次触发全局快捷键。
        modifiers.restore_physical();
    }

    #[derive(Clone, Copy)]
    struct ModifierState {
        keys: [ModifierKeyState; 8],
    }

    #[derive(Clone, Copy)]
    struct ModifierKeyState {
        key: raw_input::Key,
        was_pressed: bool,
        release_generation: u64,
    }

    impl ModifierState {
        fn record() -> Self {
            let keys = [
                raw_input::Key::ControlLeft,
                raw_input::Key::ControlRight,
                raw_input::Key::AltLeft,
                raw_input::Key::AltRight,
                raw_input::Key::ShiftLeft,
                raw_input::Key::ShiftRight,
                raw_input::Key::MetaLeft,
                raw_input::Key::MetaRight,
            ];
            let physical_state = match physical_key_state().lock() {
                Ok(state) => *state,
                Err(poisoned) => *poisoned.into_inner(),
            };
            Self {
                keys: keys.map(|key| ModifierKeyState {
                    key,
                    was_pressed: modifier_is_physically_pressed(key),
                    release_generation: physical_state.releases[key as usize],
                }),
            }
        }

        fn release_conflicting(&self) {
            let mut released = false;
            for modifier in self.keys {
                if modifier.was_pressed
                    && modifier.key != raw_input::Key::AltLeft
                    && modifier.key != raw_input::Key::AltRight
                {
                    Simulate::keyboard(modifier.key, false);
                    released = true;
                }
            }
            if released {
                thread::sleep(Duration::from_millis(10));
            }
        }

        fn release_alt(&self) {
            for modifier in self.keys {
                if modifier.was_pressed
                    && matches!(
                        modifier.key,
                        raw_input::Key::AltLeft | raw_input::Key::AltRight
                    )
                {
                    Simulate::keyboard(modifier.key, false);
                }
            }
        }

        fn restore_physical(&self) {
            // 临时用于 Ctrl+V 的左 Ctrl 不属于物理状态，只恢复粘贴前已按下的修饰键。
            let physical_state = match physical_key_state().lock() {
                Ok(state) => *state,
                Err(poisoned) => *poisoned.into_inner(),
            };
            for modifier in self.keys {
                if should_restore_modifier(
                    modifier.was_pressed,
                    modifier.release_generation,
                    physical_state.releases[modifier.key as usize],
                ) {
                    Simulate::keyboard(modifier.key, true);
                }
            }
        }
    }

    fn should_restore_modifier(
        was_pressed: bool,
        release_generation: u64,
        current_release_generation: u64,
    ) -> bool {
        was_pressed && release_generation == current_release_generation
    }

    fn lock_trigger_key() -> std::sync::MutexGuard<'static, Option<TriggerKey>> {
        match CURRENT_TRIGGER_KEY.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn current_trigger_key() -> Option<TriggerKey> {
        lock_trigger_key().as_ref().copied()
    }

    fn raw_input_key(code: global_hotkey::hotkey::Code) -> Option<raw_input::Key> {
        use global_hotkey::hotkey::Code;

        // global-hotkey 已经完成快捷键字符串到 Code 的解析；这里只做两个库之间
        // 的物理按键类型转换，Windows 扫描码和虚拟键码仍由 raw-input 负责。
        Some(match code {
            Code::Backquote => raw_input::Key::Backquote,
            Code::Backslash => raw_input::Key::Backslash,
            Code::BracketLeft => raw_input::Key::BracketLeft,
            Code::BracketRight => raw_input::Key::BracketRight,
            Code::Comma => raw_input::Key::Comma,
            Code::Digit0 => raw_input::Key::Digit0,
            Code::Digit1 => raw_input::Key::Digit1,
            Code::Digit2 => raw_input::Key::Digit2,
            Code::Digit3 => raw_input::Key::Digit3,
            Code::Digit4 => raw_input::Key::Digit4,
            Code::Digit5 => raw_input::Key::Digit5,
            Code::Digit6 => raw_input::Key::Digit6,
            Code::Digit7 => raw_input::Key::Digit7,
            Code::Digit8 => raw_input::Key::Digit8,
            Code::Digit9 => raw_input::Key::Digit9,
            Code::Equal => raw_input::Key::Equal,
            Code::IntlBackslash => raw_input::Key::IntlBackslash,
            Code::IntlRo => raw_input::Key::IntlRo,
            Code::IntlYen => raw_input::Key::IntlYen,
            Code::KeyA => raw_input::Key::KeyA,
            Code::KeyB => raw_input::Key::KeyB,
            Code::KeyC => raw_input::Key::KeyC,
            Code::KeyD => raw_input::Key::KeyD,
            Code::KeyE => raw_input::Key::KeyE,
            Code::KeyF => raw_input::Key::KeyF,
            Code::KeyG => raw_input::Key::KeyG,
            Code::KeyH => raw_input::Key::KeyH,
            Code::KeyI => raw_input::Key::KeyI,
            Code::KeyJ => raw_input::Key::KeyJ,
            Code::KeyK => raw_input::Key::KeyK,
            Code::KeyL => raw_input::Key::KeyL,
            Code::KeyM => raw_input::Key::KeyM,
            Code::KeyN => raw_input::Key::KeyN,
            Code::KeyO => raw_input::Key::KeyO,
            Code::KeyP => raw_input::Key::KeyP,
            Code::KeyQ => raw_input::Key::KeyQ,
            Code::KeyR => raw_input::Key::KeyR,
            Code::KeyS => raw_input::Key::KeyS,
            Code::KeyT => raw_input::Key::KeyT,
            Code::KeyU => raw_input::Key::KeyU,
            Code::KeyV => raw_input::Key::KeyV,
            Code::KeyW => raw_input::Key::KeyW,
            Code::KeyX => raw_input::Key::KeyX,
            Code::KeyY => raw_input::Key::KeyY,
            Code::KeyZ => raw_input::Key::KeyZ,
            Code::Minus => raw_input::Key::Minus,
            Code::Period => raw_input::Key::Period,
            Code::Quote => raw_input::Key::Quote,
            Code::Semicolon => raw_input::Key::Semicolon,
            Code::Slash => raw_input::Key::Slash,
            Code::Backspace => raw_input::Key::Backspace,
            Code::CapsLock => raw_input::Key::CapsLock,
            Code::ContextMenu => raw_input::Key::ContextMenu,
            Code::ControlLeft => raw_input::Key::ControlLeft,
            Code::ControlRight => raw_input::Key::ControlRight,
            Code::Enter => raw_input::Key::Enter,
            Code::AltLeft => raw_input::Key::AltLeft,
            Code::AltRight => raw_input::Key::AltRight,
            Code::MetaLeft => raw_input::Key::MetaLeft,
            Code::MetaRight => raw_input::Key::MetaRight,
            Code::ShiftLeft => raw_input::Key::ShiftLeft,
            Code::ShiftRight => raw_input::Key::ShiftRight,
            Code::Space => raw_input::Key::Space,
            Code::Tab => raw_input::Key::Tab,
            Code::Convert => raw_input::Key::Convert,
            Code::NonConvert => raw_input::Key::NonConvert,
            Code::Delete => raw_input::Key::Delete,
            Code::End => raw_input::Key::End,
            Code::Help => raw_input::Key::Help,
            Code::Home => raw_input::Key::Home,
            Code::Insert => raw_input::Key::Insert,
            Code::PageDown => raw_input::Key::PageDown,
            Code::PageUp => raw_input::Key::PageUp,
            Code::ArrowDown => raw_input::Key::ArrowDown,
            Code::ArrowLeft => raw_input::Key::ArrowLeft,
            Code::ArrowRight => raw_input::Key::ArrowRight,
            Code::ArrowUp => raw_input::Key::ArrowUp,
            Code::NumLock => raw_input::Key::NumLock,
            Code::Numpad0 => raw_input::Key::Numpad0,
            Code::Numpad1 => raw_input::Key::Numpad1,
            Code::Numpad2 => raw_input::Key::Numpad2,
            Code::Numpad3 => raw_input::Key::Numpad3,
            Code::Numpad4 => raw_input::Key::Numpad4,
            Code::Numpad5 => raw_input::Key::Numpad5,
            Code::Numpad6 => raw_input::Key::Numpad6,
            Code::Numpad7 => raw_input::Key::Numpad7,
            Code::Numpad8 => raw_input::Key::Numpad8,
            Code::Numpad9 => raw_input::Key::Numpad9,
            Code::NumpadAdd => raw_input::Key::NumpadAdd,
            Code::NumpadDecimal => raw_input::Key::NumpadDecimal,
            Code::NumpadDivide => raw_input::Key::NumpadDivide,
            Code::NumpadEnter => raw_input::Key::NumpadEnter,
            Code::NumpadMultiply => raw_input::Key::NumpadMultiply,
            Code::NumpadSubtract => raw_input::Key::NumpadSubtract,
            Code::Escape => raw_input::Key::Escape,
            Code::F1 => raw_input::Key::F1,
            Code::F2 => raw_input::Key::F2,
            Code::F3 => raw_input::Key::F3,
            Code::F4 => raw_input::Key::F4,
            Code::F5 => raw_input::Key::F5,
            Code::F6 => raw_input::Key::F6,
            Code::F7 => raw_input::Key::F7,
            Code::F8 => raw_input::Key::F8,
            Code::F9 => raw_input::Key::F9,
            Code::F10 => raw_input::Key::F10,
            Code::F11 => raw_input::Key::F11,
            Code::F12 => raw_input::Key::F12,
            Code::F13 => raw_input::Key::F13,
            Code::F14 => raw_input::Key::F14,
            Code::F15 => raw_input::Key::F15,
            Code::F16 => raw_input::Key::F16,
            Code::F17 => raw_input::Key::F17,
            Code::F18 => raw_input::Key::F18,
            Code::F19 => raw_input::Key::F19,
            Code::F20 => raw_input::Key::F20,
            Code::F21 => raw_input::Key::F21,
            Code::F22 => raw_input::Key::F22,
            Code::F23 => raw_input::Key::F23,
            Code::F24 => raw_input::Key::F24,
            Code::PrintScreen => raw_input::Key::PrintScreen,
            Code::ScrollLock => raw_input::Key::ScrollLock,
            Code::Pause => raw_input::Key::Pause,
            _ => return None,
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn modifier_restore_respects_physical_release_during_paste() {
            assert!(!should_restore_modifier(true, 4, 5));
            assert!(should_restore_modifier(true, 4, 4));
            assert!(!should_restore_modifier(false, 4, 4));
        }

        #[test]
        fn normal_hidden_mouse_moves_keep_sequence_without_reading_position_or_sending_events() {
            let state = Arc::new(Mutex::new(InputState::new()));
            let position = gpui::Point {
                x: -100000.0,
                y: -100000.0,
            };
            let (sender, receiver) = async_channel::unbounded();
            {
                let mut state = lock_state(&state);
                state.snapshot = Some(PointerSnapshot {
                    position,
                    buttons: PointerButtons::default(),
                    mouse_move_sequence: 0,
                });
                state.mouse_event_senders.push((0, sender));
            }
            for _ in 0..1000 {
                handle_event(
                    &state,
                    Event::MouseMove {
                        delta: raw_input::Point { x: 1.0, y: 1.0 },
                    },
                );
            }
            let state = lock_state(&state);
            let snapshot = state.snapshot.unwrap();
            assert_eq!(snapshot.position, position);
            assert_eq!(snapshot.mouse_move_sequence, 1000);
            assert!(receiver.try_recv().is_err());
        }

        #[test]
        fn normal_hidden_mouse_buttons_still_update_and_reach_subscribers() {
            let state = Arc::new(Mutex::new(InputState::new()));
            let (sender, receiver) = async_channel::unbounded();
            lock_state(&state).mouse_event_senders.push((0, sender));
            handle_event(
                &state,
                Event::MouseDown {
                    button: MouseButton::Left,
                },
            );
            assert!(lock_state(&state).snapshot.unwrap().buttons.left);
            assert!(matches!(
                receiver.try_recv(),
                Ok(GlobalMouseEvent::MouseDown { sequence: 1, .. })
            ));
            handle_event(
                &state,
                Event::MouseUp {
                    button: MouseButton::Left,
                },
            );
            assert!(!lock_state(&state).snapshot.unwrap().buttons.left);
            assert!(matches!(
                receiver.try_recv(),
                Ok(GlobalMouseEvent::MouseUp {
                    left_pressed: false,
                    ..
                })
            ));
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod windows_input {
    use super::{GlobalMouseEvent, PointerSnapshot};

    pub(crate) fn start() -> Result<(), String> {
        Ok(())
    }

    pub(crate) fn pointer_snapshot() -> Option<PointerSnapshot> {
        None
    }

    pub(crate) fn subscribe_mouse_events() -> (u64, async_channel::Receiver<GlobalMouseEvent>) {
        let (_sender, receiver) = async_channel::unbounded();
        (0, receiver)
    }

    pub(crate) fn unsubscribe_mouse_events(_subscription_id: u64) {}

    pub(crate) fn latest_mouse_down_sequence() -> Option<u64> {
        None
    }
}

pub(crate) use windows_input::{pointer_snapshot, start};

pub(crate) use windows_input::{
    latest_mouse_down_sequence, subscribe_mouse_events, unsubscribe_mouse_events,
};

#[cfg(target_os = "windows")]
pub(crate) use windows_input::{
    clear_trigger_key, set_pointer_change_notifier, set_trigger_key_code, simulate_ctrl_v,
    trigger_shortcut_is_physically_pressed,
};
