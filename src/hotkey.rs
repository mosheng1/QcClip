use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use global_hotkey::hotkey::{Code, HotKey, Modifiers as HotkeyModifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use gpui::{App, AppContext as _, BorrowAppContext as _, Global, Keystroke, Modifiers};
use gpui_component::kbd::Kbd;

use crate::config::{
    MiddleMouseShortcutConfig, QuickPasteConfig, QuickPasteShortcutConfig, SessionShortcutId,
    ShortcutBinding, ShortcutsConfig,
};

#[cfg(target_os = "windows")]
mod middle_mouse;
#[cfg(target_os = "windows")]
mod wheel_mouse;

const SESSION_SHORTCUT_INITIAL_REPEAT_DELAY: Duration = Duration::from_millis(250);
const SESSION_SHORTCUT_REPEAT_INTERVAL: Duration = Duration::from_millis(50);
const PLAIN_TEXT_PASTE_INITIAL_REPEAT_DELAY: Duration = Duration::from_millis(300);
const PLAIN_TEXT_PASTE_REPEAT_INTERVAL: Duration = Duration::from_millis(120);
const QUICK_PASTE_NAVIGATION_INITIAL_REPEAT_DELAY: Duration = Duration::from_millis(250);
const QUICK_PASTE_NAVIGATION_REPEAT_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegistrationStatus {
    Registered,
    Disabled,
    Inactive,
    Suspended,
    Failed,
}

pub(crate) struct HotkeyRuntime {
    #[cfg(target_os = "windows")]
    middle_mouse: middle_mouse::MiddleMouseRuntime,
    #[cfg(target_os = "windows")]
    wheel_mouse: wheel_mouse::WheelMouseRuntime,
    manager: Option<GlobalHotKeyManager>,
    main_hotkey: Option<HotKey>,
    settings_hotkey: Option<HotKey>,
    clipboard_monitor_hotkey: Option<HotKey>,
    plain_text_paste_hotkey: Option<HotKey>,
    quick_paste_hotkey: Option<HotKey>,
    quick_paste_index_hotkeys: HashMap<u32, HotKey>,
    failed_quick_paste_index_hotkeys: HashMap<String, String>,
    quick_paste_navigation_hotkeys: HashMap<u32, QuickPasteNavigationRegistration>,
    session_hotkeys: HashMap<SessionShortcutId, HotKey>,
    failed_session_hotkeys: HashMap<SessionShortcutId, HotKey>,
    last_session_dispatch: HashMap<SessionShortcutId, Instant>,
    pressed_session_hotkeys: HashMap<u32, (u64, u64)>,
    pressed_plain_text_paste: Option<(u32, u64, u64)>,
    pressed_quick_paste: Option<(u32, u64, u64)>,
    pressed_quick_paste_navigation: HashMap<u32, (u64, u64)>,
    suspended_main_status: Option<RegistrationStatus>,
    suspended_settings_status: Option<RegistrationStatus>,
    suspended_clipboard_monitor_status: Option<RegistrationStatus>,
    suspended_plain_text_paste_status: Option<RegistrationStatus>,
    suspended_quick_paste_status: Option<RegistrationStatus>,
    suspended_quick_paste_index_status: Option<RegistrationStatus>,
    suspended_session_statuses: HashMap<SessionShortcutId, RegistrationStatus>,
    suspended: bool,
    global_enabled: bool,
    status: RegistrationStatus,
    settings_status: RegistrationStatus,
    clipboard_monitor_status: RegistrationStatus,
    plain_text_paste_status: RegistrationStatus,
    quick_paste_status: RegistrationStatus,
    quick_paste_index_status: RegistrationStatus,
    active_commands: Arc<RwLock<HashMap<u32, ActiveHotkey>>>,
    next_generation: u64,
    next_press_generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HotkeyCommand {
    ToggleMainWindow,
    OpenSettings,
    ToggleClipboardMonitor,
    PlainTextPaste,
    QuickPaste,
    QuickPasteIndex(usize),
    QuickPasteNavigation(QuickPasteNavigation),
    Session(SessionShortcutId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QuickPasteNavigation {
    Up,
    Down,
    Left,
    Right,
}

impl QuickPasteNavigation {
    const ALL: [Self; 4] = [Self::Up, Self::Down, Self::Left, Self::Right];

    fn is_enabled(self, config: QuickPasteConfig) -> bool {
        match self {
            Self::Up | Self::Down => config.select_with_up_down,
            Self::Left | Self::Right => config.switch_list_with_left_right,
        }
    }

    fn key_code(self) -> Code {
        match self {
            Self::Up => Code::ArrowUp,
            Self::Down => Code::ArrowDown,
            Self::Left => Code::ArrowLeft,
            Self::Right => Code::ArrowRight,
        }
    }
}

#[derive(Clone, Copy)]
struct QuickPasteNavigationRegistration {
    navigation: QuickPasteNavigation,
    hotkey: HotKey,
    registered_by_quick_paste: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ActiveHotkey {
    command: HotkeyCommand,
    generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HotkeyEvent {
    id: u32,
    active: ActiveHotkey,
    state: HotKeyState,
}

impl Global for HotkeyRuntime {}

impl HotkeyRuntime {
    pub(crate) fn install(cx: &mut App) {
        let (sender, receiver) = async_channel::unbounded::<HotkeyEvent>();
        let active_commands = Arc::new(RwLock::new(HashMap::<u32, ActiveHotkey>::new()));
        let commands_for_handler = active_commands.clone();

        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            let active = match commands_for_handler.read() {
                Ok(commands) => commands.get(&event.id).copied(),
                Err(_) => {
                    tracing::error!("读取快捷键分发状态失败：状态锁状态异常");
                    None
                }
            };
            let Some(active) = active else {
                return;
            };

            if sender
                .try_send(HotkeyEvent {
                    id: event.id,
                    active,
                    state: event.state,
                })
                .is_err()
            {
                tracing::debug!("快捷键事件通道已关闭");
            }
        }));

        let manager = match GlobalHotKeyManager::new() {
            Ok(manager) => Some(manager),
            Err(error) => {
                tracing::error!("创建全局快捷键管理器失败：{error}");
                None
            }
        };
        let mut runtime = Self {
            #[cfg(target_os = "windows")]
            middle_mouse: middle_mouse::MiddleMouseRuntime::install(
                crate::config_runtime::shortcuts_config(cx).middle_mouse,
                cx,
            ),
            #[cfg(target_os = "windows")]
            wheel_mouse: wheel_mouse::WheelMouseRuntime::install(
                crate::config_runtime::shortcuts_config(cx).wheel_mouse,
                cx,
            ),
            manager,
            main_hotkey: None,
            settings_hotkey: None,
            clipboard_monitor_hotkey: None,
            plain_text_paste_hotkey: None,
            quick_paste_hotkey: None,
            quick_paste_index_hotkeys: HashMap::new(),
            failed_quick_paste_index_hotkeys: HashMap::new(),
            quick_paste_navigation_hotkeys: HashMap::new(),
            session_hotkeys: HashMap::new(),
            failed_session_hotkeys: HashMap::new(),
            last_session_dispatch: HashMap::new(),
            pressed_session_hotkeys: HashMap::new(),
            pressed_plain_text_paste: None,
            pressed_quick_paste: None,
            pressed_quick_paste_navigation: HashMap::new(),
            suspended_main_status: None,
            suspended_settings_status: None,
            suspended_clipboard_monitor_status: None,
            suspended_plain_text_paste_status: None,
            suspended_quick_paste_status: None,
            suspended_quick_paste_index_status: None,
            suspended_session_statuses: HashMap::new(),
            suspended: false,
            global_enabled: true,
            status: RegistrationStatus::Disabled,
            settings_status: RegistrationStatus::Disabled,
            clipboard_monitor_status: RegistrationStatus::Disabled,
            plain_text_paste_status: RegistrationStatus::Disabled,
            quick_paste_status: RegistrationStatus::Disabled,
            quick_paste_index_status: RegistrationStatus::Disabled,
            active_commands,
            next_generation: 0,
            next_press_generation: 0,
        };
        let shortcuts = crate::config_runtime::shortcuts_config(cx);
        if let Err(error) = runtime.apply_main(shortcuts.toggle_main_window.as_ref()) {
            tracing::error!("注册显示/隐藏窗口快捷键失败：{error}");
        }
        if let Err(error) = runtime.apply_settings(shortcuts.open_settings.as_ref()) {
            tracing::error!("注册打开设置快捷键失败：{error}");
        }
        if let Err(error) =
            runtime.apply_clipboard_monitor(shortcuts.toggle_clipboard_monitor.as_ref())
        {
            tracing::error!("注册切换剪贴板监听快捷键失败：{error}");
        }
        if let Err(error) = runtime.apply_plain_text_paste(shortcuts.plain_text_paste.as_ref()) {
            tracing::error!("注册纯文本粘贴快捷键失败：{error}");
        }
        if let Err(error) = runtime.apply_quick_paste(shortcuts.quick_paste.as_ref()) {
            tracing::error!("注册便捷粘贴快捷键失败：{error}");
        }
        runtime.apply_quick_paste_index(&shortcuts.quick_paste_1_to_9);
        cx.set_global(runtime);

        cx.spawn(async move |cx| {
            while let Ok(event) = receiver.recv().await {
                cx.update(|cx| {
                    if let Some(navigation) = quick_paste_navigation_for_event(event, cx) {
                        match event.state {
                            HotKeyState::Pressed => {
                                let generation =
                                    cx.update_global::<HotkeyRuntime, _>(|runtime, _| {
                                        runtime.mark_quick_paste_navigation_pressed(event)
                                    });
                                if let Some(generation) = generation {
                                    crate::windows::quick_paste::QuickPasteWindowService::navigate(
                                        navigation, cx,
                                    );
                                    spawn_quick_paste_navigation_repeat(
                                        navigation, event, generation, cx,
                                    );
                                }
                            }
                            HotKeyState::Released => {
                                cx.update_global::<HotkeyRuntime, _>(|runtime, _| {
                                    runtime.mark_quick_paste_navigation_released(event)
                                });
                            }
                        }
                        return;
                    }

                    match (event.active.command, event.state) {
                        (HotkeyCommand::ToggleMainWindow, HotKeyState::Pressed)
                            if is_hotkey_event_active(event, cx) =>
                        {
                            crate::windows::main::toggle_main_window(cx)
                        }
                        (HotkeyCommand::OpenSettings, HotKeyState::Pressed)
                            if is_hotkey_event_active(event, cx) =>
                        {
                            crate::windows::settings::open_settings_window(cx)
                        }
                        (HotkeyCommand::ToggleClipboardMonitor, HotKeyState::Pressed)
                            if is_hotkey_event_active(event, cx) =>
                        {
                            crate::windows::main::MainWindowService::toggle_clipboard_listener(cx)
                        }
                        (HotkeyCommand::PlainTextPaste, HotKeyState::Pressed)
                            if is_hotkey_event_active(event, cx) =>
                        {
                            set_plain_text_paste_trigger_key(event, cx);
                            let repeat_generation =
                                cx.update_global::<HotkeyRuntime, _>(|runtime, _| {
                                    runtime.mark_plain_text_paste_pressed(event)
                                });
                            if let Some(generation) = repeat_generation {
                                crate::windows::main::MainWindowService::paste_plain_text_shortcut(
                                    cx,
                                );
                                spawn_plain_text_paste_repeat(event, generation, cx);
                            }
                        }
                        (HotkeyCommand::PlainTextPaste, HotKeyState::Released)
                            if is_hotkey_event_active(event, cx) =>
                        {
                            #[cfg(target_os = "windows")]
                            let released = !plain_text_paste_is_physically_pressed(event, cx);
                            #[cfg(not(target_os = "windows"))]
                            let released = true;
                            if released {
                                cx.update_global::<HotkeyRuntime, _>(|runtime, _| {
                                    runtime.mark_plain_text_paste_released(event)
                                });
                                #[cfg(target_os = "windows")]
                                crate::platform::input::clear_trigger_key();
                            }
                        }
                        (HotkeyCommand::QuickPaste, HotKeyState::Pressed)
                            if is_hotkey_event_active(event, cx) =>
                        {
                            set_quick_paste_trigger_key(event, cx);
                            let started = cx.update_global::<HotkeyRuntime, _>(|runtime, _| {
                                runtime.mark_quick_paste_pressed(event)
                            });
                            if started {
                                crate::windows::quick_paste::QuickPasteWindowService::show(cx);
                            }
                        }
                        (HotkeyCommand::QuickPaste, HotKeyState::Released)
                            if is_hotkey_event_active(event, cx) =>
                        {
                            #[cfg(target_os = "windows")]
                            spawn_quick_paste_release(event, cx);
                            #[cfg(not(target_os = "windows"))]
                            {
                                let was_pressed =
                                    cx.update_global::<HotkeyRuntime, _>(|runtime, _| {
                                        runtime.mark_quick_paste_released(event)
                                    });
                                if was_pressed {
                                    crate::windows::quick_paste::QuickPasteWindowService::finish(
                                        cx,
                                    );
                                }
                            }
                        }
                        (HotkeyCommand::Session(id), HotKeyState::Pressed)
                            if is_hotkey_event_active(event, cx) =>
                        {
                            let repeat_generation =
                                cx.update_global::<HotkeyRuntime, _>(|runtime, _| {
                                    runtime.mark_session_hotkey_pressed(event)
                                });
                            if crate::windows::main::MainWindowService::is_logically_visible(cx) {
                                let should_dispatch =
                                    cx.update_global::<HotkeyRuntime, _>(|runtime, _| {
                                        runtime.should_dispatch_session(id)
                                    });
                                if should_dispatch {
                                    #[cfg(target_os = "windows")]
                                    let trigger_key = if id == SessionShortcutId::ExecutePaste {
                                        cx.read_global::<HotkeyRuntime, _>(|runtime, _| {
                                            runtime
                                                .session_hotkeys
                                                .get(&id)
                                                .copied()
                                                .filter(|hotkey| hotkey.id() == event.id)
                                                .map(|hotkey| hotkey.key)
                                        })
                                    } else {
                                        None
                                    };
                                    #[cfg(target_os = "windows")]
                                    if let Some(trigger_key) = trigger_key {
                                        crate::platform::input::set_trigger_key_code(trigger_key);
                                    }
                                    crate::windows::main::dispatch_session_shortcut(id, cx);
                                }
                            }
                            if let Some(generation) = repeat_generation {
                                spawn_session_shortcut_repeat(id, event, generation, cx);
                            }
                        }
                        (HotkeyCommand::Session(_), HotKeyState::Released)
                            if is_hotkey_event_active(event, cx) =>
                        {
                            cx.update_global::<HotkeyRuntime, _>(|runtime, _| {
                                runtime.mark_session_hotkey_released(event)
                            });
                            #[cfg(target_os = "windows")]
                            if event.active.command
                                == HotkeyCommand::Session(SessionShortcutId::ExecutePaste)
                            {
                                crate::platform::input::clear_trigger_key();
                            }
                        }
                        (HotkeyCommand::QuickPasteIndex(index), HotKeyState::Pressed)
                            if is_hotkey_event_active(event, cx) =>
                        {
                            set_quick_paste_index_trigger_key(event, cx);
                            crate::windows::main::MainWindowService::paste_history_index(index, cx);
                        }
                        (HotkeyCommand::QuickPasteIndex(_), HotKeyState::Released)
                            if is_hotkey_event_active(event, cx) =>
                        {
                            #[cfg(target_os = "windows")]
                            let released = !quick_paste_index_is_physically_pressed(event, cx);
                            #[cfg(not(target_os = "windows"))]
                            let released = true;
                            if released {
                                #[cfg(target_os = "windows")]
                                crate::platform::input::clear_trigger_key();
                            }
                        }
                        _ => {}
                    }
                });
            }
            tracing::debug!("快捷键事件监听任务已退出");
        })
        .detach();
    }

    fn apply_main(&mut self, binding: Option<&ShortcutBinding>) -> Result<(), String> {
        let next = binding.map(binding_to_hotkey).transpose()?;

        if self.main_hotkey == next {
            self.status = if next.is_some() {
                RegistrationStatus::Registered
            } else {
                RegistrationStatus::Disabled
            };
            return Ok(());
        }

        if self.manager.is_none() && next.is_some() {
            self.status = RegistrationStatus::Failed;
            return Err("全局快捷键管理器不可用".to_owned());
        }

        if let Some(current) = self.main_hotkey {
            if self.manager.is_none() {
                self.status = RegistrationStatus::Failed;
                return Err("全局快捷键管理器不可用".to_owned());
            }
            self.remove_active_command(current.id());
            let Some(manager) = self.manager.as_ref() else {
                self.status = RegistrationStatus::Failed;
                return Err("全局快捷键管理器不可用".to_owned());
            };
            manager
                .unregister(current)
                .map_err(|error| format!("注销原快捷键失败：{error}"))?;
            self.main_hotkey = None;
        }

        let Some(next) = next else {
            self.status = RegistrationStatus::Disabled;
            return Ok(());
        };
        let Some(manager) = self.manager.as_ref() else {
            self.status = RegistrationStatus::Failed;
            return Err("全局快捷键管理器不可用".to_owned());
        };
        if self.has_active_command(next.id()) {
            self.status = RegistrationStatus::Failed;
            return Err("快捷键与另一条快捷键重复".to_owned());
        }
        manager
            .register(next)
            .map_err(|error| format!("系统拒绝注册快捷键：{error}"))?;

        self.main_hotkey = Some(next);
        self.set_active_command(next.id(), Some(HotkeyCommand::ToggleMainWindow));
        self.status = RegistrationStatus::Registered;
        Ok(())
    }

    fn apply_settings(&mut self, binding: Option<&ShortcutBinding>) -> Result<(), String> {
        let next = binding.map(binding_to_hotkey).transpose()?;

        if self.settings_hotkey == next {
            self.settings_status = if next.is_some() {
                RegistrationStatus::Registered
            } else {
                RegistrationStatus::Disabled
            };
            return Ok(());
        }

        if self.manager.is_none() && next.is_some() {
            self.settings_status = RegistrationStatus::Failed;
            return Err("全局快捷键管理器不可用".to_owned());
        }

        if let Some(current) = self.settings_hotkey {
            self.remove_active_command(current.id());
            self.unregister(current)?;
            self.settings_hotkey = None;
        }

        let Some(next) = next else {
            self.settings_status = RegistrationStatus::Disabled;
            return Ok(());
        };
        if self.has_active_command(next.id()) {
            self.settings_status = RegistrationStatus::Failed;
            return Err("快捷键与另一条快捷键重复".to_owned());
        }
        let Some(manager) = self.manager.as_ref() else {
            self.settings_status = RegistrationStatus::Failed;
            return Err("全局快捷键管理器不可用".to_owned());
        };
        manager
            .register(next)
            .map_err(|error| format!("系统拒绝注册快捷键：{error}"))?;

        self.settings_hotkey = Some(next);
        self.set_active_command(next.id(), Some(HotkeyCommand::OpenSettings));
        self.settings_status = RegistrationStatus::Registered;
        Ok(())
    }

    fn apply_clipboard_monitor(&mut self, binding: Option<&ShortcutBinding>) -> Result<(), String> {
        let next = binding.map(binding_to_hotkey).transpose()?;

        if self.clipboard_monitor_hotkey == next {
            self.clipboard_monitor_status = if next.is_some() {
                RegistrationStatus::Registered
            } else {
                RegistrationStatus::Disabled
            };
            return Ok(());
        }

        if self.manager.is_none() && next.is_some() {
            self.clipboard_monitor_status = RegistrationStatus::Failed;
            return Err("全局快捷键管理器不可用".to_owned());
        }

        if let Some(current) = self.clipboard_monitor_hotkey {
            self.remove_active_command(current.id());
            self.unregister(current)?;
            self.clipboard_monitor_hotkey = None;
        }

        let Some(next) = next else {
            self.clipboard_monitor_status = RegistrationStatus::Disabled;
            return Ok(());
        };
        if self.has_active_command(next.id()) {
            self.clipboard_monitor_status = RegistrationStatus::Failed;
            return Err("快捷键与另一条快捷键重复".to_owned());
        }
        let Some(manager) = self.manager.as_ref() else {
            self.clipboard_monitor_status = RegistrationStatus::Failed;
            return Err("全局快捷键管理器不可用".to_owned());
        };
        manager
            .register(next)
            .map_err(|error| format!("系统拒绝注册快捷键：{error}"))?;

        self.clipboard_monitor_hotkey = Some(next);
        self.set_active_command(next.id(), Some(HotkeyCommand::ToggleClipboardMonitor));
        self.clipboard_monitor_status = RegistrationStatus::Registered;
        Ok(())
    }

    fn apply_plain_text_paste(&mut self, binding: Option<&ShortcutBinding>) -> Result<(), String> {
        let next = binding.map(binding_to_hotkey).transpose()?;

        if self.plain_text_paste_hotkey == next {
            self.plain_text_paste_status = if next.is_some() {
                RegistrationStatus::Registered
            } else {
                RegistrationStatus::Disabled
            };
            return Ok(());
        }

        if self.manager.is_none() && next.is_some() {
            self.plain_text_paste_status = RegistrationStatus::Failed;
            return Err("全局快捷键管理器不可用".to_owned());
        }

        if let Some(current) = self.plain_text_paste_hotkey {
            self.remove_active_command(current.id());
            self.unregister(current)?;
            self.plain_text_paste_hotkey = None;
        }

        let Some(next) = next else {
            self.plain_text_paste_status = RegistrationStatus::Disabled;
            return Ok(());
        };
        if self.has_active_command(next.id()) {
            self.plain_text_paste_status = RegistrationStatus::Failed;
            return Err("快捷键与另一条快捷键重复".to_owned());
        }
        let Some(manager) = self.manager.as_ref() else {
            self.plain_text_paste_status = RegistrationStatus::Failed;
            return Err("全局快捷键管理器不可用".to_owned());
        };
        manager
            .register(next)
            .map_err(|error| format!("系统拒绝注册快捷键：{error}"))?;

        self.plain_text_paste_hotkey = Some(next);
        self.set_active_command(next.id(), Some(HotkeyCommand::PlainTextPaste));
        self.plain_text_paste_status = RegistrationStatus::Registered;
        Ok(())
    }

    fn apply_quick_paste(&mut self, binding: Option<&ShortcutBinding>) -> Result<(), String> {
        let next = binding.map(binding_to_hotkey).transpose()?;

        if self.quick_paste_hotkey == next {
            self.quick_paste_status = if next.is_some() {
                RegistrationStatus::Registered
            } else {
                RegistrationStatus::Disabled
            };
            return Ok(());
        }

        if self.manager.is_none() && next.is_some() {
            self.quick_paste_status = RegistrationStatus::Failed;
            return Err("全局快捷键管理器不可用".to_owned());
        }

        if let Some(current) = self.quick_paste_hotkey {
            self.remove_active_command(current.id());
            self.unregister(current)?;
            self.quick_paste_hotkey = None;
        }

        let Some(next) = next else {
            self.quick_paste_status = RegistrationStatus::Disabled;
            return Ok(());
        };
        if self.has_active_command(next.id()) {
            self.quick_paste_status = RegistrationStatus::Failed;
            return Err("快捷键与另一条快捷键重复".to_owned());
        }
        let Some(manager) = self.manager.as_ref() else {
            self.quick_paste_status = RegistrationStatus::Failed;
            return Err("全局快捷键管理器不可用".to_owned());
        };
        manager
            .register(next)
            .map_err(|error| format!("系统拒绝注册快捷键：{error}"))?;

        self.quick_paste_hotkey = Some(next);
        self.set_active_command(next.id(), Some(HotkeyCommand::QuickPaste));
        self.quick_paste_status = RegistrationStatus::Registered;
        Ok(())
    }

    fn apply_quick_paste_index(&mut self, config: &QuickPasteShortcutConfig) {
        if !self.global_enabled {
            self.quick_paste_index_status = RegistrationStatus::Disabled;
            return;
        }
        if self.suspended {
            self.quick_paste_index_status = RegistrationStatus::Suspended;
            return;
        }
        let current = std::mem::take(&mut self.quick_paste_index_hotkeys);
        let mut failures = HashMap::new();
        for hotkey in current.into_values() {
            self.remove_active_command(hotkey.id());
            if let Err(error) = self.unregister(hotkey) {
                failures.insert(hotkey.to_string(), format!("注销快捷键失败：{error}"));
            }
        }
        self.failed_quick_paste_index_hotkeys = failures;
        if !config.enabled {
            self.quick_paste_index_status = if self.failed_quick_paste_index_hotkeys.is_empty() {
                RegistrationStatus::Disabled
            } else {
                RegistrationStatus::Failed
            };
            return;
        }
        if self.manager.is_none() {
            self.quick_paste_index_status = RegistrationStatus::Failed;
            for index in 0..9 {
                let binding = config.binding(index);
                self.failed_quick_paste_index_hotkeys.insert(
                    format_shortcut_binding(&binding),
                    "全局快捷键管理器不可用".to_owned(),
                );
            }
            return;
        }
        for index in 0..9 {
            let binding = config.binding(index);
            let label = format_shortcut_binding(&binding);
            let hotkey = match binding_to_hotkey(&binding) {
                Ok(hotkey) => hotkey,
                Err(error) => {
                    self.failed_quick_paste_index_hotkeys.insert(label, error);
                    continue;
                }
            };
            if self.has_active_command(hotkey.id()) {
                self.failed_quick_paste_index_hotkeys
                    .insert(label, "快捷键与另一条快捷键重复".to_owned());
                continue;
            }
            match self.manager.as_ref().unwrap().register(hotkey) {
                Ok(()) => {
                    self.set_active_command(
                        hotkey.id(),
                        Some(HotkeyCommand::QuickPasteIndex(index)),
                    );
                    self.quick_paste_index_hotkeys.insert(hotkey.id(), hotkey);
                }
                Err(error) => {
                    self.failed_quick_paste_index_hotkeys
                        .insert(label, format!("系统拒绝注册快捷键：{error}"));
                }
            }
        }
        self.quick_paste_index_status = if self.failed_quick_paste_index_hotkeys.is_empty() {
            RegistrationStatus::Registered
        } else {
            RegistrationStatus::Failed
        };
    }

    fn activate_quick_paste_navigation(&mut self, config: QuickPasteConfig) -> Result<(), String> {
        self.deactivate_quick_paste_navigation()?;
        if !self.global_enabled
            || self.suspended
            || (!config.select_with_up_down && !config.switch_list_with_left_right)
        {
            return Ok(());
        }
        let Some(trigger) = self.quick_paste_hotkey else {
            return Ok(());
        };
        let modifiers = trigger.mods
            & (HotkeyModifiers::CONTROL
                | HotkeyModifiers::ALT
                | HotkeyModifiers::SHIFT
                | HotkeyModifiers::SUPER);
        let mut first_error = None;
        for navigation in QuickPasteNavigation::ALL {
            if !navigation.is_enabled(config) {
                continue;
            }
            let hotkey = HotKey::new(Some(modifiers), navigation.key_code());
            let existing_command = self.active_command(hotkey.id());
            let registered_by_quick_paste = existing_command.is_none();
            if let Some(active) = existing_command
                && !matches!(active.command, HotkeyCommand::Session(_))
            {
                first_error
                    .get_or_insert_with(|| format!("便捷粘贴方向键 {} 与另一条快捷键重复", hotkey));
                continue;
            }
            if registered_by_quick_paste {
                let Some(manager) = self.manager.as_ref() else {
                    first_error.get_or_insert_with(|| "全局快捷键管理器不可用".to_owned());
                    continue;
                };
                if let Err(error) = manager.register(hotkey) {
                    first_error.get_or_insert_with(|| {
                        format!("注册便捷粘贴方向键 {} 失败：{error}", hotkey)
                    });
                    continue;
                }
                self.set_active_command(
                    hotkey.id(),
                    Some(HotkeyCommand::QuickPasteNavigation(navigation)),
                );
            }
            self.quick_paste_navigation_hotkeys.insert(
                hotkey.id(),
                QuickPasteNavigationRegistration {
                    navigation,
                    hotkey,
                    registered_by_quick_paste,
                },
            );
        }
        first_error.map_or(Ok(()), Err)
    }

    fn deactivate_quick_paste_navigation(&mut self) -> Result<(), String> {
        let registrations = std::mem::take(&mut self.quick_paste_navigation_hotkeys);
        self.pressed_quick_paste_navigation.clear();
        let mut first_error = None;
        for (id, registration) in registrations {
            if !registration.registered_by_quick_paste {
                continue;
            }
            self.failed_session_hotkeys
                .retain(|_, failed_hotkey| failed_hotkey.id() != id);
            if self.active_command(id).is_some_and(|active| {
                active.command == HotkeyCommand::QuickPasteNavigation(registration.navigation)
            }) {
                self.remove_active_command(id);
            }
            if let Err(error) = self.unregister(registration.hotkey) {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn active_command(&self, id: u32) -> Option<ActiveHotkey> {
        self.active_commands
            .read()
            .ok()
            .and_then(|commands| commands.get(&id).copied())
    }

    fn sync_session(
        &mut self,
        shortcuts: &ShortcutsConfig,
        window_visible: bool,
        pointer_inside: bool,
    ) -> Result<(), String> {
        let should_register = window_visible && !self.suspended && self.global_enabled;
        let mut desired = HashMap::new();
        if should_register {
            for id in SessionShortcutId::ALL {
                let shortcut = shortcuts.session(id);
                if shortcut.only_in_window && !pointer_inside {
                    continue;
                }
                if let Some(binding) = shortcut.binding.as_ref() {
                    desired.insert(id, binding_to_hotkey(binding)?);
                }
            }
        }

        let mut first_error = None;
        for (id, current) in self.session_hotkeys.clone() {
            let Some(next) = desired.get(&id) else {
                self.remove_active_command(current.id());
                match self.unregister(current) {
                    Ok(()) => {
                        self.session_hotkeys.remove(&id);
                        self.failed_session_hotkeys.remove(&id);
                    }
                    Err(error) => {
                        first_error.get_or_insert(error);
                    }
                }
                continue;
            };

            if *next != current {
                self.remove_active_command(current.id());
                match self.unregister(current) {
                    Ok(()) => {
                        self.session_hotkeys.remove(&id);
                        self.failed_session_hotkeys.remove(&id);
                    }
                    Err(error) => {
                        first_error.get_or_insert(error);
                        desired.remove(&id);
                    }
                }
            } else {
                self.set_active_command(current.id(), Some(HotkeyCommand::Session(id)));
            }
        }

        self.failed_session_hotkeys
            .retain(|id, failed| desired.get(id) == Some(failed));
        for (&id, &next) in &desired {
            if self.session_hotkeys.get(&id) == Some(&next) {
                continue;
            }
            if self.failed_session_hotkeys.get(&id) == Some(&next) {
                continue;
            }
            if self.has_active_command(next.id()) {
                let error = format!("快捷键 {} 与另一条快捷键重复", next);
                self.failed_session_hotkeys.insert(id, next);
                first_error.get_or_insert(error);
                continue;
            }
            let Some(manager) = self.manager.as_ref() else {
                let error = "全局快捷键管理器不可用".to_owned();
                self.failed_session_hotkeys.insert(id, next);
                first_error.get_or_insert(error);
                continue;
            };
            match manager.register(next) {
                Ok(()) => {
                    self.session_hotkeys.insert(id, next);
                    self.failed_session_hotkeys.remove(&id);
                    self.set_active_command(next.id(), Some(HotkeyCommand::Session(id)));
                }
                Err(error) => {
                    let error = format!("系统拒绝注册快捷键 {}：{error}", next);
                    self.failed_session_hotkeys.insert(id, next);
                    first_error.get_or_insert(error);
                }
            }
        }

        if let Some(error) = first_error {
            Err(error)
        } else {
            Ok(())
        }
    }

    fn unregister(&self, hotkey: HotKey) -> Result<(), String> {
        let Some(manager) = self.manager.as_ref() else {
            return Err("全局快捷键管理器不可用".to_owned());
        };
        manager
            .unregister(hotkey)
            .map_err(|error| format!("注销快捷键失败：{error}"))
    }

    fn unregister_all(&mut self) -> Result<(), String> {
        let mut first_error = self.deactivate_quick_paste_navigation().err();
        if let Some(current) = self.main_hotkey.take() {
            self.remove_active_command(current.id());
            if let Err(error) = self.unregister(current) {
                first_error.get_or_insert(error);
            }
        }
        if let Some(current) = self.settings_hotkey.take() {
            self.remove_active_command(current.id());
            if let Err(error) = self.unregister(current) {
                first_error.get_or_insert(error);
            }
        }
        if let Some(current) = self.clipboard_monitor_hotkey.take() {
            self.remove_active_command(current.id());
            if let Err(error) = self.unregister(current) {
                first_error.get_or_insert(error);
            }
        }
        if let Some(current) = self.plain_text_paste_hotkey.take() {
            self.remove_active_command(current.id());
            if let Err(error) = self.unregister(current) {
                first_error.get_or_insert(error);
            }
        }
        if let Some(current) = self.quick_paste_hotkey.take() {
            self.remove_active_command(current.id());
            if let Err(error) = self.unregister(current) {
                first_error.get_or_insert(error);
            }
        }
        for current in std::mem::take(&mut self.quick_paste_index_hotkeys).into_values() {
            self.remove_active_command(current.id());
            if let Err(error) = self.unregister(current) {
                first_error.get_or_insert(error);
            }
        }
        for (id, current) in self.session_hotkeys.clone() {
            self.remove_active_command(current.id());
            if let Err(error) = self.unregister(current) {
                first_error.get_or_insert(error);
            }
            self.session_hotkeys.remove(&id);
        }
        first_error.map_or(Ok(()), Err)
    }

    fn register_all(
        &mut self,
        shortcuts: &ShortcutsConfig,
        window_visible: bool,
        pointer_inside: bool,
    ) -> Result<(), String> {
        let main_result = self.apply_main(shortcuts.toggle_main_window.as_ref());
        let settings_result = self.apply_settings(shortcuts.open_settings.as_ref());
        let clipboard_monitor_result =
            self.apply_clipboard_monitor(shortcuts.toggle_clipboard_monitor.as_ref());
        let plain_text_paste_result =
            self.apply_plain_text_paste(shortcuts.plain_text_paste.as_ref());
        let quick_paste_result = self.apply_quick_paste(shortcuts.quick_paste.as_ref());
        self.apply_quick_paste_index(&shortcuts.quick_paste_1_to_9);
        let session_result = self.sync_session(shortcuts, window_visible, pointer_inside);
        main_result
            .and(settings_result)
            .and(clipboard_monitor_result)
            .and(plain_text_paste_result)
            .and(quick_paste_result)
            .and(session_result)
    }

    fn set_global_enabled(
        &mut self,
        enabled: bool,
        shortcuts: &ShortcutsConfig,
        window_visible: bool,
        pointer_inside: bool,
    ) -> Result<(), String> {
        if self.global_enabled == enabled {
            return Ok(());
        }
        self.global_enabled = enabled;
        #[cfg(target_os = "windows")]
        self.middle_mouse
            .configure(shortcuts.middle_mouse, enabled && !self.suspended);
        #[cfg(target_os = "windows")]
        self.wheel_mouse
            .configure(shortcuts.wheel_mouse, enabled && !self.suspended);
        if !enabled {
            return self.unregister_all();
        }
        if self.suspended {
            return Ok(());
        }
        self.register_all(shortcuts, window_visible, pointer_inside)
    }

    fn suspend(&mut self) -> Result<(), String> {
        if self.suspended {
            return Ok(());
        }
        self.suspended_main_status = Some(self.status);
        self.suspended_settings_status = Some(self.settings_status);
        self.suspended_clipboard_monitor_status = Some(self.clipboard_monitor_status);
        self.suspended_plain_text_paste_status = Some(self.plain_text_paste_status);
        self.suspended_quick_paste_status = Some(self.quick_paste_status);
        self.suspended_quick_paste_index_status = Some(self.quick_paste_index_status);
        self.suspended_session_statuses = SessionShortcutId::ALL
            .into_iter()
            .map(|id| (id, self.status_for_session(id, true)))
            .collect();
        self.suspended = true;
        #[cfg(target_os = "windows")]
        self.middle_mouse
            .configure(crate::config::MiddleMouseShortcutConfig::default(), false);
        #[cfg(target_os = "windows")]
        self.wheel_mouse
            .configure(crate::config::WheelShortcutConfig::default(), false);
        let first_error = self.unregister_all().err();
        self.status = RegistrationStatus::Suspended;
        self.quick_paste_index_status = RegistrationStatus::Suspended;
        first_error.map_or(Ok(()), Err)
    }

    fn resume(
        &mut self,
        shortcuts: &ShortcutsConfig,
        window_visible: bool,
        pointer_inside: bool,
    ) -> Result<(), String> {
        self.suspended = false;
        #[cfg(target_os = "windows")]
        self.middle_mouse
            .configure(shortcuts.middle_mouse, self.global_enabled);
        #[cfg(target_os = "windows")]
        self.wheel_mouse
            .configure(shortcuts.wheel_mouse, self.global_enabled);
        self.suspended_main_status = None;
        self.suspended_settings_status = None;
        self.suspended_clipboard_monitor_status = None;
        self.suspended_plain_text_paste_status = None;
        self.suspended_quick_paste_status = None;
        self.suspended_quick_paste_index_status = None;
        self.suspended_session_statuses.clear();
        if !self.global_enabled {
            return Ok(());
        }
        self.register_all(shortcuts, window_visible, pointer_inside)
    }

    fn status_for_session(&self, id: SessionShortcutId, configured: bool) -> RegistrationStatus {
        if !self.global_enabled || !configured {
            RegistrationStatus::Disabled
        } else if self.suspended {
            self.suspended_session_statuses
                .get(&id)
                .copied()
                .unwrap_or(RegistrationStatus::Inactive)
        } else if self.session_hotkeys.contains_key(&id)
            && self.active_commands.read().is_ok_and(|commands| {
                commands
                    .values()
                    .any(|active| active.command == HotkeyCommand::Session(id))
            })
        {
            RegistrationStatus::Registered
        } else if self.failed_session_hotkeys.contains_key(&id) {
            RegistrationStatus::Failed
        } else if self.suspended {
            RegistrationStatus::Suspended
        } else {
            RegistrationStatus::Inactive
        }
    }

    fn has_active_command(&self, id: u32) -> bool {
        self.active_commands
            .read()
            .map(|commands| commands.contains_key(&id))
            .unwrap_or(true)
    }

    fn set_active_command(&mut self, id: u32, command: Option<HotkeyCommand>) {
        match self.active_commands.write() {
            Ok(mut commands) => match command {
                Some(command) => {
                    if commands
                        .get(&id)
                        .is_some_and(|active| active.command == command)
                    {
                        return;
                    }
                    self.next_generation = self.next_generation.wrapping_add(1);
                    commands.insert(
                        id,
                        ActiveHotkey {
                            command,
                            generation: self.next_generation,
                        },
                    );
                }
                None => {
                    commands.remove(&id);
                }
            },
            Err(_) => tracing::error!("更新快捷键分发状态失败：状态锁状态异常"),
        }
    }

    fn remove_active_command(&mut self, id: u32) {
        self.set_active_command(id, None);
        self.pressed_session_hotkeys.remove(&id);
        self.pressed_quick_paste_navigation.remove(&id);
        if self
            .pressed_plain_text_paste
            .is_some_and(|(pressed_id, _, _)| pressed_id == id)
        {
            self.pressed_plain_text_paste = None;
        }
        if self
            .pressed_quick_paste
            .is_some_and(|(pressed_id, _, _)| pressed_id == id)
        {
            self.pressed_quick_paste = None;
        }
    }

    fn should_dispatch_session(&mut self, id: SessionShortcutId) -> bool {
        let now = Instant::now();
        let last_dispatch = self.last_session_dispatch.get(&id).copied();
        if session_shortcut_is_throttled(id, last_dispatch, now) {
            return false;
        }
        if session_shortcut_cooldown(id).is_some() {
            self.last_session_dispatch.insert(id, now);
        }
        true
    }

    fn mark_session_hotkey_pressed(&mut self, event: HotkeyEvent) -> Option<u64> {
        let HotkeyCommand::Session(id) = event.active.command else {
            return None;
        };
        session_shortcut_cooldown(id)?;
        self.next_press_generation = self.next_press_generation.wrapping_add(1);
        let generation = self.next_press_generation;
        self.pressed_session_hotkeys
            .insert(event.id, (event.active.generation, generation));
        Some(generation)
    }

    fn mark_session_hotkey_released(&mut self, event: HotkeyEvent) {
        if self
            .pressed_session_hotkeys
            .get(&event.id)
            .is_some_and(|(active_generation, _)| *active_generation == event.active.generation)
        {
            self.pressed_session_hotkeys.remove(&event.id);
        }
    }

    fn is_session_hotkey_pressed(&self, event: HotkeyEvent, generation: u64) -> bool {
        self.pressed_session_hotkeys.get(&event.id).is_some_and(
            |(active_generation, press_generation)| {
                *active_generation == event.active.generation && *press_generation == generation
            },
        )
    }

    fn mark_plain_text_paste_pressed(&mut self, event: HotkeyEvent) -> Option<u64> {
        self.next_press_generation = self.next_press_generation.wrapping_add(1);
        let generation = self.next_press_generation;
        self.pressed_plain_text_paste = Some((event.id, event.active.generation, generation));
        Some(generation)
    }

    fn mark_plain_text_paste_released(&mut self, event: HotkeyEvent) {
        if self
            .pressed_plain_text_paste
            .is_some_and(|(id, active_generation, _)| {
                id == event.id && active_generation == event.active.generation
            })
        {
            self.pressed_plain_text_paste = None;
        }
    }

    fn is_plain_text_paste_pressed(&self, event: HotkeyEvent, generation: u64) -> bool {
        self.pressed_plain_text_paste
            .is_some_and(|(id, active_generation, press_generation)| {
                id == event.id
                    && active_generation == event.active.generation
                    && press_generation == generation
            })
    }

    fn mark_quick_paste_pressed(&mut self, event: HotkeyEvent) -> bool {
        if self
            .pressed_quick_paste
            .is_some_and(|(id, active_generation, _)| {
                id == event.id && active_generation == event.active.generation
            })
        {
            return false;
        }
        self.next_press_generation = self.next_press_generation.wrapping_add(1);
        self.pressed_quick_paste = Some((
            event.id,
            event.active.generation,
            self.next_press_generation,
        ));
        true
    }

    fn mark_quick_paste_released(&mut self, event: HotkeyEvent) -> bool {
        if self
            .pressed_quick_paste
            .is_some_and(|(id, active_generation, _)| {
                id == event.id && active_generation == event.active.generation
            })
        {
            self.pressed_quick_paste = None;
            true
        } else {
            false
        }
    }

    fn mark_quick_paste_navigation_pressed(&mut self, event: HotkeyEvent) -> Option<u64> {
        if self.quick_paste_navigation_hotkeys.get(&event.id).is_none()
            || self.active_command(event.id) != Some(event.active)
            || self
                .pressed_quick_paste_navigation
                .get(&event.id)
                .is_some_and(|(active_generation, _)| *active_generation == event.active.generation)
        {
            return None;
        }
        self.next_press_generation = self.next_press_generation.wrapping_add(1);
        let generation = self.next_press_generation;
        self.pressed_quick_paste_navigation
            .insert(event.id, (event.active.generation, generation));
        Some(generation)
    }

    fn mark_quick_paste_navigation_released(&mut self, event: HotkeyEvent) {
        if self
            .pressed_quick_paste_navigation
            .get(&event.id)
            .is_some_and(|(active_generation, _)| *active_generation == event.active.generation)
        {
            self.pressed_quick_paste_navigation.remove(&event.id);
        }
    }

    fn is_quick_paste_navigation_pressed(&self, event: HotkeyEvent, generation: u64) -> bool {
        self.quick_paste_navigation_hotkeys.contains_key(&event.id)
            && self.active_command(event.id) == Some(event.active)
            && self
                .pressed_quick_paste_navigation
                .get(&event.id)
                .is_some_and(|(active_generation, press_generation)| {
                    *active_generation == event.active.generation && *press_generation == generation
                })
    }
}

fn set_plain_text_paste_trigger_key(event: HotkeyEvent, cx: &App) {
    #[cfg(target_os = "windows")]
    {
        let trigger_key = cx.read_global::<HotkeyRuntime, _>(|runtime, _| {
            runtime
                .plain_text_paste_hotkey
                .filter(|hotkey| hotkey.id() == event.id)
                .map(|hotkey| hotkey.key)
        });
        if let Some(trigger_key) = trigger_key {
            crate::platform::input::set_trigger_key_code(trigger_key);
        }
    }
}

fn plain_text_paste_is_physically_pressed(event: HotkeyEvent, cx: &App) -> bool {
    #[cfg(target_os = "windows")]
    {
        let modifiers = cx.read_global::<HotkeyRuntime, _>(|runtime, _| {
            runtime
                .plain_text_paste_hotkey
                .filter(|hotkey| hotkey.id() == event.id)
                .map(|hotkey| hotkey.mods)
        });
        return modifiers.is_some_and(|modifiers| {
            crate::platform::input::trigger_shortcut_is_physically_pressed(modifiers)
        });
    }

    #[cfg(not(target_os = "windows"))]
    {
        true
    }
}

fn set_quick_paste_trigger_key(event: HotkeyEvent, cx: &App) {
    #[cfg(target_os = "windows")]
    {
        let trigger_key = cx.read_global::<HotkeyRuntime, _>(|runtime, _| {
            runtime
                .quick_paste_hotkey
                .filter(|hotkey| hotkey.id() == event.id)
                .map(|hotkey| hotkey.key)
        });
        if let Some(trigger_key) = trigger_key {
            crate::platform::input::set_trigger_key_code(trigger_key);
        }
    }
}

fn quick_paste_is_physically_pressed(event: HotkeyEvent, cx: &App) -> bool {
    #[cfg(target_os = "windows")]
    {
        let modifiers = cx.read_global::<HotkeyRuntime, _>(|runtime, _| {
            runtime
                .quick_paste_hotkey
                .filter(|hotkey| hotkey.id() == event.id)
                .map(|hotkey| hotkey.mods)
        });
        return modifiers.is_some_and(|modifiers| {
            crate::platform::input::trigger_shortcut_is_physically_pressed(modifiers)
        });
    }

    #[cfg(not(target_os = "windows"))]
    {
        true
    }
}

fn set_quick_paste_index_trigger_key(event: HotkeyEvent, cx: &App) {
    #[cfg(target_os = "windows")]
    {
        let trigger_key = cx.read_global::<HotkeyRuntime, _>(|runtime, _| {
            runtime
                .quick_paste_index_hotkeys
                .get(&event.id)
                .map(|hotkey| hotkey.key)
        });
        if let Some(trigger_key) = trigger_key {
            crate::platform::input::set_trigger_key_code(trigger_key);
        }
    }
}

fn quick_paste_index_is_physically_pressed(event: HotkeyEvent, cx: &App) -> bool {
    #[cfg(target_os = "windows")]
    {
        let modifiers = cx.read_global::<HotkeyRuntime, _>(|runtime, _| {
            runtime
                .quick_paste_index_hotkeys
                .get(&event.id)
                .map(|hotkey| hotkey.mods)
        });
        return modifiers.is_some_and(|modifiers| {
            crate::platform::input::trigger_shortcut_is_physically_pressed(modifiers)
        });
    }

    #[cfg(not(target_os = "windows"))]
    {
        true
    }
}

#[cfg(target_os = "windows")]
fn spawn_quick_paste_release(event: HotkeyEvent, cx: &mut App) {
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(10))
                .await;
            let released = cx.update(|cx| {
                if !is_hotkey_event_active(event, cx) {
                    return true;
                }
                !quick_paste_is_physically_pressed(event, cx)
            });
            if released {
                break;
            }
        }
        cx.update(|cx| {
            let was_pressed = cx.update_global::<HotkeyRuntime, _>(|runtime, _| {
                runtime.mark_quick_paste_released(event)
            });
            crate::platform::input::clear_trigger_key();
            if was_pressed && is_hotkey_event_active(event, cx) {
                crate::windows::quick_paste::QuickPasteWindowService::finish(cx);
            }
        });
    })
    .detach();
}

fn spawn_plain_text_paste_repeat(event: HotkeyEvent, generation: u64, cx: &mut App) {
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(PLAIN_TEXT_PASTE_INITIAL_REPEAT_DELAY)
            .await;
        loop {
            let keep_repeating = cx.update(|cx| {
                if !is_hotkey_event_active(event, cx)
                    || !cx.read_global::<HotkeyRuntime, _>(|runtime, _| {
                        runtime.is_plain_text_paste_pressed(event, generation)
                    })
                {
                    return false;
                }
                if !plain_text_paste_is_physically_pressed(event, cx) {
                    cx.update_global::<HotkeyRuntime, _>(|runtime, _| {
                        runtime.mark_plain_text_paste_released(event)
                    });
                    #[cfg(target_os = "windows")]
                    crate::platform::input::clear_trigger_key();
                    return false;
                }
                set_plain_text_paste_trigger_key(event, cx);
                crate::windows::main::MainWindowService::paste_plain_text_shortcut(cx);
                true
            });
            if !keep_repeating {
                break;
            }
            cx.background_executor()
                .timer(PLAIN_TEXT_PASTE_REPEAT_INTERVAL)
                .await;
        }
    })
    .detach();
}

