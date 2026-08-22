//! Execution workspace policy 域类型（与 Node
//! `packages/shared/src/types/workspace-runtime.ts` + `services/execution-workspace-policy.ts` 1:1 对齐）。
//!
//! 单一职责：定义 ExecutionWorkspace 策略层用到的所有类型（mode / strategy /
//! policy / settings）+ 字符串字面量常量；零业务逻辑。

use std::collections::HashMap;

// ============================================================================
// String literal constants (mode union values)
// ============================================================================

/// `ExecutionWorkspaceMode` 字符串字面量（与 Node union 1:1 对齐）。
///
/// 使用 `&'static str` 而非 enum 是为了：
/// - 与 `serde_json::Value` 自然互通（不需要自定义 Deserialize）
/// - 允许 forward-compatibility：未知 value 不会被强制失败
/// - 减少与 Node 行为偏离
pub mod mode {
    pub const INHERIT: &str = "inherit";
    pub const SHARED_WORKSPACE: &str = "shared_workspace";
    pub const ISOLATED_WORKSPACE: &str = "isolated_workspace";
    pub const OPERATOR_BRANCH: &str = "operator_branch";
    pub const REUSE_EXISTING: &str = "reuse_existing";
    pub const AGENT_DEFAULT: &str = "agent_default";
}

/// `ProjectExecutionWorkspaceDefaultMode` 字符串字面量。
pub mod default_mode {
    pub const SHARED_WORKSPACE: &str = "shared_workspace";
    pub const ISOLATED_WORKSPACE: &str = "isolated_workspace";
    pub const OPERATOR_BRANCH: &str = "operator_branch";
    pub const ADAPTER_DEFAULT: &str = "adapter_default";
}

/// `ExecutionWorkspaceStrategyType` 字符串字面量。
pub mod strategy_type {
    pub const PROJECT_PRIMARY: &str = "project_primary";
    pub const GIT_WORKTREE: &str = "git_worktree";
    pub const ADAPTER_MANAGED: &str = "adapter_managed";
    pub const CLOUD_SANDBOX: &str = "cloud_sandbox";
}

// ============================================================================
// ExecutionWorkspaceStrategy
// ============================================================================

/// ExecutionWorkspace 策略（与 Node `ExecutionWorkspaceStrategy` 1:1 对齐）。
///
/// Optional 字段在 Rust 中用 `Option<T>` 表示；只有显式 `Some` 的字段会被保留。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ExecutionWorkspaceStrategy {
    #[serde(rename = "type")]
    pub r#type: String,
    #[serde(rename = "baseRef", skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    #[serde(rename = "branchTemplate", skip_serializing_if = "Option::is_none")]
    pub branch_template: Option<String>,
    #[serde(rename = "worktreeParentDir", skip_serializing_if = "Option::is_none")]
    pub worktree_parent_dir: Option<String>,
    #[serde(rename = "provisionCommand", skip_serializing_if = "Option::is_none")]
    pub provision_command: Option<String>,
    #[serde(rename = "teardownCommand", skip_serializing_if = "Option::is_none")]
    pub teardown_command: Option<String>,
}

impl ExecutionWorkspaceStrategy {
    pub fn new(r#type: impl Into<String>) -> Self {
        Self {
            r#type: r#type.into(),
            ..Default::default()
        }
    }
}

// ============================================================================
// ProjectExecutionWorkspacePolicy
// ============================================================================

/// 项目级 execution workspace 政策（与 Node `ProjectExecutionWorkspacePolicy` 1:1 对齐）。
///
/// 未在 Node 中以 `Option` 显式区分的策略 map（branchPolicy / pullRequestPolicy /
/// runtimePolicy / cleanupPolicy / authorizationPolicy）在 Rust 中以
/// `Option<HashMap<...>>` 表示；只有显式 `Some` 的字段会被保留。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectExecutionWorkspacePolicy {
    pub enabled: bool,
    pub default_mode: Option<String>,
    pub allow_issue_override: Option<bool>,
    pub default_project_workspace_id: Option<String>,
    pub workspace_strategy: Option<ExecutionWorkspaceStrategy>,
    pub workspace_runtime: Option<HashMap<String, serde_json::Value>>,
    pub branch_policy: Option<HashMap<String, serde_json::Value>>,
    pub pull_request_policy: Option<HashMap<String, serde_json::Value>>,
    pub runtime_policy: Option<HashMap<String, serde_json::Value>>,
    pub cleanup_policy: Option<HashMap<String, serde_json::Value>>,
    pub authorization_policy: Option<HashMap<String, serde_json::Value>>,
}

// ============================================================================
// IssueExecutionWorkspaceSettings
// ============================================================================

/// Issue 级 execution workspace 设置（与 Node `IssueExecutionWorkspaceSettings` 1:1 对齐）。
///
/// `environment_id` 是可选透传字段：当 parser 收到 `includeEnvironmentId=true`
/// 时，原始 `environmentId`（字符串或 null）会被原样保留下来，否则该字段为 `None`。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IssueExecutionWorkspaceSettings {
    pub mode: Option<String>,
    pub environment_id: Option<Option<String>>,
    pub workspace_strategy: Option<ExecutionWorkspaceStrategy>,
    pub workspace_runtime: Option<HashMap<String, serde_json::Value>>,
    pub network_egress: Option<NetworkEgress>,
}

