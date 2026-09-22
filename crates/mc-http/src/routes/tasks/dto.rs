//! M3-6（LUM-1429）响应 / 请求 DTO。
//!
//! 上游 `AgentTaskResponse`（`agent.go:430-620` + `taskToResponse` `agent.go:797`）
//! 有 **60+ 字段**，其中绝大多数是 daemon claim 面（`new_comment_count`、
//! `issue_changed_fields`、`runtime_*_overlay`、chat 渠道元数据、attribution
//! 的完整水合……）。本切片只兑现**用户面**真正会读的那一子集，收窄清单见
//! `docs/41-M3-6-TASK-QUEUE.md` §5。
//!
//! 收窄原则（与 M3-5 的 `AgentTaskDto` 同款做法）：
//! - 行内已有的列**全出**（不猜用途）；
//! - 需要额外查询才能得到的水合字段（attribution 的 initiator/originator 名字、
//!   usage 明细、`relative_work_dir` 的路径裁剪）**不出**；
//! - `omitempty` 用 `Option` + `skip_serializing_if` 等价表达；上游「恒出现」的
//!   字段（`delivered_comment_ids`）保持恒出现。

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

use mc_repos::task::{BuilderSessionRow, TaskRow};

/// RFC3339（与 `agents/dto.rs` 的 `ts()`、上游 `timestampToString` 同形）。
pub(crate) fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339()
}

pub(crate) fn ts_opt(t: Option<DateTime<Utc>>) -> Option<String> {
    t.map(ts)
}

fn uuid_opt(value: Option<uuid::Uuid>) -> Option<String> {
    value.map(|v| v.to_string())
}

/// 取消者（上游 `TaskCancellationActor`，`agent.go:875`）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct CancellationActorDto {
    /// `type`（`user` / `system` / …）。
    pub r#type: String,
    /// `id`。
    pub id: String,
    /// `name`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// 用户面 task 投影（上游 `AgentTaskResponse` 的收窄子集）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct TaskDto {
    /// `id`。
    pub id: String,
    /// `agent_id`。
    pub agent_id: String,
    /// `workspace_id`（上游由 handler 注入，非行内列）。
    pub workspace_id: String,
    /// `status`。
    pub status: String,
    /// `priority`。
    pub priority: i32,
    /// `attempt`。
    pub attempt: i32,
    /// `max_attempts`。
    pub max_attempts: i32,
    /// `is_leader_task`。
    pub is_leader_task: bool,
    /// `force_fresh_session`。
    pub force_fresh_session: bool,
    /// `created_at`。
    pub created_at: String,
    /// `delivered_comment_ids`（上游**恒出现**：空数组是权威的空回执）。
    pub delivered_comment_ids: Vec<String>,
    /// `issue_id`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<String>,
    /// `runtime_id`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<String>,
    /// `parent_task_id`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<String>,
    /// `dispatched_at`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispatched_at: Option<String>,
    /// `started_at`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// `completed_at`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    /// `result`（JSONB）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// `error`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// `failure_reason`（上游把 NULL 渲染成空串，这里按 `omitempty` 省略）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    /// `work_dir`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_dir: Option<String>,
    /// `durable_work_dir`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub durable_work_dir: Option<String>,
    /// `branch_name`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_name: Option<String>,
    /// `trigger_comment_id`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_comment_id: Option<String>,
    /// `coalesced_comment_ids`。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub coalesced_comment_ids: Vec<String>,
    /// `trigger_summary`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_summary: Option<String>,
    /// `handoff_note`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff_note: Option<String>,
    /// `comment_thread_id`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment_thread_id: Option<String>,
    /// `chat_session_id`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_session_id: Option<String>,
    /// `autopilot_run_id`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autopilot_run_id: Option<String>,
    /// `wait_reason`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_reason: Option<String>,
    /// `prepare_lease_expires_at`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prepare_lease_expires_at: Option<String>,
    /// `fire_at`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fire_at: Option<String>,
    /// `escalation_for_task_id`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub escalation_for_task_id: Option<String>,
    /// 归因链（上游 `attribution.{delegated_from_task_id,retry_of_task_id,rerun_of_task_id}`；
    /// 这里平铺，因为本仓不做 attribution 水合）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delegated_from_task_id: Option<String>,
    /// `retry_of_task_id`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_of_task_id: Option<String>,
    /// `rerun_of_task_id`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rerun_of_task_id: Option<String>,
    /// `kind`（`computeTaskKind`）。
    pub kind: String,
    /// `wakeup_id`（从 `context->>'wakeup_id'` 取出）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wakeup_id: Option<String>,
    /// `cancelled_by_comment_change`（`context.comment_change_cancelled_task_id == id`）。
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub cancelled_by_comment_change: bool,
    /// `cancelled_by`（仅 `status == 'cancelled'` 且 `cancelled_by_type` 非空时出现）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancelled_by: Option<CancellationActorDto>,
}