fn spawn_session_shortcut_repeat(
    id: SessionShortcutId,
    event: HotkeyEvent,
    generation: u64,
    cx: &mut App,
) {
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(SESSION_SHORTCUT_INITIAL_REPEAT_DELAY)
            .await;
        loop {
            let keep_repeating = cx.update(|cx| {
                if !is_hotkey_event_active(event, cx)
                    || quick_paste_navigation_for_event(event, cx).is_some()
                    || !cx.read_global::<HotkeyRuntime, _>(|runtime, _| {
                        runtime.is_session_hotkey_pressed(event, generation)
                    })
                {
                    return false;
                }
                if crate::windows::main::MainWindowService::is_logically_visible(cx) {
                    let should_dispatch = cx.update_global::<HotkeyRuntime, _>(|runtime, _| {
                        runtime.should_dispatch_session(id)
                    });
                    if should_dispatch {
                        crate::windows::main::dispatch_session_shortcut(id, cx);
                    }
                }
                true
            });
            if !keep_repeating {
                break;
            }
            cx.background_executor()
                .timer(SESSION_SHORTCUT_REPEAT_INTERVAL)
                .await;
        }
    })
    .detach();
}

fn session_shortcut_cooldown(id: SessionShortcutId) -> Option<Duration> {
    matches!(
        id,
        SessionShortcutId::SelectPreviousItem
            | SessionShortcutId::SelectNextItem
            | SessionShortcutId::SelectFirstItem
            | SessionShortcutId::SelectLastItem
            | SessionShortcutId::PreviousTab
            | SessionShortcutId::NextTab
    )
    .then_some(Duration::from_millis(50))
}

