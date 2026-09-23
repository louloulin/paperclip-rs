//! `TaskStore` 端口的 Pg 实现 + 路由面创建/写入（W3b / M3-6）。
//!
//! [`TaskRepo`](super::TaskRepo) 的**写侧**：创建、CAS 迁移、认领、取消、
//! cancel-ack、usage upsert —— 全部对照上游 `server/pkg/db/queries/agent.sql`
//! 与 `task_usage.sql` 的语句逐条移植。
//!
//! # 端口缺口（**本仓更正**）
//!
//! M3-3 的 [`TaskStore::insert`] 形状是 `insert(id, state: &TaskState)`，注释称
//! 「创建迁移的写集就是这些字段的初值」。**这不成立**：
//! `agent_task_queue.agent_id` 是 `NOT NULL`（`contracts/upstream-schema.sql:995`），
//! 而 [`TaskState`] 里没有 `agent_id` / `issue_id` / `priority` / `context` /
//! `trigger_comment_id` / `chat_session_id` / … —— 端口无法表达一行任务的**身份面**。
//!
//! 因此：
//! - 真实创建走 [`TaskRepo::create_task`]（吃 [`NewTask`]，含身份面）；
//! - 端口实现由 [`PgTaskStore`] 承担 —— 一个极薄的适配器，构造时带上要创建的那一行的
//!   [`NewTask`]，`insert` 用它补齐身份列，其余方法直接转发到 [`TaskRepo`]。
//!
//! 这个缺口无法在 M3-6 修（`mc-task/src/store.rs` 不在本切片写集内），已登记在
//! `docs/41`。端口语义（CAS 返回 `LostRace` 而非错误、认领栅栏必须在 SQL 侧、
//! 策略值由调用方传入）由本实现逐条满足。

use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use mc_core::{Id, Timestamp};
use mc_db::Db;
use mc_task::cancel::{
    delivered_comments_plan, plan_cancel_ack, plan_cancellation, CancelAck, CancelAckPlan,
    CancelAckTarget, CancelAckWrite, CancelOutcome, Cancellation, DeliveredCommentsPlan,
};
use mc_task::error::TaskError;
use mc_task::retry::RetryBudget;
use mc_task::state::{ColumnWrite, TaskEventKind, TaskState, TaskTransition};
use mc_task::status::TaskStatus;
use mc_task::store::{ClaimRequest, CommitOutcome, TaskClaim, TaskStore};
use mc_task::usage::TaskUsageRow;