impl TaskDto {
    /// `taskToResponse`（`agent.go:797`）的行内部分。
    pub(crate) fn from_row(row: &TaskRow, workspace_id: uuid::Uuid) -> Self {
        let id = row.id.to_string();
        Self {
            id: id.clone(),
            agent_id: row.agent_id.to_string(),
            workspace_id: workspace_id.to_string(),
            status: row.status.clone(),
            priority: row.priority,
            attempt: row.attempt,
            max_attempts: row.max_attempts,
            is_leader_task: row.is_leader_task,
            force_fresh_session: row.force_fresh_session,
            created_at: ts(row.created_at),
            delivered_comment_ids: row
                .delivered_comment_ids
                .iter()
                .map(ToString::to_string)
                .collect(),
            issue_id: uuid_opt(row.issue_id),
            runtime_id: uuid_opt(row.runtime_id),
            parent_task_id: uuid_opt(row.parent_task_id),
            dispatched_at: ts_opt(row.dispatched_at),
            started_at: ts_opt(row.started_at),
            completed_at: ts_opt(row.completed_at),
            result: row.result.clone(),
            error: row.error.clone(),
            failure_reason: row.failure_reason.clone(),
            work_dir: row.work_dir.clone(),
            durable_work_dir: row.durable_work_dir.clone(),
            branch_name: row.branch_name.clone(),
            trigger_comment_id: uuid_opt(row.trigger_comment_id),
            coalesced_comment_ids: row
                .coalesced_comment_ids
                .iter()
                .map(ToString::to_string)
                .collect(),
            trigger_summary: row.trigger_summary.clone(),
            handoff_note: row.handoff_note.clone(),
            comment_thread_id: uuid_opt(row.comment_thread_id),
            chat_session_id: uuid_opt(row.chat_session_id),
            autopilot_run_id: uuid_opt(row.autopilot_run_id),
            wait_reason: row.wait_reason.clone(),
            prepare_lease_expires_at: ts_opt(row.prepare_lease_expires_at),
            fire_at: ts_opt(row.fire_at),
            escalation_for_task_id: uuid_opt(row.escalation_for_task_id),
            delegated_from_task_id: uuid_opt(row.delegated_from_task_id),
            retry_of_task_id: uuid_opt(row.retry_of_task_id),
            rerun_of_task_id: uuid_opt(row.rerun_of_task_id),
            kind: task_kind(row),
            wakeup_id: context_str(row, "wakeup_id"),
            cancelled_by_comment_change: row.status == "cancelled"
                && context_str(row, "comment_change_cancelled_task_id").as_deref() == Some(&id),
            cancelled_by: cancellation_actor(row),
        }
    }
}

fn context_str(row: &TaskRow, key: &str) -> Option<String> {
    row.context
        .as_ref()
        .and_then(|v| v.get(key))
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

/// 上游 `computeTaskKind`（`agent.go:1030`）。
fn task_kind(row: &TaskRow) -> String {
    if row.chat_session_id.is_some() {
        return "chat".to_owned();
    }
    if row.autopilot_run_id.is_some() {
        return "autopilot".to_owned();
    }
    let typed = row
        .context
        .as_ref()
        .and_then(|v| v.get("type"))
        .and_then(Value::as_str)
        .is_some_and(|v| v == "quick_create");
    if typed || row.issue_id.is_none() {
        return "quick_create".to_owned();
    }
    if row.trigger_comment_id.is_some() {
        return "comment".to_owned();
    }
    "direct".to_owned()
}

/// 上游 `taskCancellationActorToResponse`（`agent.go:875`）。
fn cancellation_actor(row: &TaskRow) -> Option<CancellationActorDto> {
    if row.status != "cancelled" {
        return None;
    }
    let actor_type = row.cancelled_by_type.clone().filter(|v| !v.is_empty())?;
    Some(CancellationActorDto {
        r#type: actor_type,
        id: row
            .cancelled_by_id
            .map(|v| v.to_string())
            .unwrap_or_default(),
        name: row.cancelled_by_name.clone().filter(|v| !v.is_empty()),
    })
}

/// `GET /api/issues/:id/task-runs?scope=family` 的一行（上游 `ActiveRunSummary`，
/// `daemon.go:5493`）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ActiveRunSummaryDto {
    /// `task_id`。
    pub task_id: String,
    /// `issue_id`。
    pub issue_id: String,
    /// `issue_identifier`。
    pub issue_identifier: String,
    /// `issue_title`。
    pub issue_title: String,
    /// `agent_id`。
    pub agent_id: String,
    /// `status`。
    pub status: String,
    /// `created_at`。
    pub created_at: String,
    /// `started_at`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
}

