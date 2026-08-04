//! Execution workspace adapter config 构造（与 Node
//! `services/execution-workspace-policy.ts` 的 `buildExecutionWorkspaceAdapterConfig` 1:1 对齐）。
//!
//! ## 设计
//! - 不可变 in/out：`agentConfig` clone 起步，按 mode / policy 增量修改
//! - 返回 `serde_json::Value`（保留原始 JSON 形状，方便 adapter 解析）

use std::collections::HashMap;

use super::parse::parse_execution_workspace_strategy;
use super::types::{
    mode, strategy_type, ExecutionWorkspaceStrategy, IssueExecutionWorkspaceSettings,
    ProjectExecutionWorkspacePolicy,
};

// ============================================================================
// build_execution_workspace_adapter_config
// ============================================================================

/// 与 Node `buildExecutionWorkspaceAdapterConfig` 1:1 对齐。
pub fn build_execution_workspace_adapter_config(
    input: BuildExecutionWorkspaceAdapterConfigInput<'_>,
) -> serde_json::Value {
    let mut next_config = input.agent_config.clone();

    let project_has_policy = input.project_policy.is_some_and(|p| p.enabled);
    let issue_has_workspace_overrides = input.issue_settings.is_some_and(|s| {
        s.mode.is_some() || s.workspace_strategy.is_some() || s.workspace_runtime.is_some()
    });
    let has_workspace_control = project_has_policy
        || issue_has_workspace_overrides
        || input.legacy_use_project_workspace == Some(false);

    if !has_workspace_control {
        return next_config;
    }

    // mode == "isolated_workspace" → set workspaceStrategy
    if input.mode == mode::ISOLATED_WORKSPACE {
        let strategy = input
            .issue_settings
            .and_then(|s| s.workspace_strategy.clone())
            .or_else(|| {
                input
                    .project_policy
                    .and_then(|p| p.workspace_strategy.clone())
            })
            .or_else(|| {
                next_config
                    .as_object()
                    .and_then(|o| o.get("workspaceStrategy"))
                    .and_then(parse_execution_workspace_strategy)
            })
            .unwrap_or_else(|| ExecutionWorkspaceStrategy::new(strategy_type::GIT_WORKTREE));
        next_config.as_object_mut().unwrap().insert(
            "workspaceStrategy".to_string(),
            serde_json::to_value(&strategy).unwrap(),
        );
    } else {
        // delete workspaceStrategy key
        if let Some(obj) = next_config.as_object_mut() {
            obj.remove("workspaceStrategy");
        }
    }

    // mode == "agent_default" → delete workspaceRuntime
    // otherwise → set from issue_settings or project_policy
    if input.mode == mode::AGENT_DEFAULT {
        if let Some(obj) = next_config.as_object_mut() {
            obj.remove("workspaceRuntime");
        }
    } else if let Some(runtime) = input
        .issue_settings
        .and_then(|s| s.workspace_runtime.clone())
        .or_else(|| {
            input
                .project_policy
                .and_then(|p| p.workspace_runtime.clone())
        })
    {
        let runtime_value = runtime_to_value(&runtime);
        next_config
            .as_object_mut()
            .unwrap()
            .insert("workspaceRuntime".to_string(), runtime_value);
    }

    next_config
}

fn runtime_to_value(runtime: &HashMap<String, serde_json::Value>) -> serde_json::Value {
    let obj: serde_json::Map<String, serde_json::Value> = runtime
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    serde_json::Value::Object(obj)
}

