//! Execution workspace 解析 / 默认值计算（与 Node
//! `services/execution-workspace-policy.ts` 的 resolve / mode helpers 1:1 对齐）。
//!
//! ## 包含
//! - `resolve_effective_workspace_strategy_type`
//! - `resolve_pinned_issue_workspace_strategy_type`
//! - `resolve_execution_workspace_mode`
//! - `default_issue_execution_workspace_settings_for_project`
//! - `issue_execution_workspace_mode_for_persisted_workspace`
//! - `resolve_execution_workspace_environment_id`

use super::parse::{as_string, parse_object};
use super::types::{
    default_mode, environment_source, mode, strategy_type, ExecutionWorkspaceEnvironmentResolution,
    IssueExecutionWorkspaceSettings, ParsedExecutionWorkspaceMode, ProjectExecutionWorkspacePolicy,
};

// ============================================================================
// resolve_effective_workspace_strategy_type
// ============================================================================

/// 解析 effective strategy type（与 Node `resolveEffectiveWorkspaceStrategyType` 1:1 对齐）。
///
/// - 优先 `config.workspaceStrategy.type`（若为四个合法值之一）
/// - 否则：mode == "agent_default" → "adapter_managed"，其他 → "project_primary"
pub fn resolve_effective_workspace_strategy_type(
    parsed_mode: &str,
    config: Option<&serde_json::Value>,
) -> String {
    let config_obj = config.map(|c| parse_object(c)).unwrap_or_default();
    let type_str = as_string(
        &config_obj
            .get("workspaceStrategy")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        "",
    );
    // Wait — Node code reads config.workspaceStrategy.type directly via parseObject:
    //   const workspaceStrategy = parseObject(config?.workspaceStrategy);
    //   const type = asString(workspaceStrategy.type, "");
    // So we need a nested parseObject here.
    let _ = type_str;

    let workspace_strategy_obj = config_obj
        .get("workspaceStrategy")
        .map(|v| parse_object(v))
        .unwrap_or_default();
    let type_str = as_string(
        &workspace_strategy_obj
            .get("type")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        "",
    );
    if type_str == strategy_type::PROJECT_PRIMARY
        || type_str == strategy_type::GIT_WORKTREE
        || type_str == strategy_type::ADAPTER_MANAGED
        || type_str == strategy_type::CLOUD_SANDBOX
    {
        return type_str;
    }
    if parsed_mode == mode::AGENT_DEFAULT {
        strategy_type::ADAPTER_MANAGED.to_string()
    } else {
        strategy_type::PROJECT_PRIMARY.to_string()
    }
}

// ============================================================================
// resolve_pinned_issue_workspace_strategy_type
// ============================================================================

