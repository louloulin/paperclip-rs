//! agent 面的线上形状（wire shape）与请求体。
//!
//! 上游对照：`AgentResponse` / `AgentInvocationTargetDTO` / `AgentConversationStarter` /
//! `AgentSkillSummary` / `DisabledRuntimeSkill` / `AgentTaskResponse` /
//! `LabelResponse` / `AgentEnvResponse`（`agent.go` / `agent_env.go` /
//! `skill.go` / `agent_runtime_skills.go`）。
//!
//! 字段顺序、JSON key、`omitempty` 语义都按上游逐字段抄写（结构体按声明序序列化），
//! 只有以下几处**有意不同**（详见 `docs/40-M3-5-AGENTS.md` §5）：
//! - `runtime_availability`：在线投影属 M3-4，本片**不产出**该字段（而不是恒空串）。
//! - `system_instructions`：产品内置 prompt 表不在本片，不产出。
//! - `skills`：恒 `[]`（`agent_skill` 读写属 M6）。
//! - `AgentTaskResponse`：只投影本片真实读取的列（见 [`AgentTaskDto`]）。
//! - 时间串用本仓既有约定 `to_rfc3339()`（Go 侧是 `time.RFC3339`，秒级精度）。

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use mc_repos::agent::{
    AgentActivityBucketRow, AgentInvocationTargetRow, AgentLabelRow, AgentRow, AgentRunCountRow,
    AgentTaskRow, PERMISSION_MODE_PUBLIC_TO, TARGET_WORKSPACE, VISIBILITY_PRIVATE,
    VISIBILITY_WORKSPACE,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as JsonValue};

use super::AgentScope;

mod input;
// 入参结构体与权限解析搬到了 `dto/input.rs`（R7 单文件 800 行上限）；
// 这里重导出，下游仍走 `dto::X`。
pub(crate) use input::{
    parse_permission_input, AttachLabelRequest, CreateAgentRequest, ResolvedPermission,
    UpdateAgentEnvRequest, UpdateAgentRequest,
};

/// `runtime_config.gateway.token` 的掩码哨兵（上游 `runtimeConfigGatewayTokenMask`）。
pub(crate) const GATEWAY_TOKEN_MASK: &str = "***";

/// env 值的「保持原值」哨兵（上游 `envSentinel`）。
pub(crate) const ENV_SENTINEL: &str = "****";

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

// ---------------------------------------------------------------------------
// 值编解码 helper
// ---------------------------------------------------------------------------

/// 解析 jsonb 数组成 DTO；任何形状不符 → 空列表（上游 `json.Unmarshal` 失败即 `[]`）。
pub(crate) fn decode_conversation_starters(raw: &JsonValue) -> Vec<ConversationStarterDto> {
    serde_json::from_value(raw.clone()).unwrap_or_default()
}

