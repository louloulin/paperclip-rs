//! Hermes gateway retry policy — 指数退避 + jitter。
//!
//! 对齐 Node `backoff_with_jitter` 的语义：
//! - attempt=1 → base_ms (250 默认)
//! - attempt=2 → 2 * base_ms + jitter
//! - attempt=n → min(n * base_ms, max_ms) + jitter
//! - jitter = random(0..base_ms)

#![allow(dead_code)]

use std::time::Duration;

/// 计算退避时长（含 jitter）。
///
/// 返回毫秒数。
pub fn backoff_with_jitter(attempt: u32, base_ms: u64, max_ms: u64) -> u64 {
    let exp = (attempt as u64).saturating_mul(base_ms);
    let bounded = exp.min(max_ms);
    let jitter = simple_jitter(base_ms);
    bounded.saturating_add(jitter).min(max_ms)
}

/// 简单 jitter：用时间戳纳秒的低位取模 base_ms。
fn simple_jitter(base_ms: u64) -> u64 {
    if base_ms == 0 {
        return 0;
    }
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    nanos % base_ms
}

/// 把毫秒转 Duration。
pub fn ms(d: u64) -> Duration {
    Duration::from_millis(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_with_attempt() {
        let b1 = backoff_with_jitter(1, 100, 10_000);
        let b2 = backoff_with_jitter(2, 100, 10_000);
        let b3 = backoff_with_jitter(3, 100, 10_000);
        // b1 <= b2 <= b3 (jitter may swap at boundaries, but cap ensures bounded)
        assert!(b1 <= b3, "backoff should not decrease: b1={b1} b3={b3}");
        assert!(b2 <= b3, "backoff should not decrease: b2={b2} b3={b3}");
        assert!(b3 <= 10_000, "backoff should respect max_ms cap");
    }

    #[test]
    fn backoff_respects_max_ms() {
        let b = backoff_with_jitter(100, 100, 5_000);
        assert!(b <= 5_000);
    }

    #[test]
    fn backoff_zero_base_returns_zero() {
        let b = backoff_with_jitter(1, 0, 1000);
        assert_eq!(b, 0);
    }

    #[test]
    fn ms_converts_correctly() {
        assert_eq!(ms(250), Duration::from_millis(250));
        assert_eq!(ms(0), Duration::from_millis(0));
    }
}
