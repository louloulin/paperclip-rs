//! agent 面的**出参** DTO（拆自 `dto.rs`，R7 单文件 800 行上限）。
//!
//! 上游对照：`AgentResponse` / `AgentInvocationTargetDTO` / `AgentConversationStarter` /
//! `AgentSkillSummary` / `DisabledRuntimeSkill` / `AgentTaskResponse` / `LabelResponse` /
//! `AgentEnvResponse`（`agent.go` / `agent_env.go` / `skill.go` / `agent_runtime_skills.go`）。
//!
//! 字段顺序、JSON key、`omitempty` 语义都按上游逐字段抄写（结构体按声明序序列化）。
//! 请求体与权限解析在 `dto/input.rs`，掩码/规范化 helper 留在上级 `dto.rs`
//! （两侧都用得到，留在公共父模块避免互相 `use`）。
//!
//! ⚠️ `skills` 不再是恒 `[]`：M6-4（LUM-1669）把它接上了 `agent_skill`
//! （见 [`AgentDto::with_skills`]）；`disabled_runtime_skills` 一直是真读列。

use chrono::{DateTime, Utc};
use mc_repos::agent::{
    AgentActivityBucketRow, AgentInvocationTargetRow, AgentLabelRow, AgentRow, AgentRunCountRow,
    AgentTaskRow, PERMISSION_MODE_PUBLIC_TO, TARGET_WORKSPACE, VISIBILITY_PRIVATE,
    VISIBILITY_WORKSPACE,
};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;

use super::super::AgentScope;
use super::{
    decode_conversation_starters, decode_custom_args, decode_disabled_runtime_skills,
    mask_gateway_token,
};

fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339()
}

fn ts_opt(t: Option<DateTime<Utc>>) -> Option<String> {
    t.map(|v| v.to_rfc3339())
}

/// 上游 `omitempty` 的布尔等价物：`false` 不进 JSON。
///
/// serde 的 `skip_serializing_if` 约定把字段按引用传入，故这里必须收 `&bool`。
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
}

fn non_empty(raw: Option<&String>) -> Option<String> {
    raw.filter(|v| !v.is_empty()).cloned()
}

// ---------------------------------------------------------------------------
// 响应
// ---------------------------------------------------------------------------

/// 对话起始建议（上游 `AgentConversationStarter`：请求与响应同形）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ConversationStarterDto {
    pub label: String,
    pub prompt: String,
}

/// invoke 白名单条目（上游 `AgentInvocationTargetDTO`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct InvocationTargetDto {
    pub target_type: String,
    pub target_id: Option<String>,
}

/// 上游 `AgentSkillSummary`——本片恒为空列表（M6 拥有 `agent_skill`）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AgentSkillSummaryDto {
    pub id: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
}

/// 上游 `DisabledRuntimeSkill`（本片只做 JSON 透传 + 形状修正）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DisabledRuntimeSkillDto {
    pub runtime_id: String,
    pub provider: String,
    pub root: String,
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
}

/// 上游 `AgentResponse`。
///
/// 四个布尔位（`runtime_bound` / `has_custom_env` / `*_redacted`）逐字对齐上游
/// wire 形状；拆成枚举会让 JSON 偏离契约，故在此豁免 clippy 的数量检查。
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AgentDto {
    pub id: String,
    pub workspace_id: String,
    pub runtime_id: String,
    pub runtime_bound: bool,
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub conversation_starters: Vec<ConversationStarterDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_key: Option<String>,
    pub avatar_url: Option<String>,
    pub runtime_mode: String,
    pub runtime_config: JsonValue,
    pub custom_args: Vec<String>,
    pub mcp_config: Option<JsonValue>,
    pub has_custom_env: bool,
    pub custom_env_key_count: usize,
    pub mcp_config_redacted: bool,
    pub visibility: String,
    pub permission_mode: String,
    pub invocation_targets: Vec<InvocationTargetDto>,
    pub status: String,
    pub max_concurrent_tasks: i32,
    pub model: String,
    pub thinking_level: String,
    pub service_tier: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub composio_toolkit_allowlist: Option<Vec<String>>,
    #[serde(skip_serializing_if = "is_false")]
    pub composio_toolkit_allowlist_redacted: bool,
    pub owner_id: Option<String>,
    pub skills: Vec<AgentSkillSummaryDto>,
    pub disabled_runtime_skills: Vec<DisabledRuntimeSkillDto>,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
    pub archived_by: Option<String>,
}

