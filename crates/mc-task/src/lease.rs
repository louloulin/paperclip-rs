//! 租约（`prepare_lease_expires_at`）与「僵尸任务」清扫的**纯判定**。
//!
//! 上游把这些判定写在四条 `UPDATE ... WHERE` 里（`agent.sql` / `runtime.sql`）。
//! 本模块把它们抽成纯函数：输入是任务行 + 该行 runtime 的存活证据 + 阈值 +
//! `now`，输出是 [`StaleVerdict`]（或 [`TaskEvent::Reclaim`]）。这样 M3-6 的
//! 仓储实现只需要「取行 → 判定 → 按写集落库」，判定逻辑可以在这里被 100%
//! 覆盖，不需要数据库。
//!
//! # 阈值来源
//!
//! | 常量 | 值 | 上游出处 |
//! |---|---|---|
//! | [`PREPARE_LEASE_SECS`] | 45 | `service/task.go:195` `prepareLeaseDuration` |
//! | [`PREPARE_LEASE_EXTEND_INTERVAL_SECS`] | 15 | `startTaskPrepareLeaseExtender`（`FailStaleTasks` 注释） |
//! | [`CLAIM_RECOVERY_SECS`] | 90 | `service/task.go:194` `claimResponseRecoveryWindow` |
//! | [`RUNTIME_STALE_SECS`] | 150 | `service/task.go:189` `RuntimeClaimFreshnessSeconds` |
//! | [`DISPATCH_TIMEOUT_SECS`] | 300 | `cmd/server/runtime_sweeper.go:74` |
//! | [`RUNNING_TIMEOUT_SECS`] | 9000 | `cmd/server/runtime_sweeper.go:89` |
//! | [`RUNTIME_RECONNECT_GRACE_SECS`] | 10800 | `cmd/server/runtime_sweeper.go:43` `defaultRuntimeReconnectGrace`（3h） |
//! | [`QUEUED_GRACE_SECS`] | 10800 | `ExpireStaleQueuedTasks` 复用的 `@reconnect_grace_secs` |
//!
//! # 与 `last_heartbeat_at` 的关系
//!
//! `agent_task_queue.last_heartbeat_at` 已被上游迁移 `069` **删除**：运行中任务的
//! 存活证据是 **daemon 级**心跳 `agent_runtime.last_seen_at`（`COALESCE(last_seen_at,
//! updated_at)`），不是每任务心跳。所以这里用 [`RuntimeLiveness`] 承载它。
//!
//! # 列级差异由 SQL 适配层负责
//!
//! 四条清扫语句的 `SET` 列**并不一致**（例如 `FailStaleTasks` 清
//! `prepare_lease_expires_at`，`FailTasksForOfflineRuntimes` 只清 `wait_reason`）。
//! 本模块只判定**状态迁移与原因值**；[`crate::state::TaskState::apply`] 给出的
//! 写集是这四条的超集，M3-6 可以按语句收窄。

use mc_core::Timestamp;
use serde::Serialize;

use crate::error::TaskError;
use crate::retry::FailureReason;
use crate::state::{TaskEvent, TaskState};
use crate::status::TaskStatus;

/// `prepare_lease_expires_at` 的租期（秒）。守护进程在 claim 与 `StartTask` 之间
/// 每 15s 续一次。
pub const PREPARE_LEASE_SECS: u64 = 45;

/// claim 后续租间隔（秒）。
pub const PREPARE_LEASE_EXTEND_INTERVAL_SECS: u64 = 15;

/// `claim_recovery_secs`：派遣响应丢失后允许重投递的静默窗口（秒）。
pub const CLAIM_RECOVERY_SECS: u64 = 90;

/// `runtime_stale_secs`：心跳「新鲜」窗口（秒）。
pub const RUNTIME_STALE_SECS: u64 = 150;

/// `dispatch_timeout_secs`：`dispatched` 的挂钟上限（秒）。
pub const DISPATCH_TIMEOUT_SECS: u64 = 300;

/// `running_timeout_secs`：`running` 的挂钟上限（秒）。
pub const RUNNING_TIMEOUT_SECS: u64 = 9_000;