fn session_shortcut_is_throttled(
    id: SessionShortcutId,
    last_dispatch: Option<Instant>,
    now: Instant,
) -> bool {
    session_shortcut_cooldown(id)
        .zip(last_dispatch)
        .is_some_and(|(cooldown, last_dispatch)| {
            now.saturating_duration_since(last_dispatch) < cooldown
        })
}

pub(crate) fn registration_status(cx: &App) -> RegistrationStatus {
    cx.read_global::<HotkeyRuntime, _>(|runtime, _| {
        if !runtime.global_enabled {
            RegistrationStatus::Disabled
        } else if runtime.suspended {
            runtime.suspended_main_status.unwrap_or(runtime.status)
        } else {
            runtime.status
        }
    })
}

pub(crate) fn settings_registration_status(cx: &App) -> RegistrationStatus {
    cx.read_global::<HotkeyRuntime, _>(|runtime, _| {
        if !runtime.global_enabled {
            RegistrationStatus::Disabled
        } else if runtime.suspended {
            runtime
                .suspended_settings_status
                .unwrap_or(runtime.settings_status)
        } else {
            runtime.settings_status
        }
    })
}

pub(crate) fn clipboard_monitor_registration_status(cx: &App) -> RegistrationStatus {
    cx.read_global::<HotkeyRuntime, _>(|runtime, _| {
        if !runtime.global_enabled {
            RegistrationStatus::Disabled
        } else if runtime.suspended {
            runtime
                .suspended_clipboard_monitor_status
                .unwrap_or(runtime.clipboard_monitor_status)
        } else {
            runtime.clipboard_monitor_status
        }
    })
}

