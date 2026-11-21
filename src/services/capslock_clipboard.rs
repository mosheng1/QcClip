//! CapsLock 双剪贴板服务。
//!
//! 提供两套只在内存中保留最新值的槽位：
//! - A 槽：`CapsLock + C / X / V`
//! - B 槽：`CapsLock + Alt + C / X / V`
//!
//! 槽位不进历史、不落库、不参与同步与清理；默认剪贴板（Ctrl+C/X/V + 被动监听）
//! 完全不受影响。钩子回调只投递命令，全部剪贴板 IO 在独立工作线程串行执行。

use std::{
    sync::{
        Mutex, MutexGuard, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant},
};

use crate::config::CapslockClipboardConfig;
use crate::platform::{
    capslock_keys::{self, CapslockCommand, CapslockHook, CapslockSlot},
    clipboard::{self, ClipboardPayload, ClipboardWriteSuppressor, SelectionShortcut},
};

/// 恢复系统剪贴板的体积上限；超过后只保证不污染默认历史，跳过恢复。
const MAX_RESTORE_BYTES: usize = 32 * 1024 * 1024;
/// 粘贴后等待目标应用异步读取剪贴板，再恢复系统剪贴板。
const PASTE_RESTORE_DELAY: Duration = Duration::from_millis(300);
/// 工作线程无待恢复任务时的最长等待，仅用于周期性检查。
const IDLE_WAIT: Duration = Duration::from_secs(5);
/// 钩子命令队列容量；满时由钩子侧丢弃，绝不阻塞输入。
/// 连发由工作线程合并消化，小队列足够。
const COMMAND_QUEUE_CAPACITY: usize = 8;

static SERVICE: OnceLock<Mutex<Option<Service>>> = OnceLock::new();
static SUPPRESSOR: OnceLock<ClipboardWriteSuppressor> = OnceLock::new();
static PRIMARY_ENABLED: AtomicBool = AtomicBool::new(true);
static ALT_ENABLED: AtomicBool = AtomicBool::new(true);
static HOTKEYS_ACTIVE: AtomicBool = AtomicBool::new(true);

/// 安装服务；仅在应用启动时调用一次，后续通过 [`set_config`] 同步设置。
pub(crate) fn install(suppressor: ClipboardWriteSuppressor, config: CapslockClipboardConfig) {
    let _ = SUPPRESSOR.set(suppressor);
    apply(config);
}

/// 热更新配置：开关功能或切换“粘贴后恢复系统剪贴板”。
pub(crate) fn set_config(config: CapslockClipboardConfig) {
    apply(config);
}

/// 跟随全局快捷键的暂停/恢复状态，应用过滤命中时一并停用。
pub(crate) fn set_global_hotkeys_active(active: bool) {
    HOTKEYS_ACTIVE.store(active, Ordering::SeqCst);
    if slots_configured() {
        capslock_keys::set_enabled(active);
    }
}

fn slots_configured() -> bool {
    PRIMARY_ENABLED.load(Ordering::SeqCst) || ALT_ENABLED.load(Ordering::SeqCst)
}

fn apply(config: CapslockClipboardConfig) {
    PRIMARY_ENABLED.store(config.primary_enabled, Ordering::SeqCst);
    ALT_ENABLED.store(config.alt_enabled, Ordering::SeqCst);
    let configured = config.primary_enabled || config.alt_enabled;

    #[cfg(not(target_os = "windows"))]
    {
        let _ = configured;
        return;
    }

    #[cfg(target_os = "windows")]
    {
        let mut service = lock_service();
        if !configured {
            // 两个槽位都关闭时才卸载钩子。
            if service.take().is_some() {
                tracing::info!("CapsLock 双剪贴板已停用");
            }
            return;
        }
        capslock_keys::set_slots_enabled(config.primary_enabled, config.alt_enabled);
        if service.is_none() {
            match start_service() {
                Ok(started) => {
                    *service = Some(started);
                    tracing::info!("CapsLock 双剪贴板已启动");
                }
                Err(error) => {
                    tracing::error!("无法启动 CapsLock 双剪贴板：{error}");
                    return;
                }
            }
        }
        // 与全局快捷键暂停状态保持一致；仅暂停时不卸载钩子，恢复时可直接生效。
        capslock_keys::set_enabled(HOTKEYS_ACTIVE.load(Ordering::SeqCst));
    }
}

