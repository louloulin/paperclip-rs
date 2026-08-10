#![forbid(unsafe_code)]
//! `pc-agent-start-lock` —— per-agent 启动锁。
//!
//! 对应 Node `server/src/services/agent-start-lock.ts`（48 行）。
//!
//! 设计目标：1:1 复刻
//! - 每个 agent 同一时间只能有一个 "start" 操作正在执行（或排队中）
//! - 后续 start 会等待前一个完成；超过 `AGENT_START_LOCK_STALE_MS = 30s` 时
//!   不再等待（视为 stale 锁，自动放过）
//! - 用 `Arc<Mutex<HashMap<agentId, LockEntry>>>` 实现并发安全
//! - 时钟 + 警告回调都通过 trait / Arc<dyn Fn> 注入，便于测试
//!
//! 与 Node 端差异：
//! - Node 用全局 `Map`；Rust 用 `Arc<Mutex<HashMap>>` 包成结构体（更显式）
//! - Node 的 `logger.warn` 通过 `WarnFn` trait 注入
//! - Node 用 `setTimeout`；Rust 用 `tokio::time::sleep`

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;

/// 启动锁 stale 阈值（毫秒）—— 与 Node 常量一致。
pub const AGENT_START_LOCK_STALE_MS: u64 = 30_000;

/// 时钟 trait 对象 —— 注入当前时间（毫秒）。
pub type ClockFn = Arc<dyn Fn() -> u64 + Send + Sync>;

fn default_clock() -> ClockFn {
    Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    })
}

/// 警告回调 —— 用于记录 stale / timed-out 锁。
pub type WarnFn = Arc<dyn Fn(&str, &serde_json::Value) + Send + Sync>;

fn noop_warn() -> WarnFn {
    Arc::new(|_, _| {})
}

/// 锁条目。
#[derive(Clone)]
struct LockEntry {
    notify: Arc<Notify>,
    started_at_ms: u64,
}

/// Drop guard —— 在未来被 drop 时清理锁条目（包括 cancel / panic 路径）。
struct LockGuard<'a> {
    locks: &'a Mutex<HashMap<String, LockEntry>>,
    agent_id: String,
    started_at_ms: u64,
    notify: Arc<Notify>,
}

impl Drop for LockGuard<'_> {
    fn drop(&mut self) {
        self.notify.notify_waiters();
        let mut map = self.locks.lock().expect("locks poisoned");
        if let Some(current) = map.get(&self.agent_id) {
            if current.started_at_ms == self.started_at_ms
                && Arc::ptr_eq(&current.notify, &self.notify)
            {
                map.remove(&self.agent_id);
            }
        }
    }
}

/// Agent 启动锁。
pub struct AgentStartLock {
    locks: Mutex<HashMap<String, LockEntry>>,
    stale_ms: u64,
    clock: ClockFn,
    warn: WarnFn,
}

impl std::fmt::Debug for AgentStartLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentStartLock")
            .field("stale_ms", &self.stale_ms)
            .field("clock", &"<fn>")
            .field("warn", &"<fn>")
            .finish()
    }
}

impl AgentStartLock {
    pub fn new() -> Self {
        Self {
            locks: Mutex::new(HashMap::new()),
            stale_ms: AGENT_START_LOCK_STALE_MS,
            clock: default_clock(),
            warn: noop_warn(),
        }
    }

    pub fn with_clock(mut self, clock: ClockFn) -> Self {
        self.clock = clock;
        self
    }

    pub fn with_warn(mut self, warn: WarnFn) -> Self {
        self.warn = warn;
        self
    }

    pub fn with_stale_ms(mut self, stale_ms: u64) -> Self {
        self.stale_ms = stale_ms;
        self
    }

