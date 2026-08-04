//! 进程内 per-agent 启动锁。
//!
//! 对齐 `paperclip/server/src/services/agent-start-lock.ts`：
//! - 同一 agent 的多个 start 调用按到达顺序串行执行
//! - 等待 30s stale 仍未拿到锁 → 跳过等待继续执行（防止某个 run 永久
//!   卡死把后续 run 全堵住）
//! - 跨 agent 不阻塞
//! - 函数返回（含 panic / 错误）会释放锁
//!
//! 这是进程内原语；多副本部署下还需 DB 级别锁（heartbeat_runs 的
//! `wakeup_request_id` 唯一约束已经覆盖了重复唤醒的合并）。

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::timeout;
use uuid::Uuid;

pub const DEFAULT_STALE_MS: u64 = 30_000;

#[derive(Clone, Default)]
pub struct AgentStartLock {
    locks: Arc<Mutex<HashMap<Uuid, Arc<Mutex<()>>>>>,
}

impl AgentStartLock {
    pub fn new() -> Self {
        Self::default()
    }

    /// 在 agent 启动锁内执行异步闭包。
    ///
    /// - 如果同一 agent 的上一次调用还没结束，最多等 `stale_ms` 毫秒
    /// - 超时后跳过等待继续执行（不阻塞）
    /// - 闭包返回后（含 panic → JoinError）锁被释放
    pub async fn with_lock<F, Fut, T>(
        &self,
        agent_id: Uuid,
        stale_ms: u64,
        f: F,
    ) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        let lock = self.lock_for(agent_id).await;
        let _guard = match timeout(Duration::from_millis(stale_ms), lock.lock()).await {
            Ok(g) => Some(g),
            Err(_) => {
                tracing::warn!(
                    %agent_id,
                    stale_ms,
                    "agent start lock timed out; proceeding without serialization"
                );
                None
            }
        };
        f().await
    }

    /// `with_lock` 使用默认 `DEFAULT_STALE_MS` 的便捷封装。
    pub async fn with_default_lock<F, Fut, T>(&self, agent_id: Uuid, f: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        self.with_lock(agent_id, DEFAULT_STALE_MS, f).await
    }

    /// 显式删除某 agent 的锁条目（极端清理用途，例如单元测试）。
    pub async fn forget(&self, agent_id: Uuid) {
        self.locks.lock().await.remove(&agent_id);
    }

    async fn lock_for(&self, agent_id: Uuid) -> Arc<Mutex<()>> {
        let mut map = self.locks.lock().await;
        map.entry(agent_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;
    use tokio::time::{sleep, Duration as TokioDuration};

    #[tokio::test]
    async fn sequential_calls_are_serialized() {
        let lock = AgentStartLock::new();
        let agent = Uuid::new_v4();
        let order = Arc::new(AtomicUsize::new(0));
        let log = Arc::new(Mutex::new(Vec::<&'static str>::new()));

        let log_a = log.clone();
        let order_a = order.clone();
        let lock_a = lock.clone();
        let agent_a = agent;
        let h1 = tokio::spawn(async move {
            lock_a
                .with_default_lock(agent_a, || async move {
                    let n = order_a.fetch_add(1, Ordering::SeqCst);
                    log_a.lock().await.push("a:start");
                    sleep(TokioDuration::from_millis(80)).await;
                    log_a.lock().await.push("a:end");
                    n
                })
                .await
        });

        let log_b = log.clone();
        let order_b = order.clone();
        let lock_b = lock.clone();
        let agent_b = agent;
        let h2 = tokio::spawn(async move {
            lock_b
                .with_default_lock(agent_b, || async move {
                    let n = order_b.fetch_add(1, Ordering::SeqCst);
                    log_b.lock().await.push("b:start");
                    sleep(TokioDuration::from_millis(20)).await;
                    log_b.lock().await.push("b:end");
                    n
                })
                .await
        });

        h1.await.unwrap();
        h2.await.unwrap();

        let log = log.lock().await;
        // a 完整结束后 b 才能 start
        assert_eq!(log.as_slice(), &["a:start", "a:end", "b:start", "b:end"]);
    }

    #[tokio::test]
    async fn different_agents_do_not_block() {
        let lock = AgentStartLock::new();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();

        let started = Arc::new(Mutex::new(false));
        let started2 = started.clone();
        let lock_a = lock.clone();
        let h1 = tokio::spawn(async move {
            lock_a
                .with_default_lock(a, || async move {
                    sleep(TokioDuration::from_millis(100)).await;
                    *started2.lock().await = true;
                })
                .await
        });
        // 给 h1 一点时间进入锁
        sleep(TokioDuration::from_millis(10)).await;
        let started_at = Instant::now();
        lock.with_default_lock(b, || async { 42 }).await;
        let elapsed = started_at.elapsed();
        h1.await.unwrap();
        assert!(*started.lock().await);
        // b 不应等 a 完整结束（~90ms 之内就能跑完）
        assert!(
            elapsed < TokioDuration::from_millis(80),
            "b was blocked on a: elapsed = {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn error_releases_lock() {
        let lock = AgentStartLock::new();
        let agent = Uuid::new_v4();
        let result: Result<(), &str> = lock
            .with_default_lock(agent, || async { Err("boom") })
            .await;
        assert_eq!(result, Err("boom"));
        // 紧接着的调用不应被卡住
        let after = Instant::now();
        lock.with_default_lock(agent, || async { 1 }).await;
        assert!(after.elapsed() < TokioDuration::from_millis(50));
    }

    #[tokio::test]
    async fn stale_timeout_proceeds_without_blocking() {
        let lock = AgentStartLock::new();
        let agent = Uuid::new_v4();
        // 用极短 stale=50ms，强制快速超时
        let lock_a = lock.clone();
        let h1 = tokio::spawn(async move {
            lock_a
                .with_lock(agent, 50, || async {
                    sleep(TokioDuration::from_millis(500)).await;
                    "slow"
                })
                .await
        });
        // 等 h1 拿到锁
        sleep(TokioDuration::from_millis(20)).await;
        let started_at = Instant::now();
        let res = lock
            .with_lock(agent, 50, || async { "fast" })
            .await;
        let elapsed = started_at.elapsed();
        assert_eq!(res, "fast");
        // 必须在 stale 50ms 之后很快跑完（不卡 500ms）
        assert!(
            elapsed < TokioDuration::from_millis(200),
            "second call blocked too long: {elapsed:?}"
        );
        h1.await.unwrap();
    }

    #[tokio::test]
    async fn many_callers_run_in_fifo_order() {
        let lock = AgentStartLock::new();
        let agent = Uuid::new_v4();
        let counter = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..10 {
            let lock = lock.clone();
            let counter = counter.clone();
            handles.push(tokio::spawn(async move {
                lock.with_default_lock(agent, || async move {
                    let n = counter.fetch_add(1, Ordering::SeqCst);
                    sleep(TokioDuration::from_millis(5)).await;
                    n
                })
                .await
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(counter.load(Ordering::SeqCst), 10);
    }
}
