//! 有界策略缓存的**常量与词表** —— 实现（LRU + 三档时效）归 **M9-9**。
//!
//! 本文件在 anchor 期只定**常量**：六个数字逐字取自上游 `internal/entitlement/cache.go`
//! 与 `client.go:31-39`。把它们钉在这里（而不是散在 M9-9 的实现里）的理由与
//! `mc-cloud::transport` 的 `DEFAULT_TIMEOUT` / `MAX_RESPONSE_BODY_SIZE` 相同：
//! **判据只有一处**，M9-9 的用例与实现不会各自抄一份数字。
//!
//! # 三档时效的语义（上游 `cacheEntry`，M9-9 照着实现）
//!
//! ```text
//! receivedAt ──freshUntil──> (新鲜: 直接用, Reason::CacheFresh)
//!            ──staleUntil──> (陈旧但可用: enforce 降级为 observe, Reason::Stale)
//!            ──retryAfter──> (退避中: 不重试; 无策略 ⇒ Reason::Unavailable)
//! ```
//!
//! - `freshUntil = receivedAt + valid_for_seconds`（**回话说了算**）；
//! - `staleUntil = freshUntil + grace`（[`STALE_GRACE`]）；
//! - `retryAfter` 只在**失败**时推进（[`FAILURE_RETRY`]）；
//! - `valid_for_seconds` 本身被 [`MAX_POLICY_TTL`] **上界**校验（超过即 `InvalidPolicy`）。
//!
//! # 三条纪律（M9-9 的 `DoD` 会逐条点名）
//!
//! 1. **缓存键只能是 `Id`**（上游注释逐字：「The workspace key is never inferred from
//!    response data, preventing one tenant's policy from entering another tenant's cache
//!    slot」）⇒ 响应体里**不得**有 workspace 字段可用；
//! 2. **上限是 `MAX_ENTRIES` 的 LRU**（`get` 会 `MoveToFront`）；
//! 3. **版本回退 ⇒ 拒绝写入**（上游 `cache.put` 返回 `errVersionRegression`）——
//!    即「新策略的 `subscription_version` 比缓存里的小」时**保留旧的**并让刷新失败。
//!
//! # M9-9 落地说明（本文件 = 上游 `cache.go` 113 行的等价物）
//!
//! [`PolicyCache`] 是**纯内存**的（无 IO、无时钟读：时刻由调用方传入），因此它可以被
//! 离线复现地逐条测试。上游用 `container/list` + `map` 手搓 LRU；本仓用
//! `Vec<Id>` 作链表（`MAX_ENTRIES` 只有 1e4，`retain` 的成本可忽略）⇒ 判据仍然是
//! 「`get` 命中即 `MoveToFront`、超上限逐出最旧」这一条。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use mc_core::{Id, Timestamp};

/// 上游 `defaultMaxEntries`（`client.go:33`）：LRU 上限。
pub const MAX_ENTRIES: usize = 10_000;

/// 上游 `defaultStaleGrace`（`client.go:34`）：陈旧宽限 —— 过了新鲜期后还能用多久。
pub const STALE_GRACE: Duration = Duration::from_mins(15);

/// 上游 `defaultFailureRetry`（`client.go:35`）：失败后的退避间隔。
pub const FAILURE_RETRY: Duration = Duration::from_secs(5);

/// 上游 `maxPolicyTTL`（`client.go:36`）：**回话**的 `valid_for_seconds` 的硬上界
/// （超过即 [`crate::Reason::InvalidPolicy`]）。
pub const MAX_POLICY_TTL: Duration = Duration::from_mins(5);

/// 上游 `maxResponseBodySize`（`client.go:37`）：策略响应体上限。
pub const MAX_RESPONSE_BODY_SIZE: usize = 64 << 10;

/// 上游 `defaultRequestTimeout`（`client.go:32`）：**每次** fetch 的超时
/// （单飞刷新共用同一个超时，见上游 `refresh` 的 `context.WithoutCancel` 注释）。
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

// ---------------------------------------------------------------------------
// M9-9 落地：快照 / 条目 / 有界 LRU
// ---------------------------------------------------------------------------

use crate::types::{Action, Gate, GateName};

