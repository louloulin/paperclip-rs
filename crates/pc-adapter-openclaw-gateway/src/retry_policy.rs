//! OpenClaw Gateway retry policy — 对齐 Node
//! `execute.ts::shouldRetryGatewayError`/`classifyGatewayError`。
//!
//! 已知错误码分类：
//! - TRANSIENT  → 可重试（背退 + jitter）
//! - PERMANENT  → 不可重试，立即失败
//! - UNKNOWN    → 默认按 transient 看待（保守策略）

#![allow(dead_code)]

use crate::constants::{PERMANENT_GATEWAY_CODES, TRANSIENT_GATEWAY_CODES};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryClass {
    Transient,
    Permanent,
    Unknown,
}

/// 根据 Gateway 错误码字符串分类。
pub fn classify_gateway_code(code: &str) -> RetryClass {
    if TRANSIENT_GATEWAY_CODES
        .iter()
        .any(|c| c.eq_ignore_ascii_case(code))
    {
        RetryClass::Transient
    } else if PERMANENT_GATEWAY_CODES
        .iter()
        .any(|c| c.eq_ignore_ascii_case(code))
    {
        RetryClass::Permanent
    } else {
        RetryClass::Unknown
    }
}

/// `shouldRetryGatewayError` —— 决策谓词。
///
/// 保守策略：Unknown 视为 transient（不立刻失败）。
pub fn should_retry_gateway_error(code: Option<&str>) -> bool {
    match code {
        None => true,
        Some(c) => !matches!(classify_gateway_code(c), RetryClass::Permanent),
    }
}

/// 退避延迟（指数 + 抖动，单位：ms）。
///
/// `attempt` 从 0 开始；`base_ms` 是基础延迟（如 500ms）；
/// 上限 `max_ms` 防止无线期等待。
pub fn backoff_with_jitter(attempt: u32, base_ms: u64, max_ms: u64) -> u64 {
    let exp = base_ms.saturating_mul(1u64 << attempt.min(20));
    let capped = exp.min(max_ms);
    let jitter_seed = (attempt as u64).wrapping_mul(2654435761);
    let jitter = (jitter_seed % 100) * (capped / 100).max(1);
    capped.saturating_add(jitter).min(max_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_known_transient_codes() {
        assert_eq!(classify_gateway_code("RATE_LIMITED"), RetryClass::Transient);
        assert_eq!(classify_gateway_code("GATEWAY_BUSY"), RetryClass::Transient);
        assert_eq!(
            classify_gateway_code("UPSTREAM_TIMEOUT"),
            RetryClass::Transient
        );
        assert_eq!(classify_gateway_code("rate_limited"), RetryClass::Transient);
    }

    #[test]
    fn classify_known_permanent_codes() {
        assert_eq!(
            classify_gateway_code("INVALID_REQUEST"),
            RetryClass::Permanent
        );
        assert_eq!(classify_gateway_code("UNAUTHORIZED"), RetryClass::Permanent);
        assert_eq!(classify_gateway_code("FORBIDDEN"), RetryClass::Permanent);
        assert_eq!(classify_gateway_code("NOT_FOUND"), RetryClass::Permanent);
        assert_eq!(classify_gateway_code("BAD_STATE"), RetryClass::Permanent);
    }

    #[test]
    fn classify_unknown_code_returns_unknown() {
        assert_eq!(classify_gateway_code("UNKNOWN_ERROR"), RetryClass::Unknown);
        assert_eq!(classify_gateway_code(""), RetryClass::Unknown);
        assert_eq!(classify_gateway_code("random"), RetryClass::Unknown);
    }

    #[test]
    fn should_retry_true_for_none() {
        assert!(should_retry_gateway_error(None));
    }

    #[test]
    fn should_retry_true_for_transient_and_unknown() {
        assert!(should_retry_gateway_error(Some("RATE_LIMITED")));
        assert!(should_retry_gateway_error(Some("WEIRD_NEW_CODE")));
    }

    #[test]
    fn should_retry_false_for_permanent() {
        assert!(!should_retry_gateway_error(Some("UNAUTHORIZED")));
        assert!(!should_retry_gateway_error(Some("FORBIDDEN")));
    }

    #[test]
    fn backoff_respects_max_cap() {
        let delay = backoff_with_jitter(20, 500, 30_000);
        // Even with jitter, must be at most max_ms
        assert!(delay <= 30_000);
    }

    #[test]
    fn backoff_grows_with_attempt() {
        let d0 = backoff_with_jitter(0, 100, 100_000);
        let d1 = backoff_with_jitter(1, 100, 100_000);
        let d2 = backoff_with_jitter(2, 100, 100_000);
        // 后面的 attempt 应该 >= 前面的（去掉 jitter 的下限）
        // 实际值因为 jitter 会超出 base*2^attempt，所以 >= base*(2^attempt)
        assert!(d0 >= 100);
        assert!(d1 >= 200);
        assert!(d2 >= 400);
    }

    #[test]
    fn backoff_at_zero_attempt_minimum() {
        let d = backoff_with_jitter(0, 1000, 60_000);
        assert!(d >= 1000);
        assert!(d < 60_000);
    }
}
