//! `TaskStore` 端口的 `#[cfg(test)]` 内存实现 + 端口用例。
//!
//! 目的不是「给生产用的内存队列」（上游只有 Pg），而是**证明端口够用且可实现**：
//! 状态机 → 认领 → 终结 → 取消 → 取消确认 → usage 六个动作都能只靠端口完成。
//!
//! 内存实现复现不了的东西，都在下面的注释里点名（那也是 M3-6 必须在 SQL 里做的）：
//! `idx_one_pending_task_per_issue` 的跨行唯一性、认领的串行化栅栏、wakeup 门、
//! runtime 在线/心跳栅栏、`priority` 排序 —— `TaskState` 里连 `issue_id` /
//! `agent_id` / `priority` 都没有，因为这些是**归属/排队**列，不是状态机列。

use std::sync::Mutex;

use async_trait::async_trait;

use super::{ClaimRequest, CommitOutcome, TaskClaim, TaskStore};
use crate::cancel::{
    delivered_comments_plan, plan_cancel_ack, plan_cancellation, CancelAck, CancelAckTarget,
    CancelOutcome, Cancellation,
};
use crate::error::TaskError;
use crate::lease::{prepare_lease_deadline, StalePolicy};
use crate::retry::{FailureReason, RetryBudget};
use crate::state::{ColumnWrite, Failure, TaskEvent, TaskState};
use crate::status::TaskStatus;
use crate::usage::{normalize_provider, TaskUsageRow};
use mc_core::{Id, Timestamp};

/// `TaskState` 之外的「取消确认」列（`branch_name` / `durable_work_dir`）。
///
/// 真实表里有这两列，但它们不属于状态机，所以没进 `TaskState`；内存实现给它们
/// 一个旁挂的表，模拟 `CancelAckTarget` 的读侧。
#[derive(Debug, Clone, Default)]
struct AckExtras {
    branch_name: Option<String>,
    durable_work_dir: Option<String>,
}

/// 内存存储：一个 `Vec<(Id, TaskState)>` + usage 表 + 取消确认旁挂列。
#[derive(Debug, Default)]
struct MemoryStore {
    rows: Mutex<Vec<(Id, TaskState)>>,
    usage: Mutex<Vec<TaskUsageRow>>,
    extras: Mutex<Vec<(Id, AckExtras)>>,
}

impl MemoryStore {
    fn new() -> Self {
        Self::default()
    }

    fn with_row(id: Id, state: TaskState) -> Self {
        let store = Self::new();
        store.rows.lock().expect("锁").push((id, state));
        store
    }

    fn status_of(&self, id: Id) -> Option<TaskStatus> {
        self.rows
            .lock()
            .expect("锁")
            .iter()
            .find(|(row_id, _)| *row_id == id)
            .map(|(_, state)| state.status)
    }

    fn state_of(&self, id: Id) -> Option<TaskState> {
        self.rows
            .lock()
            .expect("锁")
            .iter()
            .find(|(row_id, _)| *row_id == id)
            .map(|(_, state)| state.clone())
    }

    fn mutate(&self, id: Id, f: impl FnOnce(&mut TaskState)) {
        let mut rows = self.rows.lock().expect("锁");
        if let Some((_, state)) = rows.iter_mut().find(|(row_id, _)| *row_id == id) {
            f(state);
        }
    }

    fn extras_of(&self, id: Id) -> AckExtras {
        self.extras
            .lock()
            .expect("锁")
            .iter()
            .find(|(row_id, _)| *row_id == id)
            .map_or_else(AckExtras::default, |(_, extras)| extras.clone())
    }

    fn set_extras(&self, id: Id, extras: AckExtras) {
        let mut guard = self.extras.lock().expect("锁");
        match guard.iter_mut().find(|(row_id, _)| *row_id == id) {
            Some(slot) => slot.1 = extras,
            None => guard.push((id, extras)),
        }
    }
}