use super::{row::TaskRow, TaskRepo, TASK_COLUMNS};

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
fn cancel_sql(recompute_delivered: bool, with_error: bool) -> String {
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

impl TaskRepo {
    /// 创建一行任务（上游 `CreateAgentTask`）。
    ///
    /// 唯一约束不被预检：重复的未决任务靠数据库拒绝（`idx_agent_task_queue_pending`
    /// 与 `idx_one_pending_task_per_issue_agent_thread`），违反时返回
    /// [`TaskError::Conflict`]（映射 409），不返回 500。
    ///
    /// `comment_thread_id` **不在写入列表里**：它是 `BEFORE INSERT` 触发器
    /// `agent_task_comment_thread` 推导出来的派生列（上游迁移 451 引入
    /// `comment_thread_root_id`，516 追加 `context->>'wakeup_id'` 优先）——
    /// 调用方要指定线程作用域只能通过 `context` 的 `wakeup_id`，或者通过
    /// `trigger_comment_id` 指向某个 comment（取其线程根）。直接绑该列是死写。
    ///
    /// # Errors
    ///
    /// - [`TaskError::Conflict`]：命中部分唯一索引。
    /// - [`TaskError::Backend`]：其它 DB 错误。
    pub async fn create_task(&self, new: &NewTask) -> Result<TaskRow, TaskError> {
        let state = new.to_state();
        let sql = format!(
            "INSERT INTO agent_task_queue AS atq (\
                 id, agent_id, issue_id, status, priority, runtime_id, context, \
                 trigger_comment_id, chat_session_id, autopilot_run_id, \
                 parent_task_id, retry_of_task_id, rerun_of_task_id, delegated_from_task_id, \
                 escalation_for_task_id, trigger_summary, handoff_note, is_leader_task, \
                 force_fresh_session, work_dir, attempt, max_attempts, fire_at, wait_reason \
             ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,\
                 $21,$22,$23,$24) RETURNING {TASK_COLUMNS}"
        );
        let result = sqlx::query_as::<_, TaskRow>(&sql)
            .bind(new.id.0)
            .bind(new.agent_id.0)
            .bind(new.issue_id.map(|v| v.0))
            .bind(state.status.as_str())
            .bind(new.priority)
            .bind(new.runtime_id.map(|v| v.0))
            .bind(new.context.as_ref())
            .bind(new.trigger_comment_id.map(|v| v.0))
            .bind(new.chat_session_id.map(|v| v.0))
            .bind(new.autopilot_run_id.map(|v| v.0))
            .bind(new.parent_task_id.map(|v| v.0))
            .bind(new.retry_of_task_id.map(|v| v.0))
            .bind(new.rerun_of_task_id.map(|v| v.0))
            .bind(new.delegated_from_task_id.map(|v| v.0))
            .bind(new.escalation_for_task_id.map(|v| v.0))
            .bind(new.trigger_summary.clone())
            .bind(new.handoff_note.clone())
            .bind(new.is_leader_task)
            .bind(new.force_fresh_session)
            .bind(new.work_dir.clone())
            .bind(i32::try_from(state.budget.attempt).unwrap_or(i32::MAX))
            .bind(i32::try_from(state.budget.max_attempts).unwrap_or(i32::MAX))
            .bind(state.fire_at.map(Timestamp::as_datetime))
            .bind(state.wait_reason.clone())
            .fetch_one(self.pool())
            .await;

        match result {
            Ok(row) => Ok(row),
            Err(sqlx::Error::Database(db_err)) if db_err.is_unique_violation() => {
                Err(TaskError::Conflict {
                    detail: "该 issue/agent/thread 上已有未决任务（部分唯一索引）",
                })
            }
            Err(err) => Err(TaskError::Backend {
                message: err.to_string(),
            }),
        }
    }

    /// 读一次领域状态（上游 `GetAgentTask`）。
    ///
    /// # Errors
    ///
    /// 未知 `status` / DB 错误统一为 [`TaskError::Backend`]。
    pub async fn read_state(&self, id: Id) -> Result<Option<TaskState>, TaskError> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM agent_task_queue atq WHERE atq.id = $1");
        let row = sqlx::query_as::<_, TaskRow>(&sql)
            .bind(id.0)
            .fetch_optional(self.pool())
            .await
            .map_err(backend)?;
        row.map(|row| {
            row.to_task_state().map_err(|e| TaskError::Backend {
                message: e.to_string(),
            })
        })
        .transpose()
    }

    /// 落一次迁移（状态列 + `transition.writes` 的 CAS）。
    ///
    /// CAS 条件恒为 `id = $1 AND status = <transition.from>`；
    /// `from == None`（创建态）必须走 [`TaskRepo::create_task`]，这里返回
    /// [`TaskError::IllegalTransition`]。
    ///
    /// # Errors
    ///
    /// - [`TaskError::IllegalTransition`]：`transition.from` 为 `None`。
    /// - [`TaskError::NotFound`]：行不存在。
    /// - [`TaskError::Backend`]：DB 错误或未知状态串。
    pub async fn commit_transition(
        &self,
        id: Id,
        transition: &TaskTransition,
    ) -> Result<CommitOutcome, TaskError> {
        let Some(from) = transition.from else {
            return Err(TaskError::NotFound { id });
        };
        let (set_clause, binds) = build_set_clause(transition);
        let sql = format!(
            "UPDATE agent_task_queue AS atq SET {set_clause} \
             WHERE atq.id = $1 AND atq.status = $2 RETURNING {TASK_COLUMNS}"
        );
        // `$1` = id、`$2` = CAS 期望状态、`$3` = 目标状态（见 `build_set_clause`），
        // 其后才是各列的值。
        let mut query = sqlx::query_as::<_, TaskRow>(&sql)
            .bind(id.0)
            .bind(from.as_str())
            .bind(transition.to.as_str());
        for bind in &binds {
            query = bind_column(query, bind);
        }
        let row = query.fetch_optional(self.pool()).await.map_err(backend)?;
        if row.is_some() {
            return Ok(CommitOutcome::Applied);
        }
        // CAS 落空：区分「行没了」与「状态被别人改了」。
        match self.read_state(id).await? {
            None => Err(TaskError::NotFound { id }),
            Some(_) => Ok(CommitOutcome::LostRace),
        }
    }

    /// 认领下一个可认领任务（`agent.sql:743` `ClaimAgentTask` 逐条移植）。
    ///
    /// # Errors
    ///
    /// [`TaskError::Backend`]：DB 错误。
    pub async fn claim_next_task(
        &self,
        request: &ClaimRequest,
    ) -> Result<Option<TaskClaim>, TaskError> {
        let sql = format!(
            "UPDATE agent_task_queue AS atq \
             SET status = 'dispatched', dispatched_at = now(), \
                 prepare_lease_expires_at = now() + make_interval(secs => $3::double precision) \
             WHERE atq.id = ( \
                 SELECT candidate.id FROM agent_task_queue candidate \
                 WHERE candidate.agent_id = $1 AND candidate.runtime_id = $2 \
                   AND candidate.status = 'queued' \
                   AND (candidate.context->>'wakeup_id' IS NULL OR EXISTS ( \
                         SELECT 1 FROM issue_wakeup w \
                         WHERE w.id = (candidate.context->>'wakeup_id')::uuid \
                           AND w.disabled_at IS NULL \
                           AND w.revision = (candidate.context->>'wakeup_revision')::bigint)) \
                   AND EXISTS ( \
                         SELECT 1 FROM agent a JOIN agent_runtime r ON r.id = candidate.runtime_id \
                         WHERE a.id = candidate.agent_id AND a.runtime_id = candidate.runtime_id \
                           AND (r.visibility = 'public' OR r.visibility = 'private') \
                           AND r.status = 'online' \
                           AND COALESCE(r.last_seen_at, r.updated_at) >= \
                               now() - make_interval(secs => $4::double precision)) \
                   AND NOT EXISTS ( \
                         SELECT 1 FROM agent_task_queue active \
                         WHERE active.agent_id = candidate.agent_id \
                           AND active.status IN ('dispatched', 'running', 'waiting_local_directory') \
                           AND ((candidate.issue_id IS NOT NULL AND active.issue_id = candidate.issue_id) \
                             OR (candidate.chat_session_id IS NOT NULL \
                                 AND active.chat_session_id = candidate.chat_session_id) \
                             OR (candidate.issue_id IS NULL AND candidate.chat_session_id IS NULL \
                                 AND candidate.autopilot_run_id IS NULL \
                                 AND active.issue_id IS NULL AND active.chat_session_id IS NULL \
                                 AND active.autopilot_run_id IS NULL))) \
                 ORDER BY candidate.priority DESC, candidate.created_at ASC, candidate.id ASC \
                 LIMIT 1 FOR UPDATE SKIP LOCKED) \
             RETURNING {TASK_COLUMNS}"
        );
        let row = sqlx::query_as::<_, TaskRow>(&sql)
            .bind(request.agent_id.0)
            .bind(request.runtime_id.0)
            .bind(as_secs(request.policy.prepare_lease_secs))
            .bind(as_secs(request.policy.runtime_stale_secs))
            .fetch_optional(self.pool())
            .await
            .map_err(backend)?;
        let Some(row) = row else { return Ok(None) };
        let state = row.to_task_state().map_err(|e| TaskError::Backend {
            message: e.to_string(),
        })?;
        let transition = TaskTransition {
            from: Some(TaskStatus::Queued),
            to: TaskStatus::Dispatched,
            event: TaskEventKind::Dispatch,
            status_changed: true,
            writes: vec![],
        };
        Ok(Some(TaskClaim {
            task_id: row.id(),
            state,
            transition,
        }))
    }

    /// 取消一行（三个 `CancelAgentTask*` 语句的合并语义）。
    ///
    /// 人工取消走 `CancelAgentTaskByUser`（含 `delivered_comment_ids` 的
    /// 三段式 CASE）；系统取消走 `CancelAgentTask{,WithReason}`。
    ///
    /// # Errors
    ///
    /// - [`TaskError::NotFound`]：行不存在。
    /// - [`TaskError::MalformedEvent`]：`explanation` 与 `user_initiated` 组合非法。
    /// - [`TaskError::Backend`]：DB 错误。
    pub async fn cancel_task(
        &self,
        id: Id,
        cancellation: &Cancellation,
        at: Timestamp,
    ) -> Result<CancelOutcome, TaskError> {
        let Some(row) = self.fetch_row(id).await? else {
            return Err(TaskError::NotFound { id });
        };
        let state = row.to_task_state().map_err(|e| TaskError::Backend {
            message: e.to_string(),
        })?;
        let shape_probe = row.trigger_comment_id.is_some() || !row.coalesced_comment_ids.is_empty();
        let coalesced: Vec<Id> = row
            .coalesced_comment_ids
            .iter()
            .copied()
            .map(Id::from)
            .collect();
        let plan = delivered_comments_plan(
            cancellation.user_initiated,
            row.trigger_comment_id.map(Id::from),
            &coalesced,
            shape_probe,
        );
        let outcome = plan_cancellation(&state, cancellation, plan, at)?;
        if !outcome.is_applied() {
            return Ok(outcome);
        }

        let user = cancellation.user_initiated;
        let error_and_reason = match (&cancellation.explanation, user) {
            (Some(explanation), false) => Some((
                explanation.message.clone(),
                explanation.reason.as_str().to_owned(),
            )),
            _ => None,
        };
        let recompute = matches!(plan, DeliveredCommentsPlan::RecomputeRecoverySignalReceipts);
        let sql = cancel_sql(recompute, error_and_reason.is_some());

        let mut query = sqlx::query_as::<_, TaskRow>(&sql)
            .bind(id.0)
            .bind(at.as_datetime())
            .bind(cancellation.by.type_str())
            .bind(cancellation.by.id().map(|v| v.0))
            .bind(cancellation.by.name().map(str::to_owned));
        if let Some((message, reason)) = &error_and_reason {
            query = query.bind(message.clone()).bind(reason.clone());
        }
        let updated = query.fetch_optional(self.pool()).await.map_err(backend);
        match updated {
            Ok(Some(_)) => Ok(outcome),
            // CAS 落空（并发改状态）：重读后按当前状态给结论。
            Ok(None) => match self.read_state(id).await? {
                Some(current) => Ok(CancelOutcome::AlreadyTerminal {
                    status: current.status,
                }),
                None => Err(TaskError::NotFound { id }),
            },
            Err(err) => Err(err),
        }
    }

    /// 应用一次取消确认（`AckTaskCancelled`）。
    ///
    /// 三条语句的顺序**恒为** durable work dir → branch name → error。
    ///
    /// # Errors
    ///
    /// - [`TaskError::NotFound`]：行不存在。
    /// - [`TaskError::Backend`]：DB 错误。
    pub async fn apply_cancel_ack_task(
        &self,
        id: Id,
        ack: &CancelAck,
    ) -> Result<CancelAckPlan, TaskError> {
        let Some(row) = self.fetch_row(id).await? else {
            return Err(TaskError::NotFound { id });
        };
        let state = row.to_task_state().map_err(|e| TaskError::Backend {
            message: e.to_string(),
        })?;
        let target = CancelAckTarget::from_state(
            &state,
            row.branch_name.clone(),
            row.durable_work_dir.clone(),
        );
        let plan = plan_cancel_ack(ack, &target);
        for write in &plan.writes {
            let sql = match write {
                CancelAckWrite::DurableWorkDir(_) => {
                    "UPDATE agent_task_queue SET durable_work_dir = COALESCE(durable_work_dir, $2) \
                     WHERE id = $1 AND status = 'cancelled'"
                }
                CancelAckWrite::BranchName(_) => {
                    "UPDATE agent_task_queue SET branch_name = COALESCE(branch_name, $2) \
                     WHERE id = $1 AND status = 'cancelled'"
                }
                CancelAckWrite::Error { .. } => {
                    "UPDATE agent_task_queue SET error = $2, \
                         failure_reason = COALESCE(failure_reason, $3) \
                     WHERE id = $1 AND (error IS NULL OR error = '') AND status = 'cancelled'"
                }
            };
            let mut query = sqlx::query(sql).bind(id.0);
            query = match write {
                CancelAckWrite::DurableWorkDir(value) | CancelAckWrite::BranchName(value) => {
                    query.bind(value.clone())
                }
                CancelAckWrite::Error {
                    message,
                    failure_reason,
                } => query.bind(message.clone()).bind(failure_reason.clone()),
            };
            query.execute(self.pool()).await.map_err(backend)?;
        }
        Ok(plan)
    }

    /// 读一个任务的 usage 行（`GetTaskUsage`，`ORDER BY model`）。
    ///
    /// # Errors
    ///
    /// [`TaskError::Backend`]：DB 错误。
    pub async fn list_task_usage(&self, task_id: Id) -> Result<Vec<TaskUsageRow>, TaskError> {
        let rows = sqlx::query(
            "SELECT task_id, provider, model, input_tokens, output_tokens, cache_read_tokens, \
                 cache_write_tokens, cost_usd_ticks, created_at, updated_at \
             FROM task_usage WHERE task_id = $1 ORDER BY model",
        )
        .bind(task_id.0)
        .fetch_all(self.pool())
        .await
        .map_err(backend)?;
        Ok(rows.iter().map(usage_row_from_sql).collect())
    }

    /// 写一条 usage（`UpsertTaskUsage`）：冲突即覆盖，刷新 `updated_at`，不碰 `created_at`。
    ///
    /// # Errors
    ///
    /// [`TaskError::Backend`]：DB 错误。
    pub async fn upsert_task_usage(&self, row: &TaskUsageRow) -> Result<(), TaskError> {
        sqlx::query(
            "INSERT INTO task_usage (task_id, provider, model, input_tokens, output_tokens, \
                 cache_read_tokens, cache_write_tokens, cost_usd_ticks, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8, now()) \
             ON CONFLICT (task_id, provider, model) DO UPDATE SET \
                 input_tokens = EXCLUDED.input_tokens, output_tokens = EXCLUDED.output_tokens, \
                 cache_read_tokens = EXCLUDED.cache_read_tokens, \
                 cache_write_tokens = EXCLUDED.cache_write_tokens, \
                 cost_usd_ticks = EXCLUDED.cost_usd_ticks, updated_at = now()",
        )
        .bind(row.task_id.0)
        .bind(row.provider.clone())
        .bind(row.model.clone())
        .bind(row.input_tokens)
        .bind(row.output_tokens)
        .bind(row.cache_read_tokens)
        .bind(row.cache_write_tokens)
        .bind(row.cost_usd_ticks)
        .execute(self.pool())
        .await
        .map_err(backend)?;
        Ok(())
    }

    /// 读原始行（`TaskRow`，含 ack / 取消判定需要的非状态列）。
    async fn fetch_row(&self, id: Id) -> Result<Option<TaskRow>, TaskError> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM agent_task_queue atq WHERE atq.id = $1");
        sqlx::query_as::<_, TaskRow>(&sql)
            .bind(id.0)
            .fetch_optional(self.pool())
            .await
            .map_err(backend)
    }
}