fn lock_service() -> MutexGuard<'static, Option<Service>> {
    match service_slot().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn service_slot() -> &'static Mutex<Option<Service>> {
    SERVICE.get_or_init(|| Mutex::new(None))
}

struct Service {
    hook: Option<CapslockHook>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Drop for Service {
    fn drop(&mut self) {
        // 钩子线程退出时丢弃命令发送端，工作线程随后收到断开通知并处理完待恢复项。
        drop(self.hook.take());
        // 不在 UI 线程等待工作线程：恢复等待与剪贴板写入可能持续数百毫秒。
        // 发送端已随钩子释放，工作线程处理完剩余命令后会自行退出。
        if let Some(worker) = self.worker.take() {
            let _ = thread::Builder::new()
                .name("qcclip-capslock-reaper".to_owned())
                .spawn(move || {
                    if worker.join().is_err() {
                        tracing::error!("CapsLock 剪贴板工作线程异常退出");
                    }
                });
        }
    }
}

fn start_service() -> Result<Service, String> {
    let suppressor = SUPPRESSOR
        .get()
        .cloned()
        .ok_or_else(|| "CapsLock 剪贴板服务未初始化".to_owned())?;
    let (sender, receiver) = mpsc::sync_channel::<CapslockCommand>(COMMAND_QUEUE_CAPACITY);
    let worker = thread::Builder::new()
        .name("qcclip-capslock-clipboard".to_owned())
        .spawn(move || run_worker(receiver, suppressor))
        .map_err(|error| format!("无法启动 CapsLock 剪贴板线程：{error}"))?;
    match CapslockHook::install(sender) {
        Ok(hook) => Ok(Service {
            hook: Some(hook),
            worker: Some(worker),
        }),
        Err(error) => {
            // 发送端随失败返回一同释放，工作线程会退出。
            let _ = worker.join();
            Err(error)
        }
    }
}

#[derive(Default)]
struct Slots {
    primary: Option<Vec<ClipboardPayload>>,
    alt: Option<Vec<ClipboardPayload>>,
}

impl Slots {
    fn get(&self, slot: CapslockSlot) -> Option<&[ClipboardPayload]> {
        match slot {
            CapslockSlot::Primary => self.primary.as_deref(),
            CapslockSlot::Alt => self.alt.as_deref(),
        }
    }

