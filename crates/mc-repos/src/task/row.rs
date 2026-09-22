//! `task` 仓储的行类型与 `TaskState` 映射（W3b / M3-6）。
//!
//! 全部字段都是**原始** SQL 类型（`Uuid` / `String` / `i32` / `DateTime<Utc>` /
//! `serde_json::Value`），因为 `mc_core::Id` 没有 sqlx `Decode`/`Encode` 实现。
//! 领域类型（`Id` / `TaskStatus` / `Failure`）通过访问器暴露。

use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

use mc_core::{Id, Timestamp};
use mc_task::retry::FailureReason;
use mc_task::retry::RetryBudget;
use mc_task::state::{CancelledBy, Failure, TaskState};
use mc_task::status::TaskStatus;

use crate::RepoError;

/// `agent_task_queue` 一行（`mod.rs` 的 [`super::TASK_COLUMNS`] 投影）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TaskRow {
    /// `id`。
    pub id: Uuid,
    /// `agent_id`。
    pub agent_id: Uuid,
    /// `issue_id`（chat / quick-create 任务为空）。
    pub issue_id: Option<Uuid>,
    /// `status`。
    pub status: String,
    /// `priority`。
    pub priority: i32,
    /// `dispatched_at`。
    pub dispatched_at: Option<DateTime<Utc>>,
    /// `started_at`。
    pub started_at: Option<DateTime<Utc>>,
    /// `completed_at`。
    pub completed_at: Option<DateTime<Utc>>,
    /// `result`。
    pub result: Option<Value>,
    /// `error`。
    pub error: Option<String>,
    /// `created_at`。
    pub created_at: DateTime<Utc>,
    /// `context`。
    pub context: Option<Value>,
    /// `runtime_id`。
    pub runtime_id: Option<Uuid>,
    /// `work_dir`。
    pub work_dir: Option<String>,
    /// `trigger_comment_id`。
    pub trigger_comment_id: Option<Uuid>,
    /// `chat_session_id`。
    pub chat_session_id: Option<Uuid>,
    /// `autopilot_run_id`。
    pub autopilot_run_id: Option<Uuid>,
    /// `attempt`。
    pub attempt: i32,
    /// `max_attempts`。
    pub max_attempts: i32,
    /// `parent_task_id`。
    pub parent_task_id: Option<Uuid>,
    /// `failure_reason`。
    pub failure_reason: Option<String>,
    /// `trigger_summary`。
    pub trigger_summary: Option<String>,
    /// `is_leader_task`。
    pub is_leader_task: bool,
    /// `wait_reason`。
    pub wait_reason: Option<String>,
    /// `handoff_note`。
    pub handoff_note: Option<String>,
    /// `prepare_lease_expires_at`。
    pub prepare_lease_expires_at: Option<DateTime<Utc>>,
    /// `escalation_for_task_id`。
    pub escalation_for_task_id: Option<Uuid>,
    /// `fire_at`。
    pub fire_at: Option<DateTime<Utc>>,
    /// `coalesced_comment_ids`。
    pub coalesced_comment_ids: Vec<Uuid>,
    /// `delivered_comment_ids`。
    pub delivered_comment_ids: Vec<Uuid>,
    /// `delegated_from_task_id`。
    pub delegated_from_task_id: Option<Uuid>,
    /// `retry_of_task_id`。
    pub retry_of_task_id: Option<Uuid>,
    /// `rerun_of_task_id`。
    pub rerun_of_task_id: Option<Uuid>,
    /// `branch_name`。
    pub branch_name: Option<String>,
    /// `durable_work_dir`。
    pub durable_work_dir: Option<String>,
    /// `comment_thread_id`。
    pub comment_thread_id: Option<Uuid>,
    /// `cancelled_by_type`。
    pub cancelled_by_type: Option<String>,
    /// `cancelled_by_id`。
    pub cancelled_by_id: Option<Uuid>,
    /// `cancelled_by_name`。
    pub cancelled_by_name: Option<String>,
    /// `force_fresh_session`。
    pub force_fresh_session: bool,
}

impl TaskRow {
    /// 领域 id。
    #[must_use]
    pub fn id(&self) -> Id {
        Id::from(self.id)
    }

    /// `agent_id` 的领域 id。
    #[must_use]
    pub fn agent_id(&self) -> Id {
        Id::from(self.agent_id)
    }

    /// `issue_id` 的领域 id。
    #[must_use]
    pub fn issue_id(&self) -> Option<Id> {
        self.issue_id.map(Id::from)
    }

