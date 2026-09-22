//! agent 面的**入参**与权限输入解析（拆自 `dto.rs`，R7 单文件 800 行上限）。
//!
//! 出参 DTO 与掩码/规范化 helper 留在上级模块，这里只放「请求 → 仓储输入」这一侧：
//! 请求结构体、区分「缺失 / `null` / 值」的 `double_option`，以及上游
//! `parsePermissionInput`（`agent_permission.go:99`）的逐分支移植。

use mc_repos::agent::{
    PERMISSION_MODE_PUBLIC_TO, TARGET_WORKSPACE, VISIBILITY_PRIVATE, VISIBILITY_WORKSPACE,
};
use serde::Deserialize;
use serde_json::Value as JsonValue;
use uuid::Uuid;

use super::{ConversationStarterDto, CustomEnv, InvocationTargetDto};

// ---------------------------------------------------------------------------
// 请求
// ---------------------------------------------------------------------------

/// 上游 `CreateAgentRequest`（本片不支持 `template` / `skill_ids`，见 docs/40 §5）。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct CreateAgentRequest {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub conversation_starters: Vec<ConversationStarterDto>,
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub runtime_id: Option<String>,
    #[serde(default)]
    pub runtime_config: Option<JsonValue>,
    #[serde(default)]
    pub custom_env: Option<CustomEnv>,
    #[serde(default)]
    pub custom_args: Option<Vec<String>>,
    #[serde(default)]
    pub mcp_config: Option<JsonValue>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub invocation_targets: Vec<InvocationTargetDto>,
    #[serde(default)]
    pub max_concurrent_tasks: Option<i32>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub thinking_level: Option<String>,
    #[serde(default)]
    pub service_tier: Option<String>,
    #[serde(default)]
    pub composio_toolkit_allowlist: Option<Vec<String>>,
}

/// 上游 `UpdateAgentRequest`（指针字段 = 「omit 与显式空值不同」；
/// `custom_env` 在本端点被硬拒，故这里不声明该字段）。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct UpdateAgentRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub conversation_starters: Option<Vec<ConversationStarterDto>>,
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub runtime_id: Option<String>,
    #[serde(default)]
    pub runtime_config: Option<JsonValue>,
    #[serde(default)]
    pub custom_args: Option<Vec<String>>,
    /// `null` → 清空；对象 → 覆盖；字段缺失 → 不动（本仓用 `Option<Option<..>>` 区分）。
    #[allow(clippy::option_option)]
    #[serde(default, deserialize_with = "double_option")]
    pub mcp_config: Option<Option<JsonValue>>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub invocation_targets: Option<Vec<InvocationTargetDto>>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub max_concurrent_tasks: Option<i32>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub thinking_level: Option<String>,
    #[serde(default)]
    pub service_tier: Option<String>,
    #[allow(clippy::option_option)]
    #[serde(default, deserialize_with = "double_option")]
    pub composio_toolkit_allowlist: Option<Option<Vec<String>>>,
}

/// 区分「字段缺失」与「显式 `null`」（上游 `rawFields` 的存在性判定）。
///
/// 三重语义（缺失 / `null` / 值）只能用嵌套 `Option` 表达，故豁免 clippy。
#[allow(clippy::option_option)]
fn double_option<'de, D, T>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(de).map(Some)
}

/// 上游 `UpdateAgentEnvRequest`。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct UpdateAgentEnvRequest {
    #[serde(default)]
    pub custom_env: Option<CustomEnv>,
}

/// 上游 `AttachLabelRequest`。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct AttachLabelRequest {
    #[serde(default)]
    pub label_id: Option<String>,
}

/// 上游 `parsePermissionInput` 的解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedPermission {
    pub mode: String,
    pub targets: Vec<(String, Uuid)>,
}

impl ResolvedPermission {
    /// 上游 `resolvedPermission.legacyVisibility()`。
    pub(crate) fn legacy_visibility(&self) -> String {
        if self.mode == PERMISSION_MODE_PUBLIC_TO
            && self.targets.iter().any(|(t, _)| t == TARGET_WORKSPACE)
        {
            VISIBILITY_WORKSPACE.to_string()
        } else {
            VISIBILITY_PRIVATE.to_string()
        }
    }
}

/// 上游 `parsePermissionInput`：`permission_mode` 权威；否则回退 legacy `visibility`；
/// 两者都没有 → `Ok(None)`（调用者默认）。
///
/// `public_to` 且解析后没有可命中目标 → 归一成单个 workspace 目标
/// （MUL-3963 裁决：空 allow-list 是「谁都不能用」的幻影）。
pub(crate) fn parse_permission_input(
    workspace_id: Uuid,
    permission_mode: Option<&str>,
    has_permission_mode: bool,
    targets: &[InvocationTargetDto],
    has_targets: bool,
    legacy_visibility: Option<&str>,
) -> Result<Option<ResolvedPermission>, String> {
    if !has_permission_mode && legacy_visibility.is_none() {
        return Ok(None);
    }
    if !has_permission_mode {
        return match legacy_visibility.unwrap_or("") {
            "workspace" => Ok(Some(ResolvedPermission {
                mode: PERMISSION_MODE_PUBLIC_TO.into(),
                targets: vec![(TARGET_WORKSPACE.into(), workspace_id)],
            })),
            "private" | "" => Ok(Some(ResolvedPermission {
                mode: mc_repos::agent::PERMISSION_MODE_PRIVATE.into(),
                targets: Vec::new(),
            })),
            _ => Err("visibility must be 'private' or 'workspace'".into()),
        };
    }

    let mode = match permission_mode.unwrap_or("") {
        "" => mc_repos::agent::PERMISSION_MODE_PRIVATE.to_string(),
        other => other.to_string(),
    };
    if mode != mc_repos::agent::PERMISSION_MODE_PRIVATE && mode != PERMISSION_MODE_PUBLIC_TO {
        return Err("permission_mode must be 'private' or 'public_to'".into());
    }
    let mut resolved = ResolvedPermission {
        mode,
        targets: Vec::new(),
    };
    if resolved.mode == mc_repos::agent::PERMISSION_MODE_PRIVATE {
        // private 忽略任何提交的目标：默认拒绝。
        return Ok(Some(resolved));
    }
    if has_targets {
        let mut seen: Vec<String> = Vec::new();
        for t in targets {
            match t.target_type.as_str() {
                TARGET_WORKSPACE => {
                    if seen.iter().any(|k| k == "workspace") {
                        continue;
                    }
                    seen.push("workspace".into());
                    resolved
                        .targets
                        .push((TARGET_WORKSPACE.into(), workspace_id));
                }
                "member" | "team" => {
                    let Some(raw) = t.target_id.as_deref().filter(|v| !v.is_empty()) else {
                        return Err(format!(
                            "{} invocation target requires target_id",
                            t.target_type
                        ));
                    };
                    let id = Uuid::parse_str(raw.trim()).map_err(|_| {
                        format!("{} invocation target_id is not a valid uuid", t.target_type)
                    })?;
                    let key = format!("{}:{raw}", t.target_type);
                    if seen.contains(&key) {
                        continue;
                    }
                    seen.push(key);
                    resolved.targets.push((t.target_type.clone(), id));
                }
                _ => {
                    return Err(
                        "invocation target_type must be 'workspace', 'member', or 'team'".into(),
                    )
                }
            }
        }
    }
    if resolved.targets.is_empty() {
        resolved
            .targets
            .push((TARGET_WORKSPACE.into(), workspace_id));
    }
    Ok(Some(resolved))
}
