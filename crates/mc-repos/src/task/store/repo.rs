//! `impl TaskRepo` —— 写侧的对外方法（创建 / 读态 / CAS 迁移 / 认领 / 取消 /
//! cancel-ack / usage）。模块文档见父模块（`mod.rs`）。

use mc_core::{Id, Timestamp};
use mc_task::cancel::{
    delivered_comments_plan, plan_cancel_ack, plan_cancellation, CancelAck, CancelAckPlan,
    CancelAckTarget, CancelAckWrite, CancelOutcome, Cancellation, DeliveredCommentsPlan,
};
use mc_task::error::TaskError;
use mc_task::state::{TaskEventKind, TaskState, TaskTransition};
use mc_task::status::TaskStatus;
use mc_task::store::{ClaimRequest, CommitOutcome, TaskClaim};
use mc_task::usage::TaskUsageRow;

use super::super::row::TaskRow;
use super::super::{TaskRepo, TASK_COLUMNS};
use super::helpers::{as_secs, backend, bind_column, build_set_clause, usage_row_from_sql};
use super::new_task::{cancel_sql, NewTask};

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
