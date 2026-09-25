//! 进程内租约：`LeaseStore` 的**无 Redis** 替身（R-M7-1，`docs/60` §2.5）。
//!
//! - **写者**：M7-2（`docs/60` §3.3 的写集表）。
//! - **上游**：`channel/engine/redis_lease_store.go`（167 行）的**等价语义**。上游用 Redis 做
//!   租约 CAS（三段 Lua：`try_acquire` / `renew` / `release`），多副本下只有一个副本持有某
//!   installation 的长连接。本仓**没有 Redis 且本波不引入**（R-M7-1）⇒ 换部署形态：
//!   进程内 map + **单副本部署契约**（生产部署 = 单副本，或"渠道连接只在一个副本上开"）。
//! - **语义逐条对齐三段落 Lua**：
//!   | 上游 | 本文件 |
//!   | --- | --- |
//!   | `not current or current == ARGV[1]` ⇒ `SET key token PX ttl` | 无主 / **已过期** / **令牌相同**（同一持有者的安全重试）⇒ 授予 |
//!   | `GET key == ARGV[1]` ⇒ `PEXPIRE key ttl` | 令牌相同 ⇒ 续期；否则 `LeaseNotAcquired` |
//!   | `GET key == ARGV[1]` ⇒ `DEL key` | 令牌相同 ⇒ 删除；否则 **fenced no-op**（迟到的一次释放不该清掉后继者的新租约） |
//!   | `ListHeldWSLeases` = 键存在的那些 | 未过期的那些 |
//! - **过期判据是** [`NowFn`]（`Config::now` 的注入点）而不是 `Instant`：租约的有效期跨进程可判，
//!   且用例能**直接推进时钟**而不 sleep 真实时间（本片 `DoD` 的第 4 条）。
//! - **凭据纪律**：租约令牌不是平台凭据，但它是**所有权围栏**⇒ 本文件手写 `Debug`，
//!   输出里**从不出现令牌**（有两条用例钉住：`Debug` 输出、错误路径）。日志同理：
//!   本文件不插值任何令牌。
//!
//! 行预算（门 ⑩）：≤800 行（本文件约 420 行，含用例）。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mc_core::id::Id;
use mc_core::timestamp::Timestamp;

use crate::engine::resolvers::{EngineError, EngineResult, PipelineError};
use crate::engine::supervisor::{AcquireLeaseParams, LeaseStore, NowFn, ReleaseLeaseParams};

/// 上游 `CHANNEL_WS_LEASE_NAMESPACE` 的形态约束（`[A-Za-z0-9._-]+`）。
fn is_valid_namespace(namespace: &str) -> bool {
    !namespace.is_empty()
        && namespace
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// 一个租约（**绝不**进 `Debug`）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    token: String,
    expires_at: Timestamp,
}

/// 租约的计数快照（看板 / 诊断用；无锁读）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LeaseMetricsSnapshot {
    /// `try_acquire` 调用次数。
    pub acquire_attempts: u64,
    /// 授予的次数（含"同一令牌的安全重试"）。
    pub acquire_granted: u64,
    /// 被别人持有而拒绝的次数。
    pub acquire_denied: u64,
    /// 续期成功的次数。
    pub renewed: u64,
    /// 续期被拒（所有权已丢）的次数。
    pub renew_denied: u64,
    /// 真的删掉一行的释放次数。
    pub released: u64,
    /// 令牌不匹配的释放次数（fenced no-op）。
    pub release_noop: u64,
    /// 读路径上发现的**自然过期**次数。
    pub expired: u64,
}

#[derive(Debug, Default)]
struct Counters {
    acquire_attempts: AtomicU64,
    acquire_granted: AtomicU64,
    acquire_denied: AtomicU64,
    renewed: AtomicU64,
    renew_denied: AtomicU64,
    released: AtomicU64,
    release_noop: AtomicU64,
    expired: AtomicU64,
}