/// `runtime_reconnect_grace_secs`：网络分区下保住本地任务的宽限（秒）。
pub const RUNTIME_RECONNECT_GRACE_SECS: u64 = 10_800;

/// `queued` 行的自身宽限（秒）—— 与重连宽限同值。
pub const QUEUED_GRACE_SECS: u64 = RUNTIME_RECONNECT_GRACE_SECS;

/// `FailStaleTasks` 的 `error` 字面量。
pub const TIMEOUT_MESSAGE: &str = "task timed out";

/// `ExpireStaleQueuedTasks` 的 `error` 字面量。
pub const QUEUED_EXPIRED_MESSAGE: &str = "runtime unavailable while task was queued";

/// `FailTasksForOfflineRuntimes` 的 `error` 字面量。
pub const RUNTIME_OFFLINE_MESSAGE: &str = "runtime went offline";

/// `FailExpiredRuntimeReconnectRetries` 的 `error` 字面量。
pub const RECONNECT_TIMEOUT_MESSAGE: &str =
    "runtime did not reconnect within the configured grace period";

/// 清扫阈值集合。`mc-task` **不依赖 `mc-config`**，所以策略值一律由调用方传入。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StalePolicy {
    /// `prepare_lease_expires_at` 的租期。
    pub prepare_lease_secs: u64,
    /// `dispatched` 挂钟上限。
    pub dispatch_timeout_secs: u64,
    /// `running` 挂钟上限。
    pub running_timeout_secs: u64,
    /// 心跳新鲜窗口。
    pub runtime_stale_secs: u64,
    /// 重连宽限。
    pub runtime_reconnect_grace_secs: u64,
    /// 重投递静默窗口。
    pub claim_recovery_secs: u64,
    /// `queued` 行自身宽限。
    pub queued_grace_secs: u64,
}

impl StalePolicy {
    /// 上游默认值。
    pub const UPSTREAM_DEFAULT: Self = Self {
        prepare_lease_secs: PREPARE_LEASE_SECS,
        dispatch_timeout_secs: DISPATCH_TIMEOUT_SECS,
        running_timeout_secs: RUNNING_TIMEOUT_SECS,
        runtime_stale_secs: RUNTIME_STALE_SECS,
        runtime_reconnect_grace_secs: RUNTIME_RECONNECT_GRACE_SECS,
        claim_recovery_secs: CLAIM_RECOVERY_SECS,
        queued_grace_secs: QUEUED_GRACE_SECS,
    };
}

impl Default for StalePolicy {
    fn default() -> Self {
        Self::UPSTREAM_DEFAULT
    }
}

/// 秒数差：`now - at`（负值表示 `at` 在未来）。全程 `i64`，不做截断转换。
fn age_secs(at: Timestamp, now: Timestamp) -> i64 {
    now.as_unix().saturating_sub(at.as_unix())
}

/// 秒数宽限转 `i64`（`u64` 上限在 `i64` 里饱和，不会绕回负数）。
fn as_i64(secs: u64) -> i64 {
    i64::try_from(secs).unwrap_or(i64::MAX)
}

/// `now + secs`。
fn plus_secs(now: Timestamp, secs: u64) -> Timestamp {
    Timestamp::from_unix(now.as_unix().saturating_add(as_i64(secs)))
}

/// 认领时要写下的租约到期时刻：`now + prepare_lease_secs`。
///
/// 上游 `ClaimAgentTask`（`agent.sql:757`）在把行改成 `dispatched` 的同一条语句里写
/// `prepare_lease_expires_at = now() + make_interval(secs => @prepare_lease_secs)`，
/// 所以「状态机 + 租约」在这里合成一个写单：`Dispatch` 事件给出的写集只有
/// [`DispatchedAt`](crate::state::ColumnWrite::DispatchedAt)，认领方还要自己补上
/// `PrepareLeaseExpiresAt(prepare_lease_deadline(now, policy))`。
///
/// 这个函数公开，是因为铸造这个值的适配层（M3-6）不在本 crate 里。
#[must_use]
pub fn prepare_lease_deadline(now: Timestamp, policy: &StalePolicy) -> Timestamp {
    plus_secs(now, policy.prepare_lease_secs)
}

