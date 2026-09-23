//! task 生命周期仓储（R7 拆分自 `daemon.rs`）。
//!
//! 上游落点：`server/pkg/db/queries/agent.sql`（`start` L？/`CompleteAgentTask` L1002 /
//! `FailAgentTask` L1251 / `ClaimNext...`）、`agent_task_message` 追加、task token 表。

// 本文件的 `impl DaemonRepo` 是 `daemon.rs` 那个 impl 的续块（R7 800 行拆分）。

use super::{
    map_sqlx_err, secs_f64, DaemonRepo, DateTime, Id, NewTaskMessage, Result, TaskMessageRow,
    TaskUsageUpsert, Utc, Uuid, Value,
};

impl DaemonRepo {
    /// 按 id 读 task（daemon 守卫 `G_task` 用；workspace 判别交给调用方）。
    pub async fn task_by_id(&self, task_id: Id) -> Result<Option<crate::task::TaskRow>> {
        sqlx::query_as::<_, crate::task::TaskRow>(
            "SELECT * FROM agent_task_queue atq WHERE atq.id = $1",
        )
        .bind(task_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// task 所属 issue 的 workspace（`G_task` 的 workspace 解析；无 issue 的 quick-create 走 agent）。
    pub async fn task_workspace_id(&self, task_id: Id) -> Result<Option<Id>> {
        let row: Option<(Option<Uuid>,)> = sqlx::query_as(
            "SELECT COALESCE(i.workspace_id, a.workspace_id) \
             FROM agent_task_queue atq \
             LEFT JOIN issue i ON i.id = atq.issue_id \
             LEFT JOIN agent a ON a.id = atq.agent_id \
             WHERE atq.id = $1",
        )
        .bind(task_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(row.and_then(|(ws,)| ws.map(Id::from)))
    }

    /// `POST /api/daemon/tasks/{id}/start`（upstream `StartAgentTask`，`agent.sql:970`）。
    ///
    /// CAS：仅 `dispatched` / `waiting_local_directory` 且尚未开工的行可迁移。
    pub async fn start_task(&self, task_id: Id) -> Result<Option<crate::task::TaskRow>> {
        sqlx::query_as::<_, crate::task::TaskRow>(
            "UPDATE agent_task_queue \
             SET status = 'running', started_at = now(), wait_reason = NULL, \
                 prepare_lease_expires_at = NULL \
             WHERE id = $1 AND status IN ('dispatched', 'waiting_local_directory') \
             RETURNING *",
        )
        .bind(task_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// `POST …/wait-local-directory`（upstream `MarkAgentTaskWaitingLocalDirectory`，`agent.sql:985`）。
    pub async fn mark_waiting_local_directory(
        &self,
        task_id: Id,
        reason: Option<String>,
        lease_secs: i64,
    ) -> Result<Option<crate::task::TaskRow>> {
        sqlx::query_as::<_, crate::task::TaskRow>(
            "UPDATE agent_task_queue \
             SET status = 'waiting_local_directory', wait_reason = $2, \
                 prepare_lease_expires_at = now() + make_interval(secs => $3::double precision) \
             WHERE id = $1 AND status = 'dispatched' \
             RETURNING *",
        )
        .bind(task_id.as_uuid())
        .bind(reason)
        .bind(secs_f64(lease_secs))
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// `POST …/prepare-lease`（upstream `ExtendAgentTaskPrepareLease`，`agent.sql:957`）。
    pub async fn extend_prepare_lease(
        &self,
        task_id: Id,
        runtime_id: Id,
        lease_secs: i64,
    ) -> Result<Option<crate::task::TaskRow>> {
        sqlx::query_as::<_, crate::task::TaskRow>(
            "UPDATE agent_task_queue \
             SET prepare_lease_expires_at = now() + make_interval(secs => $3::double precision) \
             WHERE id = $1 AND runtime_id = $2 \
               AND status IN ('dispatched', 'waiting_local_directory') AND started_at IS NULL \
             RETURNING *",
        )
        .bind(task_id.as_uuid())
        .bind(runtime_id.as_uuid())
        .bind(secs_f64(lease_secs))
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// `POST …/complete` 的终态写入（upstream `CompleteAgentTask`，`agent.sql:1002`）。
    ///
    /// `session_rollout_missing = true` 时强制 `session_id = NULL`（MUL-5305）；
    /// 其余可空字段走 `COALESCE`，「没带」不覆盖已落盘的值。
    #[allow(clippy::too_many_arguments)]
    pub async fn complete_task(
        &self,
        task_id: Id,
        result: &Value,
        session_id: Option<String>,
        work_dir: Option<String>,
        branch_name: Option<String>,
        session_rollout_missing: bool,
        retired_session_id: Option<String>,
        durable_work_dir: Option<String>,
    ) -> Result<Option<crate::task::TaskRow>> {
        sqlx::query_as::<_, crate::task::TaskRow>(
            "UPDATE agent_task_queue SET \
                status = 'completed', completed_at = now(), result = $2, \
                session_id = CASE WHEN $8 THEN NULL ELSE $3 END, \
                work_dir = $4, \
                durable_work_dir = COALESCE($5, durable_work_dir), \
                branch_name = COALESCE($6, branch_name), \
                session_rollout_missing = $8, \
                retired_session_id = COALESCE($7, retired_session_id), \
                prepare_lease_expires_at = NULL \
             WHERE id = $1 AND status = 'running' \
             RETURNING *",
        )
        .bind(task_id.as_uuid())
        .bind(result)
        .bind(session_id)
        .bind(work_dir)
        .bind(durable_work_dir)
        .bind(branch_name)
        .bind(retired_session_id)
        .bind(session_rollout_missing)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// `POST …/fail` 的终态写入（upstream `FailAgentTask`，`agent.sql:1251`）。
    ///
    /// 注意上游的绑定顺序是 `$1 id, $2 error, $3 failure_reason`，其余走 `COALESCE`。
    #[allow(clippy::too_many_arguments)]
    pub async fn fail_task(
        &self,
        task_id: Id,
        error: Option<String>,
        failure_reason: Option<String>,
        session_id: Option<String>,
        work_dir: Option<String>,
        durable_work_dir: Option<String>,
        branch_name: Option<String>,
        session_rollout_missing: bool,
        retired_session_id: Option<String>,
    ) -> Result<Option<crate::task::TaskRow>> {
        sqlx::query_as::<_, crate::task::TaskRow>(
            "UPDATE agent_task_queue SET \
                status = 'failed', completed_at = now(), error = $2, \
                failure_reason = COALESCE($3, 'agent_error'), \
                session_id = CASE WHEN $9 THEN NULL ELSE COALESCE($4, session_id) END, \
                work_dir = COALESCE($5, work_dir), \
                durable_work_dir = COALESCE($6, durable_work_dir), \
                branch_name = COALESCE($7, branch_name), \
                session_rollout_missing = $9, \
                retired_session_id = COALESCE($8, retired_session_id), \
                prepare_lease_expires_at = NULL \
             WHERE id = $1 AND status IN ('dispatched', 'running', 'waiting_local_directory') \
             RETURNING *",
        )
        .bind(task_id.as_uuid())
        .bind(error)
        .bind(failure_reason)
        .bind(session_id)
        .bind(work_dir)
        .bind(durable_work_dir)
        .bind(branch_name)
        .bind(retired_session_id)
        .bind(session_rollout_missing)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// `POST …/cancel-ack`：落 `branch_name` / `durable_work_dir` / 错误信息
    /// （upstream `RecordDurableWorkDir` + `RecordBranchName` + `RecordTaskError` 三步合并）。
    ///
    /// 三步各自 `COALESCE`，缺项不动既有值；返回被改动的行（`None` = 无此行）。
    pub async fn ack_task_cancelled(
        &self,
        task_id: Id,
        branch_name: Option<String>,
        durable_work_dir: Option<String>,
        error: Option<String>,
        failure_reason: Option<String>,
    ) -> Result<Option<crate::task::TaskRow>> {
        sqlx::query_as::<_, crate::task::TaskRow>(
            "UPDATE agent_task_queue SET \
                branch_name = COALESCE(branch_name, $2), \
                durable_work_dir = COALESCE(durable_work_dir, $3), \
                error = COALESCE(error, $4), \
                failure_reason = COALESCE(failure_reason, $5) \
             WHERE id = $1 AND status = 'cancelled' RETURNING *",
        )
        .bind(task_id.as_uuid())
        .bind(branch_name)
        .bind(durable_work_dir)
        .bind(error)
        .bind(failure_reason)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// 自动取消（upstream `CancelAgentTask`）：无显式失败原因，恢复输入保持可重放。
    ///
    /// 批量 claim 里 runtime `owner_id` 为空时会走到这里（避免发无 scope 的 Agent 凭据）。
    pub async fn cancel_task(&self, task_id: Id) -> Result<Option<crate::task::TaskRow>> {
        sqlx::query_as::<_, crate::task::TaskRow>(
            "UPDATE agent_task_queue SET \
                status = 'cancelled', completed_at = now(), \
                prepare_lease_expires_at = NULL, \
                cancelled_by_type = 'system', cancelled_by_id = NULL, \
                cancelled_by_name = NULL \
             WHERE id = $1 \
               AND status IN ('queued','dispatched','running','waiting_local_directory','deferred') \
             RETURNING *",
        )
        .bind(task_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// claim 终结化失败时把**那一次** claim 放回队列（upstream
    /// `RequeueAgentTaskAfterClaimFailure`）。`dispatched_at` 的 CAS 防止旧 handler
    /// 回退更新的 reclaim。
    pub async fn requeue_task_after_claim_failure(
        &self,
        task_id: Id,
    ) -> Result<Option<crate::task::TaskRow>> {
        sqlx::query_as::<_, crate::task::TaskRow>(
            "UPDATE agent_task_queue SET \
                status = 'queued', dispatched_at = NULL, \
                prepare_lease_expires_at = NULL, delivered_comment_ids = '{}' \
             WHERE id = $1 AND status = 'dispatched' AND started_at IS NULL \
             RETURNING *",
        )
        .bind(task_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// `POST …/session`：pin `session_id` / `work_dir`
    /// （upstream `UpdateAgentTaskSession`，`agent.sql:1282`——只填空槽，绝不覆盖，终态行不可动）。
    ///
    /// 返回受影响行数（0 = 没有可 pin 的行，上游同样静默成功 → 204）。
    pub async fn pin_task_session(
        &self,
        task_id: Id,
        session_id: Option<String>,
        work_dir: Option<String>,
    ) -> Result<u64> {
        let res = sqlx::query(
            "UPDATE agent_task_queue SET \
                session_id = COALESCE($2, session_id), \
                work_dir = COALESCE($3, work_dir) \
             WHERE id = $1 \
               AND (status IN ('dispatched', 'running') \
                    OR (status = 'cancelled' AND session_id IS NULL))",
        )
        .bind(task_id.as_uuid())
        .bind(session_id)
        .bind(work_dir)
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(res.rows_affected())
    }

    /// `GET …/runtimes/{runtimeId}/tasks/pending`：该 runtime 名下 `queued` + `dispatched`
    /// 的任务（upstream `ListPendingTasksByRuntime`，`agent.sql:2266`）。
    pub async fn list_pending_tasks(&self, runtime_id: Id) -> Result<Vec<crate::task::TaskRow>> {
        sqlx::query_as::<_, crate::task::TaskRow>(
            "SELECT * FROM agent_task_queue atq \
             WHERE atq.runtime_id = $1 AND atq.status IN ('queued','dispatched') \
             ORDER BY atq.priority DESC, atq.created_at ASC",
        )
        .bind(runtime_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// `POST …/recover-orphans`：把该 runtime 名下“上一进程还持有但没终结”的任务
    /// atomically 判失败（upstream `RecoverOrphanedTasksForRuntime`，`agent.sql:1305`）。
    ///
    /// 包含 `waiting_local_directory`：持有路径锁的就是刚死的那个进程。返回失败行，
    /// 供调用方走与 runtime sweeper 同一套后续流水线。
    pub async fn recover_orphaned_tasks(
        &self,
        runtime_id: Id,
    ) -> Result<Vec<crate::task::TaskRow>> {
        sqlx::query_as::<_, crate::task::TaskRow>(
            "UPDATE agent_task_queue SET \
                status = 'failed', completed_at = now(), \
                error = 'daemon restarted while task was in flight', \
                failure_reason = 'runtime_recovery', wait_reason = NULL, \
                prepare_lease_expires_at = NULL \
             WHERE runtime_id = $1 \
               AND status IN ('dispatched','running','waiting_local_directory') \
             RETURNING *",
        )
        .bind(runtime_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    // ---------------------------------------------------------------- messages / usage

    /// 批量追加 task 消息（upstream `InsertTaskMessage`，无幂等键 ⇒ 重发会重复入库）。
    pub async fn insert_task_messages(&self, rows: &[NewTaskMessage]) -> Result<()> {
        for row in rows {
            sqlx::query(
                "INSERT INTO task_message \
                    (task_id, seq, type, tool, content, input, output, output_truncated, call_id, \
                     created_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, COALESCE($10, now()))",
            )
            .bind(row.task_id.as_uuid())
            .bind(row.seq)
            .bind(&row.kind)
            .bind(&row.tool)
            .bind(&row.content)
            .bind(&row.input)
            .bind(&row.output)
            .bind(row.output_truncated)
            .bind(&row.call_id)
            .bind(row.created_at)
            .execute(&self.pool)
            .await
            .map_err(map_sqlx_err)?;
        }
        Ok(())
    }

    /// 读 task 消息（upstream `ListTaskMessages`；`since` 为 `seq` 下界）。
    pub async fn list_task_messages(
        &self,
        task_id: Id,
        since_seq: Option<i32>,
    ) -> Result<Vec<TaskMessageRow>> {
        let sql = match since_seq {
            Some(_) => {
                "SELECT id, task_id, seq, type, tool, content, input, output, created_at, \
                        output_truncated, call_id FROM task_message \
                 WHERE task_id = $1 AND seq > $2 ORDER BY seq ASC, created_at ASC"
            }
            None => {
                "SELECT id, task_id, seq, type, tool, content, input, output, created_at, \
                        output_truncated, call_id FROM task_message \
                 WHERE task_id = $1 ORDER BY seq ASC, created_at ASC"
            }
        };
        let mut q = sqlx::query_as::<_, TaskMessageRow>(sql).bind(task_id.as_uuid());
        if let Some(since) = since_seq {
            q = q.bind(since);
        }
        q.fetch_all(&self.pool).await.map_err(map_sqlx_err)
    }

    /// 逐条 upsert task usage（`UNIQUE (task_id, provider, model)`；`updated_at` 显式刷新，供日汇总识别更正）。
    pub async fn upsert_task_usage(&self, row: &TaskUsageUpsert) -> Result<()> {
        sqlx::query(
            "INSERT INTO task_usage \
                (task_id, provider, model, input_tokens, output_tokens, cache_read_tokens, \
                 cache_write_tokens, cost_usd_ticks) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (task_id, provider, model) DO UPDATE SET \
                input_tokens = EXCLUDED.input_tokens, \
                output_tokens = EXCLUDED.output_tokens, \
                cache_read_tokens = EXCLUDED.cache_read_tokens, \
                cache_write_tokens = EXCLUDED.cache_write_tokens, \
                cost_usd_ticks = EXCLUDED.cost_usd_ticks, \
                updated_at = now()",
        )
        .bind(row.task_id.as_uuid())
        .bind(&row.provider)
        .bind(&row.model)
        .bind(row.input_tokens)
        .bind(row.output_tokens)
        .bind(row.cache_read_tokens)
        .bind(row.cache_write_tokens)
        .bind(row.cost_usd_ticks)
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(())
    }

    // ---------------------------------------------------------------- claim

    /// 为**一个** runtime 领取下一条 queued 任务（upstream `ClaimAgentTask`，逐字移植）。
    ///
    /// "同一 `(issue, agent)` 串行" / "同一 `(chat_session, agent)` 串行" / "无链接任务串行"
    /// 三条互斥条件、`priority DESC, created_at ASC, id ASC` 排序、`FOR UPDATE SKIP LOCKED`
    /// 全部保留：它们就是 `idx_one_pending_task_per_issue` 之外的行为来源。
    ///
    /// 未移植的两个条件（登记在 `docs/32` 偏离表）：
    /// - `context->>'wakeup_id'` 的 `issue_wakeup` 活性门（本仓 `issue_wakeup` 表存在，
    ///   但 wakeup 的 revision 维护面属 M3 其它切片；未启用 wakeup 时该门恒真）；
    /// - `runtime_stale_secs` 活性门保留（只靠 `agent_runtime.status='online'`
    ///   + `last_seen_at` 新鲜度）。
    pub async fn claim_next_task_for_runtime(
        &self,
        runtime_id: Id,
        prepare_lease_secs: i64,
        runtime_stale_secs: i64,
    ) -> Result<Option<crate::task::TaskRow>> {
        sqlx::query_as::<_, crate::task::TaskRow>(
            "UPDATE agent_task_queue \
             SET status = 'dispatched', \
                 dispatched_at = now(), \
                 prepare_lease_expires_at = now() + make_interval(secs => $2::double precision) \
             WHERE id = ( \
                 SELECT atq.id FROM agent_task_queue atq \
                 WHERE atq.runtime_id = $1 \
                   AND atq.status = 'queued' \
                   AND EXISTS ( \
                       SELECT 1 FROM agent a \
                       JOIN agent_runtime r ON r.id = atq.runtime_id \
                       WHERE a.id = atq.agent_id \
                         AND a.runtime_id = atq.runtime_id \
                         AND r.status = 'online' \
                         AND COALESCE(r.last_seen_at, r.updated_at) >= \
                             now() - make_interval(secs => $3::double precision) \
                   ) \
                   AND NOT EXISTS ( \
                       SELECT 1 FROM agent_task_queue active \
                       WHERE active.agent_id = atq.agent_id \
                         AND active.status IN ('dispatched', 'running', 'waiting_local_directory') \
                         AND ( \
                           (atq.issue_id IS NOT NULL AND active.issue_id = atq.issue_id) \
                           OR (atq.chat_session_id IS NOT NULL AND active.chat_session_id = atq.chat_session_id) \
                           OR ( \
                             atq.issue_id IS NULL \
                             AND atq.chat_session_id IS NULL \
                             AND atq.autopilot_run_id IS NULL \
                             AND active.issue_id IS NULL \
                             AND active.chat_session_id IS NULL \
                             AND active.autopilot_run_id IS NULL \
                           ) \
                         ) \
                   ) \
                 ORDER BY atq.priority DESC, atq.created_at ASC, atq.id ASC \
                 LIMIT 1 \
                 FOR UPDATE SKIP LOCKED \
             ) \
             RETURNING *",
        )
        .bind(runtime_id.as_uuid())
        .bind(secs_f64(prepare_lease_secs))
        .bind(secs_f64(runtime_stale_secs))
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// 发一枚 task token（upstream `CreateTaskToken`）。返回明文之外的 id。
    pub async fn insert_task_token(
        &self,
        token_hash: &str,
        task_id: Id,
        agent_id: Id,
        workspace_id: Id,
        user_id: Id,
        ttl_secs: i64,
    ) -> Result<Id> {
        let (id,): (Uuid,) = sqlx::query_as(
            "INSERT INTO task_token \
                (token_hash, task_id, agent_id, workspace_id, user_id, expires_at) \
             VALUES ($1, $2, $3, $4, $5, now() + make_interval(secs => $6::double precision)) \
             RETURNING id",
        )
        .bind(token_hash)
        .bind(task_id.as_uuid())
        .bind(agent_id.as_uuid())
        .bind(workspace_id.as_uuid())
        .bind(user_id.as_uuid())
        .bind(secs_f64(ttl_secs))
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(Id::from(id))
    }

    /// 任务终结时清掉它的全部 task token（upstream `DeleteTaskTokensByTask`）。
    pub async fn delete_task_tokens_by_task(&self, task_id: Id) -> Result<u64> {
        let out = sqlx::query("DELETE FROM task_token WHERE task_id = $1")
            .bind(task_id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(map_sqlx_err)?;
        Ok(out.rows_affected())
    }

    /// 该 issue 上是否已有非终态任务（用于 claim 冲突的 409 诊断）。
    pub async fn has_active_task_for_issue(&self, issue_id: Id) -> Result<bool> {
        let (exists,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM agent_task_queue \
             WHERE issue_id = $1 AND status IN ('queued','dispatched','running','waiting_local_directory'))",
        )
        .bind(issue_id.as_uuid())
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(exists)
    }

    /// 授权 runtime 集合上**最近的 `deferred` 任务 `fire_at`**（upstream
    /// `NextDeferredTaskFireAtForRuntimes`，`agent.sql:2419`）。
    ///
    /// 供批量 claim 响应里的 `next_deferred_task_after_ms` 用：让 opt-in 的 daemon 在
    /// 长轮询（健康 WS 上的安全轮询）里知道「多久之后值得再 ask 一次」，而不是恒定
    /// `PollInterval`。返回 `Ok(None)` = 没有可推进的 deferred 任务 ⇒ 响应里省略该字段。
    ///
    /// 两道围栏与 `PromoteDueDeferredTasksForRuntimes` **逐字对齐**：不能让一个
    /// 「推不动」的任务广告出「立刻再来一次」，否则就是紧轮询。
    pub async fn next_deferred_task_fire_at(
        &self,
        runtime_ids: &[Id],
        runtime_stale_secs: f64,
    ) -> Result<Option<DateTime<Utc>>> {
        let ids: Vec<Uuid> = runtime_ids.iter().copied().map(Id::as_uuid).collect();
        let (next,): (Option<DateTime<Utc>>,) = sqlx::query_as(
            "SELECT MIN(t.fire_at)::timestamptz \
             FROM agent_task_queue t \
             WHERE t.runtime_id = ANY($1::uuid[]) \
               AND t.status = 'deferred' \
               AND EXISTS ( \
                   SELECT 1 FROM agent_runtime r \
                   WHERE r.id = t.runtime_id \
                     AND r.status = 'online' \
                     AND COALESCE(r.last_seen_at, r.updated_at) >= \
                         now() - make_interval(secs => $2::double precision) \
               ) \
               AND ( \
                 t.fire_at > now() \
                 OR NOT EXISTS ( \
                   SELECT 1 FROM agent_task_queue occupant \
                   WHERE occupant.issue_id = t.issue_id \
                     AND occupant.agent_id = t.agent_id \
                     AND occupant.comment_thread_id IS NOT DISTINCT FROM t.comment_thread_id \
                     AND occupant.id <> t.id \
                     AND ( \
                       occupant.status IN ('queued', 'dispatched') \
                       OR (occupant.status = 'deferred' \
                           AND occupant.context->>'channel_issue_media_pending' = 'true') \
                     ) \
                 ) \
               )",
        )
        .bind(&ids)
        .bind(runtime_stale_secs)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_err)?;
        Ok(next)
    }

    // ---------------------------------------------------------------- skills
}