/// 上游 `custom_args`：非字符串元素/非数组 → 空列表。
pub(crate) fn decode_custom_args(raw: &JsonValue) -> Vec<String> {
    raw.as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// 上游 `decodeDisabledRuntimeSkills`：坏形状 → 空列表。
pub(crate) fn decode_disabled_runtime_skills(raw: &JsonValue) -> Vec<DisabledRuntimeSkillDto> {
    if raw.is_null() {
        return Vec::new();
    }
    serde_json::from_value(raw.clone()).unwrap_or_default()
}

/// 上游 `maskGatewayToken`：`runtime_config.gateway.token` 非空即替换成掩码；
/// NULL / 非对象 → `{}`（上游 `rc == nil → map[string]any{}`）。
pub(crate) fn mask_gateway_token(runtime_config: &JsonValue) -> JsonValue {
    let mut value = if runtime_config.is_null() {
        JsonValue::Object(Map::new())
    } else {
        runtime_config.clone()
    };
    if let Some(gateway) = value.get_mut("gateway").and_then(JsonValue::as_object_mut) {
        if gateway
            .get("token")
            .and_then(JsonValue::as_str)
            .is_some_and(|t| !t.is_empty())
        {
            gateway.insert("token".into(), JsonValue::String(GATEWAY_TOKEN_MASK.into()));
        }
    }
    value
}

/// 上游 `preserveMaskedGatewayToken`：请求体里回传了掩码哨兵时，把库里真实的
/// token 塞回去（否则 GET→PUT 的往返会把 `***` 当真 token 写坏）。
pub(crate) fn preserve_masked_gateway_token(incoming: &mut JsonValue, persisted: &JsonValue) {
    let Some(gateway) = incoming
        .get_mut("gateway")
        .and_then(JsonValue::as_object_mut)
    else {
        return;
    };
    if gateway.get("token").and_then(JsonValue::as_str) != Some(GATEWAY_TOKEN_MASK) {
        return;
    }
    let previous = persisted
        .get("gateway")
        .and_then(|g| g.get("token"))
        .and_then(JsonValue::as_str)
        .filter(|t| !t.is_empty());
    match previous {
        Some(token) => {
            gateway.insert("token".into(), JsonValue::String(token.to_string()));
        }
        None => {
            gateway.remove("token");
        }
    }
}

/// 上游 `normaliseComposioToolkitAllowlist`：trim + lowercase + 去重 + 保序。
pub(crate) fn normalise_composio_allowlist(raw: &[String]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::with_capacity(raw.len());
    for item in raw {
        let slug = item.trim().to_lowercase();
        if slug.is_empty() || seen.contains(&slug) {
            continue;
        }
        seen.push(slug);
    }
    seen
}

/// 上游 `normaliseAgentConversationStarters` 的四条校验（错误串逐字对齐）。
pub(crate) fn normalise_conversation_starters(
    raw: &[ConversationStarterDto],
) -> Result<Vec<ConversationStarterDto>, String> {
    if raw.len() > mc_repos::agent::MAX_CONVERSATION_STARTERS {
        return Err(format!(
            "conversation_starters must contain at most {} items",
            mc_repos::agent::MAX_CONVERSATION_STARTERS
        ));
    }
    let mut out = Vec::with_capacity(raw.len());
    for (i, item) in raw.iter().enumerate() {
        let label = item.label.trim().to_string();
        let prompt = item.prompt.trim().to_string();
        if label.is_empty() {
            return Err(format!("conversation_starters[{i}].label is required"));
        }
        if prompt.is_empty() {
            return Err(format!("conversation_starters[{i}].prompt is required"));
        }
        if label.chars().count() > mc_repos::agent::MAX_STARTER_LABEL_LEN {
            return Err(format!(
                "conversation_starters[{i}].label must be {} characters or fewer",
                mc_repos::agent::MAX_STARTER_LABEL_LEN
            ));
        }
        if prompt.chars().count() > mc_repos::agent::MAX_STARTER_PROMPT_LEN {
            return Err(format!(
                "conversation_starters[{i}].prompt must be {} characters or fewer",
                mc_repos::agent::MAX_STARTER_PROMPT_LEN
            ));
        }
        out.push(ConversationStarterDto { label, prompt });
    }
    Ok(out)
}

/// 上游 `parsePermissionInput` 的解析结果。
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;

    fn target(target_type: &str, id: Uuid) -> AgentInvocationTargetRow {
        AgentInvocationTargetRow {
            id: Uuid::nil(),
            agent_id: Uuid::nil(),
            target_type: target_type.into(),
            target_id: id,
            created_by: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn legacy_visibility_only_widens_for_workspace_targets() {
        let ws = Uuid::new_v4();
        assert_eq!(
            derive_legacy_visibility("public_to", &[target("workspace", ws)]),
            "workspace"
        );
        assert_eq!(
            derive_legacy_visibility("public_to", &[target("member", Uuid::new_v4())]),
            "private"
        );
        assert_eq!(derive_legacy_visibility("private", &[]), "private");
    }

    #[test]
    fn gateway_token_is_masked_and_restored() {
        let persisted = json!({"gateway": {"token": "real", "mode": "openclaw"}});
        let masked = mask_gateway_token(&persisted);
        assert_eq!(masked["gateway"]["token"], GATEWAY_TOKEN_MASK);
        assert_eq!(masked["gateway"]["mode"], "openclaw");
        // 原值不被就地改写
        assert_eq!(persisted["gateway"]["token"], "real");

        let mut roundtrip = masked.clone();
        preserve_masked_gateway_token(&mut roundtrip, &persisted);
        assert_eq!(roundtrip["gateway"]["token"], "real");

        // 库里没有旧 token → 该键被删掉，而不是持久化哨兵
        let mut fresh = json!({"gateway": {"token": GATEWAY_TOKEN_MASK}});
        preserve_masked_gateway_token(&mut fresh, &json!({}));
        assert!(fresh["gateway"].get("token").is_none());
    }

    #[test]
    fn empty_runtime_config_serialises_as_object() {
        assert_eq!(mask_gateway_token(&JsonValue::Null), json!({}));
    }

    #[test]
    fn permission_input_defaults_and_normalisation() {
        let ws = Uuid::new_v4();
        // 两个字段都没给 → None（调用者取默认）
        assert_eq!(
            parse_permission_input(ws, None, false, &[], false, None).unwrap(),
            None
        );
        // legacy workspace → public_to + workspace 目标
        let resolved =
            parse_permission_input(ws, None, false, &[], false, Some("workspace")).unwrap();
        let resolved = resolved.expect("resolved");
        assert_eq!(resolved.mode, "public_to");
        assert_eq!(resolved.targets, vec![("workspace".to_string(), ws)]);
        assert_eq!(resolved.legacy_visibility(), "workspace");
        // private 忽略提交的目标
        let resolved = parse_permission_input(
            ws,
            Some("private"),
            true,
            &[InvocationTargetDto {
                target_type: "workspace".into(),
                target_id: None,
            }],
            true,
            None,
        )
        .unwrap()
        .expect("resolved");
        assert!(resolved.targets.is_empty());
        // public_to 空目标 → 归一成 workspace 目标（MUL-3963）
        let resolved = parse_permission_input(ws, Some("public_to"), true, &[], true, None)
            .unwrap()
            .expect("resolved");
        assert_eq!(resolved.targets, vec![("workspace".to_string(), ws)]);
        // 非法值逐条报错
        assert!(parse_permission_input(ws, None, false, &[], false, Some("bogus")).is_err());
        assert!(parse_permission_input(ws, Some("bogus"), true, &[], false, None).is_err());
        assert!(parse_permission_input(
            ws,
            Some("public_to"),
            true,
            &[InvocationTargetDto {
                target_type: "member".into(),
                target_id: None,
            }],
            true,
            None
        )
        .is_err());
    }

    #[test]
    fn composio_allowlist_is_trimmed_lowercased_and_deduped() {
        let raw = vec![
            " Gmail ".to_string(),
            "gmail".to_string(),
            String::new(),
            "  ".to_string(),
            "Slack".to_string(),
        ];
        assert_eq!(normalise_composio_allowlist(&raw), vec!["gmail", "slack"]);
    }

    #[test]
    fn conversation_starters_validation_matches_upstream_messages() {
        assert!(normalise_conversation_starters(&[]).unwrap().is_empty());
        let four = vec![
            ConversationStarterDto {
                label: "a".into(),
                prompt: "b".into(),
            };
            4
        ];
        assert_eq!(
            normalise_conversation_starters(&four).unwrap_err(),
            "conversation_starters must contain at most 3 items"
        );
        assert_eq!(
            normalise_conversation_starters(&[ConversationStarterDto {
                label: " ".into(),
                prompt: "b".into()
            }])
            .unwrap_err(),
            "conversation_starters[0].label is required"
        );
        assert_eq!(
            normalise_conversation_starters(&[ConversationStarterDto {
                label: "a".into(),
                prompt: String::new()
            }])
            .unwrap_err(),
            "conversation_starters[0].prompt is required"
        );
        // trim 后长度按字符（rune）计
        let long = ConversationStarterDto {
            label: "字".repeat(81),
            prompt: "b".into(),
        };
        assert!(normalise_conversation_starters(&[long]).is_err());
        let trimmed = normalise_conversation_starters(&[ConversationStarterDto {
            label: "  hi  ".into(),
            prompt: " there ".into(),
        }])
        .unwrap();
        assert_eq!(trimmed[0].label, "hi");
        assert_eq!(trimmed[0].prompt, "there");
    }

    #[test]
    fn bad_json_shapes_degrade_to_empty_lists() {
        assert!(decode_conversation_starters(&json!({"nope": 1})).is_empty());
        assert!(decode_custom_args(&json!({"a": 1})).is_empty());
        assert!(decode_disabled_runtime_skills(&json!(["not-an-object"])).is_empty());
        assert_eq!(decode_custom_args(&json!(["a", 1, "b"])), vec!["a", "b"]);
    }
}