/// 任务行的租约视图（三个真实列；**没有** `lease_expires_at` 这种自造列）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lease {
    /// `dispatched_at`。
    pub dispatched_at: Option<Timestamp>,
    /// `prepare_lease_expires_at`（迁移 `124`）。
    pub prepare_lease_expires_at: Option<Timestamp>,
    /// `started_at`。
    pub started_at: Option<Timestamp>,
}

impl Lease {
    /// 由三个列值构造。
    #[must_use]
    pub const fn new(
        dispatched_at: Option<Timestamp>,
        prepare_lease_expires_at: Option<Timestamp>,
        started_at: Option<Timestamp>,
    ) -> Self {
        Self {
            dispatched_at,
            prepare_lease_expires_at,
            started_at,
        }
    }

    /// 从任务行取租约字段。
    #[must_use]
    pub const fn from_state(state: &TaskState) -> Self {
        Self::new(
            state.dispatched_at,
            state.prepare_lease_expires_at,
            state.started_at,
        )
    }

    /// 租约是否仍在守护（`prepare_lease_expires_at >= now`）。
    ///
    /// 上游语义：`NULL` 表示**没有**守护，不是「永久守护」。
    #[must_use]
    pub fn prepare_is_active(&self, now: Timestamp) -> bool {
        match self.prepare_lease_expires_at {
            Some(expires_at) => now.as_unix() <= expires_at.as_unix(),
            None => false,
        }
    }

    /// 距到期还剩多少秒（负值 = 已过期；没有租约 ⇒ `None`）。
    #[must_use]
    pub fn remaining_secs(&self, now: Timestamp) -> Option<i64> {
        self.prepare_lease_expires_at
            .map(|expires_at| expires_at.as_unix().saturating_sub(now.as_unix()))
    }

    /// 是否到了该续租的时刻（剩余不足一个续租间隔）。
    ///
    /// 间隔是**续租器自己的节奏**（[`PREPARE_LEASE_EXTEND_INTERVAL_SECS`]），
    /// 不是调度策略，所以不从 [`StalePolicy`] 里取。
    #[must_use]
    pub fn is_due_for_extension(&self, now: Timestamp) -> bool {
        match self.remaining_secs(now) {
            Some(remaining) => remaining <= as_i64(PREPARE_LEASE_EXTEND_INTERVAL_SECS),
            None => false,
        }
    }

    /// 续租后的新到期时刻（`now + prepare_lease_secs`）。
    #[must_use]
    pub fn next_deadline(now: Timestamp, policy: &StalePolicy) -> Timestamp {
        plus_secs(now, policy.prepare_lease_secs)
    }

    /// `dispatched_at` 是否已超过派遣上限（缺 `dispatched_at` ⇒ `false`）。
    #[must_use]
    pub fn dispatch_timed_out(&self, policy: &StalePolicy, now: Timestamp) -> bool {
        match self.dispatched_at {
            Some(dispatched_at) => {
                age_secs(dispatched_at, now) > as_i64(policy.dispatch_timeout_secs)
            }
            None => false,
        }
    }

    /// `started_at` 是否已超过运行上限（缺 `started_at` ⇒ `false`）。
    #[must_use]
    pub fn running_timed_out(&self, policy: &StalePolicy, now: Timestamp) -> bool {
        match self.started_at {
            Some(started_at) => age_secs(started_at, now) > as_i64(policy.running_timeout_secs),
            None => false,
        }
    }
}

/// 该行 runtime 的存活证据。
///
/// `heartbeat_at` 恒为 `COALESCE(runtime.last_seen_at, runtime.updated_at)` ——
/// 上游用它当 daemon 级心跳（`updated_at` 是 `NOT NULL`，所以这里也不建模 `NULL`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeLiveness {
    /// `runner_id IS NULL`（fail-closed 防御分支；schema 上已不允许出现在活跃行）。
    Unbound,
    /// `agent_task_queue.runtime_id` 指向的 `agent_runtime` 行不存在
    /// （外键本应阻止；fail-closed 防御分支）。
    Dangling,
    /// 有 runtime 行：`online` 是该行的 `status = 'online'`。
    Known {
        /// `agent_runtime.status = 'online'`。
        online: bool,
        /// `COALESCE(last_seen_at, updated_at)`。
        heartbeat_at: Timestamp,
    },
}

