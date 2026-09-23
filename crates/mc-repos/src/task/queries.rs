//! 路线面查询（W3b / M3-6）。
//!
//! 本文件兑现 15 条路由里**除** `TaskStore` 端口（`store.rs`）之外的读路径与
//! agent-builder 写路径。命名与上游 `pkg/db/queries` 的 statement 名一一对应，
//! 便于审阅时逐条对照（映射表见 [`super`] 的模块文档）。
//!
//! 全部用运行时 sqlx builder + 参数绑定：构建期不连库，也不生成 compile-time 宏。
//! SQL 里出现的列名**全部**来自上游迁移，**不含**自造列（docs/15 §2.2）。

use uuid::Uuid;

use mc_core::Id;

use super::row::{
    AgentBrief, ChatSessionRow, ClientUsageUpsert, FamilyActiveTaskRow, IssueBrief,
    IssueUsageSummaryRow, RerunTaskSpec, TaskMessageRow, TaskRow, WorkingAgentRow,
};
use super::{TaskRepo, TASK_COLUMNS};
use crate::RepoError;
use mc_task::retry::RetryBudget;

/// `list_working_agents` 的筛选条件（上游 `ListWorkspaceWorkingAgents` 的具名参数）。
#[derive(Debug, Clone, Default)]
pub struct WorkingAgentFilter {
    /// `type`：`""` / `issue` / `autopilot` / `chat`。
    pub work_type: String,
    /// `relation`：`""` / `assigned` / `created` / `involved` / `any`。
    pub mine_relation: String,
    /// `scope=mine` 时的成员 id。
    pub member_id: Option<Id>,
    /// `parent`：收窄到该 issue 的直接子 issue（仅 `type=issue`）。
    pub parent_issue_id: Option<Id>,
}

impl TaskRepo {
    // -----------------------------------------------------------------------
    // issue 作用域读
    // -----------------------------------------------------------------------