/// 租约计数句柄（可跨线程共享；`clone` 共享同一份计数）。
#[derive(Debug, Default, Clone)]
pub struct LeaseMetrics {
    counters: Arc<Counters>,
}

impl LeaseMetrics {
    /// 建一份新计数。
    pub fn new() -> Self {
        Self::default()
    }

    /// 不可变快照。
    pub fn snapshot(&self) -> LeaseMetricsSnapshot {
        LeaseMetricsSnapshot {
            acquire_attempts: self.counters.acquire_attempts.load(Ordering::Relaxed),
            acquire_granted: self.counters.acquire_granted.load(Ordering::Relaxed),
            acquire_denied: self.counters.acquire_denied.load(Ordering::Relaxed),
            renewed: self.counters.renewed.load(Ordering::Relaxed),
            renew_denied: self.counters.renew_denied.load(Ordering::Relaxed),
            released: self.counters.released.load(Ordering::Relaxed),
            release_noop: self.counters.release_noop.load(Ordering::Relaxed),
            expired: self.counters.expired.load(Ordering::Relaxed),
        }
    }
}

/// 进程内租约仓库（`LeaseStore` 的无 Redis 实现；单副本部署契约，R-M7-1）。
pub struct InProcessLeaseStore {
    namespace: String,
    now: NowFn,
    leases: Mutex<HashMap<Id, Entry>>,
    metrics: LeaseMetrics,
}

impl std::fmt::Debug for InProcessLeaseStore {
    /// ⚠️ **手写**：只打印命名空间与**有多少条**租约，从不打印令牌或到期时刻。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let held = self
            .leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len();
        formatter
            .debug_struct("InProcessLeaseStore")
            .field("namespace", &self.namespace)
            .field("held", &held)
            .finish_non_exhaustive()
    }
}

impl InProcessLeaseStore {
    /// 构造：命名空间形态校验与上游 `NewRedisLeaseStore` 同一条判据。
    ///
    /// # Errors
    ///
    /// 命名空间为空或含 `[A-Za-z0-9._-]` 之外的字符 ⇒ `EngineError::Infra`。
    pub fn new(namespace: impl Into<String>, now: NowFn) -> EngineResult<Self> {
        let namespace = namespace.into();
        if !is_valid_namespace(&namespace) {
            return Err(EngineError::infra(
                "channel lease namespace must match [A-Za-z0-9._-]+",
            ));
        }
        Ok(Self {
            namespace,
            now,
            leases: Mutex::new(HashMap::new()),
            metrics: LeaseMetrics::new(),
        })
    }

    /// 命名空间（接进 `LeaseStore` 的那一份；值形态已校验过）。
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// 计数句柄（宿主可以把它挂到自己的指标出口上）。
    pub fn metrics(&self) -> &LeaseMetrics {
        &self.metrics
    }