impl RuntimeLiveness {
    /// 构造一个有 runtime 行的证据。
    #[must_use]
    pub const fn known(online: bool, heartbeat_at: Timestamp) -> Self {
        Self::Known {
            online,
            heartbeat_at,
        }
    }

    /// 是否存在 runtime 行（`Unbound` / `Dangling` 都没有）。
    #[must_use]
    pub const fn has_runtime_row(&self) -> bool {
        matches!(self, Self::Known { .. })
    }

    /// `status = 'online'`。
    #[must_use]
    pub const fn is_online(&self) -> bool {
        match self {
            Self::Known { online, .. } => *online,
            _ => false,
        }
    }

    /// 心跳是否在 `runtime_stale_secs` 内**且** runtime 在线。
    ///
    /// 对应上游的 `r.status = 'online' AND COALESCE(...) >= now() - interval`。
    #[must_use]
    pub fn is_fresh(&self, now: Timestamp, policy: &StalePolicy) -> bool {
        match self {
            Self::Known {
                online: true,
                heartbeat_at,
            } => age_secs(*heartbeat_at, now) <= as_i64(policy.runtime_stale_secs),
            _ => false,
        }
    }

    /// 心跳是否仍在重连宽限内（**不看** `online` —— 上游 `ExpireStaleQueuedTasks`
    /// 明确「Heartbeat age is read directly rather than gated on runtime.status='online'」）。
    #[must_use]
    pub fn within_reconnect_grace(&self, now: Timestamp, policy: &StalePolicy) -> bool {
        match self {
            Self::Known { heartbeat_at, .. } => {
                age_secs(*heartbeat_at, now) <= as_i64(policy.runtime_reconnect_grace_secs)
            }
            _ => false,
        }
    }
}

/// 清扫判定的结论。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StaleVerdict {
    /// 不动这一行。
    Keep,
    /// 判失败（原因值取自上游字面量）。
    Fail {
        /// `failure_reason`。
        reason: FailureReason,
        /// `error` 列的字面量。
        message: &'static str,
    },
    /// 重投递给同一个 runtime（`ReclaimStaleDispatchedTaskForRuntime`）。
    Reclaim,
}

impl StaleVerdict {
    /// 是否保持原状。
    #[must_use]
    pub const fn is_keep(&self) -> bool {
        matches!(self, Self::Keep)
    }

    /// 失败判定对应的 `(failure_reason, error)`。
    #[must_use]
    pub fn failure(&self) -> Option<(FailureReason, &'static str)> {
        match self {
            Self::Fail { reason, message } => Some((*reason, message)),
            _ => None,
        }
    }
}

/// `FailStaleTasks`（`agent.sql:1322`）：`dispatched` / `running` 的僵尸清扫。
///
/// `waiting_local_directory` 行**故意排除**：等待是守护进程自己的事（本地排队
/// 可能合法地超过派遣/运行上限），守护进程死亡时由 `RecoverOrphanedTasksForRuntime`
/// 在重启时回收。
#[must_use]
pub fn stale_task_verdict(
    state: &TaskState,
    liveness: RuntimeLiveness,
    policy: &StalePolicy,
    now: Timestamp,
) -> StaleVerdict {
    let lease = Lease::from_state(state);
    match state.status {
        TaskStatus::Dispatched => {
            // 租约还活着 ⇒ 守护进程正在续租，不杀。
            if !lease.dispatch_timed_out(policy, now) || lease.prepare_is_active(now) {
                return StaleVerdict::Keep;
            }
            match liveness {
                // 没有 runtime 证据 ⇒ 挂钟说话。
                RuntimeLiveness::Unbound | RuntimeLiveness::Dangling => fail_timeout(),
                RuntimeLiveness::Known { .. } => {
                    // 在线且新鲜 ⇒ 立刻可杀；心跳已超出整个重连宽限 ⇒ 也可杀；
                    // 「刚掉线的健康守护进程」在宽限内 ⇒ 保住。
                    if liveness.is_fresh(now, policy)
                        || !liveness.within_reconnect_grace(now, policy)
                    {
                        fail_timeout()
                    } else {
                        StaleVerdict::Keep
                    }
                }
            }
        }
        TaskStatus::Running => {
            // `running` 之后不再续租，只能靠 daemon 级心跳。
            if !lease.running_timed_out(policy, now) {
                return StaleVerdict::Keep;
            }
            match liveness {
                RuntimeLiveness::Unbound | RuntimeLiveness::Dangling => fail_timeout(),
                RuntimeLiveness::Known { .. } => {
                    if liveness.within_reconnect_grace(now, policy) {
                        StaleVerdict::Keep
                    } else {
                        fail_timeout()
                    }
                }
            }
        }
        _ => StaleVerdict::Keep,
    }
}

