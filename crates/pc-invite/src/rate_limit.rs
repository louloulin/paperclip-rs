#![forbid(unsafe_code)]
//! 通用 per-IP sliding-window rate limiter（原 `pc-invite-rate-limit` 已下沉），
//! 用于 `/invites/:token*` 这些公开 unauthenticated 端点。
//! 用于 `/invites/:token*` 这些公开 unauthenticated 端点。
//!
//! 对应 Node `server/src/services/invite-rate-limit.ts`（79 行）。设计目标：
//! 1:1 复刻语义（窗口、限额、retry-after、periodic sweep、空 IP → "unknown"）。
//!
//! 设计要点：
//!
//! - **Key shape**：IP 字符串，空字符串 fallback 到 `"unknown"`（与 Node 一致）。
//! - **Window**：默认 60 秒，最多 20 次请求；可通过 options 覆盖。
//! - **`retry_after_seconds`**：当本次被拒时，按"窗口内最旧一条命中 + windowMs - now"
//!   向上取整到秒，最小为 1 秒。
//! - **Periodic sweep**：每 `windowMs` 触发一次，清理所有命中全部过期的 key，
//!   防止 IP 投毒导致 `hitsByKey` map 无界增长。
//! - **线程安全**：内部 `Mutex<HashMap>`，`InviteRateLimiter` 自动实现 `Sync + Send`。
//! - **可测**：`now: Arc<dyn Fn() -> u64 + Send + Sync>` 注入时钟。
//!
//! 公共 API：
//!
//! - [`INVITE_RATE_LIMIT_WINDOW_MS`] / [`INVITE_RATE_LIMIT_MAX_REQUESTS`]
//! - [`InviteRateLimitResult`]
//! - [`InviteRateLimiter`] / [`create_invite_rate_limiter`]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// 默认滑动窗口长度（毫秒）—— 与 Node `INVITE_RATE_LIMIT_WINDOW_MS` 一致。
pub const INVITE_RATE_LIMIT_WINDOW_MS: u64 = 60_000;

/// 默认窗口内最大请求数 —— 与 Node `INVITE_RATE_LIMIT_MAX_REQUESTS` 一致。
pub const INVITE_RATE_LIMIT_MAX_REQUESTS: usize = 20;

/// 时钟 trait 对象类型 —— 注入假时钟用。
pub type ClockFn = Arc<dyn Fn() -> u64 + Send + Sync>;

fn default_clock() -> ClockFn {
    Arc::new(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    })
}

/// 一次 `consume` 的结果 —— 1:1 对应 Node `InviteRateLimitResult`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteRateLimitResult {
    pub allowed: bool,
    pub limit: usize,
    pub remaining: usize,
    pub retry_after_seconds: u64,
}

/// 限流器 trait —— 通过 `dyn InviteRateLimiter` 注入到 service 层。
pub trait InviteRateLimiter: Send + Sync {
    fn consume(&self, ip: &str) -> InviteRateLimitResult;
}

/// 创建限流器的可选参数。
#[derive(Clone)]
pub struct InviteRateLimiterOptions {
    pub window_ms: u64,
    pub max_requests: usize,
    /// 时钟源 —— 默认 `SystemTime::now()`，测试时可注入假时钟。
    pub now: ClockFn,
}

impl Default for InviteRateLimiterOptions {
    fn default() -> Self {
        Self {
            window_ms: INVITE_RATE_LIMIT_WINDOW_MS,
            max_requests: INVITE_RATE_LIMIT_MAX_REQUESTS,
            now: default_clock(),
        }
    }
}

impl std::fmt::Debug for InviteRateLimiterOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InviteRateLimiterOptions")
            .field("window_ms", &self.window_ms)
            .field("max_requests", &self.max_requests)
            .field("now", &"<fn>")
            .finish()
    }
}

/// 默认实现：in-memory sliding window rate limiter.
///
/// 内部 `Mutex<HashMap<String, VecDeque<u64>>>` 串行化所有 consume 调用；
/// `VecDeque` 用 `pop_front` 淘汰窗口外的旧命中，比 `Vec::retain` 更便宜。
pub struct InMemoryInviteRateLimiter {
    window_ms: u64,
    max_requests: usize,
    now: ClockFn,
    /// key -> 该 key 的命中时间戳（毫秒，按插入顺序排列）
    hits: Mutex<HashMap<String, VecDeque<u64>>>,
    /// 上次 sweep 时间戳（毫秒）
    last_sweep: Mutex<u64>,
}

impl InMemoryInviteRateLimiter {
    pub fn new(options: InviteRateLimiterOptions) -> Self {
        Self {
            window_ms: options.window_ms,
            max_requests: options.max_requests,
            now: options.now,
            hits: Mutex::new(HashMap::new()),
            last_sweep: Mutex::new(0),
        }
    }
}

