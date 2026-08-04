//! Execution workspace policy 解析器（与 Node
//! `services/execution-workspace-policy.ts` 的 `parseExecutionWorkspaceStrategy` /
//! `parseProjectExecutionWorkspacePolicy` / `parseIssueExecutionWorkspaceSettings` /
//! `selectEnvironmentExecutionWorkspaceSettings` 1:1 对齐）。
//!
//! ## 设计原则
//! - 所有 parser 接受 `&serde_json::Value`（与 Node `unknown` 等价）
//! - 未知字段被忽略；缺失字段使用 None / 默认值
//! - 严格 enum-like 字段（type / mode）必须在白名单内，否则该 parse 返回 None
//! - 空对象 / 非对象 → 返回 None

use std::collections::HashMap;

use super::types::{
    default_mode, mode, strategy_type, ExecutionWorkspaceStrategy, IssueExecutionWorkspaceSettings,
    NetworkEgress, ProjectExecutionWorkspacePolicy,
};

// ============================================================================
// Helpers (analogous to Node `parseObject` / `asString`)
// ============================================================================

/// 把任意 `serde_json::Value` 投影成可索引的 `HashMap<String, serde_json::Value>`。
///
/// 与 Node `parseObject` 1:1 对齐：
/// - 非对象 → 空 HashMap
/// - 数组 / null / primitive → 空 HashMap
pub fn parse_object(raw: &serde_json::Value) -> HashMap<String, serde_json::Value> {
    raw.as_object()
        .map(|obj| obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

/// 取字符串字段（与 Node `asString(value, fallback)` 1:1 对齐）。
pub fn as_string(raw: &serde_json::Value, fallback: &str) -> String {
    raw.as_str().unwrap_or(fallback).to_string()
}

// ============================================================================
// parse_execution_workspace_strategy
// ============================================================================

/// 解析 strategy 对象（与 Node `parseExecutionWorkspaceStrategy` 1:1 对齐）。
///
/// - type 必须在 `strategy_type::*` 四个值之一；否则返回 None
/// - 字符串字段保留；非字符串字段被忽略
pub fn parse_execution_workspace_strategy(
    raw: &serde_json::Value,
) -> Option<ExecutionWorkspaceStrategy> {
    let parsed = parse_object(raw);
    let type_str = as_string(
        &parsed
            .get("type")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        "",
    );
    if type_str != strategy_type::PROJECT_PRIMARY
        && type_str != strategy_type::GIT_WORKTREE
        && type_str != strategy_type::ADAPTER_MANAGED
        && type_str != strategy_type::CLOUD_SANDBOX
    {
        return None;
    }
    let mut s = ExecutionWorkspaceStrategy::new(type_str);
    if let Some(v) = parsed.get("baseRef") {
        if let Some(s_val) = v.as_str() {
            s.base_ref = Some(s_val.to_string());
        }
    }
    if let Some(v) = parsed.get("branchTemplate") {
        if let Some(s_val) = v.as_str() {
            s.branch_template = Some(s_val.to_string());
        }
    }
    if let Some(v) = parsed.get("worktreeParentDir") {
        if let Some(s_val) = v.as_str() {
            s.worktree_parent_dir = Some(s_val.to_string());
        }
    }
    if let Some(v) = parsed.get("provisionCommand") {
        if let Some(s_val) = v.as_str() {
            s.provision_command = Some(s_val.to_string());
        }
    }
    if let Some(v) = parsed.get("teardownCommand") {
        if let Some(s_val) = v.as_str() {
            s.teardown_command = Some(s_val.to_string());
        }
    }
    Some(s)
}

// ============================================================================
// parse_project_execution_workspace_policy
// ============================================================================

/// 解析项目级 policy（与 Node `parseProjectExecutionWorkspacePolicy` 1:1 对齐）。
///
/// - 空对象 → None
/// - `enabled` 缺省 false；非 boolean → false
/// - `defaultMode` 字符串归一化：`"project_primary"` → `"shared_workspace"`，
///   `"isolated"` → `"isolated_workspace"`，未知 → 丢弃
/// - `workspaceStrategy` 用上面 helper
/// - `workspaceRuntime` 保留原始 JSON object shape
pub fn parse_project_execution_workspace_policy(
    raw: &serde_json::Value,
) -> Option<ProjectExecutionWorkspacePolicy> {
    let parsed = parse_object(raw);
    if parsed.is_empty() {
        return None;
    }

    let enabled = parsed
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let workspace_strategy = parsed
        .get("workspaceStrategy")
        .and_then(parse_execution_workspace_strategy);

    let default_mode_str = as_string(
        &parsed
            .get("defaultMode")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        "",
    );
    let normalized_default_mode = match default_mode_str.as_str() {
        default_mode::SHARED_WORKSPACE
        | default_mode::ISOLATED_WORKSPACE
        | default_mode::OPERATOR_BRANCH
        | default_mode::ADAPTER_DEFAULT => Some(default_mode_str),
        "project_primary" => Some(default_mode::SHARED_WORKSPACE.to_string()),
        "isolated" => Some(default_mode::ISOLATED_WORKSPACE.to_string()),
        _ => None,
    };

    let default_project_workspace_id = parsed
        .get("defaultProjectWorkspaceId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let allow_issue_override = parsed.get("allowIssueOverride").and_then(|v| v.as_bool());

    let workspace_runtime = parsed.get("workspaceRuntime").and_then(|v| {
        if v.is_object() {
            Some(parse_object(v))
        } else {
            None
        }
    });

    let mut policy = ProjectExecutionWorkspacePolicy {
        enabled,
        ..Default::default()
    };
    if let Some(dm) = normalized_default_mode {
        policy.default_mode = Some(dm);
    }
    if let Some(a) = allow_issue_override {
        policy.allow_issue_override = Some(a);
    }
    if let Some(id) = default_project_workspace_id {
        policy.default_project_workspace_id = Some(id);
    }
    if let Some(s) = workspace_strategy {
        policy.workspace_strategy = Some(s);
    }
    if let Some(r) = workspace_runtime {
        policy.workspace_runtime = Some(r);
    }
    Some(policy)
}

// ============================================================================
// parse_issue_execution_workspace_settings
// ============================================================================

/// 解析 issue 级 settings（与 Node `parseIssueExecutionWorkspaceSettings` 1:1 对齐）。
///
/// - 空对象 → None
/// - `mode` 字符串归一化（与 default_mode 类似）
/// - `networkEgress.allowFqdns` / `allowCidrs` 数组 → trim + lowercase + 去空
pub fn parse_issue_execution_workspace_settings(
    raw: &serde_json::Value,
) -> Option<IssueExecutionWorkspaceSettings> {
    let parsed = parse_object(raw);
    if parsed.is_empty() {
        return None;
    }

    let workspace_strategy = parsed
        .get("workspaceStrategy")
        .and_then(parse_execution_workspace_strategy);

    let mode_str = as_string(
        &parsed
            .get("mode")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        "",
    );
    let normalized_mode = match mode_str.as_str() {
        mode::INHERIT
        | mode::SHARED_WORKSPACE
        | mode::ISOLATED_WORKSPACE
        | mode::OPERATOR_BRANCH
        | mode::REUSE_EXISTING
        | mode::AGENT_DEFAULT => Some(mode_str),
        "project_primary" => Some(mode::SHARED_WORKSPACE.to_string()),
        "isolated" => Some(mode::ISOLATED_WORKSPACE.to_string()),
        _ => None,
    };

    let workspace_runtime = parsed.get("workspaceRuntime").and_then(|v| {
        if v.is_object() {
            Some(parse_object(v))
        } else {
            None
        }
    });

    let network_egress = parsed.get("networkEgress").and_then(|v| {
        if !v.is_object() {
            return None;
        }
        let egress_obj = parse_object(v);
        let allow_fqdns: Vec<String> = egress_obj
            .get("allowFqdns")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| e.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_lowercase())
                    .collect()
            })
            .unwrap_or_default();
        let allow_cidrs: Vec<String> = egress_obj
            .get("allowCidrs")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| e.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        if allow_fqdns.is_empty() && allow_cidrs.is_empty() {
            None
        } else {
            Some(NetworkEgress {
                allow_fqdns,
                allow_cidrs,
            })
        }
    });

    let mut settings = IssueExecutionWorkspaceSettings {
        ..Default::default()
    };
    if let Some(m) = normalized_mode {
        settings.mode = Some(m);
    }
    if let Some(s) = workspace_strategy {
        settings.workspace_strategy = Some(s);
    }
    if let Some(r) = workspace_runtime {
        settings.workspace_runtime = Some(r);
    }
    if let Some(ne) = network_egress {
        settings.network_egress = Some(ne);
    }
    Some(settings)
}