/// 一次成功刷新的**不可变**策略快照（上游 `policySnapshot`）。
///
/// `gates` **两个取值都必须存在**（上游 `normalizePolicy` 逐字：缺任一 ⇒ `ErrInvalidPolicy`）
/// ⇒ 本仓用固定长度的 `[Gate; 2]` + [`GateName::ALL`] 的下标映射，
/// 「缺一个」这种状态在类型层面就表达不出来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicySnapshot {
    /// 云侧策略修订号（>0 才合法）。
    pub policy_revision: i64,
    /// 订阅版本号（>=0；版本回退的判据）。
    pub subscription_version: i64,
    /// 云侧给的 `valid_until`。
    pub cloud_valid_until: Timestamp,
    /// 两个 enforcement point 的指令，顺序与 [`GateName::ALL`] 逐项对应。
    pub gates: [Gate; 2],
}

impl PolicySnapshot {
    /// 某 gate 的指令（名字不在 [`GateName::ALL`] 里 ⇒ `off` 的 fail-open 形状）。
    #[must_use]
    pub fn gate(&self, name: GateName) -> Gate {
        GateName::ALL
            .iter()
            .position(|candidate| *candidate == name)
            .map_or_else(Gate::off, |index| self.gates[index].clone())
    }
}

/// 缓存里的一条（上游 `cacheEntry`）。
///
/// `Option<Timestamp>` 语义与上游的零值 `time.Time` 一致：未设置 ⇒ 该档**不适用**
/// （`None` 的 `fresh_until` 永远「不在新鲜期」）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CacheEntry {
    /// 有没有一份可用的策略（`false` = 只有一条「退避中」的空壳）。
    pub has_policy: bool,
    /// 策略快照。
    pub policy: PolicySnapshotCell,
    /// 新鲜期终点（`received_at + valid_for_seconds`）。
    pub fresh_until: Option<Timestamp>,
    /// 陈旧宽限期终点（`fresh_until + STALE_GRACE`）。
    pub stale_until: Option<Timestamp>,
    /// 失败退避到这一刻之前不再重试。
    pub retry_after: Option<Timestamp>,
}

impl CacheEntry {
    /// 「只有退避、还没有策略」的空壳（上游 `markFailure` 的 insert 分支）。
    #[must_use]
    pub const fn backoff(retry_after: Timestamp) -> Self {
        Self {
            has_policy: false,
            policy: PolicySnapshotCell::None,
            fresh_until: None,
            stale_until: None,
            retry_after: Some(retry_after),
        }
    }

    /// 是否仍在退避期内。
    #[must_use]
    pub fn is_backing_off(&self, now: Timestamp) -> bool {
        self.retry_after.is_some_and(|until| now < until)
    }

    /// 是否仍新鲜。
    #[must_use]
    pub fn is_fresh(&self, now: Timestamp) -> bool {
        self.has_policy && self.fresh_until.is_some_and(|until| now < until)
    }

    /// 是否有策略且**仍在陈旧宽限期内**（可观测、不可拦截）。
    #[must_use]
    pub fn is_stale_usable(&self, now: Timestamp) -> bool {
        self.has_policy && self.stale_until.is_some_and(|until| now < until)
    }
}

/// 快照在条目里的占位（`CacheEntry` 要 `Copy`，而 [`PolicySnapshot`] 不可能）。
///
/// 只允许两种状态：没有 / 有 ⇒ 「半截快照」在类型层面就表达不出来。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum PolicySnapshotCell {
    /// 无策略。
    #[default]
    None,
    /// 有策略。
    Some(Arc<PolicySnapshot>),
}

impl PolicySnapshotCell {
    /// 取快照（`None` ⇒ 调用方走 fail-open）。
    #[must_use]
    pub fn get(&self) -> Option<Arc<PolicySnapshot>> {
        match self {
            Self::None => None,
            Self::Some(snapshot) => Some(Arc::clone(snapshot)),
        }
    }
}

impl PolicySnapshot {
    /// 装箱进条目。
    #[must_use]
    pub fn into_cell(self) -> PolicySnapshotCell {
        PolicySnapshotCell::Some(Arc::new(self))
    }
}

/// 工作区键的 LRU 缓存（上游 `policyCache`）。
///
/// 纪律 1（键只能是 `Id`）由**签名**保证：所有方法都吃 [`Id`]，没有任何一个能从
/// 响应体里取键的入口。
#[derive(Debug)]
pub struct PolicyCache {
    max_entries: usize,
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    /// 命中顺序：`order[0]` 是**最近**用的。
    order: Vec<Id>,
    entries: HashMap<Id, CacheEntry>,
}

impl Inner {
    /// 已在场（`get` 的前置）。
    fn touch(&mut self, workspace_id: Id) -> bool {
        if !self.entries.contains_key(&workspace_id) {
            return false;
        }
        self.order.retain(|candidate| *candidate != workspace_id);
        self.order.insert(0, workspace_id);
        true
    }