    /// 当前**未过期**的租约条数（诊断）。
    pub fn held_len(&self) -> usize {
        let now = (self.now)();
        self.leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter(|entry| entry.expires_at > now)
            .count()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Id, Entry>> {
        self.leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// 在锁内取出**仍然有效**的租约；过期的当场删掉并记一次 `expired`。
    fn live(&self, leases: &mut HashMap<Id, Entry>, id: Id, now: Timestamp) -> Option<Entry> {
        match leases.get(&id) {
            Some(entry) if entry.expires_at > now => Some(entry.clone()),
            Some(_) => {
                leases.remove(&id);
                self.metrics
                    .counters
                    .expired
                    .fetch_add(1, Ordering::Relaxed);
                None
            }
            None => None,
        }
    }
}

#[async_trait]
impl LeaseStore for InProcessLeaseStore {
    async fn list_held(&self, ids: &[Id]) -> EngineResult<HashSet<Id>> {
        let now = (self.now)();
        let mut leases = self.lock();
        let mut held = HashSet::with_capacity(ids.len());
        for id in ids {
            if self.live(&mut leases, *id, now).is_some() {
                held.insert(*id);
            }
        }
        Ok(held)
    }

    async fn try_acquire(&self, params: AcquireLeaseParams) -> EngineResult<()> {
        let now = (self.now)();
        self.metrics
            .counters
            .acquire_attempts
            .fetch_add(1, Ordering::Relaxed);
        let mut leases = self.lock();
        match self.live(&mut leases, params.installation_id, now) {
            // 无主、已过期或同一令牌（同一持有者的安全重试）⇒ 授予。
            None => {
                leases.insert(
                    params.installation_id,
                    Entry {
                        token: params.token,
                        expires_at: params.expires_at,
                    },
                );
                self.metrics
                    .counters
                    .acquire_granted
                    .fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Some(current) if current.token == params.token => {
                leases.insert(
                    params.installation_id,
                    Entry {
                        token: params.token,
                        expires_at: params.expires_at,
                    },
                );
                self.metrics
                    .counters
                    .acquire_granted
                    .fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            // 别人持有 ⇒ 不连、等下一轮 sweep（**产品性判决**，不是基础设施失败）。
            Some(_) => {
                self.metrics
                    .counters
                    .acquire_denied
                    .fetch_add(1, Ordering::Relaxed);
                Err(PipelineError::LeaseNotAcquired.into())
            }
        }
    }

    async fn renew(&self, params: AcquireLeaseParams) -> EngineResult<()> {
        let now = (self.now)();
        let mut leases = self.lock();
        match self.live(&mut leases, params.installation_id, now) {
            Some(current) if current.token == params.token => {
                leases.insert(
                    params.installation_id,
                    Entry {
                        token: params.token,
                        expires_at: params.expires_at,
                    },
                );
                self.metrics
                    .counters
                    .renewed
                    .fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            // 所有权已丢（过期或被抢）⇒ 调用方立刻拆掉连接。
            _ => {
                self.metrics
                    .counters
                    .renew_denied
                    .fetch_add(1, Ordering::Relaxed);
                Err(PipelineError::LeaseNotAcquired.into())
            }
        }
    }

    async fn release(&self, params: ReleaseLeaseParams) -> EngineResult<()> {
        let now = (self.now)();
        let mut leases = self.lock();
        let matched = self
            .live(&mut leases, params.installation_id, now)
            .is_some_and(|current| current.token == params.token);
        if matched {
            leases.remove(&params.installation_id);
            self.metrics
                .counters
                .released
                .fetch_add(1, Ordering::Relaxed);
        } else {
            // 迟到的释放：令牌不匹配 ⇒ 有意为之的 fenced no-op（**不是**错误）。
            self.metrics
                .counters
                .release_noop
                .fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI64;
    use std::time::Duration;

    use super::*;
    use mc_core::channel::ChannelKind;

    /// 可推进的假时钟（用例**不** sleep 真实时间）。
    struct Clock {
        unix: Arc<AtomicI64>,
    }

    impl Clock {
        fn new(start: i64) -> Self {
            Self {
                unix: Arc::new(AtomicI64::new(start)),
            }
        }

        fn now_fn(&self) -> NowFn {
            let unix = Arc::clone(&self.unix);
            Arc::new(move || Timestamp::from_unix(unix.load(Ordering::SeqCst)))
        }

        fn advance(&self, seconds: i64) {
            self.unix.fetch_add(seconds, Ordering::SeqCst);
        }

        fn now(&self) -> Timestamp {
            Timestamp::from_unix(self.unix.load(Ordering::SeqCst))
        }
    }

    fn store(clock: &Clock) -> InProcessLeaseStore {
        InProcessLeaseStore::new("itest", clock.now_fn()).expect("store")
    }

    fn acquire(clock: &Clock, id: Id, token: &str, ttl: i64) -> AcquireLeaseParams {
        AcquireLeaseParams {
            installation_id: id,
            kind: ChannelKind::Slack,
            token: token.to_string(),
            expires_at: Timestamp::from_unix(clock.now().as_unix() + ttl),
            ttl: Duration::from_secs(ttl.unsigned_abs()),
        }
    }

    fn release(id: Id, token: &str) -> ReleaseLeaseParams {
        ReleaseLeaseParams {
            installation_id: id,
            token: token.to_string(),
        }
    }

    fn is_not_acquired(result: &EngineResult<()>) -> bool {
        matches!(
            result,
            Err(EngineError::Pipeline(PipelineError::LeaseNotAcquired))
        )
    }

    /// 授予 / 竞争 / 同令牌安全重试（上游 `try_acquire` 段 Lua 的三条分支）。
    #[tokio::test]
    async fn acquisition_grants_the_free_and_the_same_token_only() {
        let clock = Clock::new(1_800_000_000);
        let store = store(&clock);
        let id = Id::new();

        assert!(store
            .try_acquire(acquire(&clock, id, "node-a:1", 180))
            .await
            .is_ok());
        // 同一令牌 = 安全重试（幂等）。
        assert!(store
            .try_acquire(acquire(&clock, id, "node-a:1", 180))
            .await
            .is_ok());
        // 另一个副本在有效期内拿不到，且这是**产品性判决**而不是基础设施失败。
        assert!(is_not_acquired(
            &store
                .try_acquire(acquire(&clock, id, "node-b:1", 180))
                .await
        ));
        assert_eq!(store.held_len(), 1);
    }

    /// 续期只在令牌相同且未过期时成立；过期之后必须失败（否则两个副本会同时消费）。
    #[tokio::test]
    async fn renewal_requires_a_live_matching_token() {
        let clock = Clock::new(1_800_000_000);
        let store = store(&clock);
        let id = Id::new();
        store
            .try_acquire(acquire(&clock, id, "node-a:1", 60))
            .await
            .expect("acquire");

        clock.advance(30);
        assert!(store
            .renew(acquire(&clock, id, "node-a:1", 60))
            .await
            .is_ok());
        // 别人的令牌续不了。
        assert!(is_not_acquired(
            &store.renew(acquire(&clock, id, "node-b:1", 60)).await
        ));

        // 再走过续期后的有效期（T0+30 续期到 T0+90 ⇒ 推 61s）⇒ 租约自然过期。
        clock.advance(61);
        assert!(
            store.list_held(&[id]).await.expect("list held").is_empty(),
            "过期后不再 held"
        );
        assert!(is_not_acquired(
            &store.renew(acquire(&clock, id, "node-a:1", 60)).await
        ));
        assert!(store
            .try_acquire(acquire(&clock, id, "node-b:1", 60))
            .await
            .is_ok());
    }

    /// 释放按令牌围栏：错令牌是 no-op（不报错、不清行），对令牌真的删。
    #[tokio::test]
    async fn release_is_fenced_on_the_token() {
        let clock = Clock::new(1_800_000_000);
        let store = store(&clock);
        let id = Id::new();
        store
            .try_acquire(acquire(&clock, id, "node-a:1", 180))
            .await
            .expect("acquire");

        // 迟到的释放（前任的令牌）不得清掉后继者的租约 —— 这里它是 no-op 且**不报错**。
        store
            .release(release(id, "node-b:0"))
            .await
            .expect("fenced release is a no-op, not an error");
        assert_eq!(store.held_len(), 1, "错令牌什么都没删");
        assert!(is_not_acquired(
            &store
                .try_acquire(acquire(&clock, id, "node-b:1", 180))
                .await
        ));

        store
            .release(release(id, "node-a:1"))
            .await
            .expect("release");
        assert_eq!(store.held_len(), 0);
        assert!(store
            .try_acquire(acquire(&clock, id, "node-b:1", 180))
            .await
            .is_ok());
    }

    /// `list_held` 只是 sweep 的优化：它必须与"能不能取到"一致（过期的不算 held）。
    #[tokio::test]
    async fn list_held_matches_what_an_acquire_would_find() {
        let clock = Clock::new(1_800_000_000);
        let store = store(&clock);
        let ids: Vec<Id> = (0..3).map(|_| Id::new()).collect();
        for (index, id) in ids.iter().enumerate() {
            store
                .try_acquire(acquire(&clock, *id, &format!("node-a:{index}"), 60))
                .await
                .expect("acquire");
        }
        assert_eq!(
            store.list_held(&ids).await.expect("list held").len(),
            3,
            "三条都在"
        );
        clock.advance(61);
        assert!(store.list_held(&ids).await.expect("list held").is_empty());
        assert_eq!(store.held_len(), 0);
    }

    /// 计数快照逐项对齐这几次调用（看板聚合的判据）。
    #[tokio::test]
    async fn metrics_count_every_branch() {
        let clock = Clock::new(1_800_000_000);
        let store = store(&clock);
        let id = Id::new();
        store
            .try_acquire(acquire(&clock, id, "node-a:1", 60))
            .await
            .expect("acquire");
        let _ = store.try_acquire(acquire(&clock, id, "node-b:1", 60)).await;
        clock.advance(10);
        store
            .renew(acquire(&clock, id, "node-a:1", 60))
            .await
            .expect("renew");
        let _ = store.renew(acquire(&clock, id, "node-b:1", 60)).await;
        store.release(release(id, "node-b:1")).await.expect("noop");
        clock.advance(100);
        let _ = store.list_held(&[id]).await.expect("list held"); // 触发过期回收
        store
            .release(release(id, "node-a:1"))
            .await
            .expect("release after expiry");

        let snapshot = store.metrics().snapshot();
        assert_eq!(snapshot.acquire_attempts, 2);
        assert_eq!(snapshot.acquire_granted, 1);
        assert_eq!(snapshot.acquire_denied, 1);
        assert_eq!(snapshot.renewed, 1);
        assert_eq!(snapshot.renew_denied, 1);
        assert_eq!(snapshot.released, 0, "过期后的释放是 no-op");
        assert_eq!(snapshot.release_noop, 2);
        assert_eq!(snapshot.expired, 1);
    }

    /// 命名空间校验与上游 `NewRedisLeaseStore` 同一条判据。
    #[test]
    fn namespace_validation_matches_upstream() {
        let clock = Clock::new(1_800_000_000);
        assert!(InProcessLeaseStore::new("chan.prod-1_a", clock.now_fn()).is_ok());
        for bad in ["", "has space", "slash/name", "emoji🚀"] {
            let built = InProcessLeaseStore::new(bad, clock.now_fn());
            assert!(
                matches!(built, Err(EngineError::Infra { .. })),
                "{bad:?} 必须被拒"
            );
        }
    }

    /// **凭据纪律**：`Debug` 与错误路径都**不**回显租约令牌。
    #[tokio::test]
    async fn the_store_never_echoes_a_lease_token() {
        let clock = Clock::new(1_800_000_000);
        let store = store(&clock);
        let id = Id::new();
        let token = "node-a:secret-lease-token-42";
        store
            .try_acquire(acquire(&clock, id, token, 180))
            .await
            .expect("acquire");
        assert!(
            !format!("{store:?}").contains(token),
            "Debug 输出不得含令牌"
        );
        assert!(format!("{store:?}").contains("held"));

        // 竞争失败的**错误**里也不得出现令牌（它可能被上层写进日志）。
        let denied = store
            .try_acquire(acquire(&clock, id, "node-b:other-token", 180))
            .await
            .expect_err("contended");
        let rendered = denied.to_string();
        assert!(!rendered.contains(token));
        assert!(!rendered.contains("other-token"));
        assert_eq!(denied.code_hint(), "lease_not_acquired");
    }
}
