//! `execution_workspace_branch_reconcile_assertions` — branch reconcile 校验函数。
//!
//! 与 Node `assertBranchReconcileWorkspaceIsSafe` /
//! `assertBranchReconcileRuntimeServicesStopped` /
//! `assertLockedBranchReconcileWorkspaceStillMatchesInspection` 1:1 对齐。
//!
//! 设计目标：纯函数模块，校验失败返回 typed error；不抛 panic。
//! 接收 typed input struct，回退 typed error。
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::workspace_branch_incoherence::Cleanliness;
use crate::workspace_branch_incoherence_explain::AncestryVerdict;

// ============================================================================
// BranchRefResolution (mirrors Node ExecutionWorkspaceBranchRefResolution)
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchRefResolution {
    Resolved,
    Missing,
    Error,
}

impl Default for BranchRefResolution {
    fn default() -> Self {
        Self::Missing
    }
}

impl BranchRefResolution {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Resolved => "resolved",
            Self::Missing => "missing",
            Self::Error => "error",
        }
    }
}

// ============================================================================
// ExecutionWorkspaceBranchReconcileInspection (minimal)
// ============================================================================

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionWorkspaceBranchReconcileInspection {
    pub fingerprint: String,
    pub worktree_path: String,
    pub repo_root: String,
    pub from_branch: String,
    pub to_branch: String,
    pub from_sha: Option<String>,
    pub to_sha: Option<String>,
    pub from_branch_ref_status: BranchRefResolution,
    pub to_branch_ref_status: BranchRefResolution,
    pub ancestry_verdict: AncestryVerdict,
    pub cleanliness: Cleanliness,
    pub status_entry_count: Option<i64>,
    pub plain_language_reason: String,
}

// ============================================================================
// RuntimeServiceLite — minimal subset for assertions
// ============================================================================

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeServiceLite {
    pub id: String,
    pub service_name: String,
    pub status: String,
}