/// `GET /api/issues/:id/active-task` 的信封（上游 `{"tasks": [...]}`）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ActiveTasksResponse {
    /// `tasks`。
    pub tasks: Vec<TaskDto>,
}

/// `POST /api/issues/preview-trigger` 请求体（`issue_trigger.go:110`）。
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(crate) struct IssueTriggerPreviewRequest {
    /// `issue_ids`（默认空数组）。
    #[serde(default)]
    pub issue_ids: Vec<String>,
    /// `is_create`。
    #[serde(default)]
    pub is_create: bool,
    /// `assignee_type`。
    #[serde(default)]
    pub assignee_type: Option<String>,
    /// `assignee_id`。
    #[serde(default)]
    pub assignee_id: Option<String>,
    /// `status`。
    #[serde(default)]
    pub status: Option<String>,
}

/// `POST /api/issues/:id/rerun` 请求体（`task_lifecycle.go:175`）。
///
/// 体是可选的：空体 / `{}` / `null` 都等价于「无具名来源」（派生 rerun，重跑当前
/// assignee）。上游用 `ContentLength != 0` + `io.EOF` 容许空体，`null` 也能解成零值。
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(crate) struct RerunIssueRequest {
    /// `task_id`（可选；具名 rerun 的来源任务）。
    #[serde(default)]
    pub task_id: Option<String>,
}

/// `preview-trigger` 的一行（`issue_trigger.go:124`）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct IssueTriggerPreviewItem {
    /// `issue_id`。
    pub issue_id: String,
    /// `agent_id`。
    pub agent_id: String,
    /// `source`（`assign` / `status`）。
    pub source: String,
}

/// `preview-trigger` 响应（`issue_trigger.go:131`）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct IssueTriggerPreviewResponse {
    /// `triggers`。
    pub triggers: Vec<IssueTriggerPreviewItem>,
    /// `total_count`。
    pub total_count: usize,
}

/// `GET /api/issues/:id/usage` 响应（上游 `GetIssueUsage`，`daemon.go:5792`）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct IssueUsageDto {
    /// `total_input_tokens`。
    pub total_input_tokens: i64,
    /// `total_output_tokens`。
    pub total_output_tokens: i64,
    /// `total_cache_read_tokens`。
    pub total_cache_read_tokens: i64,
    /// `total_cache_write_tokens`。
    pub total_cache_write_tokens: i64,
    /// `cost_usd_ticks`（行内列名是 `total_cost_usd_ticks`）。
    pub cost_usd_ticks: i64,
    /// `uncosted_input_tokens`。
    pub uncosted_input_tokens: i64,
    /// `uncosted_output_tokens`。
    pub uncosted_output_tokens: i64,
    /// `uncosted_cache_read_tokens`。
    pub uncosted_cache_read_tokens: i64,
    /// `uncosted_cache_write_tokens`。
    pub uncosted_cache_write_tokens: i64,
    /// `task_count`（legacy，保持稳定）。
    pub task_count: i32,
    /// `terminal_task_count`。
    pub terminal_task_count: i32,
    /// `metered_task_count`。
    pub metered_task_count: i32,
    /// `unreported_task_count`。
    pub unreported_task_count: i32,
}

/// `GET /api/tasks/:taskId/messages` 的一行（上游 `protocol.TaskMessagePayload`，
/// `pkg/protocol/messages.go:201`）。
///
/// 除 `task_id` / `seq` / `type` / `created_at` 外全部 `omitempty`（Go 侧对
/// `pgtype.*.String` 取 `.String`，零值就是空串）⇒ 本仓用 `Option` + `is_none`
/// 表达，空串与 NULL 同样落成省略。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct TaskMessageDto {
    /// `task_id`。
    pub task_id: String,
    /// `issue_id`（无 issue 的 quick-create 任务省略）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<String>,
    /// `seq`。
    pub seq: i32,
    /// `type`。
    pub r#type: String,
    /// `tool`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// `call_id`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// `content`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// `input`（工具输入；仅 JSON 对象）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    /// `output`（工具输出，**原样字符串**，上游不解析成 JSON）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// `output_truncated`（三态：省略 = 没有 daemon 测量过这条记录）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_truncated: Option<bool>,
    /// `created_at`。
    pub created_at: String,
}

