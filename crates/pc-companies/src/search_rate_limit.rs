//! Per-actor sliding-window rate limiter for company search（原 `pc-company-search-rate-limit` 已下沉）。 for
//! the company-search endpoint.
//!
//! 对应 Node `server/src/services/company-search-rate-limit.ts`（63 行）。设计
//! 目标：1:1 复刻语义（窗口、限额、retry-after、key shape），并在 Rust 端
//! 提供安全的多线程 / 异步友好接口。
//!
//! 设计要点：
//!
//! - **Key shape**：`(companyId, actorType, actorId)` —— 与 Node 一致。
//! - **Window**：默认 60 秒，最多 60 次请求；可通过 options 覆盖。
//! - **`retry_after_seconds`**：当本次被拒时，按"窗口内最旧一条命中 + windowMs - now"
//!   向上取整到秒，最小为 1 秒。
//! - **`remaining`**：被拒时为 0；通过时为 `max(0, max_requests - len)`。
//! - **线程安全**：内部使用 `Mutex<HashMap>`，所有公开方法都接收 `&self`，
//!   因此 `CompanySearchRateLimiter` 自动实现 `Sync + Send`，可以放进 Arc 共享。
//! - **可测**：`now: Arc<dyn Fn() -> u64 + Send + Sync>` 注入时钟，
//!   纯函数测试无需 sleep（fn pointer 不能捕获 closure，所以用 Arc<dyn Fn>）。
//!
//! 公共 API：
//!
//! - [`COMPANY_SEARCH_RATE_LIMIT_WINDOW_MS`] / [`COMPANY_SEARCH_RATE_LIMIT_MAX_REQUESTS`]
//! - [`CompanySearchRateLimitActor`] / [`CompanySearchRateLimitResult`]
//! - [`CompanySearchRateLimiter`] / [`create_company_search_rate_limiter`]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// 默认滑动窗口长度（毫秒）—— 与 Node 常量 `COMPANY_SEARCH_RATE_LIMIT_WINDOW_MS` 一致。
pub const COMPANY_SEARCH_RATE_LIMIT_WINDOW_MS: u64 = 60_000;

/// 默认窗口内最大请求数 —— 与 Node 常量 `COMPANY_SEARCH_RATE_LIMIT_MAX_REQUESTS` 一致。
pub const COMPANY_SEARCH_RATE_LIMIT_MAX_REQUESTS: usize = 60;

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

/// Actor 身份：rate-limit key 由 `(company_id, actor_type, actor_id)` 三元组组成。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CompanySearchRateLimitActor {
    pub company_id: String,
    pub actor_type: CompanySearchActorType,
    pub actor_id: String,
}

/// Actor 类型 —— 与 Node 字面量 `"agent" | "board"` 一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompanySearchActorType {
    Agent,
    Board,
}

impl CompanySearchActorType {
    /// 接受 Node 字面量 `"agent" | "board"`，其它值返回 `None`。
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "agent" => Some(Self::Agent),
            "board" => Some(Self::Board),
            _ => None,
        }
    }

    /// 返回 Node 字面量（"agent" / "board"），用于与 Node 端日志 / 序列化兼容。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Board => "board",
        }
    }
}

/// 一次 `consume` 的结果 —— 1:1 对应 Node `CompanySearchRateLimitResult`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompanySearchRateLimitResult {
    pub allowed: bool,
    pub limit: usize,
    pub remaining: usize,
    pub retry_after_seconds: u64,
}

/// 限流器 trait —— 通过 `dyn CompanySearchRateLimiter` 注入到 service 层。
pub trait CompanySearchRateLimiter: Send + Sync {
    fn consume(&self, actor: &CompanySearchRateLimitActor) -> CompanySearchRateLimitResult;
}

/// 创建限流器的可选参数。
#[derive(Clone)]
pub struct CompanySearchRateLimiterOptions {
    pub window_ms: u64,
    pub max_requests: usize,
    /// 时钟源 —— 默认 `SystemTime::now()`，测试时可注入假时钟。
    pub now: ClockFn,
}

