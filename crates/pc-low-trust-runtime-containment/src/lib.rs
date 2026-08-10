//! Low-trust runtime containment：low-trust review preset 启用时的工作区隔离、
//! runtime service 许可、ancestor 链深度校验。
//!
//! 对齐 Node `services/low-trust-runtime-containment.ts`：
//! - `LOW_TRUST_RUNTIME_MANAGEMENT_TOOL_CLASS = "runtime.manage"`
//! - `isLowTrustRuntimeManagementAllowed`: 仅当 resolution.kind == low_trust_review
//!   且 boundary.allowed_tool_classes 包含 runtime.manage 时返回 true
//! - `assertLowTrustWorkspaceIsolation`: 校验 isolated workspaces 已启用 +
//!   execution workspace mode = isolated_workspace + sandbox driver +
//!   issue 在 boundary 内（直接或祖先链内）
//! - `assertLowTrustRuntimeServicesAllowed`: resolution 是 denied 时抛错；
//!   low_trust_review + 0 service 跳过；runtime.manage 不在 boundary 中则拒绝
//! - `issueIdIsDescendantOf`: 沿 parent_id 链向上至多 12 层

use pc_repos::Db;
use pc_trust_preset_resolver::{
    is_issue_within_low_trust_boundary, BoundaryIssue, LowTrustBoundaryWithCompany,
    TrustPresetResolution, LOW_TRUST_ISSUE_ANCESTRY_MAX_DEPTH,
};
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

/// Runtime 管理工具类名（与 Node 1:1 对齐）。
pub const LOW_TRUST_RUNTIME_MANAGEMENT_TOOL_CLASS: &str = "runtime.manage";

/// Containment error（对应 Node `unprocessable` HTTPError）。
#[derive(Debug, Clone, Error, Serialize, PartialEq, Eq)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum LowTrustContainmentError {
    /// `resolution.kind == "denied"`
    #[error("trust preset denied: {detail}")]
    Denied { detail: String, reason: String },
    /// `!isolatedWorkspacesEnabled`
    #[error("Low-trust execution requires isolated workspaces to be enabled.")]
    IsolationUnavailable,
    /// `effectiveExecutionWorkspaceMode != "isolated_workspace"`
    #[error("Low-trust execution requires an isolated execution workspace.")]
    RequiresIsolatedWorkspace,
    /// issue 不在 trust boundary 内（自身 + 祖先）
    #[error("Low-trust execution issue is outside the active trust boundary.")]
    BoundaryMismatch,
    /// `selectedEnvironmentDriver != "sandbox"`
    #[error("Low-trust execution requires a sandbox environment driver.")]
    RequiresSandboxEnvironment,
    /// `runtimeServiceCount > 0` 且 boundary 不含 runtime.manage
    #[error("Low-trust execution cannot start runtime services unless the boundary grants runtime.manage.")]
    RuntimeServicesDenied,
}

/// Issue 上下文（用于 workspace boundary 判断）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContainmentIssueContext {
    pub company_id: String,
    pub id: Option<String>,
    pub project_id: Option<String>,
}

impl From<&ContainmentIssueContext> for BoundaryIssue {
    fn from(value: &ContainmentIssueContext) -> Self {
        BoundaryIssue {
            company_id: value.company_id.clone(),
            id: value.id.clone(),
            project_id: value.project_id.clone(),
        }
    }
}

/// Workspace isolation assertion input。
#[derive(Debug, Clone)]
pub struct WorkspaceIsolationInput {
    pub resolution: TrustPresetResolution,
    pub isolated_workspaces_enabled: bool,
    pub effective_execution_workspace_mode: Option<String>,
    pub selected_environment_driver: Option<String>,
    pub issue: Option<ContainmentIssueContext>,
}

/// Runtime services assertion input。
#[derive(Debug, Clone)]
pub struct RuntimeServicesInput {
    pub resolution: TrustPresetResolution,
    pub runtime_service_count: usize,
}

/// 是否允许在 low-trust review boundary 内启动 runtime 管理工具。
pub fn is_low_trust_runtime_management_allowed(
    resolution: &TrustPresetResolution,
) -> bool {
    match resolution {
        TrustPresetResolution::LowTrustReview { boundary, .. } => {
            boundary
                .allowed_tool_classes
                .as_ref()
                .map(|classes| classes.iter().any(|c| c == LOW_TRUST_RUNTIME_MANAGEMENT_TOOL_CLASS))
                .unwrap_or(false)
        }
        _ => false,
    }
}

