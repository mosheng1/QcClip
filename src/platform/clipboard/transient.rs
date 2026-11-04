use std::{
    collections::VecDeque,
    sync::mpsc::{Receiver, RecvTimeoutError, Sender},
    time::{Duration, Instant},
};

use crate::{
    domain::capture::{
        CapturedClipboard, RestoreState, classify_format_role, normalize_text_bytes,
    },
    services::capture::hash_format_set,
};

const RESTORE_WINDOW: Duration = Duration::from_millis(300);
// 读取大图时应用可能延迟渲染多种格式；超过该时长仍未完成时不再阻塞已到期的内容。
const CAPTURE_IN_FLIGHT_LIMIT: Duration = Duration::from_secs(3);

pub(super) enum ClipboardEvent {
    /// 监听线程开始读取剪贴板，读取完成前不提交已到期的待确认内容。
    CaptureStarted(Instant),
    Captured(Result<ClipboardObservation, String>),
    /// 本次读取被忽略或没有内容。
    Skipped,
}

pub(super) struct ClipboardObservation {
    pub captured: CapturedClipboard,
    pub recordable: bool,
    pub observed_at: Instant,
}

struct PendingCapture {
    observation: ClipboardObservation,
    fingerprint: Vec<FormatFingerprint>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FormatFingerprint {
    namespace: String,
    key: String,
    data_hash: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ClipboardFingerprint {
    Empty,
    Content(Vec<FormatFingerprint>),
}

struct TransientClipboardFilter {
    baseline: Option<ClipboardFingerprint>,
    self_write_baseline: bool,
    baseline_restore_expected: bool,
    clipboard_is_empty: bool,
    pending: VecDeque<PendingCapture>,
}

impl TransientClipboardFilter {
    fn new(initial: Option<&CapturedClipboard>) -> Self {
        let baseline = initial.and_then(fingerprint);
        let clipboard_is_empty = baseline == Some(ClipboardFingerprint::Empty);
        Self {
            baseline,
            self_write_baseline: false,
            baseline_restore_expected: false,
            clipboard_is_empty,
            pending: VecDeque::new(),
        }
    }

    fn observe(&mut self, observation: ClipboardObservation) -> Vec<CapturedClipboard> {
        let ready = self.take_ready(observation.observed_at);
        let Some(fingerprint) = fingerprint(&observation.captured) else {
            self.baseline_restore_expected = true;
            tracing::debug!("剪贴板快照没有可比对的有效内容，保留稳定基准和待确认内容");
            return ready;
        };

        // 真实清空与读取失败分开处理，同时保留最后的有效基准用于识别旧内容还原。
        self.clipboard_is_empty = fingerprint == ClipboardFingerprint::Empty;
        let ClipboardFingerprint::Content(fingerprint) = fingerprint else {
            self.baseline_restore_expected = true;
            return ready;
        };

        // 部分应用还原时只恢复部分格式；这类快照仍视为还原而非新的复制。
        if let Some(ClipboardFingerprint::Content(baseline)) = &self.baseline
            && baseline != &fingerprint
            && is_restored_content(baseline, &fingerprint)
        {
            self.pending.clear();
            self.baseline_restore_expected = false;
            tracing::debug!("剪贴板部分恢复到稳定内容，跳过临时捕获");
            return ready;
        }

        // 回到最近基准且之前确有不同的待确认内容时，才视为临时内容还原。
        if let Some(ClipboardFingerprint::Content(baseline)) = &self.baseline
            && is_restored_content(baseline, &fingerprint)
            && self
                .pending
                .iter()
                .any(|pending| !is_restored_content(baseline, &pending.fingerprint))
        {
            self.pending.clear();
            self.baseline_restore_expected = false;
            tracing::debug!("剪贴板恢复到稳定内容，跳过临时捕获");
            return ready;
        }

        if self.baseline_restore_expected
            && let Some(ClipboardFingerprint::Content(baseline)) = &self.baseline
            && baseline == &fingerprint
        {
            self.baseline_restore_expected = false;
            return ready;
        }

        if let Some(index) = self
            .pending
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, pending)| {
                (is_restored_content(&pending.fingerprint, &fingerprint)
                    && self
                        .pending
                        .iter()
                        .skip(index + 1)
                        .any(|later| !is_restored_content(&later.fingerprint, &fingerprint)))
                .then_some(index)
            })
        {
            // 只撤销还原点之后的内容，保留此前尚未入库的正常复制。
            self.pending.truncate(index + 1);
            return ready;
        }

        // 程序自身写入的内容可能紧接着收到一次还原通知，消费该通知但允许后续相同复制。
        if self.self_write_baseline
            && let Some(ClipboardFingerprint::Content(baseline)) = &self.baseline
            && is_restored_content(baseline, &fingerprint)
        {
            self.self_write_baseline = false;
            self.pending.clear();
            return ready;
        }

        self.baseline_restore_expected = false;

        // 部分应用会分多次写入同一内容（先文件路径、再补位图），新快照包含上一条待确认内容的全部格式时视为同一次复制。
        if self.pending.back().is_some_and(|pending| {
            fingerprint.len() > pending.fingerprint.len()
                && is_restored_content(&fingerprint, &pending.fingerprint)
        }) {
            tracing::debug!("剪贴板内容在确认前补充了格式，合并为同一次捕获");
            self.pending.pop_back();
        }

        self.pending.push_back(PendingCapture {
            observation,
            fingerprint,
        });
        ready
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.pending
            .front()
            .map(|pending| pending.observation.observed_at + RESTORE_WINDOW)
    }