impl AgentDto {
    /// 行 → 响应（含白名单投影 + 两处按调用者的脱敏）。
    pub(crate) fn from_row(
        row: &AgentRow,
        scope: &AgentScope,
        targets: &[AgentInvocationTargetRow],
    ) -> Self {
        let allowlist = row
            .composio_toolkit_allowlist
            .as_ref()
            .filter(|v| !v.is_empty())
            .cloned();
        let mut dto = Self {
            id: row.id.to_string(),
            workspace_id: row.workspace_id.to_string(),
            runtime_id: row.runtime_id.map(|v| v.to_string()).unwrap_or_default(),
            runtime_bound: row.runtime_id.is_some(),
            name: row.name.clone(),
            description: row.description.clone(),
            instructions: row.instructions.clone(),
            conversation_starters: decode_conversation_starters(&row.conversation_starters),
            system_key: non_empty(row.system_key.as_ref()),
            avatar_url: row.avatar_url.clone(),
            runtime_mode: row.runtime_mode.clone(),
            runtime_config: mask_gateway_token(&row.runtime_config),
            custom_args: decode_custom_args(&row.custom_args),
            mcp_config: row.mcp_config.clone(),
            has_custom_env: row.custom_env_key_count() > 0,
            custom_env_key_count: row.custom_env_key_count(),
            mcp_config_redacted: false,
            visibility: row.visibility.clone(),
            permission_mode: row.permission_mode.clone(),
            invocation_targets: Vec::new(),
            status: row.status.clone(),
            max_concurrent_tasks: row.max_concurrent_tasks,
            model: row.model.clone().unwrap_or_default(),
            thinking_level: row.thinking_level.clone().unwrap_or_default(),
            service_tier: row.service_tier.clone().unwrap_or_default(),
            composio_toolkit_allowlist: allowlist,
            composio_toolkit_allowlist_redacted: false,
            owner_id: row.owner_id.map(|v| v.to_string()),
            skills: Vec::new(),
            disabled_runtime_skills: decode_disabled_runtime_skills(&row.disabled_runtime_skills),
            created_at: ts(row.created_at),
            updated_at: ts(row.updated_at),
            archived_at: ts_opt(row.archived_at),
            archived_by: row.archived_by.map(|v| v.to_string()),
        };
        dto.apply_invocation_targets(targets);
        if !scope.can_view_secrets(row) {
            dto.redact_mcp_config();
        }
        if !scope.is_agent_owner(row) {
            dto.redact_composio_allowlist();
        }
        dto
    }

    /// 上游 `applyInvocationTargetsToResponse`：填 `invocation_targets` 并由
    /// `permission_mode` **重算** legacy `visibility`（保证旧客户端只会看到更窄的可见性）。
    pub(crate) fn apply_invocation_targets(&mut self, targets: &[AgentInvocationTargetRow]) {
        self.invocation_targets = targets
            .iter()
            .map(|t| InvocationTargetDto {
                target_type: t.target_type.clone(),
                target_id: Some(t.target_id.to_string()),
            })
            .collect();
        self.visibility = derive_legacy_visibility(&self.permission_mode, targets);
    }

    /// 上游 `redactMcpConfig`。
    pub(crate) fn redact_mcp_config(&mut self) {
        if self.mcp_config.is_some() {
            self.mcp_config = None;
            self.mcp_config_redacted = true;
        }
    }

    /// 上游 `redactComposioToolkitAllowlist`：只有 agent owner 能看自己选了哪些
    /// 工具包（不是密钥，但会泄露 owner 的集成足迹）。
    pub(crate) fn redact_composio_allowlist(&mut self) {
        if self.composio_toolkit_allowlist.is_some() {
            self.composio_toolkit_allowlist = None;
            self.composio_toolkit_allowlist_redacted = true;
        }
    }
}

/// 上游 `deriveLegacyVisibility`：`public_to` + workspace 目标 → `workspace`，
/// 其余（含 `public_to` 只给 member/team）→ `private`。
pub(crate) fn derive_legacy_visibility(
    permission_mode: &str,
    targets: &[AgentInvocationTargetRow],
) -> String {
    if permission_mode == PERMISSION_MODE_PUBLIC_TO
        && targets.iter().any(|t| t.target_type == TARGET_WORKSPACE)
    {
        VISIBILITY_WORKSPACE.to_string()
    } else {
        VISIBILITY_PRIVATE.to_string()
    }
}

