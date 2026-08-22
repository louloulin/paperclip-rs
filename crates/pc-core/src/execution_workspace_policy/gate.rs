//! Project execution workspace policy gating（与 Node
//! `services/execution-workspace-policy.ts` 的 `gateProjectExecutionWorkspacePolicy` 1:1 对齐）。
//!
//! ## 用途
//! - 上层（HTTP / scheduler）在 `isolatedWorkspacesEnabled == false` 时
//!   整体关闭 project policy 的下游消费
//! - 输入可能是 `null`（policy 未配置），函数保持幂等

use super::types::ProjectExecutionWorkspacePolicy;

// ============================================================================
// gate_project_execution_workspace_policy
// ============================================================================

/// 与 Node `gateProjectExecutionWorkspacePolicy` 1:1 对齐。
///
/// - `isolatedWorkspacesEnabled == false` → 始终返回 `None`（policy 被 gate 掉）
/// - 否则原样返回 `projectPolicy`（包括 `None`，表示"未配置"而非"被禁用"）
pub fn gate_project_execution_workspace_policy(
    project_policy: Option<&ProjectExecutionWorkspacePolicy>,
    isolated_workspaces_enabled: bool,
) -> Option<ProjectExecutionWorkspacePolicy> {
    if !isolated_workspaces_enabled {
        return None;
    }
    project_policy.cloned()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn policy_with_mode(mode: Option<&str>) -> ProjectExecutionWorkspacePolicy {
        ProjectExecutionWorkspacePolicy {
            enabled: true,
            default_mode: mode.map(String::from),
            ..Default::default()
        }
    }

    #[test]
    fn gate_returns_none_when_disabled() {
        let p = policy_with_mode(Some("isolated_workspace"));
        assert!(gate_project_execution_workspace_policy(Some(&p), false).is_none());
    }

    #[test]
    fn gate_returns_none_for_null_policy_when_disabled() {
        assert!(gate_project_execution_workspace_policy(None, false).is_none());
    }

    #[test]
    fn gate_returns_policy_when_enabled() {
        let p = policy_with_mode(Some("isolated_workspace"));
        let r = gate_project_execution_workspace_policy(Some(&p), true).unwrap();
        assert_eq!(r.enabled, true);
        assert_eq!(r.default_mode.as_deref(), Some("isolated_workspace"));
    }

    #[test]
    fn gate_returns_none_for_null_policy_when_enabled() {
        assert!(gate_project_execution_workspace_policy(None, true).is_none());
    }

    #[test]
    fn gate_preserves_disabled_policy_when_enabled() {
        let p = ProjectExecutionWorkspacePolicy {
            enabled: false,
            ..Default::default()
        };
        let r = gate_project_execution_workspace_policy(Some(&p), true).unwrap();
        assert_eq!(r.enabled, false);
    }

    #[test]
    fn gate_is_clone_isolated_from_input() {
        let mut p = policy_with_mode(Some("shared_workspace"));
        p.default_project_workspace_id = Some("ws-1".to_string());
        let r = gate_project_execution_workspace_policy(Some(&p), true).unwrap();
        // Mutations to original should not affect gated clone
        drop(r);
        assert_eq!(p.default_project_workspace_id.as_deref(), Some("ws-1"));
    }
}