// ---------------------------------------------------------------------------
// TaskStore 端口适配器
// ---------------------------------------------------------------------------

/// [`TaskStore`] 的 Pg 适配器。
///
/// 持有「要创建的那一行」的 [`NewTask`] —— 见模块文档的「端口缺口」。
pub struct PgTaskStore {
    repo: TaskRepo,
    identity: NewTask,
}

impl PgTaskStore {
    /// 从 `Db` + 身份面构造。
    #[must_use]
    pub fn new(db: &Db, identity: NewTask) -> Self {
        Self {
            repo: TaskRepo::new(db),
            identity,
        }
    }

    /// 从已有 pool + 身份面构造（集成测试用）。
    #[must_use]
    pub fn with_pool(pool: PgPool, identity: NewTask) -> Self {
        Self {
            repo: TaskRepo::with_pool(pool),
            identity,
        }
    }

    /// 覆盖身份面（同一 pool 连续创建多行）。
    pub fn set_identity(&mut self, identity: NewTask) {
        self.identity = identity;
    }
}

#[async_trait]
impl TaskStore for PgTaskStore {
    async fn insert(&self, id: Id, state: &TaskState) -> Result<(), TaskError> {
        let mut new = self.identity.clone();
        new.id = id;
        new.budget = state.budget;
        new.fire_at = state.fire_at;
        // 身份面（agent_id 等）只能来自 `NewTask`；`state` 里的 runtime/父指针覆盖默认值。
        new.runtime_id = new.runtime_id.or(state.runtime_id);
        new.parent_task_id = new.parent_task_id.or(state.parent_task_id);
        self.repo.create_task(&new).await.map(|_| ())
    }