    /// issue 上「还活着」的任务 —— 上游 `ListActiveTasksByIssue`（`agent.sql:2493`）。
    ///
    /// 含 `queued`：runtime 离线或忙时这个窗口可能很长，静默的 UI 会被读成「没收到触发」。
    ///
    /// # Errors
    ///
    /// 同 [`TaskRepo::task_in_workspace`]。
    pub async fn list_active_tasks_by_issue(&self, issue_id: Id) -> crate::Result<Vec<TaskRow>> {
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM agent_task_queue atq \
             WHERE atq.issue_id = $1 \
               AND atq.status IN ('queued','dispatched','running','waiting_local_directory') \
             ORDER BY atq.created_at DESC"
        );
        self.fetch_rows(&sql, issue_id).await
    }

    /// issue 的执行日志（全量，倒序）—— 上游 `ListTasksByIssue`（`agent.sql:2773`）。
    ///
    /// `visibleTaskHistory` 的收敛（上游 `agent.go:783`：`escalation_for_task_id` 非空、
    /// **未启动**、且状态是 `deferred`/`cancelled` 的升级占位行不外泄）在 SQL 里完成，
    /// 因为它是纯粹的可见性谓词，不需要先取回再丢弃。
    ///
    /// # Errors
    ///
    /// 同 [`TaskRepo::task_in_workspace`]。
    pub async fn list_tasks_by_issue(&self, issue_id: Id) -> crate::Result<Vec<TaskRow>> {
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM agent_task_queue atq \
             WHERE atq.issue_id = $1 \
               AND NOT (atq.escalation_for_task_id IS NOT NULL \
                        AND atq.started_at IS NULL \
                        AND atq.status IN ('deferred','cancelled')) \
             ORDER BY atq.created_at DESC"
        );
        self.fetch_rows(&sql, issue_id).await
    }

    /// 跨 issue 的「同一族里谁在干活」协调读 —— 上游 `ListActiveTasksByIssueFamily`
    /// （`agent.sql:2503`；`daemon.go:5563` 的 `scope=family`）。
    ///
    /// 族根 = 目标 issue 的父 issue（没有父就是它自己），返回根与**全部直接子 issue**
    /// 上的在飞任务。`running` 优先排序，因为 `LIMIT` 的截断必须丢掉最不重要的行。
    ///
    /// # Errors
    ///
    /// 同 [`TaskRepo::task_in_workspace`]。
    pub async fn list_active_tasks_by_issue_family(
        &self,
        workspace_id: Id,
        root_issue_id: Id,
        row_limit: i64,
    ) -> crate::Result<Vec<FamilyActiveTaskRow>> {
        const SQL: &str = "\
SELECT atq.id AS task_id, atq.agent_id, atq.issue_id, atq.status, atq.created_at, \
       atq.started_at, w.issue_prefix, i.number AS issue_number, i.title AS issue_title \
FROM agent_task_queue atq \
JOIN issue i ON i.id = atq.issue_id \
JOIN workspace w ON w.id = i.workspace_id \
WHERE i.workspace_id = $1 \
  AND (i.id = $2 OR i.parent_issue_id = $2) \
  AND atq.status IN ('queued','dispatched','running','waiting_local_directory') \
ORDER BY CASE atq.status \
             WHEN 'running' THEN 0 \
             WHEN 'dispatched' THEN 1 \
             WHEN 'waiting_local_directory' THEN 2 \
             ELSE 3 \
         END, atq.created_at DESC \
LIMIT $3";
        sqlx::query_as::<_, FamilyActiveTaskRow>(SQL)
            .bind(workspace_id.0)
            .bind(root_issue_id.0)
            .bind(row_limit)
            .fetch_all(self.pool())
            .await
            .map_err(crate::workspace::map_sqlx_err)
    }

    /// 任务的消息流水 —— 上游 `ListTaskMessages` / `ListTaskMessagesSince`（`task_message.sql:86`）。
    /// `since_seq` 为空 ⇒ 全量；否则只取 `seq > since_seq`（增量轮询）。
    ///
    /// # Errors
    ///
    /// 同 [`TaskRepo::task_in_workspace`]。
    pub async fn list_task_messages(
        &self,
        task_id: Id,
        since_seq: Option<i32>,
    ) -> crate::Result<Vec<TaskMessageRow>> {
        let base = "SELECT id, task_id, seq, type, tool, content, input, output, created_at, \
                    output_truncated, call_id FROM task_message WHERE task_id = $1";
        let rows = if let Some(seq) = since_seq {
            let sql = format!("{base} AND seq > $2 ORDER BY seq ASC");
            sqlx::query_as::<_, TaskMessageRow>(&sql)
                .bind(task_id.0)
                .bind(seq)
                .fetch_all(self.pool())
                .await
        } else {
            let sql = format!("{base} ORDER BY seq ASC");
            sqlx::query_as::<_, TaskMessageRow>(&sql)
                .bind(task_id.0)
                .fetch_all(self.pool())
                .await
        };
        rows.map_err(crate::workspace::map_sqlx_err)
    }

    /// issue 的 token / 成本汇总 —— 上游 `GetIssueUsageSummary`（`task_usage.sql:72`）。
    ///
    /// 两个 CTE 都是聚合，`CROSS JOIN` 后恒定一行（无 usage 时为全 0），因此
    /// 这里用 `fetch_one`；issue 不存在与否不影响结果形状（路由层单独判存在性）。
    ///
    /// # Errors
    ///
    /// 同 [`TaskRepo::task_in_workspace`]。
    pub async fn issue_usage_summary(&self, issue_id: Id) -> crate::Result<IssueUsageSummaryRow> {
        const SQL: &str = "\
WITH usage AS (
    SELECT
        COALESCE(SUM(tu.input_tokens), 0)::bigint AS total_input_tokens,
        COALESCE(SUM(tu.output_tokens), 0)::bigint AS total_output_tokens,
        COALESCE(SUM(tu.cache_read_tokens), 0)::bigint AS total_cache_read_tokens,
        COALESCE(SUM(tu.cache_write_tokens), 0)::bigint AS total_cache_write_tokens,
        COALESCE(SUM(tu.cost_usd_ticks), 0)::bigint AS total_cost_usd_ticks,
        COALESCE(SUM(tu.input_tokens)       FILTER (WHERE tu.cost_usd_ticks IS NULL), 0)::bigint AS uncosted_input_tokens,
        COALESCE(SUM(tu.output_tokens)      FILTER (WHERE tu.cost_usd_ticks IS NULL), 0)::bigint AS uncosted_output_tokens,
        COALESCE(SUM(tu.cache_read_tokens)  FILTER (WHERE tu.cost_usd_ticks IS NULL), 0)::bigint AS uncosted_cache_read_tokens,
        COALESCE(SUM(tu.cache_write_tokens) FILTER (WHERE tu.cost_usd_ticks IS NULL), 0)::bigint AS uncosted_cache_write_tokens,
        COUNT(DISTINCT tu.task_id)::int AS task_count
    FROM task_usage tu
    JOIN agent_task_queue atq ON atq.id = tu.task_id
    WHERE atq.issue_id = $1
), terminal_runs AS (
    SELECT
        COUNT(*)::int AS terminal_task_count,
        COUNT(*) FILTER (WHERE EXISTS (
            SELECT 1 FROM task_usage tu WHERE tu.task_id = atq.id
        ))::int AS metered_task_count
    FROM agent_task_queue atq
    WHERE atq.issue_id = $1
      AND atq.status IN ('completed','failed','cancelled')
      AND atq.started_at IS NOT NULL
      AND atq.completed_at IS NOT NULL
)
SELECT
    usage.*,
    terminal_runs.terminal_task_count,
    terminal_runs.metered_task_count,
    (terminal_runs.terminal_task_count - terminal_runs.metered_task_count)::int AS unreported_task_count
