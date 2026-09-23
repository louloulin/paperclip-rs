//! webhook 三条滑动窗口限流（**进程级全局**）。
//!
//! - **写者**：M5-5。
//! - **上游**：`handler/webhook_rate_limiter.go`（`SlidingWindowRateLimit`18 + 内存实现 100 +
//!   Redis 实现 150）与 `handler.go:492-494` 的三条 limiter 装配。
//! - **本地**：只有内存实现。上游 Redis 实现只在 `rdb != nil` 时装配（`cmd/server/router.go`
//!   的三条 `NewRedis*` 全在 `if rdb != nil` 里），本仓库**没有 Redis 依赖** ⇒ 内存实现就是
//!   等价物，语义（滑动窗口 / 非消费 check / `Retry-After`）逐条对齐。
//!
//! # 三条闸的分工（**不要合并**）
//!
//! | 常量 | 默认 | 键 | 何时消费 | 在哪用 |
//! | --- | --- | --- | --- | --- |
//! | [`default_webhook_absolute_ip_rate_limit`] | 600 / 60s | IP | **每次请求**（`allow`） | 入站 step 1，抢在 DB 之前 |
//! | [`default_webhook_ip_rate_limit`] | 30 / 60s | IP | **只记「坏凭据」**（`check`＋事后 `allow`） | 入站 step 1 查一次，token 未命中 / 签名不合法时补记一笔 |
//! | [`default_webhook_rate_limit`] | 60 / 60s | `trigger_id` | **每次派发**（`allow`） | **worker** 侧（`process_next_delivery`），不在入站 |
//!
//! 中间那条为什么是「非消费 check + 债」：NAT 后面一整个机房的合法 GitHub hook 不该耗掉同一
//! 配额 —— 只有被打上「坏凭据」标签的请求才记账。上游注释写得很直白（`webhook_rate_limiter.go`）。
//!
//! # 为什么这些状态是 `static` 而不是 `AppState` 字段
//!
//! `crates/mc-http/src/state.rs` 是**共享锚点**（本片写集不含它，见 `docs/44` §3.2）。上游把三条
//! limiter 挂在进程级 `Handler` 上，本地等价物就是本文件的三个 `LazyLock` 单例。**偏差**（已登记
//! `docs/54` D3）：多副本时限额是每副本的 —— 与上游「无 Redis 时每进程内存」的情形一致。
//!
//! # 两处加固（上游没有，已登记 `docs/54` D4）
//!
//! 1. **键空间上限**：[`MAX_LIMITER_KEYS`]。键里有**攻击者可控**的部分（原始 token 拼出的 IP 桶、
//!    未知 token 的请求都往 IP 桶上写），无上限就是内存放大。超限时**失败开放**（fail-open，记
//!    `warn`）：与上游「限流器是安全网、不是正确性前提」的立场一致（`slidingWindowLimiterCheck`
//!    在 backend 出错时同样返回 `true`）。
//! 2. **毒锁失败开放**：`Mutex` 中毒时 `into_inner()` 继续用（限流器状态是可丢弃的近似值，
//!    没有需要保护的跨字段不变量）。若 `unwrap` 就会把一次 panic 变成全站 500。

use std::collections::{HashMap, VecDeque};
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use uuid::Uuid;

use super::WebhookError;

/// 单个 limiter 能同时记住的键数上限（加固 1）。
pub const MAX_LIMITER_KEYS: usize = 8192;

/// 上游 `SlidingWindowRateLimit`：`limit` 次 / `window` 长。`limit == 0` ⇒ **关闭该闸**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlidingWindowRateLimit {
    /// 窗口内允许的次数；`0` = 关闭。
    pub limit: u32,
    /// 窗口长度。
    pub window: Duration,
}

impl SlidingWindowRateLimit {
    /// 构造。
    #[must_use]
    pub const fn new(limit: u32, window: Duration) -> Self {
        Self { limit, window }
    }
}

/// 上游 `DefaultWebhookRateLimit`：60 / 60s（**per-token**，worker 侧消费）。
#[must_use]
pub const fn default_webhook_rate_limit() -> SlidingWindowRateLimit {
    SlidingWindowRateLimit::new(60, Duration::from_secs(60))
}

/// 上游 `DefaultWebhookIPRateLimit`：30 / 60s（per-IP 坏凭据债，非消费 check）。
#[must_use]
pub const fn default_webhook_ip_rate_limit() -> SlidingWindowRateLimit {
    SlidingWindowRateLimit::new(30, Duration::from_secs(60))
}

/// 上游 `DefaultWebhookAbsoluteIPRateLimit`：600 / 60s（per-IP 绝对天花板，消费）。
#[must_use]
pub const fn default_webhook_absolute_ip_rate_limit() -> SlidingWindowRateLimit {
    SlidingWindowRateLimit::new(600, Duration::from_secs(60))
}

/// 滑动窗口计数器的内部状态。
#[derive(Debug, Default)]
struct LimiterState {
    /// 键 → 窗口内的命中时刻（`VecDeque` 让最老的一头 O(1) 弹出）。
    hits: HashMap<String, VecDeque<Instant>>,
    /// 上次全表清扫时刻（上游 `lastSweep`）。
    last_sweep: Option<Instant>,
}