    async fn get(&self, id: Id) -> Result<Option<TaskState>, TaskError> {
        self.repo.read_state(id).await
    }

    async fn commit(
        &self,
        id: Id,
        transition: &TaskTransition,
    ) -> Result<CommitOutcome, TaskError> {
        self.repo.commit_transition(id, transition).await
    }

    async fn claim_next(&self, request: &ClaimRequest) -> Result<Option<TaskClaim>, TaskError> {
        self.repo.claim_next_task(request).await
    }

    async fn cancel(
        &self,
        id: Id,
        cancellation: &Cancellation,
        at: Timestamp,
    ) -> Result<CancelOutcome, TaskError> {
        self.repo.cancel_task(id, cancellation, at).await
    }

    async fn apply_cancel_ack(&self, id: Id, ack: &CancelAck) -> Result<CancelAckPlan, TaskError> {
        self.repo.apply_cancel_ack_task(id, ack).await
    }

    async fn list_usage(&self, task_id: Id) -> Result<Vec<TaskUsageRow>, TaskError> {
        self.repo.list_task_usage(task_id).await
    }

    async fn upsert_usage(&self, row: &TaskUsageRow) -> Result<(), TaskError> {
        self.repo.upsert_task_usage(row).await
    }
}

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

/// DB 错误 → [`TaskError::Backend`]。
#[allow(clippy::needless_pass_by_value)]
fn backend(error: sqlx::Error) -> TaskError {
    TaskError::Backend {
        message: error.to_string(),
    }
}