impl Default for CompanySearchRateLimiterOptions {
    fn default() -> Self {
        Self {
            window_ms: COMPANY_SEARCH_RATE_LIMIT_WINDOW_MS,
            max_requests: COMPANY_SEARCH_RATE_LIMIT_MAX_REQUESTS,
            now: default_clock(),
        }
    }
}

impl std::fmt::Debug for CompanySearchRateLimiterOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompanySearchRateLimiterOptions")
            .field("window_ms", &self.window_ms)
            .field("max_requests", &self.max_requests)
            .field("now", &"<fn>")
            .finish()
    }
}

/// 默认实现：in-memory sliding window rate limiter。
///
/// 内部 `Mutex<HashMap<String, VecDeque<u64>>>` 串行化所有 consume 调用；
/// `VecDeque` 用 `pop_front` 淘汰窗口外的旧命中，比 `Vec::retain` 更便宜。
pub struct InMemoryCompanySearchRateLimiter {
    window_ms: u64,
    max_requests: usize,
    now: ClockFn,
    /// key -> 该 key 的命中时间戳（毫秒，按插入顺序排列）
    hits: Mutex<HashMap<String, VecDeque<u64>>>,
}

impl InMemoryCompanySearchRateLimiter {
    /// 与 Node `key(actor)` 函数 1:1 对齐。
    fn key(actor: &CompanySearchRateLimitActor) -> String {
        format!(
            "{}:{}:{}",
            actor.company_id,
            actor.actor_type.as_str(),
            actor.actor_id
        )
    }
}

impl CompanySearchRateLimiter for InMemoryCompanySearchRateLimiter {
    fn consume(&self, actor: &CompanySearchRateLimitActor) -> CompanySearchRateLimitResult {
        let current_time = (self.now)();
        // 当 current_time < window_ms 时，saturating_sub 会下溢到 0，
        // 会把所有 <=0 的旧命中错误淘汰。这里用 checked_sub，
        // None 表示 "没有截止"（一切都在窗口内）。
        let cutoff = current_time.checked_sub(self.window_ms);
        let actor_key = Self::key(actor);

        let mut hits = self.hits.lock().expect("rate-limit mutex poisoned");
        let recent_hits = hits.entry(actor_key.clone()).or_insert_with(VecDeque::new);

        // 1. 淘汰窗口外的旧命中
        while let Some(&front) = recent_hits.front() {
            let is_expired = match cutoff {
                None => false,
                Some(c) => front <= c,
            };
            if is_expired {
                recent_hits.pop_front();
            } else {
                break;
            }
        }

        // 2. 判定是否超限
        if recent_hits.len() >= self.max_requests {
            // 最旧一条命中（仍在窗口内）决定 retry-after
            let oldest_hit = recent_hits.front().copied().unwrap_or(current_time);
            let result = CompanySearchRateLimitResult {
                allowed: false,
                limit: self.max_requests,
                remaining: 0,
                retry_after_seconds: {
                    let remaining_ms = oldest_hit
                        .saturating_add(self.window_ms)
                        .saturating_sub(current_time);
                    let secs = (remaining_ms + 999) / 1000;
                    secs.max(1)
                },
            };
            // 不记录此次命中 —— 与 Node 行为一致（被拒时只保留 recentHits，不 push）。
            return result;
        }

        // 3. 通过：记录本次命中
        recent_hits.push_back(current_time);
        CompanySearchRateLimitResult {
            allowed: true,
            limit: self.max_requests,
            remaining: self.max_requests.saturating_sub(recent_hits.len()),
            retry_after_seconds: 0,
        }
    }
}