/// `POST /api/client-usage` 请求体（`client_usage.go:70`）。
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClientUsageRequest {
    /// `install_id`。
    #[serde(default)]
    pub install_id: String,
    /// `runtime`（可选探针）。
    #[serde(default)]
    pub runtime: Option<ClientUsageRuntimeProbe>,
}

/// 客户端上报的 runtime 探针（`client_usage.go:80`）。
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClientUsageRuntimeProbe {
    /// `probe_result`。
    #[serde(default)]
    pub probe_result: Option<String>,
    /// `runtime_count`。
    #[serde(default)]
    pub runtime_count: Option<i32>,
    /// `provider_summary`（provider 名 → 计数）。
    #[serde(default)]
    pub provider_summary: Option<std::collections::BTreeMap<String, i32>>,
    /// `online_count`。
    #[serde(default)]
    pub online_count: Option<i32>,
    /// `offline_count`。
    #[serde(default)]
    pub offline_count: Option<i32>,
}

/// `POST /api/agent-builder/sessions` 请求体（`agent_builder.go:150`）。
///
/// 上游只用 `json.NewDecoder(...).Decode`（**没有** `DisallowUnknownFields`），未知字段
/// 被忽略 ⇒ 本仓不挂 `deny_unknown_fields`。
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(crate) struct CreateBuilderSessionRequest {
    /// `runtime_id`。
    #[serde(default)]
    pub runtime_id: String,
    /// `model`。
    #[serde(default)]
    pub model: Option<String>,
}

/// 创建响应（`CreateAgentBuilderSessionResponse`，`agent_builder.go:170`）。
///
/// 三个字段都以 `_id` 收尾（上游同名 JSON 键），`struct_field_names` 在这里只会添噪音。
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, Serialize)]
pub(crate) struct CreateBuilderSessionResponse {
    /// `session_id`。
    pub session_id: String,
    /// `builder_agent_id`。
    pub builder_agent_id: String,
    /// `runtime_id`。
    pub runtime_id: String,
}

/// `PATCH /api/agent-builder/sessions/:id/runtime` 请求体（`agent_builder.go:401`）。
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(crate) struct SwitchRuntimeRequest {
    /// `runtime_id`。
    #[serde(default)]
    pub runtime_id: String,
}

/// 切换 runtime 的响应（`SwitchAgentBuilderRuntimeResponse`，`agent_builder.go:397`）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SwitchRuntimeResponse {
    /// `runtime_id`（切换后的）。
    pub runtime_id: String,
}

/// 会话列表信封（上游 `ListAgentBuilderSessionsResponse`，`agent_builder.go:180`）。
///
/// 上游是 `{"sessions": [...]}` 而**不是**裸数组 —— 裸数组会让读 `.sessions` 的
/// 客户端拿到 `undefined`。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ListBuilderSessionsResponse {
    /// `sessions`。
    pub sessions: Vec<BuilderSessionDto>,
}

/// 会话列表一行（`AgentBuilderSessionSummary`，`agent_builder.go:186`）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct BuilderSessionDto {
    /// `session_id`。
    pub session_id: String,
    /// `title`。
    pub title: String,
    /// `runtime_id`（载体 agent 的）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<String>,
    /// `created_at`。
    pub created_at: String,
    /// `updated_at`。
    pub updated_at: String,
    /// `last_message_content`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_message_content: Option<String>,
    /// `last_message_role`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_message_role: Option<String>,
    /// `last_message_at`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_message_at: Option<String>,
    /// `draft`（已保存的草稿）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft: Option<Value>,
}

impl BuilderSessionDto {
    pub(crate) fn from_row(row: &BuilderSessionRow) -> Self {
        Self {
            session_id: row.id.to_string(),
            title: row.title.clone(),
            runtime_id: row.runtime_id.map(|v| v.to_string()),
            created_at: ts(row.created_at),
            updated_at: ts(row.updated_at),
            last_message_content: non_empty(&row.last_message_content),
            last_message_role: non_empty(&row.last_message_role),
            last_message_at: ts_opt(row.last_message_at),
            draft: row.stored_draft.clone(),
        }
    }
}

fn non_empty(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

/// `GET /api/working-agents` 的一行（上游 `WorkspaceWorkingAgent`，`agent.go:2766`）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct WorkingAgentDto {
    /// `id`。
    pub id: String,
    /// `name`。
    pub name: String,
    /// `avatar_url`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    /// `running_task_count`。
    pub running_task_count: i64,
    /// `issue_ids`。
    pub issue_ids: Vec<String>,
}
