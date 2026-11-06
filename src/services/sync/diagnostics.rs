//! 同步诊断：把后台反复出现的错误压缩为“状态变化时记录一次”，避免日志和通知刷屏。

use std::{
    collections::HashMap,
    io,
    sync::Mutex,
    time::{Duration, Instant},
};

use super::Error;

/// 同一问题在消息不变时的重复记录间隔。
pub(super) const LOG_REPEAT: Duration = Duration::from_secs(30 * 60);
/// 同一问题在消息不变时的重复通知间隔。
pub(super) const NOTICE_REPEAT: Duration = Duration::from_secs(6 * 60 * 60);
/// 按来源地址记录的键数量上限，防止局域网内大量陌生地址撑大表。
const MAX_KEYS: usize = 256;

#[derive(Default)]
pub(super) struct Gate {
    last: Mutex<HashMap<String, (String, Instant)>>,
}

impl Gate {
    /// 消息与上次不同，或距上次放行超过 `repeat` 时返回 true 并记下本次。
    pub(super) fn pass(&self, key: &str, message: &str, repeat: Duration) -> bool {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((previous, at)) = last.get(key)
            && previous == message
            && at.elapsed() < repeat
        {
            return false;
        }
        if last.len() >= MAX_KEYS && !last.contains_key(key) {
            last.retain(|_, (_, at)| at.elapsed() < repeat);
            if last.len() >= MAX_KEYS {
                return false;
            }
        }
        last.insert(key.into(), (message.into(), Instant::now()));
        true
    }
    /// 问题解除时清除记录；之前确实记录过问题时返回 true，便于只记录一次“已恢复”。
    pub(super) fn clear(&self, key: &str) -> bool {
        self.last
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key)
            .is_some()
    }
}

/// 网络层失败的大致类型，用于决定是否打扰用户。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Failure {
    /// 对方离线、端口未开放或网络不通：设备关机时也会出现，只记录日志。
    Unreachable,
    /// 已连上但被对方拒绝或协议校验失败：通常需要用户处理。
    Rejected,
}

/// 把同步错误转换为便于排查的说明；`Error::Io` 的默认文本不含具体原因。
pub(super) fn describe(error: &Error) -> (Failure, String) {
    let Error::Io(error) = error else {
        return (Failure::Rejected, error.to_string());
    };
    use io::ErrorKind::*;
    let (failure, text) = match error.kind() {
        TimedOut | WouldBlock => (Failure::Unreachable, "连接超时"),
        ConnectionRefused => (
            Failure::Unreachable,
            "连接被拒绝，对方可能未运行 QcClip、未开启同步或端口不一致",
        ),
        HostUnreachable | NetworkUnreachable | AddrNotAvailable | NetworkDown => {
            (Failure::Unreachable, "网络不可达")
        }
        UnexpectedEof | ConnectionReset | ConnectionAborted => (
            Failure::Rejected,
            "连接被对方关闭，对方可能未开启同步、已暂停或移除本设备，或两台设备版本不一致",
        ),
        _ => (Failure::Rejected, "网络读写失败"),
    };
    (failure, format!("{text}（{error}）"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_passes_changes_and_suppresses_repeats_until_cleared() {
        let gate = Gate::default();
        assert!(gate.pass("a", "x", LOG_REPEAT));
        assert!(!gate.pass("a", "x", LOG_REPEAT));
        assert!(gate.pass("a", "y", LOG_REPEAT));
        assert!(gate.pass("b", "y", LOG_REPEAT));
        assert!(gate.pass("a", "y", Duration::ZERO));
        assert!(gate.clear("a"));
        assert!(!gate.clear("a"));
        assert!(gate.pass("a", "y", LOG_REPEAT));
    }

    #[test]
    fn io_errors_are_classified_with_their_cause() {
        let refused = Error::Io(io::Error::from(io::ErrorKind::ConnectionRefused));
        assert_eq!(describe(&refused).0, Failure::Unreachable);
        let closed = Error::Io(io::Error::from(io::ErrorKind::UnexpectedEof));
        let (failure, text) = describe(&closed);
        assert_eq!(failure, Failure::Rejected);
        assert!(text.contains("连接被对方关闭"));
        assert_eq!(
            describe(&Error::Message("设备握手未通过".into())),
            (Failure::Rejected, "设备握手未通过".into())
        );
    }
}