/// 是否 issue（自身或祖先）属于 low-trust boundary。
pub async fn is_issue_within_boundary(
    db: Option<&Db>,
    boundary: &LowTrustBoundaryWithCompany,
    issue: &ContainmentIssueContext,
) -> bool {
    if is_issue_within_low_trust_boundary(boundary, &BoundaryIssue::from(issue)) {
        return true;
    }
    let (Some(db), Some(issue_id), Some(root_issue_id)) =
        (db, issue.id.as_ref(), boundary.root_issue_id.as_ref())
    else {
        return false;
    };
    issue_id_is_descendant_of(db, issue_id, root_issue_id, &boundary.company_id).await
}

/// 沿 `issues.parent_id` 链向上查找 root_issue_id，最多 `LOW_TRUST_ISSUE_ANCESTRY_MAX_DEPTH` 层。
async fn issue_id_is_descendant_of(
    db: &Db,
    issue_id: &str,
    root_issue_id: &str,
    company_id: &str,
) -> bool {
    let mut cursor: Option<String> = Some(issue_id.to_string());
    for _ in 0..LOW_TRUST_ISSUE_ANCESTRY_MAX_DEPTH {
        let Some(cur) = cursor.take() else { return false };
        if cur == root_issue_id {
            return true;
        }
        let cur_uuid = match Uuid::parse_str(&cur) {
            Ok(u) => u,
            Err(_) => return false,
        };
        let row: Option<(String, Option<String>)> = match sqlx::query_as(
            "SELECT company_id::text, parent_id::text FROM issues WHERE id = $1",
        )
        .bind(cur_uuid)
        .fetch_optional(db.pool())
        .await
        {
            Ok(r) => r,
            Err(_) => return false,
        };
        let Some((row_company, parent_id)) = row else { return false };
        if row_company != company_id {
            return false;
        }
        cursor = parent_id;
    }
    false
}

/// 断言 workspace 隔离满足 low-trust review 启用条件。
pub async fn assert_low_trust_workspace_isolation(
    input: &WorkspaceIsolationInput,
) -> Result<(), LowTrustContainmentError> {
    match &input.resolution {
        TrustPresetResolution::Denied { reason, source: _, detail, .. } => {
            return Err(LowTrustContainmentError::Denied {
                detail: detail.clone(),
                reason: reason.as_str().to_string(),
            });
        }
        TrustPresetResolution::Standard { .. } | TrustPresetResolution::LowTrustReview { .. } => {}
    }

    if !matches!(input.resolution, TrustPresetResolution::LowTrustReview { .. }) {
        return Ok(());
    }

    if !input.isolated_workspaces_enabled {
        return Err(LowTrustContainmentError::IsolationUnavailable);
    }
    if input.effective_execution_workspace_mode.as_deref() != Some("isolated_workspace") {
        return Err(LowTrustContainmentError::RequiresIsolatedWorkspace);
    }

    let TrustPresetResolution::LowTrustReview { boundary, .. } = &input.resolution else {
        return Ok(());
    };

    let in_boundary = match input.issue.as_ref() {
        Some(issue) => {
            is_issue_within_boundary(None, boundary, issue).await
        }
        None => false,
    };
    if !in_boundary {
        return Err(LowTrustContainmentError::BoundaryMismatch);
    }

    if input.selected_environment_driver.as_deref() != Some("sandbox") {
        return Err(LowTrustContainmentError::RequiresSandboxEnvironment);
    }
    Ok(())
}

/// 断言 runtime services 在 low-trust review boundary 内被允许启动。
pub fn assert_low_trust_runtime_services_allowed(
    input: &RuntimeServicesInput,
) -> Result<(), LowTrustContainmentError> {
    match &input.resolution {
        TrustPresetResolution::Denied { reason, source: _, detail, .. } => {
            return Err(LowTrustContainmentError::Denied {
                detail: detail.clone(),
                reason: reason.as_str().to_string(),
            });
        }
        TrustPresetResolution::Standard { .. } | TrustPresetResolution::LowTrustReview { .. } => {}
    }

    if !matches!(input.resolution, TrustPresetResolution::LowTrustReview { .. }) {
        return Ok(());
    }
    if input.runtime_service_count == 0 {
        return Ok(());
    }
    if is_low_trust_runtime_management_allowed(&input.resolution) {
        return Ok(());
    }
    Err(LowTrustContainmentError::RuntimeServicesDenied)
}
