#![forbid(unsafe_code)]

//! Company search rate-limit pure helpers — R749.
//!
//! Extracted from pc-companies/src/search_rate_limit.rs:
//! - retry_after math (window_ms, oldest_hit, current_time)
//! - result construction (allowed, limit, remaining, retry_after_seconds)
//! - actor key formatting (company_id:type:id)
//! - env var parsing for window/max_requests
//! - bucket cleanup decisions (whether a hit is expired)
//!
//! All functions are zero IO / zero DB, fully testable.

/// 在 consume 被拒时，按最旧命中 + 窗口长度算 retry-after 秒数（向上取整，最少 1 秒）。
///
/// 与 Node 行为 1:1：`Math.ceil((oldest + window - now) / 1000)`，最小为 1。
pub fn retry_after_seconds_for_blocked(
    oldest_hit_ms: u64,
    current_time_ms: u64,
    window_ms: u64,
) -> u64 {
    let remaining_ms = oldest_hit_ms
        .saturating_add(window_ms)
        .saturating_sub(current_time_ms);
    let secs = remaining_ms.div_ceil(1000);
    secs.max(1)
}

/// retry-after 最小 1 秒（当 remaining_ms == 0 时也至少返回 1）。
///
/// 与 Node `Math.max(1, secs)` 对齐。
pub fn retry_after_min_one(secs: u64) -> u64 {
    secs.max(1)
}

/// 当前窗口的截止时间戳（current_time - window_ms）。
///
/// 当 current_time < window_ms 时，返回 None —— 表示无截止（一切都在窗口内）。
pub fn cutoff_for(current_time_ms: u64, window_ms: u64) -> Option<u64> {
    current_time_ms.checked_sub(window_ms)
}

/// 判断 hit 是否在窗口内（即 hit > cutoff，hit 比 cutoff 新）。
///
/// cutoff = None 表示无窗口约束，全部都算作在窗口内。
pub fn is_hit_in_window(hit_ms: u64, cutoff: Option<u64>) -> bool {
    match cutoff {
        None => true,
        Some(c) => hit_ms > c,
    }
}

/// 构造 allowed = true 的 result。
pub fn result_allowed(max_requests: usize, current_hits: usize) -> ResultParts {
    ResultParts {
        allowed: true,
        limit: max_requests,
        remaining: max_requests.saturating_sub(current_hits),
        retry_after_seconds: 0,
    }
}

/// 构造 allowed = false 的 result（不计入 hits）。
pub fn result_blocked(max_requests: usize, oldest_hit_ms: u64, current_time_ms: u64, window_ms: u64) -> ResultParts {
    ResultParts {
        allowed: false,
        limit: max_requests,
        remaining: 0,
        retry_after_seconds: retry_after_seconds_for_blocked(oldest_hit_ms, current_time_ms, window_ms),
    }
}

/// 简化的 result 结构（pure 层不依赖 trait）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultParts {
    pub allowed: bool,
    pub limit: usize,
    pub remaining: usize,
    pub retry_after_seconds: u64,
}

/// actor key 格式：`{company_id}:{actor_type}:{actor_id}`。
pub fn actor_key(company_id_str: &str, actor_type_str: &str, actor_id_str: &str) -> String {
    format!("{}:{}:{}", company_id_str, actor_type_str, actor_id_str)
}

/// 解析环境变量为 u64 窗口毫秒数。无效值返回 None。
///
/// 与 Node `Number.isFinite + Math.max(...)` 行为对齐：负数 / 非数字 → None。
pub fn parse_window_ms(raw: Option<&str>) -> Option<u64> {
    let raw = raw?;
    let n = raw.trim().parse::<i64>().ok()?;
    if n.is_negative() {
        return None;
    }
    Some(n as u64)
}

/// 解析环境变量为 usize max requests。无效值返回 None。
pub fn parse_max_requests(raw: Option<&str>) -> Option<usize> {
    let raw = raw?;
    let n = raw.trim().parse::<i64>().ok()?;
    if n <= 0 {
        return None;
    }
    Some(n as usize)
}