fn fail_timeout() -> StaleVerdict {
    StaleVerdict::Fail {
        reason: FailureReason::Timeout,
        message: TIMEOUT_MESSAGE,
    }
}

fn fail_queued_expired() -> StaleVerdict {
    StaleVerdict::Fail {
        reason: FailureReason::QueuedExpired,
        message: QUEUED_EXPIRED_MESSAGE,
    }
}

/// `ExpireStaleQueuedTasks`（`agent.sql:1487`）的判定输入。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueuedExpiryContext {
    /// `created_at` —— 宽限从「开始排队」起算。
    pub created_at: Timestamp,
    /// `context->>'wakeup_id' IS NOT NULL` ⇒ 由 wakeup 路径拥有，不由本清扫释放。
    pub wakeup_gated: bool,
    /// `parent_task_id` 的父行 `failure_reason = 'runtime_offline'` ⇒
    /// 它在等那个 runtime 回来，`FailExpiredRuntimeReconnectRetries` 拥有它的出口。
    pub runtime_offline_retry: bool,
}

/// `ExpireStaleQueuedTasks`：runtime 已无法证明自己活着时，释放它排队的任务。
///
/// 问题不是「等了多久」而是「还有没有人能接」：只要守护进程还在心跳，排队再久
/// 也只是忙（MUL-6558），因此宽限读的是心跳而不是单纯的挂钟。
#[must_use]
pub fn queued_expiry_verdict(
    state: &TaskState,
    liveness: RuntimeLiveness,
    policy: &StalePolicy,
    context: QueuedExpiryContext,
    now: Timestamp,
) -> StaleVerdict {
    if state.status != TaskStatus::Queued || context.wakeup_gated || context.runtime_offline_retry {
        return StaleVerdict::Keep;
    }
    if age_secs(context.created_at, now) <= as_i64(policy.queued_grace_secs) {
        return StaleVerdict::Keep;
    }
    match liveness {
        RuntimeLiveness::Unbound | RuntimeLiveness::Dangling => fail_queued_expired(),
        RuntimeLiveness::Known { .. } => {
            if liveness.within_reconnect_grace(now, policy) {
                StaleVerdict::Keep
            } else {
                fail_queued_expired()
            }
        }
    }
}

/// `FailExpiredRuntimeReconnectRetries`（`agent.sql:1513`）：`runtime_offline`
/// 重试行在 `deferred` 里等满一个重连宽限后的有界出口。
///
/// 出口原因 `runtime_reconnect_timeout` **不可重试** —— 否则会无限等下去。
#[must_use]
pub fn reconnect_retry_verdict(
    state: &TaskState,
    liveness: RuntimeLiveness,
    policy: &StalePolicy,
    parent_failed_runtime_offline: bool,
    now: Timestamp,
) -> StaleVerdict {
    if state.status != TaskStatus::Deferred || !parent_failed_runtime_offline {
        return StaleVerdict::Keep;
    }
    let Some(fire_at) = state.fire_at else {
        return StaleVerdict::Keep;
    };
    if age_secs(fire_at, now) <= as_i64(policy.runtime_reconnect_grace_secs) {
        return StaleVerdict::Keep;
    }
    if liveness.is_fresh(now, policy) {
        // runtime 在本次查询跑到之前恢复了 ⇒ 让它继续 deferred，等守护进程提升。
        return StaleVerdict::Keep;
    }
    StaleVerdict::Fail {
        reason: FailureReason::RuntimeReconnectTimeout,
        message: RECONNECT_TIMEOUT_MESSAGE,
    }
}