/// `u64` 秒 → `f64`（`make_interval(secs => …)` 吃 double precision）。
#[allow(clippy::cast_precision_loss)]
fn as_secs(secs: u64) -> f64 {
    secs as f64
}

/// `task_usage` 的一行（`Timestamp` 没有 sqlx impl，手工映射）。
fn usage_row_from_sql(row: &sqlx::postgres::PgRow) -> TaskUsageRow {
    TaskUsageRow {
        task_id: Id::from(row.get::<Uuid, _>("task_id")),
        provider: row.get("provider"),
        model: row.get("model"),
        input_tokens: row.get("input_tokens"),
        output_tokens: row.get("output_tokens"),
        cache_read_tokens: row.get("cache_read_tokens"),
        cache_write_tokens: row.get("cache_write_tokens"),
        cost_usd_ticks: row.get("cost_usd_ticks"),
        created_at: Timestamp::from(row.get::<chrono::DateTime<Utc>, _>("created_at")),
        updated_at: row
            .get::<Option<chrono::DateTime<Utc>>, _>("updated_at")
            .map(Timestamp::from),
    }
}

/// `ColumnWrite` 的一列 + 绑定值。
enum Bound {
    Text(Option<String>),
    Uuid(Option<Uuid>),
    Time(Option<chrono::DateTime<Utc>>),
}