    /// 解析后的状态。
    ///
    /// # Errors
    ///
    /// `status` 不在 8 个上游取值内时返回 [`RepoError::Db`]
    /// （列有 CHECK 约束，出现即说明库被绕过写入）。
    pub fn task_status(&self) -> Result<TaskStatus, RepoError> {
        TaskStatus::parse(&self.status).map_err(|e| RepoError::Db(e.to_string()))
    }

    /// 映射成 `mc-task` 的领域状态（[`mc_task::store::TaskStore`] 的读写单元）。
    ///
    /// # Errors
    ///
    /// 同 [`TaskRow::task_status`]。
    pub fn to_task_state(&self) -> Result<TaskState, RepoError> {
        let status = self.task_status()?;
        let failure = match (&self.failure_reason, status) {
            (Some(reason), _) => Some(Failure {
                reason: FailureReason::parse(reason).map_err(|e| RepoError::Db(e.to_string()))?,
                message: self.error.clone(),
            }),
            (None, _) => None,
        };
        // `attempt` / `max_attempts` 有 CHECK 约束（>= 1）；负数只可能来自被绕过写入。
        let attempt = u32::try_from(self.attempt).map_err(|_| {
            RepoError::Db(format!("agent_task_queue.attempt 非法：{}", self.attempt))
        })?;
        let max_attempts = u32::try_from(self.max_attempts).map_err(|_| {
            RepoError::Db(format!(
                "agent_task_queue.max_attempts 非法：{}",
                self.max_attempts
            ))
        })?;
        Ok(TaskState {
            status,
            budget: RetryBudget::new(attempt, max_attempts),
            parent_task_id: self.parent_task_id.map(Id::from),
            failure,
            wait_reason: self.wait_reason.clone(),
            fire_at: self.fire_at.map(Timestamp::from),
            dispatched_at: self.dispatched_at.map(Timestamp::from),
            started_at: self.started_at.map(Timestamp::from),
            completed_at: self.completed_at.map(Timestamp::from),
            prepare_lease_expires_at: self.prepare_lease_expires_at.map(Timestamp::from),
            runtime_id: self.runtime_id.map(Id::from),
            delegated_from_task_id: self.delegated_from_task_id.map(Id::from),
            escalation_for_task_id: self.escalation_for_task_id.map(Id::from),
            cancelled_by: cancelled_by_from_columns(
                self.cancelled_by_type.as_deref(),
                self.cancelled_by_id,
                self.cancelled_by_name.clone(),
            ),
        })
    }
}

/// `cancelled_by_*` 三列 → [`CancelledBy`]。
///
/// 三列都为空（非取消行）返回 `None`；`type` 是 `'user'` ⇒
/// [`CancelledBy::User`]，其余（含未知值）⇒ [`CancelledBy::System`]
/// —— 与上游只按 `'user'` 分支的形状一致。
fn cancelled_by_from_columns(
    by_type: Option<&str>,
    id: Option<Uuid>,
    name: Option<String>,
) -> Option<CancelledBy> {
    match by_type {
        None => None,
        Some("user") => Some(CancelledBy::User {
            id: id.map(Id::from),
            name,
        }),
        Some(_) => Some(CancelledBy::System),
    }
}

/// `task_message` 一行（`task_message.sql` 的 `SELECT *`，7 列 + 2 个可选列）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TaskMessageRow {
    /// `id`。
    pub id: Uuid,
    /// `task_id`。
    pub task_id: Uuid,
    /// `seq`。
    pub seq: i32,
    /// `type`。
    pub r#type: String,
    /// `tool`。
    pub tool: Option<String>,
    /// `content`。
    pub content: Option<String>,
    /// `input`。
    pub input: Option<Value>,
    /// `output`。
    pub output: Option<String>,
    /// `created_at`。
    pub created_at: DateTime<Utc>,
    /// `output_truncated`。
    pub output_truncated: Option<bool>,
    /// `call_id`。
    pub call_id: Option<String>,
}