/// 把写单落到 `TaskState` 上。
///
/// `failure_reason` 与 `error` 在库里是两列、在 `TaskState` 里是一个
/// `Option<Failure>`，所以先收集再合并：写单里两者的相对顺序不影响结果。
/// 只有 `error` 没有 `failure_reason` 的行（库允许，老数据可能有）在域模型里
/// 表达不出来 —— 这里保持 `failure` 不变并说明，而不是编一个原因出来。
fn apply_writes(state: &mut TaskState, writes: &[ColumnWrite]) {
    let mut reason: Option<FailureReason> = None;
    let mut message: Option<String> = None;
    let mut touched = false;
    for write in writes {
        match write {
            ColumnWrite::DispatchedAt(at) => state.dispatched_at = Some(*at),
            ColumnWrite::DispatchedAtCleared => state.dispatched_at = None,
            ColumnWrite::StartedAt(at) => state.started_at = Some(*at),
            ColumnWrite::CompletedAt(at) => state.completed_at = Some(*at),
            ColumnWrite::WaitReason(value) => state.wait_reason.clone_from(value),
            ColumnWrite::PrepareLeaseExpiresAt(at) => state.prepare_lease_expires_at = *at,
            ColumnWrite::FireAt(at) => state.fire_at = *at,
            ColumnWrite::FailureReason(value) => {
                reason = Some(*value);
                touched = true;
            }
            ColumnWrite::ErrorMessage(value) => {
                message.clone_from(value);
                touched = true;
            }
            ColumnWrite::CancelledBy(value) => state.cancelled_by = Some(value.clone()),
        }
    }
    if !touched {
        return;
    }
    let existing = state.failure.clone();
    let reason = reason.or_else(|| existing.as_ref().map(|f| f.reason));
    let message = message.or_else(|| existing.as_ref().and_then(|f| f.message.clone()));
    state.failure = reason.map(|reason| Failure { reason, message });
}

#[async_trait]
impl TaskStore for MemoryStore {
    async fn insert(&self, id: Id, state: &TaskState) -> Result<(), TaskError> {
        let mut rows = self.rows.lock().expect("锁");
        if rows.iter().any(|(row_id, _)| *row_id == id) {
            return Err(TaskError::Conflict {
                detail: "duplicate task id",
            });
        }
        // 注意：`idx_one_pending_task_per_issue` 需要 issue_id，而 `TaskState` 没
        // 有它（归属列不属于状态机），所以内存实现只能查 id 重复。
        rows.push((id, state.clone()));
        Ok(())
    }

    async fn get(&self, id: Id) -> Result<Option<TaskState>, TaskError> {
        Ok(self.state_of(id))
    }

    async fn commit(
        &self,
        id: Id,
        transition: &crate::state::TaskTransition,
    ) -> Result<CommitOutcome, TaskError> {
        let Some(from) = transition.from else {
            return Err(TaskError::IllegalTransition {
                from: TaskStatus::Queued,
                event: TaskEvent::Queued,
            });
        };
        let Some(current) = self.status_of(id) else {
            return Err(TaskError::NotFound { id });
        };
        if current != from {
            return Ok(CommitOutcome::LostRace);
        }
        self.mutate(id, |state| {
            apply_writes(state, &transition.writes);
            state.status = transition.to;
        });
        Ok(CommitOutcome::Applied)
    }

    async fn claim_next(&self, request: &ClaimRequest) -> Result<Option<TaskClaim>, TaskError> {
        // 上游按 `priority DESC, created_at ASC, id ASC` 挑；这里只挑第一个 queued
        // （`TaskState` 里没有 priority/created_at，排序完全是 SQL 的活）。
        let Some((task_id, mut state)) = self
            .rows
            .lock()
            .expect("锁")
            .iter()
            .find(|(_, state)| state.status == TaskStatus::Queued)
            .map(|(id, state)| (*id, state.clone()))
        else {
            return Ok(None);
        };
        let mut transition = state.apply(TaskEvent::Dispatch, request.now)?;
        // 认领要在同一条语句里写下租约（`ClaimAgentTask`）。
        let deadline = prepare_lease_deadline(request.now, &request.policy);
        state.prepare_lease_expires_at = Some(deadline);
        transition
            .writes
            .push(ColumnWrite::PrepareLeaseExpiresAt(Some(deadline)));
        self.mutate(task_id, |row| row.clone_from(&state));
        Ok(Some(TaskClaim {
            task_id,
            state,
            transition,
        }))
    }