impl InviteRateLimiter for InMemoryInviteRateLimiter {
    fn consume(&self, ip: &str) -> InviteRateLimitResult {
        let current_time = (self.now)();
        let cutoff = current_time.saturating_sub(self.window_ms);
        let key = if ip.is_empty() { "unknown" } else { ip };

        // periodic sweep —— 每 windowMs 一次，清理所有命中已过期的 key
        {
            let mut last_sweep_guard = self.last_sweep.lock().expect("last_sweep poisoned");
            if current_time.saturating_sub(*last_sweep_guard) >= self.window_ms {
                let mut hits_guard = self.hits.lock().expect("hits poisoned");
                hits_guard.retain(|_, v| {
                    let recent: VecDeque<u64> =
                        v.iter().copied().filter(|hit| *hit > cutoff).collect();
                    if recent.is_empty() {
                        false
                    } else {
                        // 替换为精简后的列表
                        *v = recent;
                        true
                    }
                });
                *last_sweep_guard = current_time;
            }
        }

        let mut hits_guard = self.hits.lock().expect("hits poisoned");
        let recent_hits: VecDeque<u64> = hits_guard
            .get(key)
            .map(|v| v.iter().copied().filter(|hit| *hit > cutoff).collect())
            .unwrap_or_default();

        if recent_hits.len() >= self.max_requests {
            let oldest_hit = *recent_hits.front().unwrap_or(&current_time);
            hits_guard.insert(key.to_string(), recent_hits);
            let retry_after = self
                .window_ms
                .saturating_sub(current_time.saturating_sub(oldest_hit));
            return InviteRateLimitResult {
                allowed: false,
                limit: self.max_requests,
                remaining: 0,
                retry_after_seconds: std::cmp::max(1, retry_after.div_ceil(1000)),
            };
        }

        let mut new_hits = recent_hits;
        new_hits.push_back(current_time);
        let remaining = self.max_requests.saturating_sub(new_hits.len());
        hits_guard.insert(key.to_string(), new_hits);
        InviteRateLimitResult {
            allowed: true,
            limit: self.max_requests,
            remaining,
            retry_after_seconds: 0,
        }
    }
}