    /// 要求 `inner` 已被锁住。
    fn insert(&mut self, workspace_id: Id, entry: CacheEntry, max_entries: usize) {
        self.entries.insert(workspace_id, entry);
        self.order.retain(|candidate| *candidate != workspace_id);
        self.order.insert(0, workspace_id);
        while self.order.len() > max_entries {
            if let Some(oldest) = self.order.pop() {
                self.entries.remove(&oldest);
            }
        }
    }
}

impl PolicyCache {
    /// 上限为 [`MAX_ENTRIES`] 的缓存。
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(MAX_ENTRIES)
    }

    /// 自定上限（只有测试与「故意做小」的上界检查会用）。
    #[must_use]
    pub fn with_capacity(max_entries: usize) -> Self {
        Self {
            max_entries: max_entries.max(1),
            inner: Mutex::new(Inner::default()),
        }
    }

    /// 毒化只可能来自**调用方在锁外 panic**之后的重入；本文件的临界区不 panic，
    /// 因此用 `unwrap_or_else(PoisonError::into_inner)` 恢复而不是把整条平面拉黑。
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// 读一条（命中即 `MoveToFront`）。
    #[must_use]
    pub fn get(&self, workspace_id: Id) -> Option<CacheEntry> {
        let mut inner = self.lock();
        if !inner.touch(workspace_id) {
            return None;
        }
        inner.entries.get(&workspace_id).cloned()
    }

    /// 写入一条快照（`received_at` 决定新鲜/陈旧两档的起点）。
    ///
    /// 逐字复刻上游 `put` 的版本守卫：`guard_versions = current.hasPolicy &&
    /// receivedAt.Before(current.staleUntil)`；守卫生效且新版本更小 ⇒ **保留旧的**并
    /// 回 [`PutOutcome::VersionRegression`]。
    pub fn put(
        &self,
        workspace_id: Id,
        snapshot: PolicySnapshot,
        valid_for: Duration,
        received_at: Timestamp,
    ) -> PutOutcome {
        let mut inner = self.lock();
        let cell = snapshot.into_cell();
        let entry = CacheEntry {
            has_policy: true,
            policy: cell.clone(),
            fresh_until: Some(shift(received_at, valid_for)),
            stale_until: Some(shift(received_at, valid_for + STALE_GRACE)),
            retry_after: None,
        };
        if let Some(current) = inner.entries.get(&workspace_id).cloned() {
            // 逐字：guard_versions = current.hasPolicy && receivedAt.Before(current.staleUntil)
            let guard_versions = current.is_stale_usable(received_at);
            if let (true, Some(cached), Some(fresh)) =
                (guard_versions, current.policy.get(), cell.get())
            {
                if fresh.subscription_version < cached.subscription_version {
                    // 旧条目保留（它仍可用于有界陈旧观测），刷新按失败处理。
                    return PutOutcome::VersionRegression;
                }
            }
        }
        inner.insert(workspace_id, entry, self.max_entries);
        PutOutcome::Stored
    }

    /// 记一次失败：把该工作区推进退避期（已有条目就只改 `retry_after`）。
    pub fn mark_failure(&self, workspace_id: Id, retry_after: Timestamp) {
        let mut inner = self.lock();
        let entry = inner.entries.get(&workspace_id).cloned().map_or_else(
            || CacheEntry::backoff(retry_after),
            |mut current| {
                current.retry_after = Some(retry_after);
                current
            },
        );
        inner.insert(workspace_id, entry, self.max_entries);
    }

    /// 现存条数（诊断用，也是 LRU 逐出判据的证据）。
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    /// 是否空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for PolicyCache {
    fn default() -> Self {
        Self::new()
    }
}

/// `Timestamp` 的秒级平移（**不引入 chrono**：`mc-entitlement` 的依赖边被锚点冻结，
/// 而本仓的 TTL / grace / 退避三个常量全是整秒 ⇒ 秒级平移就是逐字等价）。
fn shift(origin: Timestamp, by: Duration) -> Timestamp {
    Timestamp::from_unix(origin.as_unix() + i64::try_from(by.as_secs()).unwrap_or(i64::MAX))
}

/// `put` 的结局。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PutOutcome {
    /// 已写入。
    Stored,
    /// 版本回退 ⇒ **拒绝写入**（旧条目保留，刷新按失败处理）。
    VersionRegression,
}