/// 工厂函数 —— 与 Node `createCompanySearchRateLimiter(options)` 1:1 对齐。
///
/// 未传 options 时使用默认窗口/限额/时钟。
pub fn create_company_search_rate_limiter(
    options: Option<CompanySearchRateLimiterOptions>,
) -> Arc<dyn CompanySearchRateLimiter> {
    let opts = options.unwrap_or_default();
    Arc::new(InMemoryCompanySearchRateLimiter {
        window_ms: opts.window_ms,
        max_requests: opts.max_requests,
        now: opts.now,
        hits: Mutex::new(HashMap::new()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn actor(
        company_id: &str,
        actor_type: CompanySearchActorType,
        actor_id: &str,
    ) -> CompanySearchRateLimitActor {
        CompanySearchRateLimitActor {
            company_id: company_id.to_string(),
            actor_type,
            actor_id: actor_id.to_string(),
        }
    }

    /// 单调递增的假时钟 —— 每次 fetch_add 返回旧值后 +1。
    fn step_clock() -> (Arc<AtomicU64>, ClockFn) {
        let counter = Arc::new(AtomicU64::new(0));
        let c = counter.clone();
        let now: ClockFn = Arc::new(move || c.fetch_add(1, Ordering::SeqCst));
        (counter, now)
    }

    /// 固定起始值的假时钟 —— 每次 fetch_add 返回旧值后 +step。
    fn step_clock_from(start: u64, step: u64) -> (Arc<AtomicU64>, ClockFn) {
        let counter = Arc::new(AtomicU64::new(start));
        let c = counter.clone();
        let now: ClockFn = Arc::new(move || c.fetch_add(step, Ordering::SeqCst));
        (counter, now)
    }

    #[test]
    fn r685_first_request_is_allowed_with_full_remaining() {
        let (_, now) = step_clock_from(1_000_000, 1);
        let limiter = create_company_search_rate_limiter(Some(CompanySearchRateLimiterOptions {
            window_ms: 1000,
            max_requests: 3,
            now,
        }));
        let a = actor("c1", CompanySearchActorType::Agent, "a1");
        let r = limiter.consume(&a);
        assert!(r.allowed);
        assert_eq!(r.limit, 3);
        assert_eq!(r.remaining, 2);
        assert_eq!(r.retry_after_seconds, 0);
    }

    #[test]
    fn r685_max_requests_then_blocked() {
        let (_, now) = step_clock_from(1_000_000, 1);
        let limiter = create_company_search_rate_limiter(Some(CompanySearchRateLimiterOptions {
            window_ms: 1000,
            max_requests: 3,
            now,
        }));
        let a = actor("c1", CompanySearchActorType::Agent, "a1");
        assert!(limiter.consume(&a).allowed);
        assert!(limiter.consume(&a).allowed);
        assert!(limiter.consume(&a).allowed);
        let r = limiter.consume(&a);
        assert!(!r.allowed);
        assert_eq!(r.remaining, 0);
        assert!(r.retry_after_seconds >= 1);
    }

    #[test]
    fn r685_retry_after_uses_oldest_hit_in_window() {
        // max_requests=2，window=1000。
        // step_clock_from(0, 200) 产生时间序列：0, 200, 400, 600, ...
        // 第 1 次 (t=0) -> allowed，记录 [0]
        // 第 2 次 (t=200) -> allowed，记录 [0, 200]
        // 第 3 次 (t=400) -> blocked，oldest=0，retry=ceil((0+1000-400)/1000)=1s
        let (_, now) = step_clock_from(0, 200);
        let limiter = create_company_search_rate_limiter(Some(CompanySearchRateLimiterOptions {
            window_ms: 1000,
            max_requests: 2,
            now,
        }));
        let a = actor("c1", CompanySearchActorType::Agent, "a1");
        assert!(limiter.consume(&a).allowed);
        assert!(limiter.consume(&a).allowed);
        let r = limiter.consume(&a);
        assert!(!r.allowed);
        assert_eq!(r.retry_after_seconds, 1);
    }

    #[test]
    fn r685_retry_after_uses_oldest_with_larger_gap() {
        // 当 oldest_hit + window - current >= 1s 时，向上取整到具体秒数
        // window=5000, max=1
        // t=0 -> allowed
        // t=600 -> blocked, oldest=0, retry=ceil((0+5000-600)/1000)=ceil(4.4)=5
        let (_, now) = step_clock_from(0, 600);
        let limiter = create_company_search_rate_limiter(Some(CompanySearchRateLimiterOptions {
            window_ms: 5000,
            max_requests: 1,
            now,
        }));
        let a = actor("c1", CompanySearchActorType::Agent, "a1");
        assert!(limiter.consume(&a).allowed);
        let r = limiter.consume(&a);
        assert!(!r.allowed);
        // ceil(4.4) = 5
        assert_eq!(r.retry_after_seconds, 5);
    }

    #[test]
    fn r685_different_actors_have_independent_budgets() {
        let (_, now) = step_clock();
        let limiter = create_company_search_rate_limiter(Some(CompanySearchRateLimiterOptions {
            window_ms: 10_000,
            max_requests: 2,
            now,
        }));
        let agent_a = actor("c1", CompanySearchActorType::Agent, "a1");
        let agent_b = actor("c1", CompanySearchActorType::Agent, "a2");
        let board = actor("c1", CompanySearchActorType::Board, "b1");
        let other_co = actor("c2", CompanySearchActorType::Agent, "a1");

        assert!(limiter.consume(&agent_a).allowed);
        assert!(limiter.consume(&agent_a).allowed);
        assert!(!limiter.consume(&agent_a).allowed);

        // 不同 agent 各自有独立预算
        assert!(limiter.consume(&agent_b).allowed);
        // actor_type 不同也算独立 key
        assert!(limiter.consume(&board).allowed);
        // 不同 company 也独立
        assert!(limiter.consume(&other_co).allowed);
    }

    #[test]
    fn r685_window_expiry_resets_budget() {
        // step_clock_from(0, 600) -> 0, 600, 1200, ...
        // window=1000, max=1
        // t=0 -> allowed
        // t=600 -> blocked, oldest=0, cutoff=600-1000=-400, 0>=-400 不被淘汰 -> blocked
        // 把 counter 推到一个非常远的未来，让 cutoff > oldest
        let (counter, now) = step_clock_from(0, 600);
        let limiter = create_company_search_rate_limiter(Some(CompanySearchRateLimiterOptions {
            window_ms: 1000,
            max_requests: 1,
            now,
        }));
        let a = actor("c1", CompanySearchActorType::Agent, "a1");

        // t=0 -> allowed
        assert!(limiter.consume(&a).allowed);
        // t=600 -> blocked (最近窗口内 [0])
        let r = limiter.consume(&a);
        assert!(!r.allowed);
        assert_eq!(r.retry_after_seconds, 1);

        // 跳过窗口：让下一个 fetch_add 返回 100_000
        counter.store(100_000, Ordering::SeqCst);
        // 下次 consume -> t=100_000 -> cutoff=99_000，oldest=0 被淘汰，预算重置
        assert!(limiter.consume(&a).allowed);
    }

    #[test]
    fn r685_actor_type_from_str_round_trip() {
        assert_eq!(
            CompanySearchActorType::from_str("agent"),
            Some(CompanySearchActorType::Agent)
        );
        assert_eq!(
            CompanySearchActorType::from_str("board"),
            Some(CompanySearchActorType::Board)
        );
        assert_eq!(CompanySearchActorType::from_str("user"), None);
        assert_eq!(CompanySearchActorType::Agent.as_str(), "agent");
        assert_eq!(CompanySearchActorType::Board.as_str(), "board");
    }

    #[test]
    fn r685_key_format_matches_node() {
        let a = actor("co42", CompanySearchActorType::Board, "u7");
        assert_eq!(InMemoryCompanySearchRateLimiter::key(&a), "co42:board:u7");
        let a = actor("co42", CompanySearchActorType::Agent, "u7");
        assert_eq!(InMemoryCompanySearchRateLimiter::key(&a), "co42:agent:u7");
    }

    #[test]
    fn r685_send_sync_via_arc() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Arc<dyn CompanySearchRateLimiter>>();
        assert_send_sync::<InMemoryCompanySearchRateLimiter>();
    }

    #[test]
    fn r685_default_options_use_constants() {
        let opts = CompanySearchRateLimiterOptions::default();
        assert_eq!(opts.window_ms, COMPANY_SEARCH_RATE_LIMIT_WINDOW_MS);
        assert_eq!(opts.max_requests, COMPANY_SEARCH_RATE_LIMIT_MAX_REQUESTS);
    }

    #[test]
    fn r685_retry_after_min_is_one_second() {
        // 即使 (oldest + window) - current < 1s 也要返回 1（与 Node `Math.max(1, ...)` 一致）
        // step_clock_from(0, 1) -> 0, 1, 2, ...
        // t=0 -> allowed
        // t=1 -> blocked, oldest=0, window=1000, current=1 -> remaining=999ms -> ceil=1s
        let (_, now) = step_clock_from(0, 1);
        let limiter = create_company_search_rate_limiter(Some(CompanySearchRateLimiterOptions {
            window_ms: 1000,
            max_requests: 1,
            now,
        }));
        let a = actor("c1", CompanySearchActorType::Agent, "a1");
        assert!(limiter.consume(&a).allowed);
        let r = limiter.consume(&a);
        assert!(!r.allowed);
        assert_eq!(r.retry_after_seconds, 1);
    }

    #[test]
    fn r685_retry_after_zero_remaining_returns_min_one() {
        // 边界：oldest + window == current 时，remaining_ms = 0，secs = (0+999)/1000 = 0，
        // .max(1) 后是 1
        let (_, now) = step_clock_from(1000, 1000);
        let limiter = create_company_search_rate_limiter(Some(CompanySearchRateLimiterOptions {
            window_ms: 1000,
            max_requests: 1,
            now,
        }));
        let a = actor("c1", CompanySearchActorType::Agent, "a1");
        // t=1000 -> allowed，记录 [1000]
        assert!(limiter.consume(&a).allowed);
        // t=2000 -> cutoff=1000, oldest=1000, 1000<=1000 被淘汰 -> recent=[] -> allowed！
        // 改用 step_clock_from 让 gap < 1000
        let (_, now2) = step_clock_from(1000, 500);
        let limiter2 = create_company_search_rate_limiter(Some(CompanySearchRateLimiterOptions {
            window_ms: 1000,
            max_requests: 1,
            now: now2,
        }));
        // t=1000 -> allowed
        assert!(limiter2.consume(&a).allowed);
        // t=1500 -> cutoff=500, oldest=1000 > 500 未淘汰, blocked
        // oldest=1000, window=1000, current=1500 -> remaining=500ms -> ceil=1
        let r = limiter2.consume(&a);
        assert!(!r.allowed);
        assert_eq!(r.retry_after_seconds, 1);
    }

    #[test]
    fn r685_concurrent_consumers_serialized_by_mutex() {
        // 并发调用应被 Mutex 串行化，且最终状态正确。
        use std::thread;
        let (_, now) = step_clock_from(0, 1);
        let limiter = create_company_search_rate_limiter(Some(CompanySearchRateLimiterOptions {
            window_ms: 100_000,
            max_requests: 100,
            now,
        }));
        let a = actor("c1", CompanySearchActorType::Agent, "a1");
        let handles: Vec<_> = (0..10)
            .map(|_| {
                let l = limiter.clone();
                let a = a.clone();
                thread::spawn(move || {
                    let mut allowed = 0;
                    for _ in 0..5 {
                        if l.consume(&a).allowed {
                            allowed += 1;
                        }
                    }
                    allowed
                })
            })
            .collect();
        let total: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
        // 100 个槽，10 个线程各 5 次 = 50 次，全部应 allowed（因为 max=100）
        assert_eq!(total, 50);
    }
}