/// 一个 hit 列表的过期清理：返回需要保留的 hits（按时间顺序）。
///
/// 保留条件：hit 在窗口内（hit_ms > cutoff）。cutoff = None 表示全保留。
pub fn prune_expired_hits(hits: &[u64], cutoff: Option<u64>) -> Vec<u64> {
    match cutoff {
        None => hits.to_vec(),
        Some(c) => hits.iter().copied().filter(|&h| h > c).collect(),
    }
}

/// VecDeque<u64> 风格的从头淘汰过期调用，返回新 VecDeque。
///
/// 与 `while let Some(&front) = recent_hits.front() { ... }` 内联逻辑对齐。
pub fn pop_expired_front(hits: &std::collections::VecDeque<u64>, cutoff: Option<u64>) -> std::collections::VecDeque<u64> {
    let mut out: std::collections::VecDeque<u64> = hits.iter().copied().collect();
    while let Some(&front) = out.front() {
        let expired = match cutoff {
            None => false,
            Some(c) => front <= c,
        };
        if expired {
            out.pop_front();
        } else {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r749_retry_after_simple() {
        // oldest 1000ms ago, window 5000ms -> 4 seconds remain (round up to 4)
        let r = retry_after_seconds_for_blocked(0, 1000, 5000);
        assert_eq!(r, 4);
    }

    #[test]
    fn r749_retry_after_zero_remaining_returns_one() {
        // remaining_ms = 0 -> ceil(0/1000) = 0 -> min 1
        let r = retry_after_seconds_for_blocked(1000, 1000, 1000);
        assert_eq!(r, 1);
    }

    #[test]
    fn r749_retry_after_exact_boundary() {
        // remaining_ms = 999 -> ceil(999/1000) = 1
        let r = retry_after_seconds_for_blocked(0, 1, 1000);
        assert_eq!(r, 1);
    }

    #[test]
    fn r749_retry_after_oldest_in_future() {
        // Clock skew: oldest_hit > current_time, but saturating_sub still computes the
        // natural window expiry: 2000 + 5000 - 1000 = 6000 ms -> 6 seconds.
        let r = retry_after_seconds_for_blocked(2000, 1000, 5000);
        assert_eq!(r, 6);
    }

    #[test]
    fn r749_retry_after_min_one_passthrough() {
        assert_eq!(retry_after_min_one(0), 1);
        assert_eq!(retry_after_min_one(1), 1);
        assert_eq!(retry_after_min_one(10), 10);
    }

    #[test]
    fn r749_cutoff_basic() {
        assert_eq!(cutoff_for(1000, 500), Some(500));
        assert_eq!(cutoff_for(500, 500), Some(0));
        assert_eq!(cutoff_for(500, 1000), None); // underflow
    }

    #[test]
    fn r749_is_hit_in_window() {
        assert!(is_hit_in_window(600, Some(500)));
        assert!(!is_hit_in_window(500, Some(500))); // boundary not in
        assert!(!is_hit_in_window(400, Some(500)));
        assert!(is_hit_in_window(100, None));
        assert!(is_hit_in_window(10000, None));
    }

    #[test]
    fn r749_result_allowed_remaining() {
        let r = result_allowed(10, 3);
        assert!(r.allowed);
        assert_eq!(r.limit, 10);
        assert_eq!(r.remaining, 7);
        assert_eq!(r.retry_after_seconds, 0);
    }

    #[test]
    fn r749_result_allowed_zero_hits() {
        let r = result_allowed(5, 0);
        assert_eq!(r.remaining, 5);
    }

    #[test]
    fn r749_result_allowed_saturating() {
        // current_hits > max_requests → remaining = 0 (saturating_sub)
        let r = result_allowed(5, 10);
        assert_eq!(r.remaining, 0);
    }

    #[test]
    fn r749_result_blocked_no_retry_needed() {
        let r = result_blocked(5, 0, 1000, 5000);
        assert!(!r.allowed);
        assert_eq!(r.limit, 5);
        assert_eq!(r.remaining, 0);
        assert_eq!(r.retry_after_seconds, 4);
    }

    #[test]
    fn r749_result_blocked_min_one() {
        // Exactly at boundary: oldest + window = current
        let r = result_blocked(5, 1000, 1000, 1000);
        assert_eq!(r.retry_after_seconds, 1);
    }

    #[test]
    fn r749_actor_key_format() {
        let k = actor_key("c-1", "agent", "a-1");
        assert_eq!(k, "c-1:agent:a-1");
    }

    #[test]
    fn r749_actor_key_with_special_chars() {
        // uuids 等也直接拼接，不做额外转义
        let k = actor_key("00000000-0000-0000-0000-000000000001", "board", "u1");
        assert_eq!(k, "00000000-0000-0000-0000-000000000001:board:u1");
    }

    #[test]
    fn r749_parse_window_ms_valid() {
        assert_eq!(parse_window_ms(Some("60000")), Some(60000));
        assert_eq!(parse_window_ms(Some("  5000  ")), Some(5000));
        assert_eq!(parse_window_ms(Some("0")), Some(0));
    }

    #[test]
    fn r749_parse_window_ms_invalid() {
        assert_eq!(parse_window_ms(None), None);
        assert_eq!(parse_window_ms(Some("")), None);
        assert_eq!(parse_window_ms(Some("abc")), None);
        assert_eq!(parse_window_ms(Some("-1")), None);
        assert_eq!(parse_window_ms(Some("3.5")), None);
    }

    #[test]
    fn r749_parse_max_requests_valid() {
        assert_eq!(parse_max_requests(Some("10")), Some(10));
        assert_eq!(parse_max_requests(Some("  3  ")), Some(3));
    }

    #[test]
    fn r749_parse_max_requests_invalid() {
        assert_eq!(parse_max_requests(None), None);
        assert_eq!(parse_max_requests(Some("0")), None); // <= 0 → None
        assert_eq!(parse_max_requests(Some("-5")), None);
        assert_eq!(parse_max_requests(Some("abc")), None);
    }

    #[test]
    fn r749_prune_expired_hits_no_cutoff() {
        let hits = vec![1, 2, 3];
        let r = prune_expired_hits(&hits, None);
        assert_eq!(r, vec![1, 2, 3]);
    }

    #[test]
    fn r749_prune_expired_hits_basic() {
        let hits = vec![100, 200, 300, 400, 500];
        let r = prune_expired_hits(&hits, Some(300));
        // Keep hits > 300: 400, 500
        assert_eq!(r, vec![400, 500]);
    }

    #[test]
    fn r749_prune_expired_hits_all_expired() {
        let hits = vec![100, 200];
        let r = prune_expired_hits(&hits, Some(500));
        assert!(r.is_empty());
    }

    #[test]
    fn r749_pop_expired_front_basic() {
        use std::collections::VecDeque;
        let mut d: VecDeque<u64> = VecDeque::new();
        for v in [100, 200, 300, 400, 500].iter() {
            d.push_back(*v);
        }
        let r = pop_expired_front(&d, Some(300));
        let expected: Vec<u64> = vec![400, 500];
        assert_eq!(r.iter().copied().collect::<Vec<_>>(), expected);
    }

    #[test]
    fn r749_pop_expired_front_none_cutoff() {
        use std::collections::VecDeque;
        let mut d: VecDeque<u64> = VecDeque::new();
        for v in [100, 200].iter() {
            d.push_back(*v);
        }
        let r = pop_expired_front(&d, None);
        assert_eq!(r.iter().copied().collect::<Vec<_>>(), vec![100, 200]);
    }

    #[test]
    fn r749_pop_expired_front_empty() {
        use std::collections::VecDeque;
        let d: VecDeque<u64> = VecDeque::new();
        let r = pop_expired_front(&d, Some(0));
        assert!(r.is_empty());
    }
}
