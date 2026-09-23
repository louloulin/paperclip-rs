//! task / agent-builder 仓储（M3-6 / W3b，`feat/multica-rs-m3b-task-queue`）。
//!
//! 覆盖 docs/15 §1.4 的 4 条 agent-builder 路由与 §1.6 的 11 条
//! task / lifecycle / usage / retry 路由所需的**全部**数据面，并兑现 M3-3（`mc-task`）
//! 给出的 [`TaskStore`](mc_task::store::TaskStore) port。
//!
//! 上游对应关系（`server/pkg/db/queries` + `server/internal/handler`）：
//!
//! | 本模块 | 上游 |
//! |---|---|
//! | [`TaskRepo`] + [`TaskStore`](mc_task::store::TaskStore) 实现（`store.rs`） | `agent.sql` 的 `CreateAgentTask` / `GetAgentTask` / `ClaimAgentTask` / `CancelAgentTask*` / `UpsertTaskUsage` |
//! | [`TaskRepo::list_active_tasks_by_issue`] | `agent.sql` `ListActiveTasksByIssue`（`daemon.go:5394`） |
//! | [`TaskRepo::list_tasks_by_issue`] | `agent.sql:2773` `ListTasksByIssue`（`daemon.go:5519`） |
//! | [`TaskRepo::list_task_messages`] | `task_message.sql` `ListTaskMessages{,Since}`（`daemon.go:5724`） |
//! | [`TaskRepo::issue_usage_summary`] | `task_usage.sql:72` `GetIssueUsageSummary`（`daemon.go:5792`） |
//! | [`TaskRepo::list_working_agents`] | `agent.sql:6518` `ListWorkspaceWorkingAgents`（`agent.go:2766`） |
//! | `queries.rs` 的 agent-builder 三件套 | `chat.sql:109` / `agent_builder.sql:6` / `agent.sql:72,95` |
//! | [`TaskRepo::upsert_client_usage`] | `client_usage.sql` `UpsertClientUsageDaily`（`client_usage.go:49`） |
//! | [`TaskRepo::create_quick_create_retry`] | `agent.sql:573` `CreateManualQuickCreateRetryTask` + `source_context.sql:26` |
//!
//! # 约定
//!
//! 与 M1/M2 各 Repo 一致：
//! - `Row` 用裸 `Uuid` / `String` / `DateTime<Utc>` 字段 + `Id` 访问器，手写
//!   `sqlx::FromRow`（`mc_core::Id` 没有 sqlx impl）
//! - 错误统一走 [`crate::workspace::map_sqlx_err`]
//! - 运行时 sqlx builder + 参数绑定（不用 compile-time 宏 ⇒ 构建期不需要数据库）
//! - Pg 实现 + `#[ignore]` 的 PG 集成测试（`MULTICA_TEST_DATABASE_URL`）
//!
//! # 上游硬约束
//!
//! docs/15 §2.2 列出的 7 个自造列（`retry_count` / `source_task_id` / `session_id` /
//! `lease_expires_at` / …）**一律不用**；真值是上游迁移里的列。
//!
//! ⚠️ **索引名更正**：docs/15 §2.3、`mc-task/src/store.rs` 的文档都写「部分唯一索引
//! `idx_one_pending_task_per_issue`」。该索引**已被上游迁移 `037` 删除**；当前存活的是
//! `452_agent_task_pending_thread_unique.up.sql` 建出的
//! `idx_one_pending_task_per_issue_agent_thread`
//! （键 = `(issue_id, agent_id, COALESCE(comment_thread_id, '00000000-…'))`，
//! `WHERE status IN ('queued','dispatched') OR (status='deferred' AND
//! context->>'channel_issue_media_pending' = 'true')`）。
//! 本模块的 DB 测试**真实触发**的是这个存活索引（见 `tests.rs` 的
//! `insert_conflicts_on_live_thread_partial_unique_index`）。
//!
//! # 文件布局（R7：单文件 800 行硬上限，`scripts/file_size_check.py` + 门 ⑩）
//!
//! - `mod.rs`（本文件）：模块文档 + 常量 + [`TaskRepo`] + 构造器
//! - `row.rs`：`Row` 类型 + `TaskState` ↔ row 映射 + 路线面记录类型
//! - `store.rs`：[`mc_task::store::TaskStore`] 的 Pg 实现（CAS / claim 栅栏 / 取消 / usage）
//! - `queries.rs`：路线面查询（active task / task runs / messages / usage / working agents /
//!   client-usage / quick-create 重试）
//! - `builder.rs`：agent-builder 写路径（从 `queries.rs` 拆出以守住 800 行上限）
//! - `tests.rs`：PG 集成测试（`#[ignore]`）

