//! # `SystemClock` — 真实时钟的服务提供者
//!
//! 中文职责：`Clock` 的生产实现。**它只在这一处读系统时间**。
//! English role: the production `Clock`; the single place that reads the system clock.
//! 架构位置：服务提供者。引擎与记忆层都不读系统时间，只认识 `Clock` trait（D38）。
//!
//! # 为什么它不在 `qingjian-core` 里
//!
//! `qingjian-core` 是**抽象**（trait 与数据结构），放一个真的读系统时间的类型
//! 进去，等于让"内核不读时钟"这条规则从第一天起就有一个例外。
//! 把它放在周边 crate 里，规则就还是规则。

use qingjian_core::Clock;
use std::time::{SystemTime, UNIX_EPOCH};

/// 读 `SystemTime` 的时钟。
///
/// # 两处诚实的缺省
///
/// - **系统时间早于 1970 年**（或时钟未初始化）时返回 0，而不是恐慌：
///   输入法不该因为一台时钟没配好的机器就打不了字（D26）。
/// - [`Clock::utc_offset_secs`] 返回 **0**，即"按 UTC 报时"。
///   这是 trait 文档规定的诚实缺省；真正的前端（TSF / Android）
///   应当给出本机偏移，否则 `date_translator` 会把 23:30 报成前一天。
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl SystemClock {
    /// 构造。
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Clock for SystemClock {
    fn now_secs(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }

    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_secs_is_after_2020() {
        // 这条断言很弱，但它抓的是"时钟实现整个坏掉"（例如忘了除 1000、
        // 或者 `duration_since` 报错后静默返回 0）。
        let c = SystemClock::new();
        assert!(c.now_secs() > 1_577_836_800, "系统时钟读出来是 2020 年之前");
        assert!(c.now_ms() / 1000 >= c.now_secs().saturating_sub(1));
    }

    #[test]
    fn utc_offset_is_the_honest_default() {
        // 默认按 UTC 报时：这是 trait 写明的缺省，不是"忘了实现"。
        assert_eq!(SystemClock::new().utc_offset_secs(), 0);
    }
}