    fn take_ready(&mut self, now: Instant) -> Vec<CapturedClipboard> {
        let mut ready = Vec::new();
        while self.next_deadline().is_some_and(|deadline| deadline <= now) {
            let pending = self.pending.pop_front().expect("到期的剪贴板事件应存在");
            // 到期前重新出现有效内容时仍保留正常快速复制；持续为空则丢弃临时内容。
            if self.clipboard_is_empty {
                tracing::debug!("剪贴板内容在确认前已清空，丢弃临时捕获");
                continue;
            }
            self.baseline = Some(ClipboardFingerprint::Content(pending.fingerprint));
            self.self_write_baseline = !pending.observation.recordable;
            if pending.observation.recordable {
                ready.push(pending.observation.captured);
            }
        }
        ready
    }
}

fn is_restored_content(reference: &[FormatFingerprint], current: &[FormatFingerprint]) -> bool {
    !current.is_empty() && current.iter().all(|format| reference.contains(format))
}

fn fingerprint(captured: &CapturedClipboard) -> Option<ClipboardFingerprint> {
    if captured.formats.is_empty() {
        return Some(ClipboardFingerprint::Empty);
    }
    let has_format = |key: &str| {
        captured.formats.iter().any(|format| {
            format.format_namespace == "windows"
                && format.format_key == key
                && format.capture_state.has_data()
                && format.restore_state == RestoreState::Restorable
                && format.bytes.as_ref().is_some_and(|bytes| !bytes.is_empty())
        })
    };
    let has_unicode = has_format("CF_UNICODETEXT");
    let has_dibv5 = has_format("CF_DIBV5");
    let mut formats = captured
        .formats
        .iter()
        .filter_map(|format| {
            if !format.capture_state.has_data()
                || format.restore_state != RestoreState::Restorable
                || classify_format_role(format.format_kind, &format.format_key, &format.format_name)
                    .is_internal()
            {
                return None;
            }
            if format.format_namespace == "windows" {
                // 派生文本和位图可能被系统重新转换，优先比对 Unicode 文本和 DIBV5。
                if (has_unicode && matches!(format.format_key.as_str(), "CF_TEXT" | "CF_OEMTEXT"))
                    || (has_dibv5 && format.format_key == "CF_DIB")
                {
                    return None;
                }
                if [
                    "ExcludeClipboardContentFromMonitorProcessing",
                    "CanIncludeInClipboardHistory",
                    "CanUploadToCloudClipboard",
                ]
                .iter()
                .any(|name| format.format_name.eq_ignore_ascii_case(name))
                {
                    return None;
                }
            }
            let bytes = format.bytes.as_deref().filter(|bytes| !bytes.is_empty())?;
            Some(FormatFingerprint {
                namespace: format.format_namespace.clone(),
                key: format.format_key.clone(),
                data_hash: hash_format_set([(
                    format.format_namespace.as_str(),
                    format.format_key.as_str(),
                    normalize_text_bytes(&format.format_namespace, &format.format_key, bytes),
                )]),
            })
        })
        .collect::<Vec<_>>();
    if formats.is_empty() {
        return None;
    }
    formats.sort_unstable_by(|left, right| {
        (&left.namespace, &left.key).cmp(&(&right.namespace, &right.key))
    });
    Some(ClipboardFingerprint::Content(formats))
}

