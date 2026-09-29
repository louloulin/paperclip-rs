//! 一行新任务的**身份面**（[`NewTask`]）与取消语句的 `SET` 拼装。
//!
//! 端口缺口见父模块（`mod.rs`）的模块文档。`impl TaskRepo` 侧在 `repo.rs`。

use serde_json::Value;

use mc_core::{Id, Timestamp};
use mc_task::retry::RetryBudget;
use mc_task::state::TaskState;

use super::super::TASK_COLUMNS;

/// `agent_task_queue.status` 的 5 个非终态（上游所有取消语句的 CAS 集合）。
const CANCELLABLE_STATUSES: &str =
    "('queued', 'dispatched', 'running', 'waiting_local_directory', 'deferred')";

/// 一行新任务的身份面（`CreateAgentTask` 的参数面）。
///
/// 字段名与上游列一一对应；**没有**任何 docs/15 §2.2 列出的自造列。
#[derive(Debug, Clone)]
pub struct NewTask {
    /// `id`（调用方铸造 —— 上游用 `UUIDv7` 让连续入队落在主键 B-tree 相邻区间）。
    pub id: Id,
    /// `agent_id`（NOT NULL）。
    pub agent_id: Id,
    /// `issue_id`（chat / quick-create 任务为空）。
    pub issue_id: Option<Id>,
    /// `runtime_id`（agent 重绑后这一列才是派单权威）。
    pub runtime_id: Option<Id>,
    /// `priority`（越大越先认领）。
    pub priority: i32,
    /// `context`（`wakeup_id` / `channel_issue_media_pending` 等都在这里面）。
    pub context: Option<Value>,
    /// `trigger_comment_id`。
    pub trigger_comment_id: Option<Id>,
    /// `chat_session_id`。
    pub chat_session_id: Option<Id>,
    /// `autopilot_run_id`。
    pub autopilot_run_id: Option<Id>,
    /// `parent_task_id`（自动重试子行指向失败的那一行）。
    pub parent_task_id: Option<Id>,
    /// `retry_of_task_id`。
    pub retry_of_task_id: Option<Id>,
    /// `rerun_of_task_id`。
    pub rerun_of_task_id: Option<Id>,
    /// `delegated_from_task_id`。
    pub delegated_from_task_id: Option<Id>,
    /// `escalation_for_task_id`。
    pub escalation_for_task_id: Option<Id>,
    /// `trigger_summary`。
    pub trigger_summary: Option<String>,
    /// `handoff_note`。
    pub handoff_note: Option<String>,
    /// `is_leader_task`。
    pub is_leader_task: bool,
    /// `force_fresh_session`。
    pub force_fresh_session: bool,
    /// `work_dir`。
    pub work_dir: Option<String>,
    /// 重试预算（`attempt` / `max_attempts`）。
    pub budget: RetryBudget,
    /// `∅ → deferred` 的 `fire_at`（`Some` ⇒ 停泊而不是入队）。
    pub fire_at: Option<Timestamp>,
}

/// `CancelAgentTaskByUser` 里重算 `delivered_comment_ids` 的三路 `CASE`
/// （上游 `agent.sql:1665`，逐字移植）。
///
/// 三段各管一种形状：无来源 ⇒ 原样；来源不是委派失败的恢复信号 ⇒ 原样；
/// 否则把恢复回执并进投递集合（`array_agg(DISTINCT …)` 去重）。
const RECOVERY_SIGNAL_CASE: &str = ", delivered_comment_ids = CASE \
   WHEN atq.trigger_comment_id IS NULL \
    AND COALESCE(cardinality(atq.coalesced_comment_ids), 0) = 0 \
     THEN atq.delivered_comment_ids \
   WHEN NOT EXISTS ( \
     SELECT 1 FROM comment recovery_signal \
     WHERE (recovery_signal.id = atq.trigger_comment_id \
            OR recovery_signal.id = ANY(atq.coalesced_comment_ids)) \
       AND recovery_signal.author_type = 'system' \
       AND recovery_signal.type = 'progress_update' \
       AND recovery_signal.source_task_id IS NOT NULL) \
     THEN atq.delivered_comment_ids \
   ELSE (SELECT COALESCE(array_agg(DISTINCT receipt.id), '{}')::uuid[] \
     FROM unnest(array_cat(atq.delivered_comment_ids, ARRAY( \
       SELECT recovery.id FROM comment recovery \
       JOIN agent_task_queue failed ON failed.id = recovery.source_task_id \
       JOIN agent_task_queue source ON source.id = failed.delegated_from_task_id \
       WHERE (recovery.id = atq.trigger_comment_id \
              OR recovery.id = ANY(atq.coalesced_comment_ids)) \
         AND recovery.author_type = 'system' \
         AND recovery.type = 'progress_update' \
         AND recovery.source_task_id IS NOT NULL \
         AND failed.status = 'failed' \
         AND failed.delegated_from_task_id IS NOT NULL \
         AND failed.autopilot_run_id IS NULL \
         AND failed.trigger_evidence_kind IS DISTINCT FROM 'delegated_failure' \
         AND source.autopilot_run_id IS NULL \
         AND source.issue_id = atq.issue_id \
         AND source.agent_id = atq.agent_id \
         AND recovery.issue_id = source.issue_id \
     ))) AS receipt(id)) END";

/// 拼一次取消语句的 `SET` 列表（顺序与上游语句逐字一致）。
pub(super) fn cancel_sql(recompute_delivered: bool, with_error: bool) -> String {
    let mut sql = "UPDATE agent_task_queue AS atq SET status = 'cancelled', completed_at = $2, \
             prepare_lease_expires_at = NULL, cancelled_by_type = $3, \
             cancelled_by_id = $4, cancelled_by_name = $5"
        .to_string();
    if with_error {
        sql.push_str(", error = $6, failure_reason = $7");
    }
    if recompute_delivered {
        sql.push_str(RECOVERY_SIGNAL_CASE);
    }
    sql.push_str(" WHERE atq.id = $1 AND atq.status IN ");
    sql.push_str(CANCELLABLE_STATUSES);
    sql.push_str(" RETURNING ");
    sql.push_str(TASK_COLUMNS);
    sql
}

impl NewTask {
    /// 一个最小可用的 `queued` 任务（其余字段由 builder 风格的方法补齐）。
    #[must_use]
    pub fn queued(id: Id, agent_id: Id) -> Self {
        Self {
            id,
            agent_id,
            issue_id: None,
            runtime_id: None,
            priority: 0,
            context: None,
            trigger_comment_id: None,
            chat_session_id: None,
            autopilot_run_id: None,
            parent_task_id: None,
            retry_of_task_id: None,
            rerun_of_task_id: None,
            delegated_from_task_id: None,
            escalation_for_task_id: None,
            trigger_summary: None,
            handoff_note: None,
            is_leader_task: false,
            force_fresh_session: false,
            work_dir: None,
            budget: RetryBudget::FIRST_RUN,
            fire_at: None,
        }
    }

    /// 创建时的领域状态（`∅ → queued` 或 `∅ → deferred`）。
    #[must_use]
    pub fn to_state(&self) -> TaskState {
        if let Some(fire_at) = self.fire_at {
            let (state, _) = TaskState::defer(self.budget, Some(fire_at));
            state
        } else {
            let (state, _) = TaskState::enqueue(self.budget);
            state
        }
    }
}