/// `FailTasksForOfflineRuntimes`（`runtime.sql:277`）：runtime 明确 `offline`
/// 且心跳超出重连宽限时，连带它的 `dispatched` / `running` /
/// `waiting_local_directory` 行一起退场。
#[must_use]
pub fn offline_runtime_verdict(
    state: &TaskState,
    liveness: RuntimeLiveness,
    policy: &StalePolicy,
    now: Timestamp,
) -> StaleVerdict {
    let covered = matches!(
        state.status,
        TaskStatus::Dispatched | TaskStatus::Running | TaskStatus::WaitingLocalDirectory
    );
    if !covered {
        return StaleVerdict::Keep;
    }
    match liveness {
        RuntimeLiveness::Known {
            online: false,
            heartbeat_at,
        } if age_secs(heartbeat_at, now) > as_i64(policy.runtime_reconnect_grace_secs) => {
            StaleVerdict::Fail {
                reason: FailureReason::RuntimeOffline,
                message: RUNTIME_OFFLINE_MESSAGE,
            }
        }
        _ => StaleVerdict::Keep,
    }
}

/// `ReclaimStaleDispatchedTaskForRuntime`（`agent.sql:865`）的前置判定。
///
/// 满足时返回 [`TaskEvent::Reclaim`]（`dispatched → dispatched`，刷新
/// `dispatched_at` 与租约）；否则返回**具体**原因，便于 M3-6 直接映射错误。
///
/// 注意：调用方还必须自己确保 `runtime_id` 属于**发起 reclaim 的那个** runtime
/// （上游是 `atq.runtime_id = @runtime_id` 的所有者围栏）—— 这不是行内可判定
/// 的性质，所以不在这里做。
///
/// # Errors
///
/// - [`TaskError::IllegalTransition`]：行不在 `dispatched`。
/// - [`TaskError::AlreadyStarted`]：已经 `StartTask` 过（只回收 `started_at IS NULL`）。
/// - [`TaskError::ReclaimWindowOpen`]：`claim_recovery_secs` 静默窗口没到。
/// - [`TaskError::LeaseStillActive`]：`prepare_lease_expires_at` 还活着
///   （对应上游用例 `TestClaimTaskByRuntime_DoesNotReclaimActivePrepareLease`）。
/// - [`TaskError::RuntimeNotEligible`]：runtime 不在线，或心跳不新鲜。
pub fn reclaim_stale_dispatch(
    state: &TaskState,
    liveness: RuntimeLiveness,
    policy: &StalePolicy,
    now: Timestamp,
) -> Result<TaskEvent, TaskError> {
    if state.status != TaskStatus::Dispatched {
        return Err(TaskError::IllegalTransition {
            from: state.status,
            event: TaskEvent::Reclaim,
        });
    }
    if let Some(started_at) = state.started_at {
        return Err(TaskError::AlreadyStarted { started_at });
    }
    let dispatched_at = state.dispatched_at.ok_or(TaskError::ReclaimWindowOpen {
        dispatched_at: now,
        claim_recovery_secs: policy.claim_recovery_secs,
    })?;
    if age_secs(dispatched_at, now) <= as_i64(policy.claim_recovery_secs) {
        return Err(TaskError::ReclaimWindowOpen {
            dispatched_at,
            claim_recovery_secs: policy.claim_recovery_secs,
        });
    }
    if Lease::from_state(state).prepare_is_active(now) {
        return Err(TaskError::LeaseStillActive {
            expires_at: state
                .prepare_lease_expires_at
                .expect("prepare_is_active() 已保证 Some"),
        });
    }
    if !liveness.is_fresh(now, policy) {
        return Err(TaskError::RuntimeNotEligible {
            detail:
                "只有在线且心跳新鲜的 runtime 才能重投递（agent.sql:865 的 r.status='online' 前置）",
        });
    }
    Ok(TaskEvent::Reclaim)
}

#[cfg(test)]
mod tests;
