//! Pure-Rust unit tests for low-trust runtime containment logic.

use pc_low_trust_runtime_containment::{
    assert_low_trust_runtime_services_allowed, assert_low_trust_workspace_isolation,
    is_low_trust_runtime_management_allowed, ContainmentIssueContext, LOW_TRUST_RUNTIME_MANAGEMENT_TOOL_CLASS,
    LowTrustContainmentError, RuntimeServicesInput, WorkspaceIsolationInput,
};
use pc_trust_preset_resolver::{
    LowTrustBoundaryWithCompany, TrustPresetDenyReason, TrustPresetResolution,
    LOW_TRUST_REVIEW_PRESET,
};
use std::collections::HashMap;

fn low_trust_resolution(allowed_tool_classes: Option<Vec<String>>) -> TrustPresetResolution {
    TrustPresetResolution::LowTrustReview {
        preset: LOW_TRUST_REVIEW_PRESET.to_string(),
        boundary: LowTrustBoundaryWithCompany {
            mode: LOW_TRUST_REVIEW_PRESET.to_string(),
            company_id: "co-1".to_string(),
            root_issue_id: Some("issue-root".to_string()),
            issue_ids: None,
            project_ids: None,
            allowed_agent_ids: None,
            allowed_secret_binding_ids: None,
            allowed_tool_classes,
            output_promotion_target: None,
        },
        source_presets: HashMap::new(),
    }
}

fn standard_resolution() -> TrustPresetResolution {
    TrustPresetResolution::Standard {
        preset: "standard".to_string(),
        boundary: None,
        source_presets: HashMap::new(),
    }
}

fn denied_resolution() -> TrustPresetResolution {
    TrustPresetResolution::Denied {
        reason: TrustPresetDenyReason::UnsupportedTrustPreset,
        source: None,
        detail: "trust preset denied by policy".to_string(),
        source_presets: HashMap::new(),
    }
}

#[test]
fn const_is_runtime_manage() {
    assert_eq!(LOW_TRUST_RUNTIME_MANAGEMENT_TOOL_CLASS, "runtime.manage");
}

#[test]
fn runtime_management_allowed_true_when_class_present() {
    let res = low_trust_resolution(Some(vec!["runtime.manage".to_string()]));
    assert!(is_low_trust_runtime_management_allowed(&res));
}

#[test]
fn runtime_management_allowed_false_when_class_absent() {
    let res = low_trust_resolution(Some(vec!["issue.comment".to_string()]));
    assert!(!is_low_trust_runtime_management_allowed(&res));
}

#[test]
fn runtime_management_allowed_false_when_no_classes() {
    let res = low_trust_resolution(None);
    assert!(!is_low_trust_runtime_management_allowed(&res));
}

#[test]
fn runtime_management_allowed_false_for_standard() {
    assert!(!is_low_trust_runtime_management_allowed(&standard_resolution()));
}

#[tokio::test]
async fn workspace_isolation_passes_for_standard() {
    let input = WorkspaceIsolationInput {
        resolution: standard_resolution(),
        isolated_workspaces_enabled: false,
        effective_execution_workspace_mode: None,
        selected_environment_driver: None,
        issue: None,
    };
    assert!(assert_low_trust_workspace_isolation(&input).await.is_ok());
}

#[tokio::test]
async fn workspace_isolation_denied_propagates() {
    let input = WorkspaceIsolationInput {
        resolution: denied_resolution(),
        isolated_workspaces_enabled: true,
        effective_execution_workspace_mode: Some("isolated_workspace".to_string()),
        selected_environment_driver: Some("sandbox".to_string()),
        issue: Some(ContainmentIssueContext {
            company_id: "co-1".to_string(),
            id: Some("issue-root".to_string()),
            project_id: None,
        }),
    };
    let err = assert_low_trust_workspace_isolation(&input)
        .await
        .expect_err("should error");
    assert!(matches!(err, LowTrustContainmentError::Denied { .. }));
}

#[tokio::test]
async fn workspace_isolation_requires_isolated_workspaces_enabled() {
    let input = WorkspaceIsolationInput {
        resolution: low_trust_resolution(None),
        isolated_workspaces_enabled: false,
        effective_execution_workspace_mode: Some("isolated_workspace".to_string()),
        selected_environment_driver: Some("sandbox".to_string()),
        issue: Some(ContainmentIssueContext {
            company_id: "co-1".to_string(),
            id: Some("issue-root".to_string()),
            project_id: None,
        }),
    };
    let err = assert_low_trust_workspace_isolation(&input)
        .await
        .expect_err("should error");
    assert!(matches!(err, LowTrustContainmentError::IsolationUnavailable));
}

