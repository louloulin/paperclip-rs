//! 每安装的重连退避：**纯状态机**（不碰时钟）⇒ 时间序列可直接断言，不必跑真实等待。
//!
//! - **写者**：M7-1。上游 `engine/supervisor.go` 的 `nextBackoff` + "稳定运行即重置"两条逻辑。
//! - 拆出本文件是门 ⑩ 的要求（`supervisor.rs` 超 800 行 ⇒ 拆）。

use std::time::Duration;

// =====================================================================
// 退避（纯状态机，可单独测）
// =====================================================================

/// 每安装的重连退避（上游 `nextBackoff` + "稳定运行即重置"的合并形态）。
///
/// **纯状态机**（不碰时钟）⇒ 时间序列可以直接断言，不必跑真实等待。
#[derive(Debug, Clone)]
pub struct Backoff {
    min: Duration,
    max: Duration,
    reset_after: Duration,
    current: Duration,
    failures: u32,
}

impl Backoff {
    /// 从配置构造（初始 = `min`）。
    pub fn new(min: Duration, max: Duration, reset_after: Duration) -> Self {
        Self {
            min,
            max,
            reset_after,
            current: min,
            failures: 0,
        }
    }

    /// 当前退避值。
    pub fn current(&self) -> Duration {
        self.current
    }

    /// 连续失败次数（诊断）。
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// 记录一次"这次尝试失败"：返回**本次**应等待的时长并把退避翻倍（上限 `max`）。
    pub fn record_failure(&mut self) -> Duration {
        let delay = self.current;
        self.failures = self.failures.saturating_add(1);
        self.current = (self.current * 2).min(self.max);
        delay
    }

    /// 记录连接存活时长：达到 `reset_after` 就重置（一次迟到的失败不该从上限开始）。
    pub fn record_uptime(&mut self, uptime: Duration) {
        if uptime >= self.reset_after {
            self.current = self.min;
            self.failures = 0;
        }
    }
}
