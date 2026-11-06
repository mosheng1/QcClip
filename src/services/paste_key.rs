//! 决定写入剪贴板后向前台程序模拟哪组粘贴按键。

use std::sync::Mutex;

use crate::config::{AppConfig, PasteKeyMode};

/// 实际模拟的粘贴组合键。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PasteKey {
    CtrlV,
    ShiftInsert,
}

/// 本程序快捷键是否占用了系统粘贴组合键；被占用的组合键会被全局热键拦截，
/// 模拟后无法到达前台程序，还可能再次触发本程序的快捷键。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PasteKeysInUse {
    pub(crate) ctrl_v: bool,
    pub(crate) shift_insert: bool,
}

#[derive(Clone, Copy, Debug)]
struct PasteKeyPolicy {
    mode: PasteKeyMode,
    in_use: PasteKeysInUse,
}

static POLICY: Mutex<PasteKeyPolicy> = Mutex::new(PasteKeyPolicy {
    mode: PasteKeyMode::Auto,
    in_use: PasteKeysInUse {
        ctrl_v: false,
        shift_insert: false,
    },
});

/// 默认不响应 Ctrl+V、但支持 Shift+Insert 粘贴的终端程序（进程名，不含扩展名，小写）。
const SHIFT_INSERT_PROCESSES: &[&str] = &[
    "putty",
    "kitty",
    "mintty",
    "xshell",
    "securecrt",
    "mobaxterm",
];

/// 由配置运行时在加载配置、修改粘贴方式或快捷键后调用。
pub(crate) fn sync(config: &AppConfig) {
    let policy = PasteKeyPolicy {
        mode: config.general.paste_key_mode,
        in_use: crate::hotkey::paste_keys_in_use(&config.shortcuts),
    };
    match POLICY.lock() {
        Ok(mut guard) => *guard = policy,
        Err(poisoned) => *poisoned.into_inner() = policy,
    }
}

/// `foreground_process` 为前台窗口所属进程名（不含扩展名），仅“自动”模式使用。
pub(crate) fn current(foreground_process: Option<&str>) -> PasteKey {
    let policy = match POLICY.lock() {
        Ok(guard) => *guard,
        Err(poisoned) => *poisoned.into_inner(),
    };
    resolve(policy.mode, policy.in_use, foreground_process)
}

fn resolve(
    mode: PasteKeyMode,
    in_use: PasteKeysInUse,
    foreground_process: Option<&str>,
) -> PasteKey {
    match mode {
        PasteKeyMode::CtrlV => PasteKey::CtrlV,
        PasteKeyMode::ShiftInsert => PasteKey::ShiftInsert,
        PasteKeyMode::Auto => match (in_use.ctrl_v, in_use.shift_insert) {
            (true, false) => PasteKey::ShiftInsert,
            (false, true) => PasteKey::CtrlV,
            // 两者都被占用时没有可用的替代按键，保持最通用的 Ctrl+V。
            (true, true) => PasteKey::CtrlV,
            (false, false) if foreground_process.is_some_and(prefers_shift_insert) => {
                PasteKey::ShiftInsert
            }
            (false, false) => PasteKey::CtrlV,
        },
    }
}

fn prefers_shift_insert(process: &str) -> bool {
    SHIFT_INSERT_PROCESSES
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(process))
}

#[cfg(test)]
mod tests {
    use super::{PasteKey, PasteKeysInUse, resolve};
    use crate::config::{AppConfig, PasteKeyMode, ShortcutBinding};

    const FREE: PasteKeysInUse = PasteKeysInUse {
        ctrl_v: false,
        shift_insert: false,
    };

    #[test]
    fn explicit_modes_ignore_conflicts_and_foreground() {
        let busy = PasteKeysInUse {
            ctrl_v: true,
            shift_insert: true,
        };
        assert_eq!(
            resolve(PasteKeyMode::CtrlV, busy, Some("putty")),
            PasteKey::CtrlV
        );
        assert_eq!(
            resolve(PasteKeyMode::ShiftInsert, FREE, None),
            PasteKey::ShiftInsert
        );
    }

    #[test]
    fn auto_avoids_keys_taken_by_shortcuts() {
        let ctrl_v = PasteKeysInUse {
            ctrl_v: true,
            shift_insert: false,
        };
        let shift_insert = PasteKeysInUse {
            ctrl_v: false,
            shift_insert: true,
        };
        assert_eq!(
            resolve(PasteKeyMode::Auto, ctrl_v, None),
            PasteKey::ShiftInsert
        );
        assert_eq!(
            resolve(PasteKeyMode::Auto, shift_insert, Some("PuTTY")),
            PasteKey::CtrlV
        );
    }

    #[test]
    fn auto_uses_shift_insert_for_terminals_without_ctrl_v() {
        assert_eq!(resolve(PasteKeyMode::Auto, FREE, None), PasteKey::CtrlV);
        assert_eq!(
            resolve(PasteKeyMode::Auto, FREE, Some("notepad")),
            PasteKey::CtrlV
        );
        assert_eq!(
            resolve(PasteKeyMode::Auto, FREE, Some("PuTTY")),
            PasteKey::ShiftInsert
        );
        assert_eq!(
            resolve(PasteKeyMode::Auto, FREE, Some("mintty")),
            PasteKey::ShiftInsert
        );
    }

    #[test]
    fn shortcut_conflicts_require_exact_modifiers() {
        let binding = |control, shift, key: &str| ShortcutBinding {
            control,
            alt: false,
            shift,
            platform: false,
            key: key.to_owned(),
        };
        let mut config = AppConfig::default();
        assert_eq!(crate::hotkey::paste_keys_in_use(&config.shortcuts), FREE);

        config.shortcuts.plain_text_paste = Some(binding(true, true, "v"));
        assert_eq!(crate::hotkey::paste_keys_in_use(&config.shortcuts), FREE);

        config.shortcuts.plain_text_paste = Some(binding(true, false, "v"));
        config.shortcuts.execute_paste.binding = Some(binding(false, true, "insert"));
        assert_eq!(
            crate::hotkey::paste_keys_in_use(&config.shortcuts),
            PasteKeysInUse {
                ctrl_v: true,
                shift_insert: true,
            }
        );
    }
}