    /// 用 `with_agent_start_lock(agent_id, fn)` 包裹异步操作。
    ///
    /// 与 Node `withAgentStartLock` 1:1 对齐：
    /// 1. 若有前一个锁条目且未 stale，等它完成（或超时）
    /// 2. 注册新条目
    /// 3. 执行 `fn()`
    /// 4. 通过 `LockGuard` 的 `Drop` 在未来被 drop（包括 cancel）时清理条目
    pub async fn with_lock<F, Fut, T>(&self, agent_id: &str, f: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        // 1. 等待前一个锁（若有）
        let previous = {
            let map = self.locks.lock().expect("locks poisoned");
            map.get(agent_id).cloned()
        };
        if let Some(prev) = previous {
            self.wait_for_previous(agent_id, prev).await;
        }

        // 2. 注册新条目 + Drop guard
        let notify = Arc::new(Notify::new());
        let started_at_ms = (self.clock)();
        let entry = LockEntry {
            notify: notify.clone(),
            started_at_ms,
        };
        self.locks
            .lock()
            .expect("locks poisoned")
            .insert(agent_id.to_string(), entry);

        let _guard = LockGuard {
            locks: &self.locks,
            agent_id: agent_id.to_string(),
            started_at_ms,
            notify: notify.clone(),
        };

        // 3. 执行
        f().await
        // _guard 在此处 drop，触发清理
    }

    async fn wait_for_previous(&self, agent_id: &str, prev: LockEntry) {
        let elapsed_ms = (self.clock)().saturating_sub(prev.started_at_ms);
        let remaining_ms = self.stale_ms.saturating_sub(elapsed_ms);
        if remaining_ms == 0 {
            (self.warn)(
                "agent_start_lock_stale",
                &serde_json::json!({
                    "agentId": agent_id,
                    "staleMs": elapsed_ms,
                }),
            );
            return;
        }

        // race: 前一个通知 vs timeout
        let timed_out = tokio::select! {
            _ = prev.notify.notified() => false,
            _ = tokio::time::sleep(Duration::from_millis(remaining_ms)) => true,
        };

        if timed_out {
            (self.warn)(
                "agent_start_lock_timed_out",
                &serde_json::json!({
                    "agentId": agent_id,
                    "staleMs": self.stale_ms,
                }),
            );
        }
    }
}

impl Default for AgentStartLock {
    fn default() -> Self {
        Self::new()
    }
}

