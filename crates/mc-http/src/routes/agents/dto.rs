//! agent 面的线上形状（wire shape）的**公共父模块**：helper 与重导出。
//!
//! - 出参 DTO → `dto/response.rs`；入参结构体与权限解析 → `dto/input.rs`
//!   （两个方向各自一份，见 R7 单文件 800 行上限）。两边都在这里重导出，
//!   下游仍走 `dto::AgentDto` / `dto::UpdateAgentRequest`。
//! - 本文件只留**两侧都用得到**的东西：`omitempty` 类 helper、掩码/反掩码、
//!   jsonb 解码与规范化校验。
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
//! - `skills`：M6-4（LUM-1669）起是**真读 `agent_skill`** 的字段（不再是恒 `[]`）。
//! - `AgentTaskResponse`：只投影本片真实读取的列（见 [`AgentTaskDto`]）。
//! - 时间串用本仓既有约定 `to_rfc3339()`（Go 侧是 `time.RFC3339`，秒级精度）。

use serde_json::{Map, Value as JsonValue};

mod input;
mod response;
// 入参结构体与权限解析搬到了 `dto/input.rs`（R7 单文件 800 行上限）；
// 这里重导出，下游仍走 `dto::X`。
pub(crate) use input::{
    parse_permission_input, AttachLabelRequest, CreateAgentRequest, ResolvedPermission,
    UpdateAgentEnvRequest, UpdateAgentRequest,
};
// 出参 DTO 搬到了 `dto/response.rs`（同一条 800 行规则）；同样重导出，
// 下游继续走 `dto::AgentDto` 这种路径。
pub(crate) use response::*;

/// `runtime_config.gateway.token` 的掩码哨兵（上游 `runtimeConfigGatewayTokenMask`）。
pub(crate) const GATEWAY_TOKEN_MASK: &str = "***";

/// env 值的「保持原值」哨兵（上游 `envSentinel`）。
pub(crate) const ENV_SENTINEL: &str = "****";

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
    use chrono::Utc;
    use mc_repos::agent::AgentInvocationTargetRow;
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