pub(crate) fn plain_text_paste_registration_status(cx: &App) -> RegistrationStatus {
    cx.read_global::<HotkeyRuntime, _>(|runtime, _| {
        if !runtime.global_enabled {
            RegistrationStatus::Disabled
        } else if runtime.suspended {
            runtime
                .suspended_plain_text_paste_status
                .unwrap_or(runtime.plain_text_paste_status)
        } else {
            runtime.plain_text_paste_status
        }
    })
}

pub(crate) fn quick_paste_registration_status(cx: &App) -> RegistrationStatus {
    cx.read_global::<HotkeyRuntime, _>(|runtime, _| {
        if !runtime.global_enabled {
            RegistrationStatus::Disabled
        } else if runtime.suspended {
            runtime
                .suspended_quick_paste_status
                .unwrap_or(runtime.quick_paste_status)
        } else {
            runtime.quick_paste_status
        }
    })
}

pub(crate) fn session_registration_status(id: SessionShortcutId, cx: &App) -> RegistrationStatus {
    let configured = crate::config_runtime::shortcuts_config(cx)
        .session(id)
        .binding
        .is_some();
    cx.read_global::<HotkeyRuntime, _>(|runtime, _| runtime.status_for_session(id, configured))
}

fn is_hotkey_event_active(event: HotkeyEvent, cx: &App) -> bool {
    cx.read_global::<HotkeyRuntime, _>(|runtime, _| {
        runtime
            .active_commands
            .read()
            .is_ok_and(|commands| commands.get(&event.id) == Some(&event.active))
    })
}