/// 由迁移写单生成 `SET` 子句与绑定值（列名全部是编译期字面量，无注入面）。
fn build_set_clause(transition: &TaskTransition) -> (String, Vec<Bound>) {
    let mut sets = vec!["status = $3".to_owned()];
    let mut binds = Vec::new();
    for write in &transition.writes {
        match write {
            ColumnWrite::DispatchedAt(t) => {
                sets.push(format!("dispatched_at = ${}", binds.len() + 4));
                binds.push(Bound::Time(Some(t.as_datetime())));
            }
            ColumnWrite::DispatchedAtCleared => sets.push("dispatched_at = NULL".to_owned()),
            ColumnWrite::StartedAt(t) => {
                sets.push(format!("started_at = ${}", binds.len() + 4));
                binds.push(Bound::Time(Some(t.as_datetime())));
            }
            ColumnWrite::CompletedAt(t) => {
                sets.push(format!("completed_at = ${}", binds.len() + 4));
                binds.push(Bound::Time(Some(t.as_datetime())));
            }
            ColumnWrite::WaitReason(value) => {
                sets.push(format!("wait_reason = ${}", binds.len() + 4));
                binds.push(Bound::Text(value.clone()));
            }
            ColumnWrite::PrepareLeaseExpiresAt(value) => {
                sets.push(format!("prepare_lease_expires_at = ${}", binds.len() + 4));
                binds.push(Bound::Time(value.map(Timestamp::as_datetime)));
            }
            ColumnWrite::FireAt(value) => {
                sets.push(format!("fire_at = ${}", binds.len() + 4));
                binds.push(Bound::Time(value.map(Timestamp::as_datetime)));
            }
            ColumnWrite::FailureReason(reason) => {
                sets.push(format!("failure_reason = ${}", binds.len() + 4));
                binds.push(Bound::Text(Some(reason.as_str().to_owned())));
            }
            ColumnWrite::ErrorMessage(value) => {
                sets.push(format!("error = ${}", binds.len() + 4));
                binds.push(Bound::Text(value.clone()));
            }
            ColumnWrite::CancelledBy(by) => {
                sets.push(format!("cancelled_by_type = ${}", binds.len() + 4));
                binds.push(Bound::Text(Some(by.type_str().to_owned())));
                sets.push(format!("cancelled_by_id = ${}", binds.len() + 4));
                binds.push(Bound::Uuid(by.id().map(|v| v.0)));
                sets.push(format!("cancelled_by_name = ${}", binds.len() + 4));
                binds.push(Bound::Text(by.name().map(str::to_owned)));
            }
        }
    }
    (sets.join(", "), binds)
}

/// 绑定一列。
fn bind_column<'a>(
    query: sqlx::query::QueryAs<'a, sqlx::Postgres, TaskRow, sqlx::postgres::PgArguments>,
    bound: &Bound,
) -> sqlx::query::QueryAs<'a, sqlx::Postgres, TaskRow, sqlx::postgres::PgArguments> {
    match bound {
        Bound::Text(value) => query.bind(value.clone()),
        Bound::Uuid(value) => query.bind(*value),
        Bound::Time(value) => query.bind(*value),
    }
}
