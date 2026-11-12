use std::time::Duration;

pub(super) fn next_check_delay(
    interval: Option<u64>,
    checked_at: u64,
    retry_after: u64,
    now: u64,
) -> Option<Duration> {
    let interval = interval?;
    let due_at = if retry_after != 0 {
        // 失败后按重试期限检查；系统时间回拨时最多等待一小时。
        retry_after.min(now.saturating_add(3600))
    } else if checked_at == 0 || checked_at > now {
        now
    } else {
        checked_at.saturating_add(interval)
    };
    Some(Duration::from_secs(due_at.saturating_sub(now)))
}