pub(crate) fn quick_paste_index_registration_status(cx: &App) -> RegistrationStatus {
    cx.read_global::<HotkeyRuntime, _>(|runtime, _| {
        if !runtime.global_enabled {
            RegistrationStatus::Disabled
        } else if runtime.suspended {
            runtime
                .suspended_quick_paste_index_status
                .unwrap_or(runtime.quick_paste_index_status)
        } else {
            runtime.quick_paste_index_status
        }
    })
}

pub(crate) fn quick_paste_index_registration_failures(cx: &App) -> Vec<String> {
    cx.read_global::<HotkeyRuntime, _>(|runtime, _| {
        let mut failures = runtime
            .failed_quick_paste_index_hotkeys
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        failures.sort();
        failures
    })
}

fn quick_paste_navigation_for_event(event: HotkeyEvent, cx: &App) -> Option<QuickPasteNavigation> {
    cx.read_global::<HotkeyRuntime, _>(|runtime, _| {
        let registration = runtime.quick_paste_navigation_hotkeys.get(&event.id)?;
        (runtime.active_command(event.id) == Some(event.active)).then_some(registration.navigation)
    })
}

fn spawn_quick_paste_navigation_repeat(
    navigation: QuickPasteNavigation,
    event: HotkeyEvent,
    generation: u64,
    cx: &mut App,
) {
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(QUICK_PASTE_NAVIGATION_INITIAL_REPEAT_DELAY)
            .await;
        loop {
            let keep_repeating = cx.update(|cx| {
                let pressed = cx.read_global::<HotkeyRuntime, _>(|runtime, _| {
                    runtime.is_quick_paste_navigation_pressed(event, generation)
                });
                if !pressed {
                    return false;
                }
                crate::windows::quick_paste::QuickPasteWindowService::navigate(navigation, cx);
                true
            });
            if !keep_repeating {
                break;
            }
            cx.background_executor()
                .timer(QUICK_PASTE_NAVIGATION_REPEAT_INTERVAL)
                .await;
        }
    })
    .detach();
}