/// 便利构造：一个「两个 gate 齐全」的快照（顺序逐字对应 [`GateName::ALL`]）。
#[must_use]
pub fn snapshot_for(
    issue_count: Gate,
    autopilot_runs: Gate,
    policy_revision: i64,
    subscription_version: i64,
    cloud_valid_until: Timestamp,
) -> PolicySnapshot {
    PolicySnapshot {
        policy_revision,
        subscription_version,
        cloud_valid_until,
        gates: [issue_count, autopilot_runs],
    }
}

/// 便利构造：`action` + `limit`，period 三字段齐全（`autopilot_runs` 的硬要求）。
#[must_use]
pub fn gate_with_period(action: Action, limit: i64, start: Timestamp, end: Timestamp) -> Gate {
    Gate {
        action,
        limit: Some(limit),
        period_start: Some(start),
        period_end: Some(end),
        reset_at: Some(end),
        notifications: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 六个数字必须在类型层面就能被 M9-9 复用（不许各抄一份）。
    #[test]
    fn upstream_limits_are_pinned_once() {
        assert_eq!(MAX_ENTRIES, 10_000);
        assert_eq!(STALE_GRACE, Duration::from_mins(15));
        assert_eq!(FAILURE_RETRY, Duration::from_secs(5));
        assert_eq!(MAX_POLICY_TTL, Duration::from_mins(5));
        assert_eq!(MAX_RESPONSE_BODY_SIZE, 64 * 1024);
        assert_eq!(REQUEST_TIMEOUT, Duration::from_secs(3));
        // 三档时效的序关系（实现里 `freshUntil < staleUntil`，退避与它们独立）。
        assert!(STALE_GRACE > Duration::ZERO);
        assert!(MAX_POLICY_TTL > Duration::ZERO);
    }

    fn at(secs: i64) -> Timestamp {
        Timestamp::from_unix(secs)
    }

    fn enforcing_gate(limit: i64) -> Gate {
        gate_with_period(Action::Enforce, limit, at(1_000), at(2_000))
    }

    fn snapshot(version: i64) -> PolicySnapshot {
        snapshot_for(
            enforcing_gate(10),
            enforcing_gate(20),
            1,
            version,
            at(9_999),
        )
    }

    /// 三档：新鲜 → 陈旧可用 → 两者都过。`has_policy` 与 `retry_after` 独立于时效。
    #[test]
    fn three_tiers_are_fresh_then_stale_then_nothing() {
        let cache = PolicyCache::with_capacity(4);
        let ws = Id::new();
        let t0 = at(1_000_000);
        assert_eq!(
            cache.put(ws, snapshot(1), MAX_POLICY_TTL, t0),
            PutOutcome::Stored
        );

        // 新鲜期 = received_at + valid_for（= TTL 5min）。
        let fresh = cache.get(ws).expect("entry");
        assert!(fresh.is_fresh(t0));
        assert!(fresh.is_stale_usable(t0));
        assert!(!fresh.is_backing_off(t0));

        // 过了新鲜期、还在 15m 宽限内 ⇒ 只能观测。
        let stale_at = shift(t0, MAX_POLICY_TTL + Duration::from_secs(1));
        let stale = cache.get(ws).expect("entry");
        assert!(!stale.is_fresh(stale_at));
        assert!(stale.is_stale_usable(stale_at));

        // 过了宽限期 ⇒ 什么都不剩。
        let entry = cache.get(ws).expect("entry");
        let dead_at = entry.stale_until.map_or_else(Timestamp::default, |until| {
            shift(until, Duration::from_secs(1))
        });
        assert!(!entry.is_fresh(dead_at));
        assert!(!entry.is_stale_usable(dead_at));
    }

    /// 失败只推进退避、**不动**新鲜/陈旧两档（上游 `markFailure` 逐字）。
    #[test]
    fn mark_failure_only_moves_retry_after() {
        let cache = PolicyCache::new();
        let ws = Id::new();
        let t0 = at(2_000_000);
        cache.put(ws, snapshot(1), MAX_POLICY_TTL, t0);

        let later = shift(t0, Duration::from_secs(1));
        cache.mark_failure(ws, shift(later, FAILURE_RETRY));
        let entry = cache.get(ws).expect("entry");
        assert!(entry.is_fresh(later), "退避不得让新鲜条目变陈旧");
        assert!(entry.is_backing_off(later));
        assert!(!entry.is_backing_off(shift(later, FAILURE_RETRY + Duration::from_secs(1))));

        // 从未见过的工作区 ⇒ 插入一条只有退避的空壳。
        let cold = Id::new();
        cache.mark_failure(cold, shift(t0, FAILURE_RETRY));
        let entry = cache.get(cold).expect("entry");
        assert!(!entry.has_policy);
        assert!(entry.is_backing_off(t0));
        assert!(!entry.is_fresh(t0));
        assert!(!entry.is_stale_usable(t0));
    }

    /// 版本守卫：只在「旧条目仍在陈旧宽限期内」时生效（上游 `guardVersions`）。
    #[test]
    fn version_regression_is_refused_only_while_the_old_entry_is_usable() {
        let cache = PolicyCache::new();
        let ws = Id::new();
        let t0 = at(3_000_000);
        assert_eq!(
            cache.put(ws, snapshot(7), MAX_POLICY_TTL, t0),
            PutOutcome::Stored
        );

        // 宽限期内：新版本更小 ⇒ 拒绝，旧快照原封不动。
        let mid = shift(t0, Duration::from_secs(1));
        assert_eq!(
            cache.put(ws, snapshot(6), MAX_POLICY_TTL, mid),
            PutOutcome::VersionRegression
        );
        let kept = cache.get(ws).expect("entry");
        assert_eq!(kept.policy.get().expect("snapshot").subscription_version, 7);
        assert!(kept.is_fresh(mid), "被拒的写入不得扰动现有条目");

        // 过了宽限期：守卫失效，当前回话可以恢复（避免回滚后永远刷新不出来）。
        let after = shift(t0, MAX_POLICY_TTL + STALE_GRACE + Duration::from_secs(1));
        assert_eq!(
            cache.put(ws, snapshot(6), MAX_POLICY_TTL, after),
            PutOutcome::Stored
        );
        assert_eq!(
            cache
                .get(ws)
                .expect("entry")
                .policy
                .get()
                .expect("snapshot")
                .subscription_version,
            6
        );

        // 相同版本号不是回退（上游是 `<` 不是 `<=`）。
        let same = shift(after, Duration::from_secs(1));
        assert_eq!(
            cache.put(ws, snapshot(6), MAX_POLICY_TTL, same),
            PutOutcome::Stored
        );
    }

    /// 纪律 2：上限 LRU，`get` 会把条目提到最前 ⇒ 逐出的是**最久没被问**的那个。
    #[test]
    fn lru_evicts_the_least_recently_used_workspace() {
        let cache = PolicyCache::with_capacity(2);
        let t0 = at(4_000_000);
        let (a, b, c) = (Id::new(), Id::new(), Id::new());
        for ws in [a, b] {
            cache.put(ws, snapshot(1), MAX_POLICY_TTL, t0);
        }
        assert_eq!(cache.len(), 2);
        // 问一次 a ⇒ b 变成最旧。
        assert!(cache.get(a).is_some());
        cache.put(c, snapshot(1), MAX_POLICY_TTL, t0);
        assert_eq!(cache.len(), 2);
        assert!(cache.get(a).is_some(), "a 刚被问过，不该被逐出");
        assert!(cache.get(c).is_some());
        // 磁盘上也不该有它（纪律 1：键只能是 Id）。
        assert!(cache.get(b).is_none(), "b 是最久没问的 ⇒ 被逐出");
        assert_eq!(cache.len(), 2);
    }

    /// 上限用 [`MAX_ENTRIES`] 而非某个魔数（`with_capacity(1)` 也要能工作）。
    #[test]
    fn capacity_defaults_to_the_pinned_limit_and_never_zeroes_out() {
        assert_eq!(PolicyCache::new().max_entries, MAX_ENTRIES);
        let cache = PolicyCache::with_capacity(0);
        let ws = Id::new();
        cache.mark_failure(ws, at(1));
        assert_eq!(
            cache.len(),
            1,
            "容量 0 会被抬到 1，否则 put 进去就丢、读出来永远 miss"
        );
    }

    /// 快照的两个 gate 按 [`GateName::ALL`] 的顺序取；名字不在表里 ⇒ fail-open。
    #[test]
    fn snapshot_gate_lookup_follows_the_pinned_order() {
        let snap = snapshot_for(enforcing_gate(10), enforcing_gate(20), 1, 1, at(1));
        assert_eq!(snap.gate(GateName::IssueCount).limit, Some(10));
        assert_eq!(snap.gate(GateName::AutopilotRuns).limit, Some(20));
        assert!(PolicySnapshotCell::None.get().is_none());
    }
}