use sqlx::PgPool;

use mc_core::Id;
use mc_db::Db;

pub(crate) mod builder;
pub(crate) mod queries;
pub(crate) mod row;
pub(crate) mod store;
pub use store::{NewTask, PgTaskStore};

#[cfg(test)]
mod tests;

pub use builder::{CreatedBuilderSession, NewBuilderSession};
pub use queries::WorkingAgentFilter;
// `mc-task` 的取消载荷 / 结果与领域错误**从本模块再导出**：路由层要构造一次人工取消
// （`Cancellation::by_user`）并把 `TaskError` 映射成 HTTP 错误，让它直接依赖数据层
// 拥有的 port 类型，而不是在 `mc-http` 上再加一条 `mc-task` 依赖边（`docs/41` §5）。
pub use mc_task::cancel::{CancelOutcome, Cancellation};
pub use mc_task::error::TaskError;
pub use mc_task::state::CancelledBy;
pub use row::{
    AgentBrief, AgentRuntimeRow, BuilderSessionRow, ChatSessionRow, ClientUsageUpsert,
    FamilyActiveTaskRow, IssueBrief, IssueUsageSummaryRow, RerunTaskSpec, SaveDraftOutcome,
    SwitchRuntimeOutcome, TaskMessageRow, TaskRow, WorkingAgentRow,
};

/// agent-builder 隐藏执行载体的 `system_key` 前缀（上游 `agent_builder.go` /
/// `runtime_blocking_agents.go:14` 的 `agentBuilderSystemKeyPrefix`）。
pub const AGENT_BUILDER_SYSTEM_KEY_PREFIX: &str = "agent_builder:";

/// agent-builder 会话默认标题（上游 `CreateAgentBuilderSession` 的字面量）。
pub const AGENT_BUILDER_SESSION_TITLE: &str = "Create an agent";

/// agent-builder 隐藏载体的指令（逐字取自上游 `agent_builder.go:19` 的
/// `agentBuilderInstructions`）。协议由 studio 与这段提示词共同定义，服务端不解析。
pub const AGENT_BUILDER_INSTRUCTIONS: &str = "\
You are Multica Agent Builder. Help the user design one practical AI agent through a short conversation.

Your job is to propose and refine configuration, never to create resources yourself. Ask only questions that materially change behavior. Prefer making a reasonable draft immediately, then ask at most two focused questions per turn.