pub(crate) fn activate_quick_paste_navigation(config: QuickPasteConfig, cx: &mut App) {
    let result = cx.update_global::<HotkeyRuntime, _>(|runtime, _| {
        runtime.activate_quick_paste_navigation(config)
    });
    if let Err(error) = result {
        tracing::error!("启用便捷粘贴方向键失败：{error}");
    }
}

pub(crate) fn deactivate_quick_paste_navigation(cx: &mut App) {
    let result = cx.update_global::<HotkeyRuntime, _>(|runtime, _| {
        runtime.deactivate_quick_paste_navigation()
    });
    if let Err(error) = result {
        tracing::error!("停用便捷粘贴方向键失败：{error}");
    }
    let (window_visible, pointer_inside) =
        crate::windows::main::MainWindowService::session_shortcut_scope(cx);
    sync_session_shortcuts(window_visible, pointer_inside, cx);
}

pub(crate) fn sync_session_shortcuts(window_visible: bool, pointer_inside: bool, cx: &mut App) {
    let shortcuts = crate::config_runtime::shortcuts_config(cx);
    let previous = cx.read_global::<HotkeyRuntime, _>(|runtime, _| {
        (
            runtime.session_hotkeys.clone(),
            runtime.failed_session_hotkeys.clone(),
        )
    });
    let result = cx.update_global::<HotkeyRuntime, _>(|runtime, _| {
        runtime.sync_session(&shortcuts, window_visible, pointer_inside)
    });
    if let Err(error) = result {
        tracing::error!("同步会话级快捷键失败：{error}");
    }
    let changed = cx.read_global::<HotkeyRuntime, _>(|runtime, _| {
        previous
            != (
                runtime.session_hotkeys.clone(),
                runtime.failed_session_hotkeys.clone(),
            )
    });
    if changed {
        cx.refresh_windows();
    }
}

pub(crate) fn global_hotkeys_enabled(cx: &App) -> bool {
    cx.read_global::<HotkeyRuntime, _>(|runtime, _| runtime.global_enabled)
}

pub(crate) fn toggle_global_hotkeys(cx: &mut App) -> Result<(), String> {
    let enabled = !global_hotkeys_enabled(cx);
    let shortcuts = crate::config_runtime::shortcuts_config(cx);
    let (window_visible, pointer_inside) =
        crate::windows::main::MainWindowService::session_shortcut_scope(cx);
    if !enabled {
        crate::windows::quick_paste::QuickPasteWindowService::cancel(cx);
    }
    let result = cx.update_global::<HotkeyRuntime, _>(|runtime, _| {
        runtime.set_global_enabled(enabled, &shortcuts, window_visible, pointer_inside)
    });
    cx.refresh_windows();
    result
}

fn synchronize_shortcuts(cx: &mut App) -> Result<(), String> {
    let shortcuts = crate::config_runtime::shortcuts_config(cx);
    let (window_visible, pointer_inside) =
        crate::windows::main::MainWindowService::session_shortcut_scope(cx);
    cx.update_global::<HotkeyRuntime, _>(|runtime, _| {
        runtime.resume(&shortcuts, window_visible, pointer_inside)
    })
}