// ============================================================================
// BranchReconcileError
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BranchReconcileError {
    /// Workspace 状态不允许 reconcile
    WorkspaceStatusInvalid {
        workspace_status: String,
        allowed_statuses: Vec<String>,
        inspection: ExecutionWorkspaceBranchReconcileInspection,
    },
    /// Worktree 不干净
    WorktreeNotClean {
        inspection: ExecutionWorkspaceBranchReconcileInspection,
    },
    /// 还有 runtime service 没停
    RuntimeServicesNotStopped {
        inspection: ExecutionWorkspaceBranchReconcileInspection,
        active_services: Vec<RuntimeServiceLite>,
    },
    /// Locked row 与 inspection 不一致（乐观锁冲突）
    WorkspaceChangedDuringReconcile {
        workspace_id: String,
        expected: ExpectedWorkspaceState,
        current_locked: LockedWorkspaceState,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExpectedWorkspaceState {
    pub status: String,
    pub source_issue_id: Option<String>,
    pub project_workspace_id: Option<String>,
    pub branch_name: Option<String>,
    pub worktree_path: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LockedWorkspaceState {
    pub source_issue_id: Option<String>,
    pub project_workspace_id: Option<String>,
    pub branch_name: Option<String>,
    pub worktree_path: Option<String>,
}

// ============================================================================
// assertBranchReconcileRuntimeServicesStopped
// ============================================================================

/// `assertBranchReconcileRuntimeServicesStopped(input)`：
///
/// 与 Node 1:1 对齐：所有 runtime service.status 必须为 "stopped"，否则返回
/// `RuntimeServicesNotStopped`，附带 inspection + active services。
pub fn assert_branch_reconcile_runtime_services_stopped(
    inspection: &ExecutionWorkspaceBranchReconcileInspection,
    runtime_services: &[RuntimeServiceLite],
) -> Result<(), BranchReconcileError> {
    let active: Vec<RuntimeServiceLite> = runtime_services
        .iter()
        .filter(|r| r.status != "stopped")
        .cloned()
        .collect();
    if !active.is_empty() {
        return Err(BranchReconcileError::RuntimeServicesNotStopped {
            inspection: inspection.clone(),
            active_services: active,
        });
    }
    Ok(())
}

// ============================================================================
// assertBranchReconcileWorkspaceIsSafe
// ============================================================================

/// `assertBranchReconcileWorkspaceIsSafe(input)`：
///
/// 与 Node 1:1 对齐：
/// - allowActiveWorkspace=false → 允许 idle
/// - allowActiveWorkspace=true → 允许 idle / active
/// - cleanliness 必须为 Clean
/// - runtime services 必须全部 stopped
pub fn assert_branch_reconcile_workspace_is_safe(
    workspace_status: &str,
    inspection: &ExecutionWorkspaceBranchReconcileInspection,
    runtime_services: &[RuntimeServiceLite],
    allow_active_workspace: bool,
) -> Result<(), BranchReconcileError> {
    let allowed: Vec<String> = if allow_active_workspace {
        vec!["idle".to_string(), "active".to_string()]
    } else {
        vec!["idle".to_string()]
    };
    if !allowed.iter().any(|s| s == workspace_status) {
        return Err(BranchReconcileError::WorkspaceStatusInvalid {
            workspace_status: workspace_status.to_string(),
            allowed_statuses: allowed,
            inspection: inspection.clone(),
        });
    }
    if inspection.cleanliness != Cleanliness::Clean {
        return Err(BranchReconcileError::WorktreeNotClean {
            inspection: inspection.clone(),
        });
    }
    assert_branch_reconcile_runtime_services_stopped(inspection, runtime_services)
}

// ============================================================================
// assertLockedBranchReconcileWorkspaceStillMatchesInspection
// ============================================================================

/// `LockedRow`：用于 reconcile 比对的 minimal row 视图。
#[derive(Debug, Clone, Default)]
pub struct LockedRow {
    pub id: String,
    pub source_issue_id: Option<String>,
    pub project_workspace_id: Option<String>,
    pub branch_name: Option<String>,
    pub provider_ref: Option<String>,
    pub cwd: Option<String>,
    pub status: String,
}

/// `InspectedRow`：用于比对的 minimal row 视图。
#[derive(Debug, Clone, Default)]
pub struct InspectedRow {
    pub status: String,
    pub source_issue_id: Option<String>,
    pub project_workspace_id: Option<String>,
}

/// `assertLockedBranchReconcileWorkspaceStillMatchesInspection(input)`：
///
/// 与 Node 1:1 对齐：
/// - locked_path = providerRef ?? cwd (resolved via canonicalize)
/// - locked_branch = branchName
/// - 检查 sourceIssueId/projectWorkspaceId/branch/path 全部匹配
/// - 不匹配 → `WorkspaceChangedDuringReconcile`
pub fn assert_locked_branch_reconcile_workspace_still_matches_inspection(
    locked_row: &LockedRow,
    inspected_row: &InspectedRow,
    inspection: &ExecutionWorkspaceBranchReconcileInspection,
) -> Result<(), BranchReconcileError> {
    let locked_path = locked_row
        .provider_ref
        .clone()
        .or_else(|| locked_row.cwd.clone());
    let locked_branch = locked_row.branch_name.clone();
    let current_path = locked_path.as_deref().map(canonicalize_path_lossy);

    let inspection_path_canonical = canonicalize_path_lossy(&inspection.worktree_path);

    let source_issue_matches = locked_row.source_issue_id == inspected_row.source_issue_id;
    let project_workspace_matches =
        locked_row.project_workspace_id == inspected_row.project_workspace_id;
    let branch_matches = locked_branch.as_deref() == Some(inspection.from_branch.as_str());
    let path_matches = current_path.as_deref() == Some(inspection_path_canonical.as_str());

    if !(source_issue_matches && project_workspace_matches && branch_matches && path_matches) {
        return Err(BranchReconcileError::WorkspaceChangedDuringReconcile {
            workspace_id: locked_row.id.clone(),
            expected: ExpectedWorkspaceState {
                status: inspected_row.status.clone(),
                source_issue_id: inspected_row.source_issue_id.clone(),
                project_workspace_id: inspected_row.project_workspace_id.clone(),
                branch_name: Some(inspection.from_branch.clone()),
                worktree_path: Some(inspection_path_canonical),
            },
            current_locked: LockedWorkspaceState {
                source_issue_id: locked_row.source_issue_id.clone(),
                project_workspace_id: locked_row.project_workspace_id.clone(),
                branch_name: locked_branch,
                worktree_path: current_path,
            },
        });
    }
    Ok(())
}

/// 尝试 canonicalize；失败时返回原 path（保持与 Node 行为对齐：Node 用 `path.resolve`，失败时回退原值）。
fn canonicalize_path_lossy(p: &str) -> String {
    Path::new(p)
        .canonicalize()
        .map(|c| c.to_string_lossy().to_string())
        .unwrap_or_else(|_| p.to_string())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn inspection() -> ExecutionWorkspaceBranchReconcileInspection {
        ExecutionWorkspaceBranchReconcileInspection {
            fingerprint: "fp".into(),
            worktree_path: "/wt".into(),
            repo_root: "/repo".into(),
            from_branch: "feat/x".into(),
            to_branch: "feat/y".into(),
            from_sha: Some("sha1".into()),
            to_sha: Some("sha2".into()),
            from_branch_ref_status: BranchRefResolution::Resolved,
            to_branch_ref_status: BranchRefResolution::Resolved,
            ancestry_verdict: AncestryVerdict::Ancestor,
            cleanliness: Cleanliness::Clean,
            status_entry_count: Some(0),
            plain_language_reason: "ok".into(),
        }
    }

    fn stopped_services() -> Vec<RuntimeServiceLite> {
        vec![RuntimeServiceLite {
            id: "s1".into(),
            service_name: "web".into(),
            status: "stopped".into(),
        }]
    }

    // ----- assertBranchReconcileRuntimeServicesStopped -----

    #[test]
    fn runtime_services_stopped_ok() {
        let s = stopped_services();
        let out = assert_branch_reconcile_runtime_services_stopped(&inspection(), &s);
        assert!(out.is_ok());
    }

    #[test]
    fn runtime_services_active_fails() {
        let s = vec![
            RuntimeServiceLite {
                id: "s1".into(),
                service_name: "web".into(),
                status: "stopped".into(),
            },
            RuntimeServiceLite {
                id: "s2".into(),
                service_name: "db".into(),
                status: "running".into(),
            },
        ];
        let out = assert_branch_reconcile_runtime_services_stopped(&inspection(), &s);
        match out {
            Err(BranchReconcileError::RuntimeServicesNotStopped {
                active_services, ..
            }) => {
                assert_eq!(active_services.len(), 1);
                assert_eq!(active_services[0].id, "s2");
                assert_eq!(active_services[0].status, "running");
            }
            _ => panic!("expected RuntimeServicesNotStopped"),
        }
    }

    // ----- assertBranchReconcileWorkspaceIsSafe -----

    #[test]
    fn workspace_safe_default_allows_idle() {
        let s = stopped_services();
        let out = assert_branch_reconcile_workspace_is_safe("idle", &inspection(), &s, false);
        assert!(out.is_ok());
    }

    #[test]
    fn workspace_safe_rejects_active_by_default() {
        let s = stopped_services();
        let out = assert_branch_reconcile_workspace_is_safe("active", &inspection(), &s, false);
        match out {
            Err(BranchReconcileError::WorkspaceStatusInvalid {
                workspace_status,
                allowed_statuses,
                ..
            }) => {
                assert_eq!(workspace_status, "active");
                assert_eq!(allowed_statuses, vec!["idle"]);
            }
            _ => panic!("expected WorkspaceStatusInvalid"),
        }
    }

    #[test]
    fn workspace_safe_allows_active_with_flag() {
        let s = stopped_services();
        let out = assert_branch_reconcile_workspace_is_safe("active", &inspection(), &s, true);
        assert!(out.is_ok());
    }

    #[test]
    fn workspace_safe_rejects_dirty() {
        let mut ins = inspection();
        ins.cleanliness = Cleanliness::Dirty;
        let s = stopped_services();
        let out = assert_branch_reconcile_workspace_is_safe("idle", &ins, &s, false);
        match out {
            Err(BranchReconcileError::WorktreeNotClean { .. }) => {}
            _ => panic!("expected WorktreeNotClean"),
        }
    }

    #[test]
    fn workspace_safe_propagates_runtime_services_error() {
        let s = vec![RuntimeServiceLite {
            id: "s1".into(),
            service_name: "web".into(),
            status: "running".into(),
        }];
        let out = assert_branch_reconcile_workspace_is_safe("idle", &inspection(), &s, false);
        assert!(matches!(
            out,
            Err(BranchReconcileError::RuntimeServicesNotStopped { .. })
        ));
    }

    // ----- assertLockedBranchReconcileWorkspaceStillMatchesInspection -----

    #[test]
    fn locked_matches_ok() {
        let locked = LockedRow {
            id: "ws-1".into(),
            source_issue_id: Some("iss-1".into()),
            project_workspace_id: Some("pws-1".into()),
            branch_name: Some("feat/x".into()),
            provider_ref: None,
            cwd: Some("/wt".into()),
            status: "idle".into(),
        };
        let inspected = InspectedRow {
            status: "idle".into(),
            source_issue_id: Some("iss-1".into()),
            project_workspace_id: Some("pws-1".into()),
        };
        let out = assert_locked_branch_reconcile_workspace_still_matches_inspection(
            &locked,
            &inspected,
            &inspection(),
        );
        assert!(out.is_ok());
    }

    #[test]
    fn locked_source_issue_mismatch() {
        let locked = LockedRow {
            id: "ws-1".into(),
            source_issue_id: Some("iss-2".into()),
            project_workspace_id: Some("pws-1".into()),
            branch_name: Some("feat/x".into()),
            provider_ref: None,
            cwd: Some("/wt".into()),
            status: "idle".into(),
        };
        let inspected = InspectedRow {
            status: "idle".into(),
            source_issue_id: Some("iss-1".into()),
            project_workspace_id: Some("pws-1".into()),
        };
        let out = assert_locked_branch_reconcile_workspace_still_matches_inspection(
            &locked,
            &inspected,
            &inspection(),
        );
        assert!(matches!(
            out,
            Err(BranchReconcileError::WorkspaceChangedDuringReconcile { .. })
        ));
    }

    #[test]
    fn locked_branch_mismatch() {
        let locked = LockedRow {
            id: "ws-1".into(),
            source_issue_id: Some("iss-1".into()),
            project_workspace_id: Some("pws-1".into()),
            branch_name: Some("feat/z".into()),
            provider_ref: None,
            cwd: Some("/wt".into()),
            status: "idle".into(),
        };
        let inspected = InspectedRow {
            status: "idle".into(),
            source_issue_id: Some("iss-1".into()),
            project_workspace_id: Some("pws-1".into()),
        };
        let out = assert_locked_branch_reconcile_workspace_still_matches_inspection(
            &locked,
            &inspected,
            &inspection(),
        );
        assert!(matches!(
            out,
            Err(BranchReconcileError::WorkspaceChangedDuringReconcile { .. })
        ));
    }

    #[test]
    fn locked_provider_ref_used_over_cwd() {
        let locked = LockedRow {
            id: "ws-1".into(),
            source_issue_id: Some("iss-1".into()),
            project_workspace_id: Some("pws-1".into()),
            branch_name: Some("feat/x".into()),
            provider_ref: Some("/wt".into()),
            cwd: Some("/different".into()),
            status: "idle".into(),
        };
        let inspected = InspectedRow {
            status: "idle".into(),
            source_issue_id: Some("iss-1".into()),
            project_workspace_id: Some("pws-1".into()),
        };
        let out = assert_locked_branch_reconcile_workspace_still_matches_inspection(
            &locked,
            &inspected,
            &inspection(),
        );
        assert!(out.is_ok());
    }
}