/// `custom_env` 的键值对（`GET/PUT /api/agents/:id/env` 的明文形状）。
///
/// `BTreeMap` 而非 `HashMap`：上游 `sortedKeys` 保证审计/响应里的键序稳定。
pub(crate) type CustomEnv = BTreeMap<String, String>;

/// 上游 `AgentEnvResponse`。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AgentEnvDto {
    pub agent_id: String,
    pub custom_env: CustomEnv,
}

/// 上游 `LabelResponse`（agent label 的 `usage_count` 恒 0：`labelToResponse` 不填）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct LabelDto {
    pub id: String,
    pub workspace_id: String,
    pub resource_type: String,
    pub name: String,
    pub description: String,
    pub color: String,
    pub usage_count: i32,
    pub created_at: String,
    pub updated_at: String,
}

impl LabelDto {
    pub(crate) fn from_row(row: &AgentLabelRow) -> Self {
        Self {
            id: row.id.to_string(),
            workspace_id: row.workspace_id.to_string(),
            resource_type: row.resource_type.clone(),
            name: row.name.clone(),
            description: row.description.clone(),
            color: row.color.clone(),
            usage_count: 0,
            created_at: ts(row.created_at),
            updated_at: ts(row.updated_at),
        }
    }
}

/// `{"labels": [...]}`（上游 attach/detach 的响应包）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct LabelsDto {
    pub labels: Vec<LabelDto>,
}

/// `{"cancelled": n}`（上游 `CancelAgentTasks`）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct CancelTasksDto {
    pub cancelled: u64,
}

/// `agent_task_queue` 的**窄投影**响应。
///
/// 上游 `AgentTaskResponse`（`agent.go:359`）还带 daemon-only 字段
/// （`remote_mcp_*` / `plugin_hook_tools` / `workspace_context` / `issue_statuses` /
/// usage / attributions）。本片只读 task 表、不拥有其状态机（M3-3/M3-6），
/// 因此只投影真实读到的列，**不伪造**空值字段；本仓 `agent_task_queue` 无
/// `workspace_id` 列（workspace 由调用上下文给定），`escalation_for_task_id`
/// 对应上游 wire 的 `parent_task_id`（见 docs/40 §5）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AgentTaskDto {
    pub id: String,
    pub agent_id: String,
    pub runtime_id: Option<String>,
    pub issue_id: Option<String>,
    pub status: String,
    pub priority: i32,
    pub dispatched_at: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub error: Option<String>,
    pub failure_reason: Option<String>,
    pub attempt: i32,
    pub max_attempts: i32,
    pub escalation_for_task_id: Option<String>,
    pub created_at: String,
}

impl AgentTaskDto {
    pub(crate) fn from_row(row: &AgentTaskRow) -> Self {
        Self {
            id: row.id.to_string(),
            agent_id: row.agent_id.to_string(),
            runtime_id: row.runtime_id.map(|v| v.to_string()),
            issue_id: row.issue_id.map(|v| v.to_string()),
            status: row.status.clone(),
            priority: row.priority,
            dispatched_at: ts_opt(row.dispatched_at),
            started_at: ts_opt(row.started_at),
            completed_at: ts_opt(row.completed_at),
            error: row.error.clone(),
            failure_reason: row.failure_reason.clone(),
            attempt: row.attempt,
            max_attempts: row.max_attempts,
            escalation_for_task_id: row.escalation_for_task_id.map(|v| v.to_string()),
            created_at: ts(row.created_at),
        }
    }
}

/// `GET /api/agent-run-counts` 的元素（上游 `AgentRunCount`）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RunCountDto {
    pub agent_id: String,
    pub run_count: i32,
}

impl RunCountDto {
    pub(crate) fn from_row(row: &AgentRunCountRow) -> Self {
        Self {
            agent_id: row.agent_id.to_string(),
            run_count: row.run_count,
        }
    }
}

/// `GET /api/agent-activity-30d` 的元素（上游 `AgentActivityBucket`）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ActivityBucketDto {
    pub agent_id: String,
    pub bucket_at: String,
    pub task_count: i32,
    pub failed_count: i32,
    pub completed_count: i32,
    pub cancelled_count: i32,
}

impl ActivityBucketDto {
    pub(crate) fn from_row(row: &AgentActivityBucketRow) -> Self {
        Self {
            agent_id: row.agent_id.to_string(),
            bucket_at: ts(row.bucket),
            task_count: row.task_count,
            failed_count: row.failed_count,
            completed_count: row.completed_count,
            cancelled_count: row.cancelled_count,
        }
    }
}