/// 内存滑动窗口限流器（上游 `memoryWebhookRateLimiter` 的等价物）。
#[derive(Debug)]
pub struct SlidingWindowLimiter {
    limit: SlidingWindowRateLimit,
    state: Mutex<LimiterState>,
}

impl SlidingWindowLimiter {
    /// 构造。
    #[must_use]
    pub fn new(limit: SlidingWindowRateLimit) -> Self {
        Self {
            limit,
            state: Mutex::new(LimiterState::default()),
        }
    }

    /// 消费一次（上游 `Allow` / `AllowWithError`）：窗口没满就记一笔并放行。
    pub fn allow(&self, key: &str) -> bool {
        self.evaluate(key, true)
    }

    /// 只看不记（上游 `Check` / `CheckWithError`）。
    pub fn check(&self, key: &str) -> bool {
        self.evaluate(key, false)
    }

    /// 上游 `RetryAfter`：等到**最老那次命中**滑出窗口即可再试；没有记录时返回整窗口。
    #[must_use]
    pub fn retry_after(&self, key: &str) -> Duration {
        if self.limit.limit == 0 {
            return Duration::ZERO;
        }
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(oldest) = state.hits.get(key).and_then(VecDeque::front).copied() else {
            return self.limit.window;
        };
        let retry = oldest
            .checked_add(self.limit.window)
            .map_or(self.limit.window, |deadline| {
                deadline.saturating_duration_since(Instant::now())
            });
        if retry.is_zero() {
            Duration::from_secs(1)
        } else {
            retry
        }
    }

    /// 上游 `memoryWebhookRateLimiter.evaluate`（**先裁后数，数完再决定记不记**）。
    fn evaluate(&self, key: &str, consume: bool) -> bool {
        if self.limit.limit == 0 {
            return true;
        }
        let now = Instant::now();
        // `checked_sub` 返回 `None` = 进程启动还没到一整个窗口 ⇒ 没有任何记录过期。
        let cutoff = now.checked_sub(self.limit.window);
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.sweep(now, cutoff, self.limit.window);

        if state.hits.len() >= MAX_LIMITER_KEYS && !state.hits.contains_key(key) {
            // 加固 1：键空间已满 ⇒ 失败开放，绝不为一条新键分配内存。
            tracing::warn!(
                keys = state.hits.len(),
                limit = MAX_LIMITER_KEYS,
                "webhook rate limiter key space exhausted; failing open"
            );
            return true;
        }

        let hits = state.hits.entry(key.to_owned()).or_default();
        if let Some(cutoff) = cutoff {
            while hits.front().is_some_and(|at| *at <= cutoff) {
                hits.pop_front();
            }
        }
        if hits.len() >= self.limit.limit as usize {
            return false;
        }
        if consume {
            hits.push_back(now);
        }
        true
    }
}

impl LimiterState {
    /// 上游 `sweepExpired`：清扫间隔 = `min(window, 1min)`，避免每次请求都 O(键数)。
    fn sweep(&mut self, now: Instant, cutoff: Option<Instant>, window: Duration) {
        let interval = if window.is_zero() || window > Duration::from_secs(60) {
            Duration::from_secs(60)
        } else {
            window
        };
        if let Some(last) = self.last_sweep {
            if now.saturating_duration_since(last) < interval {
                return;
            }
        }
        self.last_sweep = Some(now);
        let Some(cutoff) = cutoff else {
            return;
        };
        // 最后一个命中都过期了，这条键就能整条丢掉。
        self.hits
            .retain(|_, hits| hits.back().is_some_and(|at| *at > cutoff));
    }
}

/// 绝对 IP 天花板（消费）。入站最先过它。
pub static WEBHOOK_ABSOLUTE_IP_LIMITER: LazyLock<SlidingWindowLimiter> =
    LazyLock::new(|| SlidingWindowLimiter::new(default_webhook_absolute_ip_rate_limit()));

/// 坏凭据 IP 债（非消费 check + 事后补记）。
pub static WEBHOOK_IP_LIMITER: LazyLock<SlidingWindowLimiter> =
    LazyLock::new(|| SlidingWindowLimiter::new(default_webhook_ip_rate_limit()));

/// per-trigger 派发配额（worker 侧消费）。
pub static WEBHOOK_TRIGGER_LIMITER: LazyLock<SlidingWindowLimiter> =
    LazyLock::new(|| SlidingWindowLimiter::new(default_webhook_rate_limit()));

/// 上游 `writeWebhookRateLimit` 的秒数换算：向上取整，**至少 1**（`Retry-After: 0` 会被客户端理解成「立刻重试」）。
#[must_use]
pub fn retry_after_secs(retry_after: Duration) -> u64 {
    let secs = retry_after.as_secs();
    let rounded = if retry_after.subsec_nanos() > 0 {
        secs.saturating_add(1)
    } else {
        secs
    };
    rounded.max(1)
}