#[tokio::test]
async fn workspace_isolation_requires_isolated_workspace_mode() {
    let input = WorkspaceIsolationInput {
        resolution: low_trust_resolution(None),
        isolated_workspaces_enabled: true,
        effective_execution_workspace_mode: Some("shared".to_string()),
        selected_environment_driver: Some("sandbox".to_string()),
        issue: Some(ContainmentIssueContext {
            company_id: "co-1".to_string(),
            id: Some("issue-root".to_string()),
            project_id: None,
        }),
    };
    let err = assert_low_trust_workspace_isolation(&input)
        .await
        .expect_err("should error");
    assert!(matches!(err, LowTrustContainmentError::RequiresIsolatedWorkspace));
}

#[tokio::test]
async fn workspace_isolation_requires_sandbox_driver() {
    let input = WorkspaceIsolationInput {
        resolution: low_trust_resolution(None),
        isolated_workspaces_enabled: true,
        effective_execution_workspace_mode: Some("isolated_workspace".to_string()),
        selected_environment_driver: Some("docker".to_string()),
        issue: Some(ContainmentIssueContext {
            company_id: "co-1".to_string(),
            id: Some("issue-root".to_string()),
            project_id: None,
        }),
    };
    let err = assert_low_trust_workspace_isolation(&input)
        .await
        .expect_err("should error");
    assert!(matches!(err, LowTrustContainmentError::RequiresSandboxEnvironment));
}

#[tokio::test]
async fn workspace_isolation_boundary_mismatch_when_no_issue() {
    let input = WorkspaceIsolationInput {
        resolution: low_trust_resolution(None),
        isolated_workspaces_enabled: true,
        effective_execution_workspace_mode: Some("isolated_workspace".to_string()),
        selected_environment_driver: Some("sandbox".to_string()),
        issue: None,
    };
    let err = assert_low_trust_workspace_isolation(&input)
        .await
        .expect_err("should error");
    assert!(matches!(err, LowTrustContainmentError::BoundaryMismatch));
}

#[tokio::test]
async fn workspace_isolation_boundary_mismatch_when_issue_unrelated() {
    let input = WorkspaceIsolationInput {
        resolution: low_trust_resolution(None),
        isolated_workspaces_enabled: true,
        effective_execution_workspace_mode: Some("isolated_workspace".to_string()),
        selected_environment_driver: Some("sandbox".to_string()),
        issue: Some(ContainmentIssueContext {
            company_id: "co-1".to_string(),
            id: Some("issue-other".to_string()),
            project_id: None,
        }),
    };
    let err = assert_low_trust_workspace_isolation(&input)
        .await
        .expect_err("should error");
    assert!(matches!(err, LowTrustContainmentError::BoundaryMismatch));
}

#[tokio::test]
async fn workspace_isolation_passes_for_root_issue() {
    let input = WorkspaceIsolationInput {
        resolution: low_trust_resolution(None),
        isolated_workspaces_enabled: true,
        effective_execution_workspace_mode: Some("isolated_workspace".to_string()),
        selected_environment_driver: Some("sandbox".to_string()),
        issue: Some(ContainmentIssueContext {
            company_id: "co-1".to_string(),
            id: Some("issue-root".to_string()),
            project_id: None,
        }),
    };
    assert!(assert_low_trust_workspace_isolation(&input).await.is_ok());
}

#[test]
fn runtime_services_passes_for_standard() {
    let input = RuntimeServicesInput {
        resolution: standard_resolution(),
        runtime_service_count: 5,
    };
    assert!(assert_low_trust_runtime_services_allowed(&input).is_ok());
}

#[test]
fn runtime_services_denied_propagates() {
    let input = RuntimeServicesInput {
        resolution: denied_resolution(),
        runtime_service_count: 0,
    };
    let err = assert_low_trust_runtime_services_allowed(&input).expect_err("should error");
    assert!(matches!(err, LowTrustContainmentError::Denied { .. }));
}

#[test]
fn runtime_services_zero_count_skips_check_even_without_class() {
    let input = RuntimeServicesInput {
        resolution: low_trust_resolution(None),
        runtime_service_count: 0,
    };
    assert!(assert_low_trust_runtime_services_allowed(&input).is_ok());
}

#[test]
fn runtime_services_denied_when_class_missing() {
    let input = RuntimeServicesInput {
        resolution: low_trust_resolution(Some(vec!["issue.comment".to_string()])),
        runtime_service_count: 1,
    };
    let err = assert_low_trust_runtime_services_allowed(&input).expect_err("should error");
    assert!(matches!(err, LowTrustContainmentError::RuntimeServicesDenied));
}

#[test]
fn runtime_services_passes_when_class_present() {
    let input = RuntimeServicesInput {
        resolution: low_trust_resolution(Some(vec!["runtime.manage".to_string()])),
        runtime_service_count: 3,
    };
    assert!(assert_low_trust_runtime_services_allowed(&input).is_ok());
}