/// Network egress 白名单（与 Node `IssueExecutionWorkspaceSettings.networkEgress` 1:1 对齐）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkEgress {
    pub allow_fqdns: Vec<String>,
    pub allow_cidrs: Vec<String>,
}

// ============================================================================
// ParsedExecutionWorkspaceMode
// ============================================================================

/// Parsed mode（与 Node `ParsedExecutionWorkspaceMode = Exclude<ExecutionWorkspaceMode, "inherit" | "reuse_existing">` 1:1 对齐）。
///
/// 在 Rust 中是 type alias + runtime check，因为 Rust enum 不能直接等价于
/// `Exclude<union, ...>`。
pub type ParsedExecutionWorkspaceMode = String;

pub fn is_parsed_mode(mode: &str) -> bool {
    mode != mode::INHERIT && mode != mode::REUSE_EXISTING
}

// ============================================================================
// UnrunnableWorktreeIssueRef
// ============================================================================

/// Issue 引用（用于 unrunnable worktree 检测）。
///
/// 与 Node `UnrunnableWorktreeIssueRef` 1:1 对齐（全部字段可选）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnrunnableWorktreeIssueRef {
    pub project_id: Option<String>,
    pub project_workspace_id: Option<String>,
    pub execution_workspace_id: Option<String>,
    pub execution_workspace_preference: Option<String>,
}

// ============================================================================
// ExecutionWorkspaceEnvironmentSource / Resolution
// ============================================================================

/// Environment ID 来源（与 Node `ExecutionWorkspaceEnvironmentSource` 1:1 对齐）。
pub mod environment_source {
    pub const AGENT: &str = "agent";
    pub const INSTANCE: &str = "instance";
    pub const DEFAULT: &str = "default";
}

/// 解析后的 environment ID + 来源（与 Node `ExecutionWorkspaceEnvironmentResolution` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionWorkspaceEnvironmentResolution {
    pub environment_id: String,
    pub source: String,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_constants_match_node() {
        assert_eq!(mode::INHERIT, "inherit");
        assert_eq!(mode::SHARED_WORKSPACE, "shared_workspace");
        assert_eq!(mode::ISOLATED_WORKSPACE, "isolated_workspace");
        assert_eq!(mode::OPERATOR_BRANCH, "operator_branch");
        assert_eq!(mode::REUSE_EXISTING, "reuse_existing");
        assert_eq!(mode::AGENT_DEFAULT, "agent_default");
    }

    #[test]
    fn default_mode_constants_match_node() {
        assert_eq!(default_mode::SHARED_WORKSPACE, "shared_workspace");
        assert_eq!(default_mode::ISOLATED_WORKSPACE, "isolated_workspace");
        assert_eq!(default_mode::OPERATOR_BRANCH, "operator_branch");
        assert_eq!(default_mode::ADAPTER_DEFAULT, "adapter_default");
    }

    #[test]
    fn strategy_type_constants_match_node() {
        assert_eq!(strategy_type::PROJECT_PRIMARY, "project_primary");
        assert_eq!(strategy_type::GIT_WORKTREE, "git_worktree");
        assert_eq!(strategy_type::ADAPTER_MANAGED, "adapter_managed");
        assert_eq!(strategy_type::CLOUD_SANDBOX, "cloud_sandbox");
    }

    #[test]
    fn is_parsed_mode_excludes_inherit_and_reuse() {
        assert!(!is_parsed_mode(mode::INHERIT));
        assert!(!is_parsed_mode(mode::REUSE_EXISTING));
        assert!(is_parsed_mode(mode::SHARED_WORKSPACE));
        assert!(is_parsed_mode(mode::ISOLATED_WORKSPACE));
        assert!(is_parsed_mode(mode::OPERATOR_BRANCH));
        assert!(is_parsed_mode(mode::AGENT_DEFAULT));
    }

    #[test]
    fn environment_source_constants_match_node() {
        assert_eq!(environment_source::AGENT, "agent");
        assert_eq!(environment_source::INSTANCE, "instance");
        assert_eq!(environment_source::DEFAULT, "default");
    }

    #[test]
    fn strategy_new_sets_type_only() {
        let s = ExecutionWorkspaceStrategy::new(strategy_type::GIT_WORKTREE);
        assert_eq!(s.r#type, strategy_type::GIT_WORKTREE);
        assert_eq!(s.base_ref, None);
        assert_eq!(s.branch_template, None);
        assert_eq!(s.worktree_parent_dir, None);
        assert_eq!(s.provision_command, None);
        assert_eq!(s.teardown_command, None);
    }

    #[test]
    fn policy_default_is_empty() {
        let p = ProjectExecutionWorkspacePolicy::default();
        assert!(!p.enabled);
        assert_eq!(p.default_mode, None);
        assert_eq!(p.allow_issue_override, None);
    }

    #[test]
    fn issue_settings_default_is_empty() {
        let s = IssueExecutionWorkspaceSettings::default();
        assert_eq!(s.mode, None);
        assert_eq!(s.workspace_strategy, None);
        assert_eq!(s.workspace_runtime, None);
        assert_eq!(s.network_egress, None);
    }

    #[test]
    fn unrunnable_worktree_issue_ref_default_is_empty() {
        let r = UnrunnableWorktreeIssueRef::default();
        assert_eq!(r.project_id, None);
        assert_eq!(r.execution_workspace_id, None);
    }
}