pub(crate) fn set_toggle_main_window_shortcut(
    binding: Option<ShortcutBinding>,
    cx: &mut App,
) -> Result<(), String> {
    let mut shortcuts = crate::config_runtime::shortcuts_config(cx);
    shortcuts.toggle_main_window = binding;
    validate_shortcuts_config(&shortcuts)?;
    crate::config_runtime::set_shortcuts_config(shortcuts, cx)
        .map_err(|error| error.to_string())?;
    synchronize_shortcuts(cx)?;
    cx.refresh_windows();
    Ok(())
}

pub(crate) fn set_middle_mouse_config(
    config: MiddleMouseShortcutConfig,
    cx: &mut App,
) -> Result<(), String> {
    let mut shortcuts = crate::config_runtime::shortcuts_config(cx);
    shortcuts.middle_mouse = config.normalized();
    let config = shortcuts.middle_mouse;
    crate::config_runtime::set_shortcuts_config(shortcuts, cx)
        .map_err(|error| error.to_string())?;
    #[cfg(target_os = "windows")]
    cx.update_global::<HotkeyRuntime, _>(|runtime, _| {
        runtime
            .middle_mouse
            .configure(config, runtime.global_enabled && !runtime.suspended);
    });
    #[cfg(not(target_os = "windows"))]
    let _ = config;
    cx.refresh_windows();
    Ok(())
}

pub(crate) fn set_wheel_mouse_config(
    config: crate::config::WheelShortcutConfig,
    cx: &mut App,
) -> Result<(), String> {
    let mut shortcuts = crate::config_runtime::shortcuts_config(cx);
    shortcuts.wheel_mouse = config.normalized();
    let config = shortcuts.wheel_mouse;
    crate::config_runtime::set_shortcuts_config(shortcuts, cx)
        .map_err(|error| error.to_string())?;
    #[cfg(target_os = "windows")]
    cx.update_global::<HotkeyRuntime, _>(|runtime, _| {
        runtime
            .wheel_mouse
            .configure(config, runtime.global_enabled && !runtime.suspended);
    });
    #[cfg(not(target_os = "windows"))]
    let _ = config;
    cx.refresh_windows();
    Ok(())
}

pub(crate) fn set_open_settings_shortcut(
    binding: Option<ShortcutBinding>,
    cx: &mut App,
) -> Result<(), String> {
    let mut shortcuts = crate::config_runtime::shortcuts_config(cx);
    shortcuts.open_settings = binding;
    validate_shortcuts_config(&shortcuts)?;
    crate::config_runtime::set_shortcuts_config(shortcuts, cx)
        .map_err(|error| error.to_string())?;
    synchronize_shortcuts(cx)?;
    cx.refresh_windows();
    Ok(())
}

pub(crate) fn set_toggle_clipboard_monitor_shortcut(
    binding: Option<ShortcutBinding>,
    cx: &mut App,
) -> Result<(), String> {
    let mut shortcuts = crate::config_runtime::shortcuts_config(cx);
    shortcuts.toggle_clipboard_monitor = binding;
    validate_shortcuts_config(&shortcuts)?;
    crate::config_runtime::set_shortcuts_config(shortcuts, cx)
        .map_err(|error| error.to_string())?;
    synchronize_shortcuts(cx)?;
    cx.refresh_windows();
    Ok(())
}

pub(crate) fn set_plain_text_paste_shortcut(
    binding: Option<ShortcutBinding>,
    cx: &mut App,
) -> Result<(), String> {
    let mut shortcuts = crate::config_runtime::shortcuts_config(cx);
    shortcuts.plain_text_paste = binding;
    validate_shortcuts_config(&shortcuts)?;
    crate::config_runtime::set_shortcuts_config(shortcuts, cx)
        .map_err(|error| error.to_string())?;
    synchronize_shortcuts(cx)?;
    cx.refresh_windows();
    Ok(())
}

pub(crate) fn set_quick_paste_shortcut(
    binding: Option<ShortcutBinding>,
    cx: &mut App,
) -> Result<(), String> {
    let mut shortcuts = crate::config_runtime::shortcuts_config(cx);
    shortcuts.quick_paste = binding;
    validate_shortcuts_config(&shortcuts)?;
    crate::config_runtime::set_shortcuts_config(shortcuts, cx)
        .map_err(|error| error.to_string())?;
    crate::windows::quick_paste::QuickPasteWindowService::cancel(cx);
    synchronize_shortcuts(cx)?;
    cx.refresh_windows();
    Ok(())
}

pub(crate) fn set_quick_paste_index_config(
    config: QuickPasteShortcutConfig,
    cx: &mut App,
) -> Result<(), String> {
    let mut shortcuts = crate::config_runtime::shortcuts_config(cx);
    shortcuts.quick_paste_1_to_9 = config;
    crate::config_runtime::set_shortcuts_config(shortcuts, cx)
        .map_err(|error| error.to_string())?;
    let config = crate::config_runtime::shortcuts_config(cx).quick_paste_1_to_9;
    cx.update_global::<HotkeyRuntime, _>(|runtime, _| {
        runtime.apply_quick_paste_index(&config);
    });
    cx.refresh_windows();
    Ok(())
}

pub(crate) fn set_session_shortcut_binding(
    id: SessionShortcutId,
    binding: Option<ShortcutBinding>,
    cx: &mut App,
) -> Result<(), String> {
    let mut shortcuts = crate::config_runtime::shortcuts_config(cx);
    shortcuts.session_mut(id).binding = binding;
    validate_shortcuts_config(&shortcuts)?;
    crate::config_runtime::set_shortcuts_config(shortcuts, cx)
        .map_err(|error| error.to_string())?;
    synchronize_shortcuts(cx)?;
    cx.refresh_windows();
    Ok(())
}

pub(crate) fn set_session_shortcut_only_in_window(
    id: SessionShortcutId,
    only_in_window: bool,
    cx: &mut App,
) -> Result<(), String> {
    let mut shortcuts = crate::config_runtime::shortcuts_config(cx);
    shortcuts.session_mut(id).only_in_window = only_in_window;
    crate::config_runtime::set_shortcuts_config(shortcuts, cx)
        .map_err(|error| error.to_string())?;
    synchronize_shortcuts(cx)?;
    cx.refresh_windows();
    Ok(())
}

pub(crate) fn set_all_session_shortcuts_only_in_window(
    only_in_window: bool,
    cx: &mut App,
) -> Result<(), String> {
    let mut shortcuts = crate::config_runtime::shortcuts_config(cx);
    for id in SessionShortcutId::ALL {
        shortcuts.session_mut(id).only_in_window = only_in_window;
    }
    crate::config_runtime::set_shortcuts_config(shortcuts, cx)
        .map_err(|error| error.to_string())?;
    synchronize_shortcuts(cx)?;
    cx.refresh_windows();
    Ok(())
}

fn validate_shortcuts_config(shortcuts: &ShortcutsConfig) -> Result<(), String> {
    let mut registered = HashMap::new();
    let mut bindings = Vec::with_capacity(SessionShortcutId::ALL.len() + 5);
    bindings.push(("全局显示/隐藏窗口", shortcuts.toggle_main_window.as_ref()));
    bindings.push(("全局打开设置", shortcuts.open_settings.as_ref()));
    bindings.push((
        "全局切换剪贴板监听",
        shortcuts.toggle_clipboard_monitor.as_ref(),
    ));
    bindings.push(("全局纯文本粘贴", shortcuts.plain_text_paste.as_ref()));
    bindings.push(("全局便捷粘贴", shortcuts.quick_paste.as_ref()));
    bindings.extend(
        SessionShortcutId::ALL
            .into_iter()
            .map(|id| (id.label_key(), shortcuts.session(id).binding.as_ref())),
    );

    for (name, binding) in bindings {
        let Some(binding) = binding else {
            continue;
        };
        let hotkey = binding_to_hotkey(binding)?;
        if let Some(previous) = registered.insert(hotkey.id(), name) {
            return Err(format!("快捷键与“{previous}”重复"));
        }
    }
    if shortcuts.quick_paste_1_to_9.enabled {
        for index in 0..9 {
            let binding = shortcuts.quick_paste_1_to_9.binding(index);
            let hotkey = binding_to_hotkey(&binding)?;
            if let Some(previous) = registered.insert(hotkey.id(), "全局快捷粘贴 1~9") {
                return Err(format!("快捷键与“{previous}”重复"));
            }
        }
    }
    Ok(())
}

pub(crate) fn reset_toggle_main_window_shortcut(cx: &mut App) -> Result<(), String> {
    set_toggle_main_window_shortcut(default_toggle_main_window_shortcut(), cx)
}

pub(crate) fn reset_open_settings_shortcut(cx: &mut App) -> Result<(), String> {
    set_open_settings_shortcut(None, cx)
}

pub(crate) fn reset_toggle_clipboard_monitor_shortcut(cx: &mut App) -> Result<(), String> {
    set_toggle_clipboard_monitor_shortcut(None, cx)
}

pub(crate) fn reset_plain_text_paste_shortcut(cx: &mut App) -> Result<(), String> {
    set_plain_text_paste_shortcut(None, cx)
}

pub(crate) fn reset_quick_paste_shortcut(cx: &mut App) -> Result<(), String> {
    set_quick_paste_shortcut(None, cx)
}

pub(crate) fn reset_session_shortcut_binding(
    id: SessionShortcutId,
    cx: &mut App,
) -> Result<(), String> {
    let binding = ShortcutsConfig::default().session(id).binding.clone();
    set_session_shortcut_binding(id, binding, cx)
}

pub(crate) fn reset_shortcuts(cx: &mut App) -> Result<(), String> {
    crate::windows::quick_paste::QuickPasteWindowService::cancel(cx);
    if let Err(error) = cancel_toggle_main_window_recording(cx) {
        tracing::warn!(%error, "重置快捷键前恢复当前快捷键失败");
    }
    crate::config_runtime::reset_shortcuts_config(cx).map_err(|error| error.to_string())?;
    synchronize_shortcuts(cx)?;
    cx.refresh_windows();
    Ok(())
}

pub(crate) fn begin_toggle_main_window_recording(cx: &mut App) -> Result<(), String> {
    crate::windows::quick_paste::QuickPasteWindowService::cancel(cx);
    let result = cx.update_global::<HotkeyRuntime, _>(|runtime, _| runtime.suspend());
    if result.is_ok() {
        cx.refresh_windows();
    }
    result
}