/// `GetIssueUsageSummary` 的结果（`task_usage.sql:72`）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct IssueUsageSummaryRow {
    /// `total_input_tokens`。
    pub total_input_tokens: i64,
    /// `total_output_tokens`。
    pub total_output_tokens: i64,
    /// `total_cache_read_tokens`。
    pub total_cache_read_tokens: i64,
    /// `total_cache_write_tokens`。
    pub total_cache_write_tokens: i64,
    /// `total_cost_usd_ticks`。
    pub total_cost_usd_ticks: i64,
    /// 未定价任务的 input token（`cost_usd_ticks IS NULL`）。
    pub uncosted_input_tokens: i64,
    /// 未定价任务的 output token。
    pub uncosted_output_tokens: i64,
    /// 未定价任务的 cache read token。
    pub uncosted_cache_read_tokens: i64,
    /// 未定价任务的 cache write token。
    pub uncosted_cache_write_tokens: i64,
    /// 有 usage 行的任务数（legacy `task_count`）。
    pub task_count: i32,
    /// 有限终态运行数（`completed`/`failed`/`cancelled` 且 `started_at`/`completed_at` 均非空）。
    pub terminal_task_count: i32,
    /// 上述运行里**有** `task_usage` 行的条数。
    pub metered_task_count: i32,
    /// `terminal_task_count - metered_task_count`。
    pub unreported_task_count: i32,
}

/// `ListWorkspaceWorkingAgents` 一行（`agent.sql:6518`）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct WorkingAgentRow {
    /// `agent_id`。
    pub id: Uuid,
    /// `name`。
    pub name: String,
    /// `avatar_url`。
    pub avatar_url: Option<String>,
    /// `running_task_count`。
    pub running_task_count: i64,
    /// `issue_ids`（该 agent 正在处理、且与筛选条件相符的 issue）。
    pub issue_ids: Vec<Uuid>,
}

/// `ListAgentBuilderSessionsByCreator` 一行（`chat.sql:109`）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct BuilderSessionRow {
    /// `chat_session.id`。
    pub id: Uuid,
    /// `chat_session.title`。
    pub title: String,
    /// `chat_session.created_at`。
    pub created_at: DateTime<Utc>,
    /// `chat_session.updated_at`。
    pub updated_at: DateTime<Utc>,
    /// **载体 agent** 的 `runtime_id`（不是 `chat_session.runtime_id`，见上游注释）。
    pub runtime_id: Option<Uuid>,
    /// 最后一条消息内容（`COALESCE(lm.content, '')`）。
    pub last_message_content: String,
    /// 最后一条消息角色（`COALESCE(lm.role, '')`）。
    pub last_message_role: String,
    /// 最后一条消息时间（无消息时为空）。
    pub last_message_at: Option<DateTime<Utc>>,
    /// 已保存的草稿（`agent_builder_draft.draft`，无草稿时为空）。
    pub stored_draft: Option<Value>,
}

/// agent-builder 运行时解析所需的 `agent_runtime` 子集。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AgentRuntimeRow {
    /// `id`。
    pub id: Uuid,
    /// `runtime_mode`（`local` / `cloud`）。
    pub runtime_mode: String,
    /// `status`（`online` / `offline`）。
    pub status: String,
    /// `visibility`（`private` / `public`）。
    pub visibility: String,
    /// `owner_id`（上游 `canUseRuntimeForAgent` 的私有 runtime 判定；可空）。
    pub owner_id: Option<Uuid>,
}

/// preview-trigger 需要的 issue 子集（`issue.go` 的 `WillEnqueueRun` 入参形状）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct IssueBrief {
    /// `id`。
    pub id: Uuid,
    /// `status`（可能是自定义 status key）。
    pub status: String,
    /// `status_name`（展示名；仅用于调试，本模块不外传）。
    pub status_name: Option<String>,
    /// `assignee_type`。
    pub assignee_type: Option<String>,
    /// `assignee_id`。
    pub assignee_id: Option<Uuid>,
    /// `triage_state`。
    pub triage_state: Option<String>,
    /// `project_id`。
    pub project_id: Option<Uuid>,
    /// `parent_issue_id`（`task-runs?scope=family` 的族根判定）。
    pub parent_issue_id: Option<Uuid>,
    /// `identifier`。
    pub identifier: String,
    /// `title`。
    pub title: String,
}

/// preview-trigger / 重跑需要的最小 agent 子集。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AgentBrief {
    /// `id`。
    pub id: Uuid,
    /// `runtime_id`。
    pub runtime_id: Option<Uuid>,
    /// `archived_at`。
    pub archived_at: Option<DateTime<Utc>>,
    /// `visibility`（`private` / `public`）。
    pub visibility: String,
}