/// `build_execution_workspace_adapter_config` 的输入参数。
#[derive(Debug, Clone)]
pub struct BuildExecutionWorkspaceAdapterConfigInput<'a> {
    pub agent_config: &'a serde_json::Value,
    pub project_policy: Option<&'a ProjectExecutionWorkspacePolicy>,
    pub issue_settings: Option<&'a IssueExecutionWorkspaceSettings>,
    pub mode: &'a str,
    pub legacy_use_project_workspace: Option<bool>,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn policy(enabled: bool, mode_str: Option<&str>) -> ProjectExecutionWorkspacePolicy {
        ProjectExecutionWorkspacePolicy {
            enabled,
            default_mode: mode_str.map(String::from),
            ..Default::default()
        }
    }

    fn settings_with_mode(m: Option<&str>) -> IssueExecutionWorkspaceSettings {
        IssueExecutionWorkspaceSettings {
            mode: m.map(String::from),
            ..Default::default()
        }
    }

    // ----- no workspace control -----

    #[test]
    fn no_changes_when_no_policy_and_no_issue_overrides() {
        let config = json!({"workspaceStrategy": {"type": "git_worktree"}});
        let input = BuildExecutionWorkspaceAdapterConfigInput {
            agent_config: &config,
            project_policy: None,
            issue_settings: None,
            mode: mode::SHARED_WORKSPACE,
            legacy_use_project_workspace: None,
        };
        let result = build_execution_workspace_adapter_config(input);
        // No control → return as-is
        assert_eq!(
            result
                .pointer("/workspaceStrategy/type")
                .and_then(|v| v.as_str()),
            Some("git_worktree")
        );
    }

    #[test]
    fn legacy_false_implies_workspace_control() {
        let config = json!({});
        let input = BuildExecutionWorkspaceAdapterConfigInput {
            agent_config: &config,
            project_policy: None,
            issue_settings: None,
            mode: mode::SHARED_WORKSPACE,
            legacy_use_project_workspace: Some(false),
        };
        let result = build_execution_workspace_adapter_config(input);
        // shared_workspace non-isolated → delete workspaceStrategy; non-agent_default → no runtime change
        assert!(result
            .as_object()
            .unwrap()
            .get("workspaceStrategy")
            .is_none());
    }

    // ----- mode == isolated_workspace -----

    #[test]
    fn isolated_workspace_uses_issue_strategy() {
        let mut s = ExecutionWorkspaceStrategy::new(strategy_type::GIT_WORKTREE);
        s.base_ref = Some("main".to_string());
        let settings = IssueExecutionWorkspaceSettings {
            workspace_strategy: Some(s),
            ..Default::default()
        };
        let config = json!({});
        let input = BuildExecutionWorkspaceAdapterConfigInput {
            agent_config: &config,
            project_policy: Some(&policy(true, Some(mode::ISOLATED_WORKSPACE))),
            issue_settings: Some(&settings),
            mode: mode::ISOLATED_WORKSPACE,
            legacy_use_project_workspace: None,
        };
        let result = build_execution_workspace_adapter_config(input);
        assert_eq!(
            result
                .pointer("/workspaceStrategy/type")
                .and_then(|v| v.as_str()),
            Some("git_worktree")
        );
        assert_eq!(
            result
                .pointer("/workspaceStrategy/baseRef")
                .and_then(|v| v.as_str()),
            Some("main")
        );
    }

    #[test]
    fn isolated_workspace_uses_project_strategy() {
        let mut s = ExecutionWorkspaceStrategy::new(strategy_type::GIT_WORKTREE);
        s.base_ref = Some("develop".to_string());
        let policy = ProjectExecutionWorkspacePolicy {
            enabled: true,
            workspace_strategy: Some(s),
            ..Default::default()
        };
        let config = json!({});
        let input = BuildExecutionWorkspaceAdapterConfigInput {
            agent_config: &config,
            project_policy: Some(&policy),
            issue_settings: None,
            mode: mode::ISOLATED_WORKSPACE,
            legacy_use_project_workspace: None,
        };
        let result = build_execution_workspace_adapter_config(input);
        assert_eq!(
            result
                .pointer("/workspaceStrategy/baseRef")
                .and_then(|v| v.as_str()),
            Some("develop")
        );
    }

    #[test]
    fn isolated_workspace_uses_agent_config_strategy() {
        let config = json!({"workspaceStrategy": {"type": "git_worktree"}});
        let input = BuildExecutionWorkspaceAdapterConfigInput {
            agent_config: &config,
            project_policy: Some(&policy(true, Some(mode::ISOLATED_WORKSPACE))),
            issue_settings: None,
            mode: mode::ISOLATED_WORKSPACE,
            legacy_use_project_workspace: None,
        };
        let result = build_execution_workspace_adapter_config(input);
        assert_eq!(
            result
                .pointer("/workspaceStrategy/type")
                .and_then(|v| v.as_str()),
            Some("git_worktree")
        );
    }

    #[test]
    fn isolated_workspace_default_strategy_when_no_source() {
        let config = json!({});
        let input = BuildExecutionWorkspaceAdapterConfigInput {
            agent_config: &config,
            project_policy: Some(&policy(true, Some(mode::ISOLATED_WORKSPACE))),
            issue_settings: None,
            mode: mode::ISOLATED_WORKSPACE,
            legacy_use_project_workspace: None,
        };
        let result = build_execution_workspace_adapter_config(input);
        assert_eq!(
            result
                .pointer("/workspaceStrategy/type")
                .and_then(|v| v.as_str()),
            Some("git_worktree")
        );
    }

    // ----- non-isolated mode deletes workspaceStrategy -----

    #[test]
    fn shared_workspace_deletes_strategy() {
        let config = json!({"workspaceStrategy": {"type": "git_worktree"}});
        let settings = settings_with_mode(Some(mode::OPERATOR_BRANCH));
        let input = BuildExecutionWorkspaceAdapterConfigInput {
            agent_config: &config,
            project_policy: None,
            issue_settings: Some(&settings),
            mode: mode::OPERATOR_BRANCH,
            legacy_use_project_workspace: None,
        };
        let result = build_execution_workspace_adapter_config(input);
        assert!(result
            .as_object()
            .unwrap()
            .get("workspaceStrategy")
            .is_none());
    }

    // ----- mode == agent_default -----

    #[test]
    fn agent_default_deletes_runtime() {
        let config = json!({"workspaceRuntime": {"foo": "bar"}});
        let input = BuildExecutionWorkspaceAdapterConfigInput {
            agent_config: &config,
            project_policy: Some(&policy(true, Some("adapter_default"))),
            issue_settings: None,
            mode: mode::AGENT_DEFAULT,
            legacy_use_project_workspace: None,
        };
        let result = build_execution_workspace_adapter_config(input);
        assert!(result
            .as_object()
            .unwrap()
            .get("workspaceRuntime")
            .is_none());
    }

    #[test]
    fn shared_workspace_uses_issue_runtime() {
        let mut runtime = HashMap::new();
        runtime.insert("foo".to_string(), serde_json::json!("bar"));
        let settings = IssueExecutionWorkspaceSettings {
            workspace_runtime: Some(runtime),
            ..Default::default()
        };
        let config = json!({});
        let input = BuildExecutionWorkspaceAdapterConfigInput {
            agent_config: &config,
            project_policy: Some(&policy(true, Some(mode::SHARED_WORKSPACE))),
            issue_settings: Some(&settings),
            mode: mode::SHARED_WORKSPACE,
            legacy_use_project_workspace: None,
        };
        let result = build_execution_workspace_adapter_config(input);
        assert_eq!(
            result
                .pointer("/workspaceRuntime/foo")
                .and_then(|v| v.as_str()),
            Some("bar")
        );
    }

    #[test]
    fn shared_workspace_uses_project_runtime_when_no_issue_runtime() {
        let mut runtime = HashMap::new();
        runtime.insert("cmd".to_string(), serde_json::json!("echo"));
        let policy = ProjectExecutionWorkspacePolicy {
            enabled: true,
            workspace_runtime: Some(runtime),
            ..Default::default()
        };
        let config = json!({});
        let input = BuildExecutionWorkspaceAdapterConfigInput {
            agent_config: &config,
            project_policy: Some(&policy),
            issue_settings: None,
            mode: mode::SHARED_WORKSPACE,
            legacy_use_project_workspace: None,
        };
        let result = build_execution_workspace_adapter_config(input);
        assert_eq!(
            result
                .pointer("/workspaceRuntime/cmd")
                .and_then(|v| v.as_str()),
            Some("echo")
        );
    }
}