// ============================================================================
// select_environment_execution_workspace_settings
// ============================================================================

/// 按 `isolatedWorkspacesEnabled` 选择 settings 投影（与 Node
/// `selectEnvironmentExecutionWorkspaceSettings` 1:1 对齐）。
///
/// - enabled → 保留全部 settings
/// - disabled → 仅保留 `networkEgress`（去掉 mode / strategy / runtime）
/// - parsedSettings 为 None → 返回 None
pub fn select_environment_execution_workspace_settings(
    parsed_settings: Option<IssueExecutionWorkspaceSettings>,
    isolated_workspaces_enabled: bool,
) -> Option<IssueExecutionWorkspaceSettings> {
    let parsed = parsed_settings?;
    if isolated_workspaces_enabled {
        return Some(parsed);
    }
    match parsed.network_egress {
        Some(ne) => Some(IssueExecutionWorkspaceSettings {
            network_egress: Some(ne),
            ..Default::default()
        }),
        None => None,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ----- parse_object -----

    #[test]
    fn parse_object_non_object_returns_empty() {
        assert!(parse_object(&json!(null)).is_empty());
        assert!(parse_object(&json!("string")).is_empty());
        assert!(parse_object(&json!(42)).is_empty());
        assert!(parse_object(&json!([1, 2, 3])).is_empty());
    }

    #[test]
    fn parse_object_returns_all_keys() {
        let v = json!({"a": 1, "b": "x"});
        let obj = parse_object(&v);
        assert_eq!(obj.len(), 2);
        assert!(obj.contains_key("a"));
        assert!(obj.contains_key("b"));
    }

    // ----- as_string -----

    #[test]
    fn as_string_returns_str_when_string() {
        assert_eq!(as_string(&json!("hello"), "fallback"), "hello");
    }

    #[test]
    fn as_string_returns_fallback_otherwise() {
        assert_eq!(as_string(&json!(42), "fb"), "fb");
        assert_eq!(as_string(&json!(null), "fb"), "fb");
        assert_eq!(as_string(&json!({}), "fb"), "fb");
    }

    // ----- parse_execution_workspace_strategy -----

    #[test]
    fn parse_strategy_valid_project_primary() {
        let s = parse_execution_workspace_strategy(&json!({
            "type": "project_primary"
        }))
        .unwrap();
        assert_eq!(s.r#type, "project_primary");
    }

    #[test]
    fn parse_strategy_valid_git_worktree() {
        let s = parse_execution_workspace_strategy(&json!({
            "type": "git_worktree",
            "baseRef": "main",
            "branchTemplate": "agent/{id}",
            "worktreeParentDir": "/tmp/wt",
            "provisionCommand": "echo provision",
            "teardownCommand": "echo teardown"
        }))
        .unwrap();
        assert_eq!(s.r#type, "git_worktree");
        assert_eq!(s.base_ref.as_deref(), Some("main"));
        assert_eq!(s.branch_template.as_deref(), Some("agent/{id}"));
        assert_eq!(s.worktree_parent_dir.as_deref(), Some("/tmp/wt"));
        assert_eq!(s.provision_command.as_deref(), Some("echo provision"));
        assert_eq!(s.teardown_command.as_deref(), Some("echo teardown"));
    }

    #[test]
    fn parse_strategy_unknown_type_returns_none() {
        assert!(parse_execution_workspace_strategy(&json!({"type": "bogus"})).is_none());
        assert!(parse_execution_workspace_strategy(&json!({"type": ""})).is_none());
        assert!(parse_execution_workspace_strategy(&json!({})).is_none());
        assert!(parse_execution_workspace_strategy(&json!("not an object")).is_none());
    }

    #[test]
    fn parse_strategy_drops_non_string_fields() {
        let s = parse_execution_workspace_strategy(&json!({
            "type": "git_worktree",
            "baseRef": 42,
            "branchTemplate": true
        }))
        .unwrap();
        assert_eq!(s.base_ref, None);
        assert_eq!(s.branch_template, None);
    }

    // ----- parse_project_execution_workspace_policy -----

    #[test]
    fn parse_policy_empty_returns_none() {
        assert!(parse_project_execution_workspace_policy(&json!({})).is_none());
        assert!(parse_project_execution_workspace_policy(&json!(null)).is_none());
    }

    #[test]
    fn parse_policy_minimal() {
        let p = parse_project_execution_workspace_policy(&json!({
            "enabled": true
        }))
        .unwrap();
        assert!(p.enabled);
        assert_eq!(p.default_mode, None);
    }

    #[test]
    fn parse_policy_normalizes_project_primary_to_shared_workspace() {
        let p = parse_project_execution_workspace_policy(&json!({
            "enabled": true,
            "defaultMode": "project_primary"
        }))
        .unwrap();
        assert_eq!(p.default_mode.as_deref(), Some("shared_workspace"));
    }

    #[test]
    fn parse_policy_normalizes_isolated_to_isolated_workspace() {
        let p = parse_project_execution_workspace_policy(&json!({
            "enabled": true,
            "defaultMode": "isolated"
        }))
        .unwrap();
        assert_eq!(p.default_mode.as_deref(), Some("isolated_workspace"));
    }

    #[test]
    fn parse_policy_accepts_canonical_default_modes() {
        for mode in [
            "shared_workspace",
            "isolated_workspace",
            "operator_branch",
            "adapter_default",
        ] {
            let p = parse_project_execution_workspace_policy(&json!({
                "enabled": true,
                "defaultMode": mode
            }))
            .unwrap();
            assert_eq!(p.default_mode.as_deref(), Some(mode));
        }
    }

    #[test]
    fn parse_policy_unknown_default_mode_dropped() {
        let p = parse_project_execution_workspace_policy(&json!({
            "enabled": true,
            "defaultMode": "bogus"
        }))
        .unwrap();
        assert_eq!(p.default_mode, None);
    }

    #[test]
    fn parse_policy_workspace_strategy() {
        let p = parse_project_execution_workspace_policy(&json!({
            "enabled": true,
            "workspaceStrategy": {"type": "git_worktree"}
        }))
        .unwrap();
        assert!(p.workspace_strategy.is_some());
        assert_eq!(p.workspace_strategy.unwrap().r#type, "git_worktree");
    }

    #[test]
    fn parse_policy_workspace_runtime_kept_as_object() {
        let p = parse_project_execution_workspace_policy(&json!({
            "enabled": true,
            "workspaceRuntime": {"foo": "bar"}
        }))
        .unwrap();
        let wr = p.workspace_runtime.unwrap();
        assert_eq!(wr.get("foo").unwrap().as_str(), Some("bar"));
    }

    #[test]
    fn parse_policy_workspace_runtime_non_object_dropped() {
        let p = parse_project_execution_workspace_policy(&json!({
            "enabled": true,
            "workspaceRuntime": "not-an-object"
        }))
        .unwrap();
        assert_eq!(p.workspace_runtime, None);
    }

    #[test]
    fn parse_policy_allow_issue_override() {
        let p = parse_project_execution_workspace_policy(&json!({
            "enabled": true,
            "allowIssueOverride": false
        }))
        .unwrap();
        assert_eq!(p.allow_issue_override, Some(false));
    }

    #[test]
    fn parse_policy_default_project_workspace_id() {
        let p = parse_project_execution_workspace_policy(&json!({
            "enabled": true,
            "defaultProjectWorkspaceId": "ws-1"
        }))
        .unwrap();
        assert_eq!(p.default_project_workspace_id.as_deref(), Some("ws-1"));
    }

    #[test]
    fn parse_policy_default_project_workspace_id_empty_dropped() {
        let p = parse_project_execution_workspace_policy(&json!({
            "enabled": true,
            "defaultProjectWorkspaceId": ""
        }))
        .unwrap();
        assert_eq!(p.default_project_workspace_id, None);
    }

    // ----- parse_issue_execution_workspace_settings -----

    #[test]
    fn parse_issue_settings_empty_returns_none() {
        assert!(parse_issue_execution_workspace_settings(&json!({})).is_none());
    }

    #[test]
    fn parse_issue_settings_normalizes_modes() {
        let s = parse_issue_execution_workspace_settings(&json!({
            "mode": "project_primary"
        }))
        .unwrap();
        assert_eq!(s.mode.as_deref(), Some("shared_workspace"));

        let s = parse_issue_execution_workspace_settings(&json!({
            "mode": "isolated"
        }))
        .unwrap();
        assert_eq!(s.mode.as_deref(), Some("isolated_workspace"));
    }

    #[test]
    fn parse_issue_settings_accepts_canonical_modes() {
        for m in [
            "inherit",
            "shared_workspace",
            "isolated_workspace",
            "operator_branch",
            "reuse_existing",
            "agent_default",
        ] {
            let s = parse_issue_execution_workspace_settings(&json!({"mode": m})).unwrap();
            assert_eq!(s.mode.as_deref(), Some(m));
        }
    }

    #[test]
    fn parse_issue_settings_unknown_mode_dropped() {
        let s = parse_issue_execution_workspace_settings(&json!({
            "mode": "bogus"
        }))
        .unwrap();
        assert_eq!(s.mode, None);
    }

    #[test]
    fn parse_issue_settings_network_egress_filters() {
        let s = parse_issue_execution_workspace_settings(&json!({
            "networkEgress": {
                "allowFqdns": ["Example.COM", "  ", "Foo.Bar"],
                "allowCidrs": ["10.0.0.0/8", "  ", ""]
            }
        }))
        .unwrap();
        let ne = s.network_egress.unwrap();
        assert_eq!(ne.allow_fqdns, vec!["example.com", "foo.bar"]);
        assert_eq!(ne.allow_cidrs, vec!["10.0.0.0/8"]);
    }

    #[test]
    fn parse_issue_settings_network_egress_empty_dropped() {
        let s = parse_issue_execution_workspace_settings(&json!({
            "networkEgress": {
                "allowFqdns": [],
                "allowCidrs": []
            }
        }))
        .unwrap();
        assert_eq!(s.network_egress, None);
    }

    #[test]
    fn parse_issue_settings_network_egress_non_object_dropped() {
        let s = parse_issue_execution_workspace_settings(&json!({
            "networkEgress": "not-an-object"
        }))
        .unwrap();
        assert_eq!(s.network_egress, None);
    }

    // ----- select_environment_execution_workspace_settings -----

    #[test]
    fn select_returns_parsed_when_isolated_enabled() {
        let parsed = IssueExecutionWorkspaceSettings {
            mode: Some("isolated_workspace".to_string()),
            ..Default::default()
        };
        let result = select_environment_execution_workspace_settings(Some(parsed.clone()), true);
        assert_eq!(result, Some(parsed));
    }

    #[test]
    fn select_strips_to_network_egress_when_disabled() {
        let parsed = IssueExecutionWorkspaceSettings {
            mode: Some("isolated_workspace".to_string()),
            network_egress: Some(NetworkEgress {
                allow_fqdns: vec!["a.com".to_string()],
                allow_cidrs: vec![],
            }),
            ..Default::default()
        };
        let result = select_environment_execution_workspace_settings(Some(parsed), false).unwrap();
        assert_eq!(result.mode, None);
        assert!(result.network_egress.is_some());
        assert_eq!(
            result.network_egress.unwrap().allow_fqdns,
            vec!["a.com".to_string()]
        );
    }

    #[test]
    fn select_returns_none_when_no_egress_and_disabled() {
        let parsed = IssueExecutionWorkspaceSettings {
            mode: Some("isolated_workspace".to_string()),
            ..Default::default()
        };
        assert!(select_environment_execution_workspace_settings(Some(parsed), false).is_none());
    }

    #[test]
    fn select_returns_none_for_none_input() {
        assert!(select_environment_execution_workspace_settings(None, true).is_none());
        assert!(select_environment_execution_workspace_settings(None, false).is_none());
    }
}