    async fn cancel(
        &self,
        id: Id,
        cancellation: &Cancellation,
        at: Timestamp,
    ) -> Result<CancelOutcome, TaskError> {
        let Some(state) = self.state_of(id) else {
            return Err(TaskError::NotFound { id });
        };
        // 内存实现没有 comment 表 ⇒ 形状探测一律按「没有恢复信号」处理：人工取消
        // 走 `KeepUnchanged` 高频路径。真正的 join 在 M3-6。
        let delivered = delivered_comments_plan(cancellation.user_initiated, None, &[], false);
        let outcome = plan_cancellation(&state, cancellation, delivered, at)?;
        if let CancelOutcome::Applied { transition, .. } = &outcome {
            let transition = transition.clone();
            self.mutate(id, |row| {
                apply_writes(row, &transition.writes);
                row.status = transition.to;
            });
        }
        Ok(outcome)
    }

    async fn apply_cancel_ack(
        &self,
        id: Id,
        ack: &crate::cancel::CancelAck,
    ) -> Result<crate::cancel::CancelAckPlan, TaskError> {
        let Some(state) = self.state_of(id) else {
            return Err(TaskError::NotFound { id });
        };
        let extras = self.extras_of(id);
        let target = CancelAckTarget::from_state(
            &state,
            extras.branch_name.clone(),
            extras.durable_work_dir.clone(),
        );
        let plan = plan_cancel_ack(ack, &target);
        let mut next = extras;
        let mut state_writes: Vec<ColumnWrite> = Vec::new();
        for write in &plan.writes {
            match write {
                crate::cancel::CancelAckWrite::DurableWorkDir(value) => {
                    next.durable_work_dir = Some(value.clone());
                }
                crate::cancel::CancelAckWrite::BranchName(value) => {
                    next.branch_name = Some(value.clone());
                }
                crate::cancel::CancelAckWrite::Error {
                    message,
                    failure_reason,
                } => {
                    state_writes.push(ColumnWrite::ErrorMessage(Some(message.clone())));
                    if let Some(reason) = failure_reason {
                        if let Ok(reason) = FailureReason::parse(reason) {
                            state_writes.push(ColumnWrite::FailureReason(reason));
                        }
                    }
                }
            }
        }
        self.set_extras(id, next);
        if !state_writes.is_empty() {
            self.mutate(id, |row| apply_writes(row, &state_writes));
        }
        Ok(plan)
    }

    async fn list_usage(&self, task_id: Id) -> Result<Vec<TaskUsageRow>, TaskError> {
        let mut rows: Vec<TaskUsageRow> = self
            .usage
            .lock()
            .expect("锁")
            .iter()
            .filter(|row| row.task_id == task_id)
            .cloned()
            .collect();
        // `GetTaskUsage` 就是 `ORDER BY model`。
        rows.sort_by(|a, b| a.model.cmp(&b.model));
        Ok(rows)
    }

    async fn upsert_usage(&self, row: &TaskUsageRow) -> Result<(), TaskError> {
        let mut usage = self.usage.lock().expect("锁");
        let key = row.key();
        match usage.iter_mut().find(|stored| stored.key() == key) {
            Some(stored) => stored.overwrite_with(
                &crate::usage::UsageReport {
                    provider: row.provider.clone(),
                    model: row.model.clone(),
                    input_tokens: row.input_tokens,
                    output_tokens: row.output_tokens,
                    cache_read_tokens: row.cache_read_tokens,
                    cache_write_tokens: row.cache_write_tokens,
                    cost_usd_ticks: row.cost_usd_ticks.unwrap_or(0),
                },
                &row.provider,
                row.updated_at.unwrap_or(row.created_at),
            ),
            None => usage.push(row.clone()),
        }
        Ok(())
    }
}

fn t(unix: i64) -> Timestamp {
    Timestamp::from_unix(unix)
}

fn id(n: u8) -> Id {
    Id(uuid::Uuid::from_u128(u128::from(n)))
}

fn policy() -> StalePolicy {
    StalePolicy::UPSTREAM_DEFAULT
}

fn request(now: Timestamp) -> ClaimRequest {
    ClaimRequest::new(id(9), id(8), now, policy())
}