FROM usage
CROSS JOIN terminal_runs";
        sqlx::query_as::<_, IssueUsageSummaryRow>(SQL)
            .bind(issue_id.0)
            .fetch_one(self.pool())
            .await
            .map_err(crate::workspace::map_sqlx_err)
    }

    /// 工作区里「正在干活」的 agent —— 上游 `ListWorkspaceWorkingAgents`（`agent.sql:2646`）。
    ///
    /// 只看真正开始执行的 `running` 任务（`queued` / `dispatched` 不算「working」，
    /// 这与 issue 详情的 live banner 不同，是有意的）。`work_type` 的优先级
    /// `chat > autopilot > issue` 与上游 `computeTaskKind` 一致；quick-create
    /// 只在不过滤的投影里出现（它还没有 source FK）。
    ///
    /// # Errors
    ///
    /// 同 [`TaskRepo::task_in_workspace`]。
    pub async fn list_working_agents(
        &self,
        workspace_id: Id,
        filter: &WorkingAgentFilter,
    ) -> crate::Result<Vec<WorkingAgentRow>> {
        const SQL: &str = "\
SELECT
  a.id,
  a.name,
  a.avatar_url,
  COUNT(*)::bigint AS running_task_count,
  COALESCE(
    ARRAY_AGG(DISTINCT atq.issue_id ORDER BY atq.issue_id)
      FILTER (WHERE atq.issue_id IS NOT NULL),
    ARRAY[]::uuid[]
  )::uuid[] AS issue_ids
FROM agent a
JOIN agent_task_queue atq ON atq.agent_id = a.id
WHERE a.workspace_id = $1
  AND a.kind = 'user'
  AND a.archived_at IS NULL
  AND atq.status = 'running'
  AND (
    $2::text = ''
    OR ($2::text = 'chat' AND atq.chat_session_id IS NOT NULL)
    OR ($2::text = 'autopilot' AND atq.chat_session_id IS NULL AND atq.autopilot_run_id IS NOT NULL)
    OR ($2::text = 'issue' AND atq.chat_session_id IS NULL AND atq.autopilot_run_id IS NULL AND atq.issue_id IS NOT NULL)
  )
  AND (
    $3::text = ''
    OR EXISTS (
      SELECT 1 FROM issue i
      WHERE i.id = atq.issue_id
        AND i.workspace_id = a.workspace_id
        AND (
          ($3::text IN ('assigned','any') AND i.assignee_type = 'member' AND i.assignee_id = $4::uuid)
          OR ($3::text IN ('created','any') AND i.creator_type = 'member' AND i.creator_id = $4::uuid)
          OR ($3::text IN ('involved','any') AND (
            (i.assignee_type = 'agent' AND EXISTS (
              SELECT 1 FROM agent owned_agent
              WHERE owned_agent.id = i.assignee_id
                AND owned_agent.workspace_id = a.workspace_id
                AND owned_agent.owner_id = $4::uuid))
            OR (i.assignee_type = 'squad' AND EXISTS (
              SELECT 1 FROM squad s
              WHERE s.id = i.assignee_id AND s.workspace_id = a.workspace_id
                AND (
                  EXISTS (SELECT 1 FROM squad_member sm
                          WHERE sm.squad_id = s.id AND sm.member_type = 'member' AND sm.member_id = $4::uuid)
                  OR EXISTS (SELECT 1 FROM agent leader
                             WHERE leader.id = s.leader_id AND leader.workspace_id = a.workspace_id
                               AND leader.owner_id = $4::uuid)
                  OR EXISTS (SELECT 1 FROM squad_member sm
                             JOIN agent owned_member ON owned_member.id = sm.member_id
                             WHERE sm.squad_id = s.id AND sm.member_type = 'agent'
                               AND owned_member.workspace_id = a.workspace_id
                               AND owned_member.owner_id = $4::uuid))))
          ))
        )
    )
  )
  AND (
    $5::uuid IS NULL
    OR EXISTS (
      SELECT 1 FROM issue child
      WHERE child.id = atq.issue_id
        AND child.workspace_id = a.workspace_id
        AND child.parent_issue_id = $5::uuid
    )
  )
GROUP BY a.id, a.name, a.avatar_url, a.created_at
ORDER BY a.created_at ASC";
        sqlx::query_as::<_, WorkingAgentRow>(SQL)
            .bind(workspace_id.0)
            .bind(&filter.work_type)
            .bind(&filter.mine_relation)
            .bind(filter.member_id.map(|id| id.0))
            .bind(filter.parent_issue_id.map(|id| id.0))
            .fetch_all(self.pool())
            .await
            .map_err(crate::workspace::map_sqlx_err)
    }

    // -----------------------------------------------------------------------
    // 存在性 / 归属
    // -----------------------------------------------------------------------

    /// issue 是否属于该 workspace，并取回 preview / rerun 需要的最小字段。
    ///
    /// # Errors
    ///
    /// 同 [`TaskRepo::task_in_workspace`]。
    pub async fn issue_for_workspace(
        &self,
        issue_id: Id,
        workspace_id: Id,
    ) -> crate::Result<Option<IssueBrief>> {
        const SQL: &str = "SELECT id, status, status_name, assignee_type, assignee_id, \
                                  triage_state, project_id, parent_issue_id, identifier, title \
                           FROM issue WHERE id = $1 AND workspace_id = $2";
        sqlx::query_as::<_, IssueBrief>(SQL)
            .bind(issue_id.0)
            .bind(workspace_id.0)
            .fetch_optional(self.pool())
            .await
            .map_err(crate::workspace::map_sqlx_err)
    }

    /// 上游 `issuestatus.Effective` 的 SQL 镜像。
    ///
    /// `migrations/upstream/494_issue_status_category_read_contract.up.sql` 建出的
    /// `issue_effective_status(p_workspace_id, p_status)` 与 Go 侧逐字等价：内建键
    /// 短路返回自身；自定义键按 `issue_status.category` 折叠
    /// （`done` → `done`，`closed` → `cancelled`），未登记的键原样返回。
    ///
    /// preview-trigger 的 `backlog` 停放判定 / 终态判定必须走这个口径，否则自定义
    /// 终态（归档的 `done` 类状态）会被误判成「可放行」。
    ///
    /// # Errors
    ///
    /// 同 [`TaskRepo::task_in_workspace`]。
    pub async fn effective_status(&self, workspace_id: Id, status: &str) -> crate::Result<String> {
        let raw: Option<String> = sqlx::query_scalar("SELECT issue_effective_status($1, $2)")
            .bind(workspace_id.0)
            .bind(status)
            .fetch_one(self.pool())
            .await
            .map_err(crate::workspace::map_sqlx_err)?;
        Ok(raw.unwrap_or_else(|| status.to_owned()))
    }

    /// 按 **UUID 或 identifier**（`LUM-42`）解析 issue —— 上游 `loadIssueForUser`
    /// （`handler.go:1029`）：先试 identifier，再按 UUID。
    ///
    /// `:id` 路径参数两条都要接：上游前端走 UUID，VCS/webhook 面走 identifier。
    /// 都不是时返回 `Ok(None)` —— 上游对「非法 UUID 且不是 identifier」也回
    /// `issue not found`（404），不是 400。
    ///
    /// # Errors
    ///
    /// 同 [`TaskRepo::task_in_workspace`]。
    pub async fn issue_for_workspace_ref(
        &self,
        raw: &str,
        workspace_id: Id,
    ) -> crate::Result<Option<IssueBrief>> {
        const SQL: &str = "SELECT id, status, status_name, assignee_type, assignee_id, \
                                  triage_state, project_id, parent_issue_id, identifier, title \
                           FROM issue WHERE workspace_id = $1 \
                             AND (id = $2::uuid OR identifier = $3) LIMIT 1";
        let needle = raw.trim();
        sqlx::query_as::<_, IssueBrief>(SQL)
            .bind(workspace_id.0)
            .bind(Uuid::parse_str(needle).ok())
            .bind(needle.to_uppercase())
            .fetch_optional(self.pool())
            .await
            .map_err(crate::workspace::map_sqlx_err)
    }

    /// agent 的最小投影（preview-trigger 判定「有没有 runtime / 是否归档」）。
    ///
    /// **不含** `kind`：preview 的私有 agent 门槛属于 M3-5（LUM-1428），本切片
    /// 不复制那份判定（见 `docs/41` 的差异清单）。
    ///
    /// # Errors
    ///
    /// 同 [`TaskRepo::task_in_workspace`]。
    pub async fn agent_brief(&self, agent_id: Id) -> crate::Result<Option<AgentBrief>> {
        const SQL: &str = "SELECT id, runtime_id, archived_at, visibility FROM agent WHERE id = $1";
        sqlx::query_as::<_, AgentBrief>(SQL)
            .bind(agent_id.0)
            .fetch_optional(self.pool())
            .await
            .map_err(crate::workspace::map_sqlx_err)
    }

    // -----------------------------------------------------------------------
    // client usage / quick-create 重试
    // -----------------------------------------------------------------------

    /// 客户端日活/探针上报 upsert —— 上游 `UpsertClientUsageDaily`
    /// （`client_usage.sql:1`，`client_usage.go:49`）。
    ///
    /// `activity_date` 由 SQL 现算（`CURRENT_TIMESTAMP AT TIME ZONE 'UTC'::date`），
    /// 与上游一致 —— 客户端时钟不参与分桶。`has_runtime_probe` 决定探针列是
    /// 「本次写入」还是「保留旧值」（`CASE WHEN` 的两个分支在 SQL 里各出现一次）。
    ///
    /// # Errors
    ///
    /// 同 [`TaskRepo::task_in_workspace`]。
    pub async fn upsert_client_usage(&self, payload: &ClientUsageUpsert) -> crate::Result<()> {
        const SQL: &str = "\
INSERT INTO client_usage_daily (
    user_id, client_type, install_id, activity_date, workspace_id, client_version, os,
    first_active_at, last_active_at, runtime_probed_at, probe_result, runtime_count,
    provider_summary, online_count, offline_count
) VALUES (
    $1, $2, $3, (CURRENT_TIMESTAMP AT TIME ZONE 'UTC')::date, $4, $5, $6,
    CURRENT_TIMESTAMP, CURRENT_TIMESTAMP,
    CASE WHEN $7::boolean THEN CURRENT_TIMESTAMP ELSE NULL END,
    $8, $9, $10, $11, $12
)
ON CONFLICT (user_id, client_type, install_id, activity_date) DO UPDATE SET
    workspace_id = COALESCE(EXCLUDED.workspace_id, client_usage_daily.workspace_id),
    client_version = EXCLUDED.client_version,
    os = EXCLUDED.os,
    last_active_at = EXCLUDED.last_active_at,
    runtime_probed_at = CASE WHEN $7::boolean THEN EXCLUDED.runtime_probed_at ELSE client_usage_daily.runtime_probed_at END,
    probe_result = CASE WHEN $7::boolean THEN EXCLUDED.probe_result ELSE client_usage_daily.probe_result END,
    runtime_count = CASE WHEN $7::boolean THEN EXCLUDED.runtime_count ELSE client_usage_daily.runtime_count END,
    provider_summary = CASE WHEN $7::boolean THEN EXCLUDED.provider_summary ELSE client_usage_daily.provider_summary END,
    online_count = CASE WHEN $7::boolean THEN EXCLUDED.online_count ELSE client_usage_daily.online_count END,
    offline_count = CASE WHEN $7::boolean THEN EXCLUDED.offline_count ELSE client_usage_daily.offline_count END,
    updated_at = CURRENT_TIMESTAMP";
        let has_probe = payload.runtime_probed_at.is_some();
        sqlx::query(SQL)
            .bind(payload.user_id.0)
            .bind(&payload.client_type)
            .bind(payload.install_id)
            .bind(payload.workspace_id.map(|id| id.0))
            .bind(&payload.client_version)
            .bind(&payload.os)
            .bind(has_probe)
            .bind(payload.probe_result.clone())
            .bind(payload.runtime_count)
            .bind(payload.provider_summary.clone())
            .bind(payload.online_count)
            .bind(payload.offline_count)
            .execute(self.pool())
            .await
            .map_err(crate::workspace::map_sqlx_err)?;
        Ok(())
    }

    /// 人工重试一个 issue-less quick-create —— 上游 `CreateManualQuickCreateRetryTask`
    /// （`agent.sql:573`）+ `TransferPendingIssueSourceContextTask`（`source_context.sql:44`）。
    ///
    /// 同一个事务：先把 pending 的 source context 转到新任务上，再按父行复制出
    /// 一条 `direct_human` / `rerun_of_task_id` 的新任务。源任务必须是
    /// `failed` 且**没有任何 source FK**（issue / chat / autopilot 三者皆空）——
    /// 那正是「quick-create 还没有落地成 issue」的定义。
    ///
    /// 返回 `None` 表示源任务不满足条件（路由层映射 404/400），而不是错误。
    ///
    /// # Errors
    ///
    /// 同 [`TaskRepo::task_in_workspace`]。
    pub async fn create_quick_create_retry(
        &self,
        workspace_id: Id,
        source_task_id: Id,
        actor_user_id: Id,
    ) -> crate::Result<Option<TaskRow>> {
        const INSERT: &str = "\
WITH inserted AS (
INSERT INTO agent_task_queue (
    agent_id, runtime_id, status, priority, context,
    force_fresh_session, is_leader_task, squad_id,
    originator_user_id, accountable_user_id,
    runtime_mcp_overlay, runtime_connected_apps,
    originator_source, rerun_of_task_id, id
)
SELECT
    p.agent_id, p.runtime_id, 'queued', p.priority, p.context,
    TRUE, p.is_leader_task, p.squad_id,
    $1, $1,
    p.runtime_mcp_overlay, p.runtime_connected_apps,
    'direct_human', p.id, $2
FROM agent_task_queue p
JOIN agent a ON a.id = p.agent_id
WHERE p.id = $3
  AND a.workspace_id = $4
  AND p.status = 'failed'
  AND p.issue_id IS NULL
  AND p.chat_session_id IS NULL
  AND p.autopilot_run_id IS NULL
RETURNING *
)
SELECT {TASK_COLUMNS} FROM inserted atq";
        let new_task_id = Uuid::now_v7();
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(crate::workspace::map_sqlx_err)?;

        // 先转移 pending 的 source context，它才认得出新任务的来源。
        sqlx::query(
            "UPDATE issue_source_context SET origin_task_id = $1 \
             WHERE workspace_id = $2 AND origin_task_id = $3 AND state = 'pending'",
        )
        .bind(new_task_id)
        .bind(workspace_id.0)
        .bind(source_task_id.0)
        .execute(&mut *tx)
        .await
        .map_err(crate::workspace::map_sqlx_err)?;

        let sql = INSERT.replace("{TASK_COLUMNS}", TASK_COLUMNS);
        let row = sqlx::query_as::<_, TaskRow>(&sql)
            .bind(actor_user_id.0)
            .bind(new_task_id)
            .bind(source_task_id.0)
            .bind(workspace_id.0)
            .fetch_optional(&mut *tx)
            .await
            .map_err(crate::workspace::map_sqlx_err)?;
        if let Some(row) = row {
            tx.commit().await.map_err(crate::workspace::map_sqlx_err)?;
            Ok(Some(row))
        } else {
            tx.rollback()
                .await
                .map_err(crate::workspace::map_sqlx_err)?;
            Ok(None)
        }
    }

    /// 该 agent 在 issue 上是否已有待运行的（未领取的）计划 —— 上游
    /// `HasPendingTaskForIssueAndAgent`（`agent.sql:1792`）。
    ///
    /// 本仓不发 `head_sha`（issue 与 PR 的关联面尚未落地），因此落到上游的
    /// `COALESCE(head_sha,'') = ''` 分支：只按 `(issue_id, agent_id)` 去重，且
    /// **排除** wakeup 触发（`context->>'wakeup_id' IS NOT NULL`）与已领取/终态的行。
    ///
    /// # Errors
    ///
    /// 同 [`TaskRepo::task_in_workspace`]。
    pub async fn has_pending_task_for_issue_agent(
        &self,
        issue_id: Id,
        agent_id: Id,
    ) -> crate::Result<bool> {
        const SQL: &str = "\
SELECT count(*) > 0 AS has_pending FROM agent_task_queue
WHERE context->>'wakeup_id' IS NULL AND issue_id = $1 AND agent_id = $2
  AND (
    status IN ('queued', 'dispatched')
    OR (status = 'deferred' AND context->>'channel_issue_media_pending' = 'true')
  )";
        sqlx::query_scalar::<_, bool>(SQL)
            .bind(issue_id.0)
            .bind(agent_id.0)
            .fetch_one(self.pool())
            .await
            .map_err(crate::workspace::map_sqlx_err)
    }

    /// 清掉 `(issue, agent)` 在某条评论线程里的待运行计划 —— 上游
    /// `CancelPendingTasksByIssueAndAgentInThread`（`agent.sql:654`）。
    ///
    /// 只动**还没开始**的那批（`queued` / `dispatched` / `deferred`）：正在
    /// 跑的任务属于 `CancelTask` 的职权，重跑不该悄悄杀掉它在做的那一遍。
    /// 线程归属走 `comment_thread_root_id()`（迁移 `451`）——`thread_comment_id`
    /// 为空时 `IS NOT DISTINCT FROM NULL` 命中「任务级」（无线程）的那批。
    ///
    /// 取消载荷与 `Cancellation::by_system()` 逐字一致（`cancelled_by_id` 置空）。
    ///
    /// # Errors
    ///
    /// 同 [`TaskRepo::task_in_workspace`]。
    pub async fn cancel_pending_tasks_in_thread(
        &self,
        issue_id: Id,
        agent_id: Id,
        thread_comment_id: Option<Id>,
    ) -> crate::Result<Vec<TaskRow>> {
        let sql = format!(
            "UPDATE agent_task_queue atq \
             SET status = 'cancelled', completed_at = now(), prepare_lease_expires_at = NULL, \
                 cancelled_by_type = 'system', cancelled_by_id = NULL, cancelled_by_name = NULL \
             WHERE issue_id = $1 AND agent_id = $2 \
               AND status IN ('queued', 'dispatched', 'deferred') \
               AND comment_thread_id IS NOT DISTINCT FROM \
                   comment_thread_root_id($3::uuid) \
             RETURNING {TASK_COLUMNS}"
        );
        sqlx::query_as::<_, TaskRow>(&sql)
            .bind(issue_id.0)
            .bind(agent_id.0)
            .bind(thread_comment_id.map(|v| v.0))
            .fetch_all(self.pool())
            .await
            .map_err(crate::workspace::map_sqlx_err)
    }

    /// 人工重跑的入队面 —— 上游 `enqueueRerunTask`（`task.go:5849`）里
    /// `CreateAgentTask` 的**可落地子集**（见 `docs/41` §5 的偏离登记）。
    ///
    /// 与 [`TaskRepo::create_quick_create_retry`] 一样，归因列
    /// （`originator_user_id` / `accountable_user_id`）直接写入，两者同值
    /// 以满足 `agent_task_queue_accountable_matches_originator`。
    /// `runtime_id` **必须**非空：`agent_task_queue_active_requires_runtime`
    /// （`agent_task_queue` 的 CHECK）不允许一条没有 runtime 的在飞行；
    /// 调用方先把「目标没有 runtime」判成 403。
    ///
    /// 唯一索引 `idx_one_pending_task_per_issue_agent_thread` 冲突时返回
    /// [`RepoError::Conflict`]，调用方清一次槽位后重试。
    ///
    /// # Errors
    ///
    /// 同 [`TaskRepo::task_in_workspace`]。
    pub async fn enqueue_rerun_task(&self, spec: &RerunTaskSpec) -> crate::Result<TaskRow> {
        let sql = format!(
            "INSERT INTO agent_task_queue AS atq (\
                id, agent_id, issue_id, runtime_id, status, priority, trigger_comment_id, \
                is_leader_task, force_fresh_session, rerun_of_task_id, originator_source, \
                originator_user_id, accountable_user_id, attempt, max_attempts) \
             VALUES ($1, $2, $3, $4, 'queued', $5, $6, $7, TRUE, $8, 'direct_human', \
                     $9, $9, $10, $11) \
             RETURNING {TASK_COLUMNS}"
        );
        sqlx::query_as::<_, TaskRow>(&sql)
            .bind(spec.id.0)
            .bind(spec.agent_id.0)
            .bind(spec.issue_id.0)
            .bind(spec.runtime_id.0)
            .bind(spec.priority)
            .bind(spec.trigger_comment_id.map(|v| v.0))
            .bind(spec.is_leader_task)
            .bind(spec.rerun_of_task_id.map(|v| v.0))
            .bind(spec.actor_user_id.0)
            .bind(RetryBudget::FIRST_RUN.attempt.cast_signed())
            .bind(RetryBudget::FIRST_RUN.max_attempts.cast_signed())
            .fetch_one(self.pool())
            .await
            .map_err(crate::workspace::map_sqlx_err)
    }

    /// 会话的鉴权子集（`POST /api/tasks/:taskId/cancel` 的 `chat_session` 归属判定）——
    /// 上游 `CancelTaskByUser`（`chat.go:1780`）先确认取消者就是会话发起人。
    ///
    /// # Errors
    ///
    /// 同 [`TaskRepo::task_in_workspace`]。
    pub async fn chat_session_in_workspace(
        &self,
        session_id: Id,
        workspace_id: Id,
    ) -> crate::Result<Option<ChatSessionRow>> {
        sqlx::query_as::<_, ChatSessionRow>(
            "SELECT id, workspace_id, agent_id, creator_id, status FROM chat_session \
             WHERE id = $1 AND workspace_id = $2",
        )
        .bind(session_id.0)
        .bind(workspace_id.0)
        .fetch_optional(self.pool())
        .await
        .map_err(crate::workspace::map_sqlx_err)
    }

    /// `task_message` 的最大 `seq`（重试/重跑时给前端一个游标位）。
    ///
    /// # Errors
    ///
    /// 同 [`TaskRepo::task_in_workspace`]。
    pub async fn max_task_message_seq(&self, task_id: Id) -> crate::Result<Option<i32>> {
        sqlx::query_scalar::<_, Option<i32>>("SELECT MAX(seq) FROM task_message WHERE task_id = $1")
            .bind(task_id.0)
            .fetch_one(self.pool())
            .await
            .map_err(|e| RepoError::Db(e.to_string()))
    }

    /// 共用的一行取回（`TASK_COLUMNS` + 单个 `$1` 谓词）。
    async fn fetch_rows(&self, sql: &str, issue_id: Id) -> crate::Result<Vec<TaskRow>> {
        sqlx::query_as::<_, TaskRow>(sql)
            .bind(issue_id.0)
            .fetch_all(self.pool())
            .await
            .map_err(crate::workspace::map_sqlx_err)
    }
}