Every response MUST end with exactly one <agent_draft> JSON block using this shape:
<agent_draft>{\"name\":\"\",\"description\":\"\",\"instructions\":\"\",\"conversation_starters\":[],\"model\":\"\",\"skill_ids\":[],\"permission_scope\":\"private\",\"member_ids\":[]}</agent_draft>

Rules:
- The JSON must be valid, compact JSON on one physical line. Do not wrap it in Markdown fences.
- Escape every line break inside instructions as \\n. Never place a literal newline inside a JSON string.
- Preserve good existing draft fields supplied in the user's message unless the user asks to change them.
- name is concise and suitable for a workspace list.
- description is one sentence, at most 200 characters.
- instructions are a complete Markdown system prompt describing role, workflow, output, and constraints.
- conversation_starters contains up to three objects with a concise label and a complete prompt. Each should demonstrate a useful first task for this specific agent; never include generic filler.
- model must be empty, preserve current_draft.model, or exactly match an id explicitly listed in AVAILABLE RUNTIME MODELS. Never use a model label as the id.
- When AVAILABLE RUNTIME MODELS is null or empty, preserve current_draft.model and never invent a model id.
- skill_ids may only contain IDs explicitly listed in AVAILABLE WORKSPACE SKILLS.
- permission_scope must be private, workspace, or members. Default to private unless the user explicitly requests sharing.
- member_ids may only contain IDs explicitly listed in AVAILABLE WORKSPACE MEMBERS, and only when permission_scope is members.
- Never request, expose, or place secrets, tokens, passwords, or environment-variable values in the draft.
- Do not claim that the agent has been created. The user must review and confirm the draft in the UI.";

/// `/api/issues/:id/task-runs` 单页上限（上游 `ListTasksByIssue` 的 `LIMIT` 由客户端传，
/// handler 收敛到 200）。
pub const TASK_RUNS_MAX_LIMIT: i64 = 200;
/// `/api/issues/:id/task-runs` 默认页大小。
pub const TASK_RUNS_DEFAULT_LIMIT: i64 = 50;

/// `agent_task_queue` 的投影列（带 `atq` 别名，便于与 `agent` 做 join）。
///
/// 列清单逐字取自 `contracts/upstream-schema.sql:995` 的 `CREATE TABLE`，
/// **不含**代理键之外的任何本仓自造列。
pub(crate) const TASK_COLUMNS: &str = "atq.id, atq.agent_id, atq.issue_id, atq.status, \
     atq.priority, atq.dispatched_at, atq.started_at, atq.completed_at, atq.result, atq.error, \
     atq.created_at, atq.context, atq.runtime_id, atq.work_dir, atq.trigger_comment_id, \
     atq.chat_session_id, atq.autopilot_run_id, atq.attempt, atq.max_attempts, \
     atq.parent_task_id, atq.failure_reason, atq.trigger_summary, atq.is_leader_task, \
     atq.wait_reason, atq.handoff_note, atq.prepare_lease_expires_at, atq.escalation_for_task_id, \
     atq.fire_at, atq.coalesced_comment_ids, atq.delivered_comment_ids, \
     atq.delegated_from_task_id, atq.retry_of_task_id, atq.rerun_of_task_id, atq.branch_name, \
     atq.durable_work_dir, atq.comment_thread_id, atq.cancelled_by_type, atq.cancelled_by_id, \
     atq.cancelled_by_name, atq.force_fresh_session";

/// task 仓储。
pub struct TaskRepo {
    pool: PgPool,
}

impl TaskRepo {
    /// 从应用共享 `Db` 句柄构造。
    #[must_use]
    pub fn new(db: &Db) -> Self {
        Self {
            pool: db.pool().clone(),
        }
    }

    /// 用自定义 pool 构造（集成测试用）。
    #[must_use]
    pub fn with_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// 底层连接池。
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// 该 task 是否属于给定 workspace —— 上游 `GetAgentTaskInWorkspace`（`agent.sql:731`）。
    ///
    /// tenancy 一律走 agent join：每条 `agent_task_queue` 都有 NOT NULL `agent_id`
    /// （`ON DELETE CASCADE`），agent 才是 workspace 作用域的，因此这条谓词对
    /// issue 任务 / chat 任务 / quick-create 任务（`issue_id IS NULL`）都成立。
    ///
    /// # Errors
    ///
    /// DB 错误统一映射为 [`crate::RepoError`]。
    pub async fn task_in_workspace(
        &self,
        task_id: Id,
        workspace_id: Id,
    ) -> crate::Result<Option<TaskRow>> {
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM agent_task_queue atq \
             JOIN agent a ON a.id = atq.agent_id \
             WHERE atq.id = $1 AND a.workspace_id = $2"
        );
        let row = sqlx::query_as::<_, TaskRow>(&sql)
            .bind(task_id.0)
            .bind(workspace_id.0)
            .fetch_optional(&self.pool)
            .await
            .map_err(crate::workspace::map_sqlx_err)?;
        Ok(row)
    }

    /// 该 task 是否属于给定 issue（`GET /api/issues/:id/tasks/:taskId/cancel` 的存在性判定）。
    ///
    /// # Errors
    ///
    /// DB 错误统一映射为 [`crate::RepoError`]。
    pub async fn task_for_issue(
        &self,
        task_id: Id,
        issue_id: Id,
    ) -> crate::Result<Option<TaskRow>> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM agent_task_queue atq WHERE atq.id = $1 AND atq.issue_id = $2");
        let row = sqlx::query_as::<_, TaskRow>(&sql)
            .bind(task_id.0)
            .bind(issue_id.0)
            .fetch_optional(&self.pool)
            .await
            .map_err(crate::workspace::map_sqlx_err)?;
        Ok(row)
    }
}