/// 创建默认 in-memory 限流器。
pub fn create_invite_rate_limiter(options: InviteRateLimiterOptions) -> InMemoryInviteRateLimiter {
    InMemoryInviteRateLimiter::new(options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// 假时钟：返回可由测试控制的当前时间（毫秒）。
    fn fake_clock(start: u64) -> (ClockFn, Arc<AtomicU64>) {
        let counter = Arc::new(AtomicU64::new(start));
        let c2 = counter.clone();
        let clock: ClockFn = Arc::new(move || c2.load(Ordering::SeqCst));
        (clock, counter)
    }

    fn default_opts(now: ClockFn) -> InviteRateLimiterOptions {
        InviteRateLimiterOptions {
            window_ms: INVITE_RATE_LIMIT_WINDOW_MS,
            max_requests: INVITE_RATE_LIMIT_MAX_REQUESTS,
            now,
        }
    }

    #[test]
    fn r701_first_request_allowed() {
        let (clock, _c) = fake_clock(1_000_000);
        let l = create_invite_rate_limiter(default_opts(clock));
        let r = l.consume("1.2.3.4");
        assert!(r.allowed);
        assert_eq!(r.limit, 20);
        assert_eq!(r.remaining, 19);
        assert_eq!(r.retry_after_seconds, 0);
    }

    #[test]
    fn r701_empty_ip_becomes_unknown() {
        let (clock, _c) = fake_clock(1_000_000);
        let l = create_invite_rate_limiter(default_opts(clock));
        // empty IP 不应 panic，且 consumed 视为 "unknown"
        let r = l.consume("");
        assert!(r.allowed);
        assert_eq!(r.remaining, 19);
    }

    #[test]
    fn r701_max_requests_then_blocked() {
        let (clock, _c) = fake_clock(1_000_000);
        let l = create_invite_rate_limiter(default_opts(clock));
        for _ in 0..20 {
            assert!(l.consume("1.2.3.4").allowed);
        }
        let r = l.consume("1.2.3.4");
        assert!(!r.allowed);
        assert_eq!(r.limit, 20);
        assert_eq!(r.remaining, 0);
        assert!(r.retry_after_seconds >= 1);
    }

    #[test]
    fn r701_different_ips_have_independent_budget() {
        let (clock, _c) = fake_clock(1_000_000);
        let l = create_invite_rate_limiter(default_opts(clock));
        for _ in 0..20 {
            assert!(l.consume("1.2.3.4").allowed);
        }
        // 不同 IP 不受影响
        let r = l.consume("5.6.7.8");
        assert!(r.allowed);
        assert_eq!(r.remaining, 19);
    }

    #[test]
    fn r701_window_slides_after_time_advances() {
        let (clock, counter) = fake_clock(1_000_000);
        let l = create_invite_rate_limiter(default_opts(clock));
        for _ in 0..20 {
            assert!(l.consume("1.2.3.4").allowed);
        }
        // 推进时间超过 windowMs
        counter.store(
            1_000_000 + INVITE_RATE_LIMIT_WINDOW_MS + 1,
            Ordering::SeqCst,
        );
        let r = l.consume("1.2.3.4");
        assert!(r.allowed);
        assert_eq!(r.remaining, 19);
    }

    #[test]
    fn r701_retry_after_at_least_one_second() {
        let (clock, _c) = fake_clock(1_000_000);
        let l = create_invite_rate_limiter(default_opts(clock));
        // 在 t=0 一次性消费 20 次
        for _ in 0..20 {
            assert!(l.consume("1.2.3.4").allowed);
        }
        // 立刻再请求
        let r = l.consume("1.2.3.4");
        assert!(!r.allowed);
        // windowMs=60_000, retry_after 应该接近 60
        assert_eq!(r.retry_after_seconds, 60);
    }

    #[test]
    fn r701_retry_after_decreases_as_window_passes() {
        let (clock, counter) = fake_clock(1_000_000);
        let l = create_invite_rate_limiter(default_opts(clock));
        for _ in 0..20 {
            assert!(l.consume("1.2.3.4").allowed);
        }
        // 推进时间 30 秒
        counter.store(1_000_000 + 30_000, Ordering::SeqCst);
        let r = l.consume("1.2.3.4");
        assert!(!r.allowed);
        // retry_after = ceil((60_000 - 30_000)/1000) = 30
        assert_eq!(r.retry_after_seconds, 30);
    }

    #[test]
    fn r701_custom_options_override_defaults() {
        let (clock, _c) = fake_clock(1_000_000);
        let opts = InviteRateLimiterOptions {
            window_ms: 1_000,
            max_requests: 3,
            now: clock,
        };
        let l = create_invite_rate_limiter(opts);
        assert!(l.consume("1.2.3.4").allowed);
        assert!(l.consume("1.2.3.4").allowed);
        assert!(l.consume("1.2.3.4").allowed);
        let r = l.consume("1.2.3.4");
        assert!(!r.allowed);
        assert_eq!(r.limit, 3);
        assert_eq!(r.retry_after_seconds, 1);
    }

    #[test]
    fn r701_sweep_clears_stale_keys() {
        let (clock, counter) = fake_clock(1_000_000);
        let l = create_invite_rate_limiter(default_opts(clock));
        // 用多个 IP 各 hit 一次
        for i in 0..100 {
            assert!(l.consume(&format!("ip-{}", i)).allowed);
        }
        // 推进时间超过 windowMs，触发 sweep
        counter.store(
            1_000_000 + INVITE_RATE_LIMIT_WINDOW_MS * 2,
            Ordering::SeqCst,
        );
        // 新 hit 应该清空旧 entries
        let r = l.consume("fresh-ip");
        assert!(r.allowed);
        // 内部 map 大小应该很小
        let hits = l.hits.lock().unwrap();
        // 经过 sweep，"fresh-ip" 是新加入的，
        // 所有旧 ip-{} 因为窗口外 + 全部过期被删除
        assert!(hits.contains_key("fresh-ip"));
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn r701_sweep_preserves_keys_with_recent_hits() {
        let (clock, counter) = fake_clock(1_000_000);
        let l = create_invite_rate_limiter(default_opts(clock));
        // 旧 IP：t=0 hit 一次
        assert!(l.consume("old-ip").allowed);
        // 推进时间到 t=30s
        counter.store(1_000_000 + 30_000, Ordering::SeqCst);
        // 新 IP：t=30s hit 一次
        assert!(l.consume("new-ip").allowed);
        // 推进时间到 t=89s+999ms，cutoff=29999 < 30000，所以新 IP 仍在窗口内
        counter.store(1_000_000 + 89_999, Ordering::SeqCst);
        let _ = l.consume("trigger");
        let hits = l.hits.lock().unwrap();
        assert!(!hits.contains_key("old-ip"));
        assert!(hits.contains_key("new-ip"));
        assert!(hits.contains_key("trigger"));
    }

    #[test]
    fn r701_remaining_decrements_correctly() {
        let (clock, _c) = fake_clock(1_000_000);
        let l = create_invite_rate_limiter(default_opts(clock));
        let mut last_remaining = 20;
        for i in 0..5 {
            let r = l.consume("1.2.3.4");
            assert!(r.allowed);
            assert_eq!(r.remaining, last_remaining - 1);
            last_remaining = r.remaining;
            assert_eq!(r.remaining, 20 - 1 - i);
        }
    }

    #[test]
    fn r701_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<InMemoryInviteRateLimiter>();
        assert_send_sync::<Box<dyn InviteRateLimiter>>();
    }
}