fn queued() -> (Id, TaskState) {
    let (state, _) = TaskState::enqueue(RetryBudget::FIRST_RUN);
    (id(1), state)
}

fn usage_row(model: &str, input: i64, cost: Option<i64>, at: i64) -> TaskUsageRow {
    TaskUsageRow {
        task_id: id(1),
        provider: normalize_provider("GROK"),
        model: model.to_owned(),
        input_tokens: input,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cost_usd_ticks: cost,
        created_at: t(at),
        updated_at: Some(t(at)),
    }
}

#[tokio::test]
async fn the_port_drives_a_full_lifecycle() {
    let store = MemoryStore::new();
    let (task_id, state) = queued();
    store.insert(task_id, &state).await.expect("插入");
    assert_eq!(store.status_of(task_id), Some(TaskStatus::Queued));

    // 重复 id ⇒ 冲突（映射 409）。
    let conflict = store.insert(task_id, &state).await;
    assert!(matches!(conflict, Err(TaskError::Conflict { .. })));

    let claim = store
        .claim_next(&request(t(100)))
        .await
        .expect("认领")
        .expect("有 queued 行");
    assert_eq!(claim.task_id, task_id);
    assert_eq!(claim.state.status, TaskStatus::Dispatched);
    assert_eq!(claim.state.dispatched_at, Some(t(100)));
    assert_eq!(
        claim.state.prepare_lease_expires_at,
        Some(t(100 + 45)),
        "认领同时写下 prepare 租约"
    );

    let mut running = claim.state.clone();
    let to_running = running.apply(TaskEvent::Running, t(110)).expect("开工");
    assert_eq!(
        store.commit(task_id, &to_running).await.expect("提交"),
        CommitOutcome::Applied
    );
    assert_eq!(store.status_of(task_id), Some(TaskStatus::Running));

    let mut completed = running.clone();
    let to_completed = completed.apply(TaskEvent::Completed, t(200)).expect("完成");
    assert_eq!(
        store.commit(task_id, &to_completed).await.expect("提交"),
        CommitOutcome::Applied
    );
    let stored = store.get(task_id).await.expect("读").expect("存在");
    assert_eq!(stored.status, TaskStatus::Completed);
    assert_eq!(stored.started_at, Some(t(110)));
    assert_eq!(stored.completed_at, Some(t(200)));

    // 旧写单再提交 = CAS 落空（不是错误）。
    assert_eq!(
        store.commit(task_id, &to_running).await.expect("提交"),
        CommitOutcome::LostRace
    );

    // 没有 queued 行可供认领。
    assert!(store
        .claim_next(&request(t(300)))
        .await
        .expect("认领")
        .is_none());
}

#[tokio::test]
async fn committing_a_creation_transition_is_rejected() {
    let store = MemoryStore::new();
    let (task_id, state) = queued();
    store.insert(task_id, &state).await.expect("插入");
    let (_, creation) = TaskState::enqueue(RetryBudget::FIRST_RUN);
    assert!(matches!(
        store.commit(task_id, &creation).await,
        Err(TaskError::IllegalTransition { .. })
    ));
    // 不存在的行 = NotFound，而不是静默成功。
    assert!(matches!(
        store.commit(id(42), &creation).await,
        Err(TaskError::IllegalTransition { .. })
    ));
}

#[tokio::test]
async fn cancel_is_idempotent_on_a_cancelled_row_but_not_after_completion() {
    let (task_id, state) = queued();
    let store = MemoryStore::with_row(task_id, state);
    let now = t(100);

    let first = store
        .cancel(
            task_id,
            &Cancellation::by_user(Some(id(7)), Some("loulou".to_owned())),
            now,
        )
        .await
        .expect("取消");
    assert!(first.is_applied());
    let stored = store.get(task_id).await.expect("读").expect("存在");
    assert_eq!(stored.status, TaskStatus::Cancelled);
    assert_eq!(stored.completed_at, Some(now));
    assert_eq!(
        stored.cancelled_by,
        Some(crate::state::CancelledBy::User {
            id: Some(id(7)),
            name: Some("loulou".to_owned()),
        })
    );
    assert!(!stored.is_delegated_failure());

    let again = store
        .cancel(
            task_id,
            &Cancellation::by_user(Some(id(7)), Some("loulou".to_owned())),
            t(200),
        )
        .await
        .expect("重复取消");
    assert!(again.is_idempotent_replay(), "重复取消 = 幂等成功");

    // 完成后再取消：也是 AlreadyTerminal，但不是幂等重放。
    let (other_id, other) = queued();
    let late_store = MemoryStore::with_row(other_id, other);
    late_store.mutate(other_id, |row| {
        row.status = TaskStatus::Completed;
        row.completed_at = Some(t(50));
    });
    let late = late_store
        .cancel(other_id, &Cancellation::by_system(), t(300))
        .await
        .expect("迟到取消");
    assert_eq!(
        late,
        CancelOutcome::AlreadyTerminal {
            status: TaskStatus::Completed
        }
    );
    assert!(!late.is_idempotent_replay());
}