/// 归一化后的 IP 桶键：空串与 `None` 都表示「拿不到远端地址」（上游 `ip == ""`）。
fn key_of(peer_ip: Option<&str>) -> Option<&str> {
    peer_ip.filter(|ip| !ip.is_empty())
}

/// 入站 step 1：两道 IP 闸（**排在 token 查询之前**）。
///
/// 顺序与上游一致：先消费绝对天花板，再用非消费 check 看坏凭据债。任一道拦下 ⇒ 429 + `Retry-After`。
pub fn gate_before_lookup(peer_ip: Option<&str>) -> Result<(), WebhookError> {
    let Some(ip) = key_of(peer_ip) else {
        return Ok(());
    };
    if !WEBHOOK_ABSOLUTE_IP_LIMITER.allow(ip) {
        return Err(WebhookError::RateLimited {
            retry_after_secs: retry_after_secs(WEBHOOK_ABSOLUTE_IP_LIMITER.retry_after(ip)),
        });
    }
    if !WEBHOOK_IP_LIMITER.check(ip) {
        return Err(WebhookError::RateLimited {
            retry_after_secs: retry_after_secs(WEBHOOK_IP_LIMITER.retry_after(ip)),
        });
    }
    Ok(())
}

/// 给「坏凭据」补记一笔（token 未命中、签名不合法/缺失时调用）。这是**唯一**给 IP 债记账的入口。
pub fn charge_bad_credential(peer_ip: Option<&str>) {
    if let Some(ip) = key_of(peer_ip) {
        // 消费一次但**不看结果**：债是「欠着」的，下一次 `check` 自然会发现。
        let _charged = WEBHOOK_IP_LIMITER.allow(ip);
    }
}

/// worker 侧 per-trigger 闸（上游 `ProcessNext` 里对 `trigger_id` 的那次 `Allow`）。
pub fn allow_trigger(trigger_id: Uuid) -> Result<(), WebhookError> {
    let key = trigger_id.to_string();
    if WEBHOOK_TRIGGER_LIMITER.allow(&key) {
        return Ok(());
    }
    Err(WebhookError::RateLimited {
        retry_after_secs: retry_after_secs(WEBHOOK_TRIGGER_LIMITER.retry_after(&key)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter(limit: u32, window_secs: u64) -> SlidingWindowLimiter {
        SlidingWindowLimiter::new(SlidingWindowRateLimit::new(
            limit,
            Duration::from_secs(window_secs),
        ))
    }

    #[test]
    fn limit_zero_disables_the_gate() {
        let l = limiter(0, 60);
        for _ in 0..100 {
            assert!(l.allow("k"));
        }
        assert_eq!(l.retry_after("k"), Duration::ZERO);
    }

    #[test]
    fn allow_consumes_and_check_does_not() {
        let l = limiter(2, 60);
        // check 反复调用不消费 ⇒ 永远不会因为「看」而把自己看死。
        assert!(l.check("k"));
        assert!(l.check("k"));
        assert!(l.check("k"));
        assert!(l.allow("k"));
        assert!(l.check("k"));
        assert!(l.allow("k"));
        // 第 3 次消费被拒；此时 check 也被拒（同一个计数）。
        assert!(!l.allow("k"));
        assert!(!l.check("k"));
    }

    #[test]
    fn keys_are_isolated() {
        let l = limiter(1, 60);
        assert!(l.allow("a"));
        assert!(!l.allow("a"));
        assert!(l.allow("b"));
    }

    #[test]
    fn retry_after_is_positive_and_bounded_by_window() {
        let l = limiter(1, 60);
        assert!(l.allow("k"));
        let retry = l.retry_after("k");
        assert!(retry > Duration::ZERO && retry <= Duration::from_secs(60));
        // 向上取整到秒，且至少 1。
        assert!(retry_after_secs(retry) >= 1);
        // 没有记录的键 ⇒ 报整窗口（上游同款）。
        assert_eq!(l.retry_after("nope"), Duration::from_secs(60));
    }

    #[test]
    fn window_expiry_frees_the_budget() {
        let l = limiter(1, 1);
        assert!(l.allow("k"));
        assert!(!l.allow("k"));
        std::thread::sleep(Duration::from_millis(1_100));
        assert!(l.allow("k"));
    }

    #[test]
    fn missing_peer_ip_skips_both_gates() {
        assert!(gate_before_lookup(None).is_ok());
        assert!(gate_before_lookup(Some("")).is_ok());
    }

    #[test]
    fn bad_credential_debt_trips_after_limit_hits() {
        // 唯一 IP ⇒ 不与其它用例（可能并发）共享桶。
        let ip = "203.0.113.201";
        // 债闸是非消费的：重复 charge 之前 gate 一直放行。
        for _ in 0..default_webhook_ip_rate_limit().limit {
            assert!(gate_before_lookup(Some(ip)).is_ok());
            charge_bad_credential(Some(ip));
        }
        let err = gate_before_lookup(Some(ip)).expect_err("debt bucket must trip");
        assert!(matches!(err, WebhookError::RateLimited { .. }));
    }
}