    fn store(&mut self, slot: CapslockSlot, payloads: Vec<ClipboardPayload>) {
        match slot {
            CapslockSlot::Primary => self.primary = Some(payloads),
            CapslockSlot::Alt => self.alt = Some(payloads),
        }
    }
}

/// 操作前的系统剪贴板快照。
#[derive(Clone)]
enum PreviousClipboard {
    /// 操作前剪贴板为空；恢复阶段需要显式清空，避免槽位内容残留。
    Empty,
    Payloads(Vec<ClipboardPayload>),
}

impl PreviousClipboard {
    fn restorable(&self) -> bool {
        match self {
            Self::Empty => true,
            Self::Payloads(payloads) => total_bytes(payloads) <= MAX_RESTORE_BYTES,
        }
    }
}

struct PendingRestore {
    deadline: Instant,
    payloads: PreviousClipboard,
    /// 槽位写入系统剪贴板后的序列号；用于检测外部（原生复制等）是否动过剪贴板。
    written_seq: u32,
    slot: CapslockSlot,
}

/// 最近一次"恢复完成"后的基线：序列号未变时可直接复用，省一次剪贴板读取。
#[derive(Clone)]
struct Baseline {
    seq: u32,
    payloads: Vec<ClipboardPayload>,
}

#[derive(Default)]
struct WorkerState {
    slots: Slots,
    pending: Option<PendingRestore>,
    baseline: Option<Baseline>,
}

fn run_worker(receiver: Receiver<CapslockCommand>, suppressor: ClipboardWriteSuppressor) {
    let mut state = WorkerState::default();
    loop {
        let timeout = state.pending.as_ref().map_or(IDLE_WAIT, |pending| {
            pending.deadline.saturating_duration_since(Instant::now())
        });
        match receiver.recv_timeout(timeout) {
            Ok(command) => {
                let mut commands = vec![command];
                // 连发时钩子会以键盘重复率投递命令；取出积压并合并后再串行处理。
                while let Ok(more) = receiver.try_recv() {
                    commands.push(more);
                }
                for command in coalesce_commands(commands) {
                    tracing::debug!(?command, "CapsLock 剪贴板命令");
                    if handle_pending_before_command(&mut state, &command, &suppressor) {
                        continue;
                    }
                    handle_command(command, &mut state, &suppressor);
                }
            }
            Err(RecvTimeoutError::Timeout) => flush_restore(&mut state, &suppressor),
            Err(RecvTimeoutError::Disconnected) => {
                flush_restore(&mut state, &suppressor);
                break;
            }
        }
    }
    tracing::debug!("CapsLock 剪贴板工作线程已退出");
}

/// 合并连续重复的命令，只保留最后一条；不同命令保持顺序。
fn coalesce_commands(commands: Vec<CapslockCommand>) -> Vec<CapslockCommand> {
    let mut merged: Vec<CapslockCommand> = Vec::with_capacity(commands.len());
    for command in commands {
        if merged.last() == Some(&command) {
            continue;
        }
        merged.push(command);
    }
    merged
}

/// 处理新命令前同步"待恢复"状态；返回 `true` 表示命令已被快路径消化。
fn handle_pending_before_command(
    state: &mut WorkerState,
    command: &CapslockCommand,
    suppressor: &ClipboardWriteSuppressor,
) -> bool {
    let Some(mut pending) = state.pending.take() else {
        return false;
    };
    if clipboard::current_sequence_number() != pending.written_seq {
        // 粘贴之后用户又做了原生复制等操作：保留用户的新内容，不写回旧快照。
        tracing::debug!("CapsLock 待恢复内容已被外部写入覆盖，放弃恢复");
        state.baseline = None;
        return false;
    }
    if let CapslockCommand::Paste(slot) = command
        && *slot == pending.slot
    {
        // 连发快路径：系统剪贴板仍是槽位内容，只需补发粘贴键。
        tracing::debug!(?slot, "CapsLock 连发快路径：补发粘贴键");
        pending.deadline = Instant::now() + PASTE_RESTORE_DELAY;
        state.pending = Some(pending);
        if let Err(error) = clipboard::resend_paste(*slot == CapslockSlot::Primary) {
            tracing::warn!(%error, "CapsLock 连发粘贴失败");
        }
        return true;
    }
    // 其他命令：先把系统剪贴板恢复为操作前内容，再正常处理。
    restore_baseline(state, Some(pending.payloads), suppressor);
    false
}

fn handle_command(
    command: CapslockCommand,
    state: &mut WorkerState,
    suppressor: &ClipboardWriteSuppressor,
) {
    let slot = match command {
        CapslockCommand::Copy(slot) | CapslockCommand::Cut(slot) | CapslockCommand::Paste(slot) => {
            slot
        }
    };
    // 槽位可能在命令排队期间被设置页关闭，执行前再确认一次。
    if !slot_enabled(slot) {
        tracing::debug!(?slot, "CapsLock 对应剪贴板已关闭，忽略命令");
        return;
    }
    // 前台应用命中“指定应用”过滤时，不执行任何 CapsLock 剪贴板操作。
    if crate::services::application_filter::foreground_filter_matched() {
        tracing::debug!(?command, "前台应用命中应用过滤，忽略 CapsLock 命令");
        return;
    }
    match command {
        CapslockCommand::Copy(slot) => copy_slot(slot, false, state, suppressor),
        CapslockCommand::Cut(slot) => copy_slot(slot, true, state, suppressor),
        CapslockCommand::Paste(slot) => paste_slot(slot, state, suppressor),
    }
}

fn slot_enabled(slot: CapslockSlot) -> bool {
    match slot {
        CapslockSlot::Primary => PRIMARY_ENABLED.load(Ordering::SeqCst),
        CapslockSlot::Alt => ALT_ENABLED.load(Ordering::SeqCst),
    }
}

fn copy_slot(
    slot: CapslockSlot,
    cut: bool,
    state: &mut WorkerState,
    suppressor: &ClipboardWriteSuppressor,
) {
    let previous = snapshot_for_operation(state);
    let shortcut = if cut {
        SelectionShortcut::Cut
    } else {
        SelectionShortcut::Copy
    };
    // A 槽没有 Alt 参与，模拟按键后正常恢复修饰键；B 槽的 Alt 已被钩子吞掉。
    let restore_alt = slot == CapslockSlot::Primary;
    match clipboard::capture_selection_with_shortcut(suppressor, shortcut, restore_alt) {
        Ok(Some(captured)) => {
            let payloads = clipboard::captured_to_payloads(&captured);
            if payloads.is_empty() {
                tracing::debug!("CapsLock 捕获结果没有可还原格式，保留旧值");
            } else {
                tracing::debug!(
                    ?slot,
                    formats = payloads.len(),
                    bytes = total_bytes(&payloads),
                    "CapsLock 槽位已更新"
                );
                state.slots.store(slot, payloads);
                crate::services::sound::play(crate::services::sound::SoundEvent::Copy);
            }
        }
        Ok(None) => tracing::debug!("CapsLock 未检测到选区或剪贴板变化，保留旧值"),
        Err(error) => tracing::warn!(%error, "CapsLock 捕获选区失败，保留旧值"),
    }
    restore_baseline(state, previous, suppressor);
}

fn paste_slot(slot: CapslockSlot, state: &mut WorkerState, suppressor: &ClipboardWriteSuppressor) {
    let Some(payloads) = state.slots.get(slot).map(<[ClipboardPayload]>::to_vec) else {
        tracing::debug!("CapsLock 剪贴板为空，忽略粘贴");
        return;
    };
    let previous = snapshot_for_operation(state);
    if let Err(error) = suppressor.write_with_suppression(|| clipboard::write_payloads(&payloads)) {
        tracing::warn!(%error, "写入 CapsLock 剪贴板失败");
        return;
    }
    let written_seq = clipboard::current_sequence_number();
    if let Err(error) = clipboard::simulate_paste(slot == CapslockSlot::Primary) {
        tracing::warn!(%error, "模拟粘贴失败");
        // 剪贴板已被本次写入替换，仍需恢复操作前内容，避免槽位内容残留。
        restore_baseline(state, previous, suppressor);
        return;
    }
    crate::services::sound::play(crate::services::sound::SoundEvent::Paste);

    match previous.filter(PreviousClipboard::restorable) {
        Some(previous) => {
            // 延迟恢复：目标应用可能异步读取剪贴板，过早写回会粘贴到旧内容。
            // 连发快路径会顺延 deadline，只有松手空闲后才真正恢复。
            state.pending = Some(PendingRestore {
                deadline: Instant::now() + PASTE_RESTORE_DELAY,
                payloads: previous,
                written_seq,
                slot,
            });
        }
        None => {
            // 操作前剪贴板读取失败或超过恢复上限：保持现状，只保证不污染默认历史。
            state.baseline = None;
        }
    }
}

/// 到达恢复期限或线程退出时执行恢复。
fn flush_restore(state: &mut WorkerState, suppressor: &ClipboardWriteSuppressor) {
    let Some(pending) = state.pending.take() else {
        return;
    };
    if clipboard::current_sequence_number() != pending.written_seq {
        tracing::debug!("CapsLock 待恢复内容已被外部写入覆盖，放弃恢复");
        state.baseline = None;
        return;
    }
    restore_baseline(state, Some(pending.payloads), suppressor);
}

/// 把操作前内容写回系统剪贴板，并记录恢复后的基线供下次复用。
///
/// “写回”包括操作前剪贴板为空的情况：此时显式清空剪贴板，避免槽位内容残留。
fn restore_baseline(
    state: &mut WorkerState,
    previous: Option<PreviousClipboard>,
    suppressor: &ClipboardWriteSuppressor,
) {
    match previous {
        Some(PreviousClipboard::Payloads(payloads))
            if total_bytes(&payloads) <= MAX_RESTORE_BYTES =>
        {
            match suppressor.write_with_suppression(|| clipboard::write_payloads(&payloads)) {
                Ok(()) => {
                    state.baseline = Some(Baseline {
                        seq: clipboard::current_sequence_number(),
                        payloads,
                    });
                }
                Err(error) => {
                    tracing::warn!(%error, "恢复系统剪贴板失败");
                    state.baseline = None;
                }
            }
        }
        Some(PreviousClipboard::Empty) => {
            if let Err(error) = suppressor.write_with_suppression(clipboard::clear_clipboard) {
                tracing::warn!(%error, "清空系统剪贴板失败");
            }
            state.baseline = None;
        }
        // 超过恢复上限或操作前内容无法读取：跳过恢复，不做破坏性写入。
        Some(PreviousClipboard::Payloads(_)) | None => state.baseline = None,
    }
}

/// 获取本次操作的"操作前内容"；基线未失效时直接复用，省一次剪贴板读取。
fn snapshot_for_operation(state: &mut WorkerState) -> Option<PreviousClipboard> {
    if let Some(baseline) = &state.baseline
        && clipboard::current_sequence_number() == baseline.seq
    {
        return Some(PreviousClipboard::Payloads(baseline.payloads.clone()));
    }
    state.baseline = None;
    snapshot_payloads()
}

fn snapshot_payloads() -> Option<PreviousClipboard> {
    match clipboard::snapshot_current_payloads() {
        Ok(Some(payloads)) => Some(PreviousClipboard::Payloads(payloads)),
        // 读取结果为空可能是空剪贴板，也可能是被忽略或不可还原的内容；
        // 只有确认剪贴板确实为空，才在恢复阶段清空，避免破坏用户内容。
        Ok(None) => match clipboard::clipboard_is_empty() {
            Ok(true) => Some(PreviousClipboard::Empty),
            Ok(false) => {
                tracing::debug!("操作前剪贴板内容不可还原，跳过恢复");
                None
            }
            Err(error) => {
                tracing::warn!(%error, "无法确认系统剪贴板是否为空");
                None
            }
        },
        Err(error) => {
            tracing::warn!(%error, "读取系统剪贴板快照失败");
            None
        }
    }
}

fn total_bytes(payloads: &[ClipboardPayload]) -> usize {
    payloads.iter().map(|payload| payload.bytes.len()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_payload(text: &str) -> ClipboardPayload {
        ClipboardPayload {
            format_kind: "text".to_owned(),
            format_namespace: "windows".to_owned(),
            format_key: "CF_UNICODETEXT".to_owned(),
            format_name: "CF_UNICODETEXT".to_owned(),
            native_format_id: Some(13),
            bytes: text
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        }
    }

    #[test]
    fn slots_keep_only_latest_value_and_stay_independent() {
        let mut slots = Slots::default();
        assert!(slots.get(CapslockSlot::Primary).is_none());

        slots.store(CapslockSlot::Primary, vec![text_payload("first")]);
        slots.store(CapslockSlot::Alt, vec![text_payload("alt")]);
        slots.store(CapslockSlot::Primary, vec![text_payload("second")]);

        assert_eq!(
            slots
                .get(CapslockSlot::Primary)
                .map(<[ClipboardPayload]>::len),
            Some(1)
        );
        assert_eq!(slot_text(&slots, CapslockSlot::Primary), "second");
        assert_eq!(slot_text(&slots, CapslockSlot::Alt), "alt");
    }

    fn slot_text(slots: &Slots, slot: CapslockSlot) -> String {
        let bytes = &slots.get(slot).expect("槽位应有内容")[0].bytes;
        let units = bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        String::from_utf16_lossy(&units)
    }

    #[test]
    fn total_bytes_sums_all_formats() {
        let payloads = vec![text_payload("ab"), text_payload("cde")];
        assert_eq!(total_bytes(&payloads), 4 + 6);
    }

    #[test]
    fn empty_previous_clipboard_is_restorable_but_oversized_is_not() {
        assert!(PreviousClipboard::Empty.restorable());
        assert!(PreviousClipboard::Payloads(vec![text_payload("ok")]).restorable());

        let oversized = ClipboardPayload {
            bytes: vec![0; MAX_RESTORE_BYTES + 1],
            ..text_payload("")
        };
        assert!(!PreviousClipboard::Payloads(vec![oversized]).restorable());
    }

    #[test]
    fn coalesce_collapses_consecutive_duplicates_and_keeps_order() {
        let commands = vec![
            CapslockCommand::Paste(CapslockSlot::Primary),
            CapslockCommand::Paste(CapslockSlot::Primary),
            CapslockCommand::Paste(CapslockSlot::Alt),
            CapslockCommand::Paste(CapslockSlot::Alt),
            CapslockCommand::Copy(CapslockSlot::Primary),
            CapslockCommand::Copy(CapslockSlot::Primary),
            CapslockCommand::Paste(CapslockSlot::Primary),
        ];
        assert_eq!(
            coalesce_commands(commands),
            vec![
                CapslockCommand::Paste(CapslockSlot::Primary),
                CapslockCommand::Paste(CapslockSlot::Alt),
                CapslockCommand::Copy(CapslockSlot::Primary),
                CapslockCommand::Paste(CapslockSlot::Primary),
            ]
        );
    }
}