pub(super) fn filter_events(
    receiver: Receiver<ClipboardEvent>,
    sender: Sender<Result<CapturedClipboard, String>>,
    initial: Option<CapturedClipboard>,
) {
    let mut filter = TransientClipboardFilter::new(initial.as_ref());
    drop(initial);
    let mut capture_started_at = None;
    loop {
        // 分步写入的后续快照可能因读取耗时晚于前一条的确认时间到达，读取期间推迟确认以便合并。
        let deadline = filter
            .next_deadline()
            .map(|deadline| match capture_started_at {
                Some(started_at) => deadline.max(started_at + CAPTURE_IN_FLIGHT_LIMIT),
                None => deadline,
            });
        let event = match deadline {
            Some(deadline) => {
                receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            }
            None => receiver.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        let ready = match event {
            Ok(ClipboardEvent::CaptureStarted(started_at)) => {
                capture_started_at = Some(started_at);
                continue;
            }
            Ok(ClipboardEvent::Skipped) => {
                capture_started_at = None;
                continue;
            }
            Ok(ClipboardEvent::Captured(Ok(observation))) => {
                capture_started_at = None;
                filter.observe(observation)
            }
            Ok(ClipboardEvent::Captured(Err(error))) => {
                capture_started_at = None;
                if sender.send(Err(error)).is_err() {
                    break;
                }
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {
                capture_started_at = None;
                filter.take_ready(Instant::now())
            }
            // 停止监听时丢弃尚未确认的内容，避免关闭监听后继续入库和提示。
            Err(RecvTimeoutError::Disconnected) => break,
        };
        for captured in ready {
            if sender.send(Ok(captured)).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::capture::{FormatKind, RawFormat, SourceApplication};

    fn text(value: &str) -> CapturedClipboard {
        CapturedClipboard {
            formats: vec![RawFormat::captured(
                "windows",
                "CF_UNICODETEXT",
                "CF_UNICODETEXT",
                FormatKind::Text,
                value
                    .encode_utf16()
                    .chain([0])
                    .flat_map(u16::to_le_bytes)
                    .collect(),
                0,
                true,
            )],
            primary_format_identity: None,
            preview_candidate: None,
            source_application: SourceApplication::default(),
        }
    }

    fn observation(
        captured: CapturedClipboard,
        observed_at: Instant,
        recordable: bool,
    ) -> ClipboardObservation {
        ClipboardObservation {
            captured,
            observed_at,
            recordable,
        }
    }

    fn excel_clipboard() -> CapturedClipboard {
        let mut captured = text("Excel 原文本");
        for (index, (key, kind)) in [
            ("registered:Biff8", FormatKind::Custom),
            ("registered:HTML Format", FormatKind::Html),
            ("registered:Csv", FormatKind::Custom),
            ("registered:Object Descriptor", FormatKind::Custom),
            ("CF_DIB", FormatKind::Image),
            ("CF_DIBV5", FormatKind::Image),
        ]
        .into_iter()
        .enumerate()
        {
            captured.formats.push(RawFormat::captured(
                "windows",
                key,
                key,
                kind,
                vec![index as u8 + 1; 4],
                index as u32 + 1,
                true,
            ));
        }
        captured
    }

    fn partially_restored_excel() -> CapturedClipboard {
        let mut captured = excel_clipboard();
        captured.formats.retain(|format| {
            !matches!(
                format.format_key.as_str(),
                "registered:Biff8" | "registered:HTML Format"
            )
        });
        captured
    }

    #[test]
    fn partial_excel_restores_and_derived_bitmap_changes_do_not_emit_fragments() {
        let now = Instant::now();
        let original = excel_clipboard();
        let partial = partially_restored_excel();
        let mut image_only = partial.clone();
        image_only
            .formats
            .retain(|format| matches!(format.format_key.as_str(), "CF_DIB" | "CF_DIBV5"));
        image_only
            .formats
            .iter_mut()
            .find(|format| format.format_key == "CF_DIB")
            .unwrap()
            .bytes = Some(vec![99; 4]);
        let mut filter = TransientClipboardFilter::new(Some(&original));
        for (index, restored) in [partial, image_only].into_iter().enumerate() {
            let start = now + Duration::from_secs(index as u64);
            assert!(
                filter
                    .observe(observation(text("划词内容"), start, true))
                    .is_empty()
            );
            assert!(
                filter
                    .observe(observation(
                        restored,
                        start + Duration::from_millis(50),
                        true
                    ))
                    .is_empty()
            );
            assert!(filter.take_ready(start + RESTORE_WINDOW).is_empty());
        }
        assert_eq!(filter.baseline, fingerprint(&original));
    }

    #[test]
    fn partial_restore_of_a_pending_excel_copy_retains_the_complete_original_once() {
        let now = Instant::now();
        let original = excel_clipboard();
        let mut filter = TransientClipboardFilter::new(Some(&text("之前的内容")));
        filter.observe(observation(original.clone(), now, true));
        filter.observe(observation(
            text("划词内容"),
            now + Duration::from_millis(50),
            true,
        ));
        filter.observe(observation(
            partially_restored_excel(),
            now + Duration::from_millis(100),
            true,
        ));
        assert_eq!(filter.take_ready(now + RESTORE_WINDOW), vec![original]);
        assert!(filter.take_ready(now + Duration::from_secs(1)).is_empty());
    }

    #[test]
    fn partial_restores_without_a_captured_selection_are_also_silent() {
        let now = Instant::now();
        let mut filter = TransientClipboardFilter::new(Some(&excel_clipboard()));
        filter.observe(observation(partially_restored_excel(), now, true));
        assert!(filter.take_ready(now + RESTORE_WINDOW).is_empty());
    }

    #[test]
    fn changed_primary_bitmap_or_rich_text_is_still_recorded() {
        let now = Instant::now();
        let original = excel_clipboard();
        for key in ["CF_DIBV5", "registered:HTML Format"] {
            let mut changed = original.clone();
            changed
                .formats
                .iter_mut()
                .find(|format| format.format_key == key)
                .unwrap()
                .bytes = Some(vec![99; 4]);
            let mut filter = TransientClipboardFilter::new(Some(&original));
            filter.observe(observation(changed.clone(), now, true));
            assert_eq!(filter.take_ready(now + RESTORE_WINDOW), vec![changed]);
        }
    }

    #[test]
    fn adding_a_new_format_to_existing_content_is_still_recorded() {
        let now = Instant::now();
        let original = excel_clipboard();
        let mut changed = original.clone();
        changed.formats.push(RawFormat::captured(
            "windows",
            "registered:PNG",
            "PNG",
            FormatKind::Image,
            vec![9; 4],
            10,
            true,
        ));
        let mut filter = TransientClipboardFilter::new(Some(&original));
        filter.observe(observation(changed.clone(), now, true));
        assert_eq!(filter.take_ready(now + RESTORE_WINDOW), vec![changed]);
    }

    #[test]
    fn worker_filters_excel_format_fragments_and_keeps_a_later_normal_copy() {
        let now = Instant::now() - Duration::from_secs(2);
        let normal = text("正常复制");
        let (input, receiver) = std::sync::mpsc::channel();
        let (sender, output) = std::sync::mpsc::channel();
        for (captured, delay) in [
            (text("划词内容"), 0),
            (partially_restored_excel(), 50),
            (normal.clone(), 500),
            (text("关闭时尚未确认"), 900),
        ] {
            input
                .send(ClipboardEvent::Captured(Ok(observation(
                    captured,
                    now + Duration::from_millis(delay),
                    true,
                ))))
                .unwrap();
        }
        drop(input);
        filter_events(receiver, sender, Some(excel_clipboard()));
        assert_eq!(output.into_iter().collect::<Vec<_>>(), vec![Ok(normal)]);
    }

    fn invalid_snapshots() -> Vec<CapturedClipboard> {
        let mut snapshots = Vec::new();
        let mut empty = text("");
        empty.formats.clear();
        snapshots.push(empty);

        for format in [
            RawFormat::failed(
                "windows",
                "CF_UNICODETEXT",
                "CF_UNICODETEXT",
                FormatKind::Text,
                0,
                "clipboard_read_failed",
                "读取剪贴板失败",
            ),
            RawFormat::unsupported(
                "windows",
                "CF_BITMAP",
                "CF_BITMAP",
                FormatKind::Image,
                0,
                "native_handle_unsupported",
                "不支持该句柄格式",
            ),
            RawFormat::captured(
                "windows",
                "CF_UNICODETEXT",
                "CF_UNICODETEXT",
                FormatKind::Text,
                Vec::new(),
                0,
                true,
            ),
            RawFormat::captured(
                "windows",
                "CF_BITMAP",
                "CF_BITMAP",
                FormatKind::Image,
                vec![1; 4],
                0,
                false,
            ),
            RawFormat::captured(
                "windows",
                "CF_LOCALE",
                "CF_LOCALE",
                FormatKind::Custom,
                vec![1; 4],
                0,
                true,
            ),
            RawFormat::captured(
                "windows",
                "registered:CanUploadToCloudClipboard",
                "CanUploadToCloudClipboard",
                FormatKind::Custom,
                vec![1; 4],
                0,
                true,
            ),
        ] {
            let mut captured = text("");
            captured.formats = vec![format];
            snapshots.push(captured);
        }
        let mut missing_bytes = text("读取不完整");
        missing_bytes.formats[0].bytes = None;
        snapshots.push(missing_bytes);
        snapshots
    }

    #[test]
    fn invalid_snapshots_do_not_replace_the_baseline_even_after_the_restore_window() {
        let now = Instant::now();
        let original = text("原内容");
        for invalid in invalid_snapshots() {
            let mut filter = TransientClipboardFilter::new(Some(&original));
            assert!(filter.observe(observation(invalid, now, true)).is_empty());
            assert!(filter.take_ready(now + RESTORE_WINDOW).is_empty());
            assert!(filter.next_deadline().is_none());
            assert!(
                filter
                    .observe(observation(
                        original.clone(),
                        now + RESTORE_WINDOW * 2,
                        true,
                    ))
                    .is_empty()
            );
            assert!(filter.take_ready(now + RESTORE_WINDOW * 3).is_empty());

            let next = text("主动复制的新内容");
            filter.observe(observation(next.clone(), now + RESTORE_WINDOW * 3, true));
            assert_eq!(filter.take_ready(now + RESTORE_WINDOW * 4), vec![next]);
        }
    }

    #[test]
    fn unreadable_snapshots_preserve_pending_normal_copies() {
        let now = Instant::now();
        let next = text("正常快速复制");
        for invalid in invalid_snapshots()
            .into_iter()
            .filter(|snapshot| !snapshot.formats.is_empty())
        {
            let mut filter = TransientClipboardFilter::new(Some(&text("原内容")));
            filter.observe(observation(next.clone(), now, true));
            assert!(
                filter
                    .observe(observation(invalid, now + Duration::from_millis(50), true,))
                    .is_empty()
            );
            assert_eq!(filter.take_ready(now + RESTORE_WINDOW), vec![next.clone()]);
        }
    }

    #[test]
    fn invalid_initial_snapshots_do_not_discard_the_first_valid_copy() {
        let now = Instant::now();
        let first = text("第一份内容");
        for invalid in invalid_snapshots() {
            let mut filter = TransientClipboardFilter::new(Some(&invalid));
            filter.observe(observation(first.clone(), now, true));
            if !invalid.formats.is_empty() {
                filter.observe(observation(invalid, now + Duration::from_millis(50), true));
            }
            assert_eq!(filter.take_ready(now + RESTORE_WINDOW), vec![first.clone()]);
        }
    }

    #[test]
    fn temporary_copy_and_restore_do_not_reach_history_or_notifications() {
        let now = Instant::now();
        let original = text("原内容");
        let mut filter = TransientClipboardFilter::new(Some(&original));
        assert!(
            filter
                .observe(observation(text("划词内容"), now, true))
                .is_empty()
        );
        assert!(
            filter
                .observe(observation(original, now + Duration::from_millis(52), true))
                .is_empty()
        );
        assert!(filter.take_ready(now + RESTORE_WINDOW).is_empty());
        assert!(filter.next_deadline().is_none());
    }

    #[test]
    fn rapid_distinct_copies_are_all_retained_at_their_own_deadlines() {
        let now = Instant::now();
        let mut filter = TransientClipboardFilter::new(Some(&text("原内容")));
        let copies = [text("第一条"), text("第二条"), text("第三条")];
        for (index, captured) in copies.iter().enumerate() {
            assert!(
                filter
                    .observe(observation(
                        captured.clone(),
                        now + Duration::from_millis(index as u64 * 50),
                        true
                    ))
                    .is_empty()
            );
        }
        assert!(
            filter
                .take_ready(now + RESTORE_WINDOW - Duration::from_millis(1))
                .is_empty()
        );
        for (index, captured) in copies.into_iter().enumerate() {
            assert_eq!(
                filter.take_ready(now + RESTORE_WINDOW + Duration::from_millis(index as u64 * 50)),
                vec![captured]
            );
        }
    }

    #[test]
    fn restoring_a_pending_normal_copy_preserves_it_and_discards_only_temporary_content() {
        let now = Instant::now();
        let original = text("新复制的内容");
        let mut filter = TransientClipboardFilter::new(Some(&text("旧内容")));
        filter.observe(observation(original.clone(), now, true));
        filter.observe(observation(
            text("划词内容"),
            now + Duration::from_millis(50),
            true,
        ));
        filter.observe(observation(
            original.clone(),
            now + Duration::from_millis(100),
            true,
        ));
        assert_eq!(filter.take_ready(now + RESTORE_WINDOW), vec![original]);
    }

    #[test]
    fn copies_outside_the_restore_window_are_retained() {
        let now = Instant::now();
        let original = text("原内容");
        let next = text("主动复制的内容");
        let mut filter = TransientClipboardFilter::new(Some(&original));
        filter.observe(observation(next.clone(), now, true));
        assert_eq!(
            filter.observe(observation(original.clone(), now + RESTORE_WINDOW, true)),
            vec![next]
        );
        assert_eq!(filter.take_ready(now + RESTORE_WINDOW * 2), vec![original]);
    }

    #[test]
    fn self_writes_update_restore_baseline_without_becoming_history_events() {
        let now = Instant::now();
        let own_copy = text("从历史复制的内容");
        let mut filter = TransientClipboardFilter::new(Some(&text("旧内容")));
        filter.observe(observation(own_copy.clone(), now, false));
        assert!(filter.take_ready(now + RESTORE_WINDOW).is_empty());
        filter.observe(observation(text("划词内容"), now + RESTORE_WINDOW, true));
        filter.observe(observation(
            own_copy,
            now + RESTORE_WINDOW + Duration::from_millis(63),
            true,
        ));
        assert!(filter.take_ready(now + RESTORE_WINDOW * 2).is_empty());
    }

    #[test]
    fn restoring_an_empty_clipboard_also_discards_temporary_content() {
        let now = Instant::now();
        let mut empty = text("");
        empty.formats.clear();
        let mut filter = TransientClipboardFilter::new(Some(&empty));
        filter.observe(observation(text("划词内容"), now, true));
        filter.observe(observation(empty, now + Duration::from_millis(50), true));
        assert!(filter.take_ready(now + RESTORE_WINDOW).is_empty());
    }

    #[test]
    fn runtime_clear_keeps_filtering_repeated_selection_copies_after_a_normal_copy() {
        let now = Instant::now();
        let mut empty = text("");
        empty.formats.clear();
        let mut filter = TransientClipboardFilter::new(Some(&text("正常复制的内容")));
        filter.observe(observation(empty.clone(), now, true));
        assert!(filter.take_ready(now + RESTORE_WINDOW).is_empty());

        for index in 1..=5 {
            let start = now + Duration::from_secs(index);
            filter.observe(observation(text(&format!("划词内容{index}")), start, true));
            filter.observe(observation(
                empty.clone(),
                start + Duration::from_millis(50),
                true,
            ));
            assert!(filter.take_ready(start + RESTORE_WINDOW).is_empty());
            assert!(filter.next_deadline().is_none());
        }

        let normal = text("后续正常复制");
        filter.observe(observation(
            normal.clone(),
            now + Duration::from_secs(6),
            true,
        ));
        assert_eq!(
            filter.take_ready(now + Duration::from_secs(6) + RESTORE_WINDOW),
            vec![normal]
        );
    }

    #[test]
    fn clearing_a_selection_without_an_initial_baseline_discards_it() {
        let now = Instant::now();
        let mut empty = text("");
        empty.formats.clear();
        let mut filter = TransientClipboardFilter::new(None);
        filter.observe(observation(text("划词内容"), now, true));
        filter.observe(observation(empty, now + Duration::from_millis(50), true));
        assert!(filter.take_ready(now + RESTORE_WINDOW).is_empty());
    }

    #[test]
    fn brief_clear_between_normal_copies_preserves_both_copies() {
        let now = Instant::now();
        let mut empty = text("");
        empty.formats.clear();
        for initial in [text("原内容"), empty.clone()] {
            let mut filter = TransientClipboardFilter::new(Some(&initial));
            let first = text("第一条正常复制");
            let second = text("第二条正常复制");
            filter.observe(observation(first.clone(), now, true));
            filter.observe(observation(
                empty.clone(),
                now + Duration::from_millis(50),
                true,
            ));
            filter.observe(observation(
                second.clone(),
                now + Duration::from_millis(100),
                true,
            ));
            assert_eq!(filter.take_ready(now + RESTORE_WINDOW), vec![first]);
            assert_eq!(
                filter.take_ready(now + RESTORE_WINDOW + Duration::from_millis(100)),
                vec![second]
            );
        }
    }

    #[test]
    fn clearing_after_the_restore_window_preserves_the_confirmed_copy() {
        let now = Instant::now();
        let normal = text("正常复制");
        let mut empty = text("");
        empty.formats.clear();
        let mut filter = TransientClipboardFilter::new(Some(&text("原内容")));
        filter.observe(observation(normal.clone(), now, true));
        assert_eq!(
            filter.observe(observation(empty, now + RESTORE_WINDOW, true)),
            vec![normal]
        );
        assert!(filter.take_ready(now + RESTORE_WINDOW * 2).is_empty());
    }

    #[test]
    fn copying_the_current_baseline_is_recorded() {
        let now = Instant::now();
        let original = text("原内容");
        let mut filter = TransientClipboardFilter::new(Some(&original));
        filter.observe(observation(original.clone(), now, true));
        assert_eq!(filter.take_ready(now + RESTORE_WINDOW), vec![original]);
    }

    #[test]
    fn consecutive_copies_matching_the_baseline_are_each_recorded() {
        let now = Instant::now();
        let original = text("原内容");
        let mut filter = TransientClipboardFilter::new(Some(&original));
        filter.observe(observation(original.clone(), now, true));
        filter.observe(observation(
            original.clone(),
            now + Duration::from_millis(50),
            true,
        ));
        assert_eq!(
            filter.take_ready(now + RESTORE_WINDOW),
            vec![original.clone()]
        );
        assert_eq!(
            filter.take_ready(now + RESTORE_WINDOW + Duration::from_millis(50)),
            vec![original]
        );
        assert!(filter.next_deadline().is_none());
    }

    #[test]
    fn restore_after_a_self_write_is_silent_even_if_temporary_content_was_not_captured() {
        let now = Instant::now();
        let own_copy = text("从历史复制的内容");
        let mut filter = TransientClipboardFilter::new(Some(&text("旧内容")));
        filter.observe(observation(own_copy.clone(), now, false));
        assert!(filter.take_ready(now + RESTORE_WINDOW).is_empty());
        filter.observe(observation(own_copy, now + RESTORE_WINDOW, true));
        assert!(filter.take_ready(now + RESTORE_WINDOW * 2).is_empty());
    }

    #[test]
    fn first_capture_without_an_initial_baseline_is_still_recorded() {
        let now = Instant::now();
        let first = text("第一份内容");
        let mut filter = TransientClipboardFilter::new(None);
        filter.observe(observation(first.clone(), now, true));
        assert_eq!(filter.take_ready(now + RESTORE_WINDOW), vec![first]);
    }

    #[test]
    fn restore_matching_ignores_source_format_order_ansi_and_internal_metadata() {
        let original = text("原内容");
        let mut restored = original.clone();
        restored.source_application.name = Some("取词工具".to_owned());
        restored.formats[0].format_order = 4;
        restored.formats[0].bytes.as_mut().unwrap().extend([0; 8]);
        for name in [
            "CF_TEXT",
            "CF_OEMTEXT",
            "GPUI internal metadata",
            "CanUploadToCloudClipboard",
        ] {
            restored.formats.push(RawFormat::captured(
                "windows",
                name,
                name,
                FormatKind::Custom,
                vec![1; 4],
                0,
                true,
            ));
        }
        assert_eq!(fingerprint(&original), fingerprint(&restored));
    }

    #[test]
    fn matching_text_does_not_hide_changes_to_images_or_rich_text() {
        let plain = text("相同文本");
        for (key, kind) in [
            ("CF_DIB", FormatKind::Image),
            ("registered:HTML Format", FormatKind::Html),
        ] {
            let mut rich = plain.clone();
            rich.formats.push(RawFormat::captured(
                "windows",
                key,
                key,
                kind,
                vec![1; 4],
                1,
                true,
            ));
            assert_ne!(fingerprint(&plain), fingerprint(&rich));
            let mut changed = rich.clone();
            changed.formats[1].bytes = Some(vec![2; 4]);
            assert_ne!(fingerprint(&rich), fingerprint(&changed));
        }
    }

    #[test]
    fn worker_discards_pending_events_when_monitor_stops() {
        let (input, receiver) = std::sync::mpsc::channel();
        let (sender, output) = std::sync::mpsc::channel();
        input
            .send(ClipboardEvent::Captured(Ok(observation(
                text("未确认的内容"),
                Instant::now(),
                true,
            ))))
            .unwrap();
        drop(input);
        filter_events(receiver, sender, Some(text("原内容")));
        assert!(output.try_recv().is_err());
    }

    #[test]
    fn worker_skips_invalid_snapshots_and_late_restores_but_delivers_normal_copies() {
        let now = Instant::now() - Duration::from_secs(3);
        let original = text("原内容");
        let next = text("正常复制");
        for invalid in invalid_snapshots() {
            let (input, receiver) = std::sync::mpsc::channel();
            let (sender, output) = std::sync::mpsc::channel();
            for (captured, delay) in [
                (invalid, 0),
                (original.clone(), 400),
                (next.clone(), 800),
                (text("关闭时尚未确认的内容"), 1200),
            ] {
                input
                    .send(ClipboardEvent::Captured(Ok(observation(
                        captured,
                        now + Duration::from_millis(delay),
                        true,
                    ))))
                    .unwrap();
            }
            input
                .send(ClipboardEvent::Captured(Err("处理完成".to_owned())))
                .unwrap();
            drop(input);
            filter_events(receiver, sender, Some(original.clone()));
            assert_eq!(
                output.into_iter().collect::<Vec<_>>(),
                vec![Ok(next.clone()), Err("处理完成".to_owned()),]
            );
        }
    }

    #[test]
    fn worker_filters_repeated_selection_clears_after_a_nonempty_baseline() {
        let now = Instant::now() - Duration::from_secs(4);
        let mut empty = text("");
        empty.formats.clear();
        let normal = text("正常复制");
        let (input, receiver) = std::sync::mpsc::channel();
        let (sender, output) = std::sync::mpsc::channel();
        for (captured, delay) in [
            (text("第一次划词"), 0),
            (empty.clone(), 50),
            (text("第二次划词"), 500),
            (empty.clone(), 550),
            (text("第三次划词"), 1000),
            (empty, 1050),
            (normal.clone(), 1500),
            (text("关闭时尚未确认的内容"), 1900),
        ] {
            input
                .send(ClipboardEvent::Captured(Ok(observation(
                    captured,
                    now + Duration::from_millis(delay),
                    true,
                ))))
                .unwrap();
        }
        drop(input);
        filter_events(receiver, sender, Some(text("原内容")));
        assert_eq!(output.into_iter().collect::<Vec<_>>(), vec![Ok(normal)]);
    }

    #[test]
    fn progressive_writes_before_confirmation_emit_only_the_complete_capture() {
        let now = Instant::now();
        let file_only = CapturedClipboard {
            formats: vec![RawFormat::captured(
                "windows",
                "CF_HDROP",
                "CF_HDROP",
                FormatKind::FileList,
                vec![1; 8],
                0,
                true,
            )],
            primary_format_identity: None,
            preview_candidate: None,
            source_application: SourceApplication::default(),
        };
        let mut complete = file_only.clone();
        complete.formats.push(RawFormat::captured(
            "windows",
            "CF_DIBV5",
            "CF_DIBV5",
            FormatKind::Image,
            vec![2; 8],
            1,
            true,
        ));
        let mut filter = TransientClipboardFilter::new(Some(&text("原内容")));
        assert!(filter.observe(observation(file_only, now, true)).is_empty());
        assert!(
            filter
                .observe(observation(
                    complete.clone(),
                    now + Duration::from_millis(31),
                    true
                ))
                .is_empty()
        );
        assert_eq!(
            filter.take_ready(now + Duration::from_millis(31) + RESTORE_WINDOW),
            vec![complete]
        );
    }

    #[test]
    fn worker_waits_for_a_slow_capture_before_confirming_its_format_subset() {
        let file_only = CapturedClipboard {
            formats: vec![RawFormat::captured(
                "windows",
                "CF_HDROP",
                "CF_HDROP",
                FormatKind::FileList,
                vec![1; 8],
                0,
                true,
            )],
            primary_format_identity: None,
            preview_candidate: None,
            source_application: SourceApplication::default(),
        };
        let mut complete = file_only.clone();
        complete.formats.push(RawFormat::captured(
            "windows",
            "CF_DIBV5",
            "CF_DIBV5",
            FormatKind::Image,
            vec![2; 8],
            1,
            true,
        ));
        let (input, receiver) = std::sync::mpsc::channel();
        let (sender, output) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            filter_events(receiver, sender, Some(text("原内容")));
        });
        let now = Instant::now();
        input
            .send(ClipboardEvent::Captured(Ok(observation(
                file_only, now, true,
            ))))
            .unwrap();
        let started_at = now + Duration::from_millis(130);
        input
            .send(ClipboardEvent::CaptureStarted(started_at))
            .unwrap();
        // 模拟读取大图耗时超过前一条的确认时间。
        std::thread::sleep(RESTORE_WINDOW + Duration::from_millis(200));
        input
            .send(ClipboardEvent::Captured(Ok(observation(
                complete.clone(),
                started_at,
                true,
            ))))
            .unwrap();
        std::thread::sleep(RESTORE_WINDOW + Duration::from_millis(200));
        drop(input);
        worker.join().unwrap();
        assert_eq!(output.into_iter().collect::<Vec<_>>(), vec![Ok(complete)]);
    }

    #[test]
    fn worker_filters_a_queued_round_trip_using_capture_times() {
        let now = Instant::now() - Duration::from_secs(1);
        let original = text("原内容");
        let (input, receiver) = std::sync::mpsc::channel();
        let (sender, output) = std::sync::mpsc::channel();
        input
            .send(ClipboardEvent::Captured(Ok(observation(
                text("划词内容"),
                now,
                true,
            ))))
            .unwrap();
        input
            .send(ClipboardEvent::Captured(Ok(observation(
                original.clone(),
                now + Duration::from_millis(52),
                true,
            ))))
            .unwrap();
        drop(input);
        filter_events(receiver, sender, Some(original));
        assert!(output.try_recv().is_err());
    }
}