/// 解析 issue 固定的 strategy type（与 Node `resolvePinnedIssueWorkspaceStrategyType` 1:1 对齐）。
///
/// - 优先 `issueSettings.workspaceStrategy.type`
/// - 否则：mode == "agent_default" → "adapter_managed"，其他 → "project_primary"
pub fn resolve_pinned_issue_workspace_strategy_type(
    parsed_mode: &str,
    issue_settings: Option<&IssueExecutionWorkspaceSettings>,
) -> String {
    let type_str = issue_settings
        .and_then(|s| s.workspace_strategy.as_ref())
        .map(|s| s.r#type.as_str())
        .unwrap_or("");
    if type_str == strategy_type::PROJECT_PRIMARY
        || type_str == strategy_type::GIT_WORKTREE
        || type_str == strategy_type::ADAPTER_MANAGED
        || type_str == strategy_type::CLOUD_SANDBOX
    {
        return type_str.to_string();
    }
    if parsed_mode == mode::AGENT_DEFAULT {
        strategy_type::ADAPTER_MANAGED.to_string()
    } else {
        strategy_type::PROJECT_PRIMARY.to_string()
    }
}

// ============================================================================
// default_issue_execution_workspace_settings_for_project
// ============================================================================

/// 默认 issue settings（与 Node `defaultIssueExecutionWorkspaceSettingsForProject` 1:1 对齐）。
///
/// - policy 未启用 → None
/// - 否则按 defaultMode 选择 mode 字段
pub fn default_issue_execution_workspace_settings_for_project(
    project_policy: Option<&ProjectExecutionWorkspacePolicy>,
) -> Option<IssueExecutionWorkspaceSettings> {
    let policy = project_policy?;
    if !policy.enabled {
        return None;
    }
    let m = policy.default_mode.as_deref().unwrap_or("");
    let resolved_mode = if m == default_mode::ISOLATED_WORKSPACE {
        mode::ISOLATED_WORKSPACE
    } else if m == default_mode::OPERATOR_BRANCH {
        mode::OPERATOR_BRANCH
    } else if m == default_mode::ADAPTER_DEFAULT {
        mode::AGENT_DEFAULT
    } else {
        mode::SHARED_WORKSPACE
    };
    Some(IssueExecutionWorkspaceSettings {
        mode: Some(resolved_mode.to_string()),
        ..Default::default()
    })
}

// ============================================================================
// issue_execution_workspace_mode_for_persisted_workspace
// =========================================================================}

/// 把持久化的 mode 字符串映射回 issue settings.mode（与 Node
/// `issueExecutionWorkspaceModeForPersistedWorkspace` 1:1 对齐）。
pub fn issue_execution_workspace_mode_for_persisted_workspace(mode: Option<&str>) -> String {
    let m = match mode {
        None => return mode::AGENT_DEFAULT.to_string(),
        Some(v) => v,
    };
    if m == mode::ISOLATED_WORKSPACE || m == mode::OPERATOR_BRANCH || m == mode::SHARED_WORKSPACE {
        return m.to_string();
    }
    if m == strategy_type::ADAPTER_MANAGED || m == strategy_type::CLOUD_SANDBOX {
        return mode::AGENT_DEFAULT.to_string();
    }
    mode::SHARED_WORKSPACE.to_string()
}

// ============================================================================
// resolve_execution_workspace_mode
// ============================================================================

/// 解析最终 effective mode（与 Node `resolveExecutionWorkspaceMode` 1:1 对齐）。
///
/// 优先级：
/// 1. issue settings.mode（除非 inherit / reuse_existing）
/// 2. project policy.enabled + defaultMode
/// 3. legacyUseProjectWorkspace == false → agent_default
/// 4. 默认 shared_workspace
pub fn resolve_execution_workspace_mode(
    project_policy: Option<&ProjectExecutionWorkspacePolicy>,
    issue_settings: Option<&IssueExecutionWorkspaceSettings>,
    legacy_use_project_workspace: Option<bool>,
) -> ParsedExecutionWorkspaceMode {
    if let Some(settings) = issue_settings {
        if let Some(m) = &settings.mode {
            if m != mode::INHERIT && m != mode::REUSE_EXISTING {
                return m.clone();
            }
        }
    }
    if let Some(policy) = project_policy {
        if policy.enabled {
            let dm = policy.default_mode.as_deref().unwrap_or("");
            if dm == default_mode::ISOLATED_WORKSPACE {
                return mode::ISOLATED_WORKSPACE.to_string();
            }
            if dm == default_mode::OPERATOR_BRANCH {
                return mode::OPERATOR_BRANCH.to_string();
            }
            if dm == default_mode::ADAPTER_DEFAULT {
                return mode::AGENT_DEFAULT.to_string();
            }
            return mode::SHARED_WORKSPACE.to_string();
        }
    }
    if legacy_use_project_workspace == Some(false) {
        return mode::AGENT_DEFAULT.to_string();
    }
    mode::SHARED_WORKSPACE.to_string()
}

// ============================================================================
// resolve_execution_workspace_environment_id
// ============================================================================

/// 解析 effective environment ID（与 Node `resolveExecutionWorkspaceEnvironmentId` 1:1 对齐）。
///
/// 优先级：agent > instance > local default
pub fn resolve_execution_workspace_environment_id(
    agent_default_environment_id: Option<&str>,
    instance_default_environment_id: Option<&str>,
    local_default_environment_id: &str,
) -> ExecutionWorkspaceEnvironmentResolution {
    if let Some(id) = agent_default_environment_id {
        return ExecutionWorkspaceEnvironmentResolution {
            environment_id: id.to_string(),
            source: environment_source::AGENT.to_string(),
        };
    }
    if let Some(id) = instance_default_environment_id {
        return ExecutionWorkspaceEnvironmentResolution {
            environment_id: id.to_string(),
            source: environment_source::INSTANCE.to_string(),
        };
    }
    ExecutionWorkspaceEnvironmentResolution {
        environment_id: local_default_environment_id.to_string(),
        source: environment_source::DEFAULT.to_string(),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ----- resolve_effective_workspace_strategy_type -----

    #[test]
    fn resolve_strategy_explicit_project_primary() {
        assert_eq!(
            resolve_effective_workspace_strategy_type(
                mode::ISOLATED_WORKSPACE,
                Some(&json!({"workspaceStrategy": {"type": "project_primary"}})),
            ),
            strategy_type::PROJECT_PRIMARY
        );
    }

    #[test]
    fn resolve_strategy_explicit_git_worktree() {
        assert_eq!(
            resolve_effective_workspace_strategy_type(
                mode::ISOLATED_WORKSPACE,
                Some(&json!({"workspaceStrategy": {"type": "git_worktree"}})),
            ),
            strategy_type::GIT_WORKTREE
        );
    }

    #[test]
    fn resolve_strategy_explicit_adapter_managed() {
        assert_eq!(
            resolve_effective_workspace_strategy_type(
                mode::AGENT_DEFAULT,
                Some(&json!({"workspaceStrategy": {"type": "adapter_managed"}})),
            ),
            strategy_type::ADAPTER_MANAGED
        );
    }

    #[test]
    fn resolve_strategy_default_agent_default_yields_adapter_managed() {
        assert_eq!(
            resolve_effective_workspace_strategy_type(mode::AGENT_DEFAULT, None),
            strategy_type::ADAPTER_MANAGED
        );
    }

    #[test]
    fn resolve_strategy_default_other_yields_project_primary() {
        assert_eq!(
            resolve_effective_workspace_strategy_type(mode::SHARED_WORKSPACE, None),
            strategy_type::PROJECT_PRIMARY
        );
    }

    #[test]
    fn resolve_strategy_invalid_type_falls_back_to_default() {
        let result = resolve_effective_workspace_strategy_type(
            mode::AGENT_DEFAULT,
            Some(&json!({"workspaceStrategy": {"type": "bogus"}})),
        );
        assert_eq!(result, strategy_type::ADAPTER_MANAGED);
    }

    // ----- resolve_pinned_issue_workspace_strategy_type -----

    #[test]
    fn resolve_pinned_uses_issue_strategy() {
        let settings = IssueExecutionWorkspaceSettings {
            workspace_strategy: Some(
                crate::execution_workspace_policy::types::ExecutionWorkspaceStrategy::new(
                    strategy_type::CLOUD_SANDBOX,
                ),
            ),
            ..Default::default()
        };
        assert_eq!(
            resolve_pinned_issue_workspace_strategy_type(mode::ISOLATED_WORKSPACE, Some(&settings)),
            strategy_type::CLOUD_SANDBOX
        );
    }

    #[test]
    fn resolve_pinned_fallback_default() {
        let settings = IssueExecutionWorkspaceSettings::default();
        assert_eq!(
            resolve_pinned_issue_workspace_strategy_type(mode::SHARED_WORKSPACE, Some(&settings)),
            strategy_type::PROJECT_PRIMARY
        );
        assert_eq!(
            resolve_pinned_issue_workspace_strategy_type(mode::AGENT_DEFAULT, Some(&settings)),
            strategy_type::ADAPTER_MANAGED
        );
    }

    // ----- default_issue_execution_workspace_settings_for_project -----

    #[test]
    fn default_issue_settings_disabled_returns_none() {
        let policy = ProjectExecutionWorkspacePolicy {
            enabled: false,
            ..Default::default()
        };
        assert!(default_issue_execution_workspace_settings_for_project(Some(&policy)).is_none());
    }

    #[test]
    fn default_issue_settings_none_returns_none() {
        assert!(default_issue_execution_workspace_settings_for_project(None).is_none());
    }

    #[test]
    fn default_issue_settings_isolated_workspace() {
        let policy = ProjectExecutionWorkspacePolicy {
            enabled: true,
            default_mode: Some(default_mode::ISOLATED_WORKSPACE.to_string()),
            ..Default::default()
        };
        let s = default_issue_execution_workspace_settings_for_project(Some(&policy)).unwrap();
        assert_eq!(s.mode.as_deref(), Some(mode::ISOLATED_WORKSPACE));
    }

    #[test]
    fn default_issue_settings_shared_workspace_default() {
        let policy = ProjectExecutionWorkspacePolicy {
            enabled: true,
            ..Default::default()
        };
        let s = default_issue_execution_workspace_settings_for_project(Some(&policy)).unwrap();
        assert_eq!(s.mode.as_deref(), Some(mode::SHARED_WORKSPACE));
    }

    // ----- issue_execution_workspace_mode_for_persisted_workspace -----

    #[test]
    fn persisted_mode_none_is_agent_default() {
        assert_eq!(
            issue_execution_workspace_mode_for_persisted_workspace(None),
            mode::AGENT_DEFAULT
        );
    }

    #[test]
    fn persisted_mode_known_passes_through() {
        for m in [
            mode::ISOLATED_WORKSPACE,
            mode::OPERATOR_BRANCH,
            mode::SHARED_WORKSPACE,
        ] {
            assert_eq!(
                issue_execution_workspace_mode_for_persisted_workspace(Some(m)),
                m
            );
        }
    }

    #[test]
    fn persisted_mode_adapter_managed_maps_to_agent_default() {
        assert_eq!(
            issue_execution_workspace_mode_for_persisted_workspace(Some(
                strategy_type::ADAPTER_MANAGED
            )),
            mode::AGENT_DEFAULT
        );
    }

    #[test]
    fn persisted_mode_cloud_sandbox_maps_to_agent_default() {
        assert_eq!(
            issue_execution_workspace_mode_for_persisted_workspace(Some(
                strategy_type::CLOUD_SANDBOX
            )),
            mode::AGENT_DEFAULT
        );
    }

    #[test]
    fn persisted_mode_unknown_defaults_to_shared_workspace() {
        assert_eq!(
            issue_execution_workspace_mode_for_persisted_workspace(Some("bogus")),
            mode::SHARED_WORKSPACE
        );
    }

    // ----- resolve_execution_workspace_mode -----

    #[test]
    fn resolve_mode_issue_settings_takes_priority() {
        let settings = IssueExecutionWorkspaceSettings {
            mode: Some(mode::OPERATOR_BRANCH.to_string()),
            ..Default::default()
        };
        let policy = ProjectExecutionWorkspacePolicy {
            enabled: true,
            default_mode: Some(default_mode::ISOLATED_WORKSPACE.to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_execution_workspace_mode(Some(&policy), Some(&settings), None),
            mode::OPERATOR_BRANCH
        );
    }

    #[test]
    fn resolve_mode_inherit_falls_through_to_policy() {
        let settings = IssueExecutionWorkspaceSettings {
            mode: Some(mode::INHERIT.to_string()),
            ..Default::default()
        };
        let policy = ProjectExecutionWorkspacePolicy {
            enabled: true,
            default_mode: Some(default_mode::OPERATOR_BRANCH.to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_execution_workspace_mode(Some(&policy), Some(&settings), None),
            mode::OPERATOR_BRANCH
        );
    }

    #[test]
    fn resolve_mode_reuse_existing_falls_through_to_policy() {
        let settings = IssueExecutionWorkspaceSettings {
            mode: Some(mode::REUSE_EXISTING.to_string()),
            ..Default::default()
        };
        let policy = ProjectExecutionWorkspacePolicy {
            enabled: true,
            default_mode: Some(default_mode::ISOLATED_WORKSPACE.to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_execution_workspace_mode(Some(&policy), Some(&settings), None),
            mode::ISOLATED_WORKSPACE
        );
    }

    #[test]
    fn resolve_mode_policy_disabled_falls_through_to_legacy() {
        let policy = ProjectExecutionWorkspacePolicy {
            enabled: false,
            ..Default::default()
        };
        assert_eq!(
            resolve_execution_workspace_mode(Some(&policy), None, Some(false)),
            mode::AGENT_DEFAULT
        );
    }

    #[test]
    fn resolve_mode_default_is_shared_workspace() {
        assert_eq!(
            resolve_execution_workspace_mode(None, None, None),
            mode::SHARED_WORKSPACE
        );
    }

    #[test]
    fn resolve_mode_policy_adapter_default_yields_agent_default() {
        let policy = ProjectExecutionWorkspacePolicy {
            enabled: true,
            default_mode: Some(default_mode::ADAPTER_DEFAULT.to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_execution_workspace_mode(Some(&policy), None, None),
            mode::AGENT_DEFAULT
        );
    }

    // ----- resolve_execution_workspace_environment_id -----

    #[test]
    fn environment_id_priority_agent() {
        let r = resolve_execution_workspace_environment_id(
            Some("agent-env"),
            Some("instance-env"),
            "default-env",
        );
        assert_eq!(r.environment_id, "agent-env");
        assert_eq!(r.source, environment_source::AGENT);
    }

    #[test]
    fn environment_id_priority_instance() {
        let r =
            resolve_execution_workspace_environment_id(None, Some("instance-env"), "default-env");
        assert_eq!(r.environment_id, "instance-env");
        assert_eq!(r.source, environment_source::INSTANCE);
    }

    #[test]
    fn environment_id_priority_local_default() {
        let r = resolve_execution_workspace_environment_id(None, None, "default-env");
        assert_eq!(r.environment_id, "default-env");
        assert_eq!(r.source, environment_source::DEFAULT);
    }
}
