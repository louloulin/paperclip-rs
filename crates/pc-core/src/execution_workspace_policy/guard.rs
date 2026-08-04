//! Execution workspace 不可运行 worktree 守卫（与 Node
//! `services/execution-workspace-policy.ts` 的 `WORKSPACE_WORKTREE_REQUIRES_PROJECT_*`
//! / `hasReusableExecutionWorkspaceBinding` / `isUnrunnableWorktreeCombo` 1:1 对齐）。
//!
//! ## 失败语义
//! - `is_unrunnable_worktree_combo` 返回 `true` 表示 issue 当前配置 **不可运行**
//!   （缺 project + 缺 reusable workspace + strategy=git_worktree + 没有 prior session workspace）
//! - 调用方应配合 `WORKSPACE_WORKTREE_REQUIRES_PROJECT_*` 常量向上抛错

use super::types::{mode, strategy_type, UnrunnableWorktreeIssueRef};

// ============================================================================
// Constants
// ============================================================================

/// 与 Node `WORKSPACE_WORKTREE_REQUIRES_PROJECT_CODE` 1:1 对齐。
pub const WORKSPACE_WORKTREE_REQUIRES_PROJECT_CODE: &str = "workspace_worktree_requires_project";

/// 与 Node `WORKSPACE_WORKTREE_REQUIRES_PROJECT_REMEDIATION` 1:1 对齐。
pub const WORKSPACE_WORKTREE_REQUIRES_PROJECT_REMEDIATION: &str =
    "Attach a project to the task, or bind a reusable execution workspace, then retry.";

/// 与 Node `WORKSPACE_WORKTREE_REQUIRES_PROJECT_MESSAGE` 1:1 对齐。
pub const WORKSPACE_WORKTREE_REQUIRES_PROJECT_MESSAGE: &str = concat!(
    "This task is set to run in an isolated git worktree, but it has no project and no reusable ",
    "execution workspace to create the worktree from. ",
    "Attach a project to the task, or bind a reusable execution workspace, then retry."
);

// ============================================================================
// has_reusable_execution_workspace_binding
// ============================================================================

/// 与 Node `hasReusableExecutionWorkspaceBinding` 1:1 对齐。
pub fn has_reusable_execution_workspace_binding(issue: &UnrunnableWorktreeIssueRef) -> bool {
    issue.execution_workspace_id.is_some()
        && issue.execution_workspace_preference.as_deref() == Some(mode::REUSE_EXISTING)
}

// ============================================================================
// is_unrunnable_worktree_combo
// ============================================================================

/// 与 Node `isUnrunnableWorktreeCombo` 1:1 对齐。
///
/// 当 issue 配置需要 git_worktree 但缺少 project + reusable workspace +
/// prior session workspace 时返回 `true`（不可运行）。
pub fn is_unrunnable_worktree_combo(input: IsUnrunnableWorktreeComboInput<'_>) -> bool {
    if input.resolved_mode != mode::ISOLATED_WORKSPACE
        && input.resolved_mode != mode::OPERATOR_BRANCH
    {
        return false;
    }
    if input.resolved_strategy != Some(strategy_type::GIT_WORKTREE) {
        return false;
    }
    if input.issue.project_id.is_some() || input.issue.project_workspace_id.is_some() {
        return false;
    }
    let has_reusable_workspace = input
        .reusable_execution_workspace_available
        .unwrap_or_else(|| has_reusable_execution_workspace_binding(input.issue));
    if has_reusable_workspace {
        return false;
    }
    input.has_resolvable_prior_session_workspace != Some(true)
}