pub(crate) fn cancel_toggle_main_window_recording(cx: &mut App) -> Result<(), String> {
    let result = synchronize_shortcuts(cx);
    if result.is_ok() {
        cx.refresh_windows();
    }
    result
}

pub(crate) fn reject_shortcut_recording(global_shortcut: bool, cx: &mut App) {
    if let Err(error) = cancel_toggle_main_window_recording(cx) {
        tracing::warn!(%error, "拒绝新快捷键后恢复原快捷键失败");
    }
    if global_shortcut {
        cx.update_global::<HotkeyRuntime, _>(|runtime, _| {
            runtime.status = RegistrationStatus::Failed
        });
    }
    cx.refresh_windows();
}

pub(crate) fn reject_open_settings_recording(cx: &mut App) {
    if let Err(error) = cancel_toggle_main_window_recording(cx) {
        tracing::warn!(%error, "拒绝新快捷键后恢复原快捷键失败");
    }
    cx.update_global::<HotkeyRuntime, _>(|runtime, _| {
        runtime.settings_status = RegistrationStatus::Failed
    });
    cx.refresh_windows();
}

pub(crate) fn reject_toggle_clipboard_monitor_recording(cx: &mut App) {
    if let Err(error) = cancel_toggle_main_window_recording(cx) {
        tracing::warn!(%error, "拒绝新快捷键后恢复原快捷键失败");
    }
    cx.update_global::<HotkeyRuntime, _>(|runtime, _| {
        runtime.clipboard_monitor_status = RegistrationStatus::Failed
    });
    cx.refresh_windows();
}

pub(crate) fn reject_plain_text_paste_recording(cx: &mut App) {
    if let Err(error) = cancel_toggle_main_window_recording(cx) {
        tracing::warn!(%error, "拒绝新快捷键后恢复原快捷键失败");
    }
    cx.update_global::<HotkeyRuntime, _>(|runtime, _| {
        runtime.plain_text_paste_status = RegistrationStatus::Failed
    });
    cx.refresh_windows();
}

pub(crate) fn reject_quick_paste_recording(cx: &mut App) {
    if let Err(error) = cancel_toggle_main_window_recording(cx) {
        tracing::warn!(%error, "拒绝新快捷键后恢复原快捷键失败");
    }
    cx.update_global::<HotkeyRuntime, _>(|runtime, _| {
        runtime.quick_paste_status = RegistrationStatus::Failed
    });
    cx.refresh_windows();
}

pub(crate) fn default_toggle_main_window_shortcut() -> Option<ShortcutBinding> {
    Some(ShortcutBinding::toggle_main_window_default())
}

pub(crate) fn is_win_v_binding(binding: &ShortcutBinding) -> bool {
    !binding.control
        && !binding.alt
        && !binding.shift
        && binding.platform
        && binding.key.eq_ignore_ascii_case("v")
}

pub(crate) fn binding_to_keystroke(binding: &ShortcutBinding) -> Keystroke {
    Keystroke {
        modifiers: Modifiers {
            control: binding.control,
            alt: binding.alt,
            shift: binding.shift,
            platform: binding.platform,
            function: false,
        },
        key: binding.key.clone(),
        key_char: None,
    }
}

pub(crate) fn format_shortcut_binding(binding: &ShortcutBinding) -> String {
    Kbd::format(&binding_to_keystroke(binding))
}

pub(crate) fn shortcut_binding_from_keystroke(
    keystroke: &Keystroke,
) -> Result<ShortcutBinding, String> {
    if keystroke.modifiers.function {
        return Err("快捷键不支持 Fn 修饰键".to_owned());
    }
    if is_modifier_key(&keystroke.key) {
        return Err("请至少选择一个非修饰键".to_owned());
    }

    let (key, shift) = recorded_key(&keystroke.key, keystroke.modifiers.shift);
    Ok(ShortcutBinding {
        control: keystroke.modifiers.control,
        alt: keystroke.modifiers.alt,
        shift,
        platform: keystroke.modifiers.platform,
        key,
    })
}

fn binding_to_hotkey(binding: &ShortcutBinding) -> Result<HotKey, String> {
    if binding.key.trim().is_empty() {
        return Err("快捷键缺少主按键".to_owned());
    }

    let mut parts = Vec::with_capacity(5);
    if binding.control {
        parts.push("control".to_owned());
    }
    if binding.alt {
        parts.push("alt".to_owned());
    }
    if binding.shift {
        parts.push("shift".to_owned());
    }
    if binding.platform {
        parts.push("super".to_owned());
    }
    parts.push(global_key_token(&binding.key));
    parts.join("+").parse::<HotKey>().map_err(|error| {
        format!(
            "不支持的快捷键 {}：{error}",
            format_shortcut_binding(binding)
        )
    })
}

fn recorded_key(key: &str, shift: bool) -> (String, bool) {
    let base_key = match key {
        "!" => Some("1"),
        "@" => Some("2"),
        "#" => Some("3"),
        "$" => Some("4"),
        "%" => Some("5"),
        "^" => Some("6"),
        "&" => Some("7"),
        "*" => Some("8"),
        "(" => Some("9"),
        ")" => Some("0"),
        "_" => Some("-"),
        "+" => Some("="),
        "{" => Some("["),
        "}" => Some("]"),
        "|" => Some("\\"),
        ":" => Some(";"),
        "\"" => Some("'"),
        "<" => Some(","),
        ">" => Some("."),
        "?" => Some("/"),
        "~" => Some("`"),
        _ => None,
    };

    match base_key {
        Some(key) => (key.to_owned(), true),
        None => (key.to_ascii_lowercase(), shift),
    }
}

fn global_key_token(key: &str) -> String {
    match key.to_ascii_lowercase().as_str() {
        "up" | "arrowup" => "ArrowUp".to_owned(),
        "down" | "arrowdown" => "ArrowDown".to_owned(),
        "left" | "arrowleft" => "ArrowLeft".to_owned(),
        "right" | "arrowright" => "ArrowRight".to_owned(),
        "-" => "Minus".to_owned(),
        "=" => "Equal".to_owned(),
        "[" => "BracketLeft".to_owned(),
        "]" => "BracketRight".to_owned(),
        "\\" => "Backslash".to_owned(),
        "`" => "Backquote".to_owned(),
        "," => "Comma".to_owned(),
        "." => "Period".to_owned(),
        "'" => "Quote".to_owned(),
        ";" => "Semicolon".to_owned(),
        "/" => "Slash".to_owned(),
        value if value.len() == 1 && value.as_bytes()[0].is_ascii_alphabetic() => {
            format!("Key{}", value.to_ascii_uppercase())
        }
        value if value.len() == 1 && value.as_bytes()[0].is_ascii_digit() => {
            format!("Digit{value}")
        }
        value => value.to_owned(),
    }
}

fn is_modifier_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "control" | "ctrl" | "alt" | "shift" | "super" | "cmd" | "command" | "win" | "meta" | "fn"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ShortcutsConfig;

    #[test]
    fn recorded_shortcut_should_convert_to_binding() {
        let keystroke = Keystroke::parse("ctrl-shift-a").expect("应解析测试按键");
        let binding = shortcut_binding_from_keystroke(&keystroke).expect("应转换录制按键");

        assert_eq!(format_shortcut_binding(&binding), "Ctrl+Shift+A");
        assert!(binding_to_hotkey(&binding).is_ok());
    }

    #[test]
    fn modifier_only_shortcut_should_be_rejected() {
        let keystroke = Keystroke {
            modifiers: Modifiers::default(),
            key: "control".to_owned(),
            key_char: None,
        };

        assert!(shortcut_binding_from_keystroke(&keystroke).is_err());
    }

    #[test]
    fn shifted_symbols_should_keep_their_base_key() {
        let keystroke = Keystroke {
            modifiers: Modifiers::default(),
            key: "!".to_owned(),
            key_char: Some("!".to_owned()),
        };

        let binding = shortcut_binding_from_keystroke(&keystroke).expect("应转换移位符号");
        assert_eq!(binding.key, "1");
        assert!(binding.shift);
    }

    #[test]
    fn default_shortcut_config_should_use_ctrl_shift_x() {
        assert_eq!(
            ShortcutsConfig::default().toggle_main_window,
            Some(ShortcutBinding::toggle_main_window_default())
        );
    }

    #[test]
    fn default_session_shortcuts_should_be_valid_and_unique() {
        let shortcuts = ShortcutsConfig::default();
        let mut registered_ids = Vec::new();
        let main_hotkey = binding_to_hotkey(
            shortcuts
                .toggle_main_window
                .as_ref()
                .expect("应配置全局快捷键"),
        )
        .expect("全局快捷键应有效");
        registered_ids.push(main_hotkey.id());

        for id in SessionShortcutId::ALL {
            let shortcut = shortcuts.session(id);
            assert!(!shortcut.only_in_window);
            let hotkey =
                binding_to_hotkey(shortcut.binding.as_ref().expect("默认窗口快捷键应有绑定"))
                    .expect("默认窗口快捷键应有效");
            assert!(!registered_ids.contains(&hotkey.id()), "默认快捷键不能重复");
            registered_ids.push(hotkey.id());
        }
    }

    #[test]
    fn navigation_shortcuts_should_be_throttled_for_50_milliseconds() {
        let now = Instant::now();
        for id in [
            SessionShortcutId::SelectPreviousItem,
            SessionShortcutId::SelectNextItem,
            SessionShortcutId::PreviousTab,
            SessionShortcutId::NextTab,
        ] {
            assert!(session_shortcut_is_throttled(
                id,
                Some(now),
                now + Duration::from_millis(49)
            ));
            assert!(!session_shortcut_is_throttled(
                id,
                Some(now),
                now + Duration::from_millis(50)
            ));
        }
    }

    #[test]
    fn non_navigation_shortcuts_should_not_be_throttled() {
        assert!(!session_shortcut_is_throttled(
            SessionShortcutId::ExecutePaste,
            Some(Instant::now()),
            Instant::now()
        ));
    }
}
