//! `readRecoveryTimerIntervalMs` 纯函数 —— recovery timer interval 配置读取。
//!
//! 对齐 Node `services/recovery/service.ts` 的 `readRecoveryTimerIntervalMs`：
//! ```ts
//! function readRecoveryTimerIntervalMs(raw: unknown, fallback: number) {
//!   return Math.max(1, Math.floor(asNumber(raw, fallback)));
//! }
//! ```
//!
//! 语义：
//! - 仅接受有限数字；字符串 / null / 其他值使用 `fallback`
//! - 钳制到最小值 1（防止 0 或负数）
//! - 向下取整（不接受小数毫秒）
//!
//! 设计：
//! - 纯函数：无 DB I/O，无副作用
//! - 单一职责：仅做 interval 读取 + 钳制
//! - 高复用：被 pc-server `heartbeat_scheduler` ticker 使用
use serde_json::Value;

/// Read recovery timer interval from raw config value, with fallback.
///
/// 与 Node `readRecoveryTimerIntervalMs(raw, fallback)` 1:1 对齐：
/// - `asNumber` 仅接受有限 JSON number，字符串不会隐式转换
/// - 非数字 → fallback
/// - 负数或 0 → 钳制到 1（最小 1ms）
/// - 小数 → 向下取整
pub fn read_recovery_timer_interval_ms(raw: Option<&Value>, fallback: i64) -> i64 {
    let parsed = raw
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .unwrap_or(fallback as f64)
        .floor()
        .max(1.0);
    parsed as i64
}