#[tokio::test]
async fn system_cancel_with_reason_persists_error_and_failure_reason() {
    let (task_id, state) = queued();
    let store = MemoryStore::with_row(task_id, state);
    let cancellation =
        Cancellation::by_system_with_reason("  runtime 被回收  ", FailureReason::Timeout)
            .expect("系统取消带原因");
    let outcome = store
        .cancel(task_id, &cancellation, t(100))
        .await
        .expect("取消");
    assert!(outcome.is_applied());
    let stored = store.get(task_id).await.expect("读").expect("存在");
    let failure = stored.failure.expect("有 failure");
    assert_eq!(failure.reason, FailureReason::Timeout);
    assert_eq!(failure.message.as_deref(), Some("runtime 被回收"));
    assert!(!stored.cancelled_by.expect("cancelled_by").is_user());
}

#[tokio::test]
async fn cancel_ack_writes_once_and_never_overwrites() {
    let (task_id, state) = queued();
    let store = MemoryStore::with_row(task_id, state);
    store.mutate(task_id, |row| {
        row.status = TaskStatus::Cancelled;
        row.completed_at = Some(t(50));
    });

    let ack = CancelAck {
        branch_name: Some("agent/fix".to_owned()),
        durable_work_dir: Some("/srv/wt/agent-fix".to_owned()),
        error_message: Some("daemon 收尾失败".to_owned()),
        failure_reason: Some("timeout".to_owned()),
    };
    let plan = store.apply_cancel_ack(task_id, &ack).await.expect("确认");
    assert!(plan.rebroadcast, "写了列才重播");
    let extras = store.extras_of(task_id);
    assert_eq!(extras.branch_name.as_deref(), Some("agent/fix"));
    assert_eq!(
        extras.durable_work_dir.as_deref(),
        Some("/srv/wt/agent-fix")
    );
    let stored = store.get(task_id).await.expect("读").expect("存在");
    assert_eq!(
        stored.failure.expect("failure").reason,
        FailureReason::Timeout
    );

    // 重放：列都已存在 ⇒ 空计划、不重播、也不报错。
    let replay = store.apply_cancel_ack(task_id, &ack).await.expect("确认");
    assert!(replay.is_noop());
    assert!(!replay.rebroadcast);
}

#[tokio::test]
async fn usage_upsert_overwrites_and_list_is_ordered_by_model() {
    let store = MemoryStore::new();
    store
        .upsert_usage(&usage_row("z-model", 10, Some(100), 1_000))
        .await
        .expect("写");
    store
        .upsert_usage(&usage_row("a-model", 20, None, 1_000))
        .await
        .expect("写");
    // 同一 (task, provider, model) 再报一次：覆盖，不是叠加。
    store
        .upsert_usage(&usage_row("z-model", 3, None, 2_000))
        .await
        .expect("写");

    let rows = store.list_usage(id(1)).await.expect("读");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].model, "a-model");
    assert_eq!(rows[1].model, "z-model");
    assert_eq!(rows[1].input_tokens, 3, "覆盖");
    assert_eq!(rows[1].cost_usd_ticks, None, "0 覆盖成 NULL");
    assert_eq!(rows[1].created_at, t(1_000), "created_at 不动");
    assert_eq!(rows[1].updated_at, Some(t(2_000)));
    assert!(store.list_usage(id(99)).await.expect("读").is_empty());
}