/// `is_unrunnable_worktree_combo` 的输入参数。
#[derive(Debug, Clone)]
pub struct IsUnrunnableWorktreeComboInput<'a> {
    pub issue: &'a UnrunnableWorktreeIssueRef,
    pub resolved_mode: &'a str,
    pub resolved_strategy: Option<&'a str>,
    pub reusable_execution_workspace_available: Option<bool>,
    pub has_resolvable_prior_session_workspace: Option<bool>,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn ref_with(
        project: Option<&str>,
        ws_id: Option<&str>,
        pref: Option<&str>,
    ) -> UnrunnableWorktreeIssueRef {
        UnrunnableWorktreeIssueRef {
            project_id: project.map(String::from),
            project_workspace_id: None,
            execution_workspace_id: ws_id.map(String::from),
            execution_workspace_preference: pref.map(String::from),
        }
    }

    #[test]
    fn constants_match_node() {
        assert_eq!(
            WORKSPACE_WORKTREE_REQUIRES_PROJECT_CODE,
            "workspace_worktree_requires_project"
        );
        assert_eq!(
            WORKSPACE_WORKTREE_REQUIRES_PROJECT_REMEDIATION,
            "Attach a project to the task, or bind a reusable execution workspace, then retry."
        );
        assert!(WORKSPACE_WORKTREE_REQUIRES_PROJECT_MESSAGE.contains("git worktree"));
    }

    // ----- has_reusable_execution_workspace_binding -----

    #[test]
    fn has_reusable_binding_with_id_and_reuse_existing() {
        let r = ref_with(None, Some("ws-1"), Some(mode::REUSE_EXISTING));
        assert!(has_reusable_execution_workspace_binding(&r));
    }

    #[test]
    fn no_reusable_binding_without_id() {
        let r = ref_with(None, None, Some(mode::REUSE_EXISTING));
        assert!(!has_reusable_execution_workspace_binding(&r));
    }

    #[test]
    fn no_reusable_binding_without_preference() {
        let r = ref_with(None, Some("ws-1"), None);
        assert!(!has_reusable_execution_workspace_binding(&r));
    }

    #[test]
    fn no_reusable_binding_with_wrong_preference() {
        let r = ref_with(None, Some("ws-1"), Some("shared_workspace"));
        assert!(!has_reusable_execution_workspace_binding(&r));
    }

    // ----- is_unrunnable_worktree_combo -----

    #[test]
    fn not_unrunnable_when_mode_is_not_worktree_mode() {
        let issue = ref_with(None, None, None);
        let input = IsUnrunnableWorktreeComboInput {
            issue: &issue,
            resolved_mode: mode::SHARED_WORKSPACE,
            resolved_strategy: Some(strategy_type::GIT_WORKTREE),
            reusable_execution_workspace_available: None,
            has_resolvable_prior_session_workspace: None,
        };
        assert!(!is_unrunnable_worktree_combo(input));
    }

    #[test]
    fn not_unrunnable_when_strategy_is_not_git_worktree() {
        let issue = ref_with(None, None, None);
        let input = IsUnrunnableWorktreeComboInput {
            issue: &issue,
            resolved_mode: mode::ISOLATED_WORKSPACE,
            resolved_strategy: Some(strategy_type::PROJECT_PRIMARY),
            reusable_execution_workspace_available: None,
            has_resolvable_prior_session_workspace: None,
        };
        assert!(!is_unrunnable_worktree_combo(input));
    }

    #[test]
    fn not_unrunnable_when_project_id_present() {
        let issue = ref_with(Some("proj-1"), None, None);
        let input = IsUnrunnableWorktreeComboInput {
            issue: &issue,
            resolved_mode: mode::ISOLATED_WORKSPACE,
            resolved_strategy: Some(strategy_type::GIT_WORKTREE),
            reusable_execution_workspace_available: None,
            has_resolvable_prior_session_workspace: None,
        };
        assert!(!is_unrunnable_worktree_combo(input));
    }

    #[test]
    fn not_unrunnable_when_project_workspace_id_present() {
        let issue = UnrunnableWorktreeIssueRef {
            project_id: None,
            project_workspace_id: Some("pw-1".to_string()),
            execution_workspace_id: None,
            execution_workspace_preference: None,
        };
        let input = IsUnrunnableWorktreeComboInput {
            issue: &issue,
            resolved_mode: mode::ISOLATED_WORKSPACE,
            resolved_strategy: Some(strategy_type::GIT_WORKTREE),
            reusable_execution_workspace_available: None,
            has_resolvable_prior_session_workspace: None,
        };
        assert!(!is_unrunnable_worktree_combo(input));
    }

    #[test]
    fn not_unrunnable_when_reusable_workspace_available() {
        let issue = ref_with(None, Some("ws-1"), Some(mode::REUSE_EXISTING));
        let input = IsUnrunnableWorktreeComboInput {
            issue: &issue,
            resolved_mode: mode::ISOLATED_WORKSPACE,
            resolved_strategy: Some(strategy_type::GIT_WORKTREE),
            reusable_execution_workspace_available: None,
            has_resolvable_prior_session_workspace: None,
        };
        assert!(!is_unrunnable_worktree_combo(input));
    }

    #[test]
    fn not_unrunnable_when_prior_session_workspace_resolvable() {
        let issue = ref_with(None, None, None);
        let input = IsUnrunnableWorktreeComboInput {
            issue: &issue,
            resolved_mode: mode::ISOLATED_WORKSPACE,
            resolved_strategy: Some(strategy_type::GIT_WORKTREE),
            reusable_execution_workspace_available: None,
            has_resolvable_prior_session_workspace: Some(true),
        };
        assert!(!is_unrunnable_worktree_combo(input));
    }

    #[test]
    fn unrunnable_when_all_conditions_met() {
        let issue = ref_with(None, None, None);
        let input = IsUnrunnableWorktreeComboInput {
            issue: &issue,
            resolved_mode: mode::ISOLATED_WORKSPACE,
            resolved_strategy: Some(strategy_type::GIT_WORKTREE),
            reusable_execution_workspace_available: Some(false),
            has_resolvable_prior_session_workspace: Some(false),
        };
        assert!(is_unrunnable_worktree_combo(input));
    }

    #[test]
    fn unrunnable_resolved_strategy_null() {
        let issue = ref_with(None, None, None);
        let input = IsUnrunnableWorktreeComboInput {
            issue: &issue,
            resolved_mode: mode::OPERATOR_BRANCH,
            resolved_strategy: None,
            reusable_execution_workspace_available: Some(false),
            has_resolvable_prior_session_workspace: Some(false),
        };
        // resolved_strategy null != "git_worktree" → not unrunnable
        assert!(!is_unrunnable_worktree_combo(input));
    }
}