/// 便捷函数：默认锁实例。
pub async fn with_agent_start_lock<F, Fut, T>(agent_id: &str, f: F) -> T
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    static LOCK: once_cell::sync::Lazy<AgentStartLock> =
        once_cell::sync::Lazy::new(AgentStartLock::new);
    LOCK.with_lock(agent_id, f).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn fake_clock(start: u64) -> (ClockFn, Arc<AtomicU64>) {
        let counter = Arc::new(AtomicU64::new(start));
        let c2 = counter.clone();
        (Arc::new(move || c2.load(Ordering::SeqCst)), counter)
    }

    fn recording_warn() -> (WarnFn, Arc<Mutex<Vec<(String, serde_json::Value)>>>) {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let r2 = recorded.clone();
        let warn: WarnFn = Arc::new(move |msg, data| {
            r2.lock().unwrap().push((msg.to_string(), data.clone()));
        });
        (warn, recorded)
    }

    #[tokio::test]
    async fn r703_single_call_executes() {
        let lock = AgentStartLock::new();
        let r: i32 = lock
            .with_lock("agent-1", || async { 42 })
            .await;
        assert_eq!(r, 42);
    }

    #[tokio::test]
    async fn r703_concurrent_calls_are_serialized() {
        let lock = Arc::new(AgentStartLock::new());
        let order = Arc::new(Mutex::new(Vec::new()));

        let l1 = lock.clone();
        let o1 = order.clone();
        let h1 = tokio::spawn(async move {
            l1.with_lock("agent-1", || async {
                o1.lock().unwrap().push("a-start");
                tokio::time::sleep(Duration::from_millis(50)).await;
                o1.lock().unwrap().push("a-end");
            })
            .await;
        });

        // 给第一个任务一点时间开始
        tokio::time::sleep(Duration::from_millis(10)).await;

        let l2 = lock.clone();
        let o2 = order.clone();
        let h2 = tokio::spawn(async move {
            l2.with_lock("agent-1", || async {
                o2.lock().unwrap().push("b-start");
                o2.lock().unwrap().push("b-end");
            })
            .await;
        });

        h1.await.unwrap();
        h2.await.unwrap();

        let log = order.lock().unwrap().clone();
        // 必须 a-end 在 b-start 之前
        let a_end = log.iter().position(|s| *s == "a-end").unwrap();
        let b_start = log.iter().position(|s| *s == "b-start").unwrap();
        assert!(a_end < b_start, "expected a-end before b-start, got {log:?}");
    }

    #[tokio::test]
    async fn r703_different_agents_run_concurrently() {
        let lock = Arc::new(AgentStartLock::new());
        let in_flight = Arc::new(AtomicU64::new(0));
        let max_in_flight = Arc::new(AtomicU64::new(0));

        let i1 = in_flight.clone();
        let m1 = max_in_flight.clone();
        let l1 = lock.clone();
        let h1 = tokio::spawn(async move {
            l1.with_lock("agent-1", || async {
                let cur = i1.fetch_add(1, Ordering::SeqCst) + 1;
                m1.fetch_max(cur, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                i1.fetch_sub(1, Ordering::SeqCst);
            })
            .await;
        });

        let i2 = in_flight.clone();
        let m2 = max_in_flight.clone();
        let l2 = lock.clone();
        let h2 = tokio::spawn(async move {
            l2.with_lock("agent-2", || async {
                let cur = i2.fetch_add(1, Ordering::SeqCst) + 1;
                m2.fetch_max(cur, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                i2.fetch_sub(1, Ordering::SeqCst);
            })
            .await;
        });

        h1.await.unwrap();
        h2.await.unwrap();
        assert_eq!(max_in_flight.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn r703_stale_lock_does_not_wait() {
        let (clock, counter) = fake_clock(1_000_000);
        let (warn, warns) = recording_warn();
        let lock = AgentStartLock::new()
            .with_clock(clock)
            .with_warn(warn);

        // 模拟已有 stale 锁：直接插入一个 started_at_ms < now - staleMs 的条目
        {
            let mut map = lock.locks.lock().unwrap();
            map.insert(
                "agent-1".to_string(),
                LockEntry {
                    notify: Arc::new(Notify::new()),
                    started_at_ms: 1_000_000, // elapsed = 100000 > 30000
                },
            );
        }

        // 立即推进时间到 1_100_000（> staleMs）
        counter.store(1_100_000, Ordering::SeqCst);

        let started = std::time::Instant::now();
        let r = lock
            .with_lock("agent-1", || async { 99 })
            .await;
        let elapsed = started.elapsed();

        assert_eq!(r, 99);
        // 不应等待 staleMs（30 秒）
        assert!(elapsed < Duration::from_millis(500), "stale lock blocked: {elapsed:?}");

        let w = warns.lock().unwrap();
        assert!(w.iter().any(|(m, _)| m == "agent_start_lock_stale"));
    }

    #[tokio::test]
    async fn r703_lock_cleared_after_completion() {
        let lock = AgentStartLock::new();
        let _: i32 = lock
            .with_lock("agent-x", || async { 1 })
            .await;
        assert!(lock.locks.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn r703_lock_cleared_even_when_fn_panics() {
        // fn 返回 Future 不会 panic，但 try_join 模拟错误的传播路径：
        // 用 select! 强制 cancel future 时仍应清理
        let lock = AgentStartLock::new();
        let lock_clone = &lock;
        let result: Result<i32, &'static str> = tokio::select! {
            r = lock_clone.with_lock("agent-y", || async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                1
            }) => Ok(r),
            _ = tokio::time::sleep(Duration::from_millis(10)) => Err("timeout"),
        };
        assert!(result.is_err());
        // 锁已被清理
        assert!(lock.locks.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn r703_timed_out_warn_when_wait_exceeds_stale() {
        // 模拟一个永不释放的锁：notify 不触发
        let (clock, _counter) = fake_clock(1_000_000);
        let (warn, warns) = recording_warn();
        let lock = AgentStartLock::new()
            .with_clock(clock)
            .with_warn(warn)
            .with_stale_ms(50); // 50ms stale

        // 预插入一个锁，started_at=1_000_000
        {
            let mut map = lock.locks.lock().unwrap();
            map.insert(
                "agent-1".to_string(),
                LockEntry {
                    notify: Arc::new(Notify::new()),
                    started_at_ms: 1_000_000,
                },
            );
        }

        // 不推进时间 → 0 elapsed < 50ms stale → 等待 staleMs
        let started = std::time::Instant::now();
        let _ = lock
            .with_lock("agent-1", || async { 1 })
            .await;
        let elapsed = started.elapsed();

        // 至少等待 staleMs
        assert!(elapsed >= Duration::from_millis(50), "should wait ~50ms: {elapsed:?}");

        let w = warns.lock().unwrap();
        assert!(w.iter().any(|(m, _)| m == "agent_start_lock_timed_out"));


    }
}