/// `chat_session` 的鉴权/锁定子集（agent-builder 三件套共用）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ChatSessionRow {
    /// `id`。
    pub id: Uuid,
    /// `workspace_id`。
    pub workspace_id: Uuid,
    /// `agent_id`（隐藏载体）。
    pub agent_id: Uuid,
    /// `creator_id`。
    pub creator_id: Uuid,
    /// `status`（`active` / `archived`）。
    pub status: String,
}

/// 切换 agent-builder 运行时的结果（路由层直接映射 HTTP 状态码）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwitchRuntimeOutcome {
    /// 切换成功，返回新 `runtime_id`。
    Rebound {
        /// 承载 agent 的新 `runtime_id`。
        runtime_id: Id,
    },
    /// 会话不存在，或不属于 (workspace, creator)。
    SessionNotFound,
    /// 会话的 agent 不是 `kind='system'` + `agent_builder:` 前缀的载体。
    NotBuilderCarrier,
    /// 会话已归档。
    ArchivedSession,
    /// 该会话还有在飞的任务，客户端应先停止回复。
    PendingTask,
}

/// 保存 agent-builder 草稿的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveDraftOutcome {
    /// 已写入。
    Saved,
    /// 会话不存在，或不属于 (workspace, creator)。
    SessionNotFound,
    /// 会话的 agent 不是 builder 载体。
    NotBuilderCarrier,
    /// 会话已归档。
    ArchivedSession,
}

/// `agent_task_queue_active_requires_runtime`（`contracts/upstream-schema.sql:2727`）
/// 不允许一条没有 runtime 的在飞行，因此重跑入队前先把「目标无 runtime」判成 403。
#[derive(Debug, Clone)]
pub struct RerunTaskSpec {
    /// 新任务 id（调用方生成 `Uuid::now_v7()`）。
    pub id: Id,
    /// 目标 agent。
    pub agent_id: Id,
    /// 所属 issue。
    pub issue_id: Id,
    /// 目标 agent 绑定的 runtime（必填）。
    pub runtime_id: Id,
    /// `priority`（沿用源任务或 issue 侧默认）。
    pub priority: i32,
    /// `trigger_comment_id`。
    pub trigger_comment_id: Option<Id>,
    /// `is_leader_task`。
    pub is_leader_task: bool,
    /// 重跑操作者（同时写入 originator / accountable）。
    pub actor_user_id: Id,
    /// `rerun_of_task_id`（重跑血缘）。
    pub rerun_of_task_id: Option<Id>,
}

/// `client_usage_daily` 的 upsert 载荷（`client_usage.go` 的已验证探针）。
#[derive(Debug, Clone)]
pub struct ClientUsageUpsert {
    /// `user_id`。
    pub user_id: Id,
    /// `client_type`（`web` / `desktop`）。
    pub client_type: String,
    /// `install_id`。
    pub install_id: Uuid,
    /// `workspace_id`。
    pub workspace_id: Option<Id>,
    /// `client_version`。
    pub client_version: String,
    /// `os`。
    pub os: String,
    /// `runtime_probed_at`（无探针时为空）。
    pub runtime_probed_at: Option<DateTime<Utc>>,
    /// `probe_result`（`success` / `error`）。
    pub probe_result: Option<String>,
    /// `runtime_count`。
    pub runtime_count: Option<i32>,
    /// `provider_summary`。
    pub provider_summary: Option<Value>,
    /// `online_count`。
    pub online_count: Option<i32>,
    /// `offline_count`。
    pub offline_count: Option<i32>,
}

/// `/api/issues/:id/task-runs?scope=family` 的一行 —— 上游
/// `ListActiveTasksByIssueFamily`（`agent.sql:2503`）。
///
/// 这是**协调读**，不是执行日志：只带调用者回答「族里还有谁在干活」所需的最小列，
/// `result` / `context` / `work_dir` / attribution 都不取（上游同款注释：
/// 取这些 JSONB 会让 Postgres 每个行都 detoast，付出约 5 倍字节）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct FamilyActiveTaskRow {
    /// `task_id`。
    pub task_id: Uuid,
    /// `agent_id`。
    pub agent_id: Uuid,
    /// `issue_id`。
    pub issue_id: Uuid,
    /// `status`。
    pub status: String,
    /// `created_at`。
    pub created_at: DateTime<Utc>,
    /// `started_at`。
    pub started_at: Option<DateTime<Utc>>,
    /// `workspace.issue_prefix`。
    pub issue_prefix: String,
    /// `issue.number`。
    pub issue_number: i32,
    /// `issue.title`。
    pub issue_title: String,
}
