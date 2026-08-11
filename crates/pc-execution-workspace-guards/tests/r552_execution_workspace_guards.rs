//! R552 — pc-execution-workspace-guards 综合测试。

#![allow(clippy::doc_markdown)]

use pc_execution_workspace_guards::{
    closed_execution_workspace_statuses, get_closed_isolated_execution_workspace_message,
    is_closed_isolated_execution_workspace, ExecutionWorkspaceGuardTarget, ExecutionWorkspaceMode,
    ExecutionWorkspaceStatus,
};

#[test]
fn r552_closed_status_set() {
    let s = closed_execution_workspace_statuses();
    assert_eq!(s.len(), 2);
    assert!(s.contains("archived"));
    assert!(s.contains("cleanup_failed"));
    assert!(!s.contains("active"));
}

#[test]
fn r552_status_round_trip() {
    for s in [
        ExecutionWorkspaceStatus::Active,
        ExecutionWorkspaceStatus::Idle,
        ExecutionWorkspaceStatus::InReview,
        ExecutionWorkspaceStatus::Archived,
        ExecutionWorkspaceStatus::CleanupFailed,
    ] {
        let v = s.as_str();
        assert_eq!(ExecutionWorkspaceStatus::parse(v), Some(s));
    }
    assert!(ExecutionWorkspaceStatus::parse("nope").is_none());
}

#[test]
fn r552_mode_round_trip() {
    for m in [
        ExecutionWorkspaceMode::SharedWorkspace,
        ExecutionWorkspaceMode::IsolatedWorkspace,
        ExecutionWorkspaceMode::OperatorBranch,
        ExecutionWorkspaceMode::ReuseExisting,
        ExecutionWorkspaceMode::Inherit,
        ExecutionWorkspaceMode::AgentDefault,
    ] {
        let v = m.as_str();
        assert_eq!(ExecutionWorkspaceMode::parse(v), Some(m));
    }
    assert!(ExecutionWorkspaceMode::parse("nope").is_none());
}

#[test]
fn r552_is_closed_none_is_false() {
    assert!(!is_closed_isolated_execution_workspace(None));
}

#[test]
fn r552_is_closed_non_isolated_is_false() {
    let w = ExecutionWorkspaceGuardTarget {
        closed_at: None,
        mode: ExecutionWorkspaceMode::SharedWorkspace,
        name: "shared".into(),
        status: ExecutionWorkspaceStatus::Archived,
    };
    assert!(!is_closed_isolated_execution_workspace(Some(&w)));
}

#[test]
fn r552_is_closed_isolated_active_open_is_false() {
    let w = ExecutionWorkspaceGuardTarget {
        closed_at: None,
        mode: ExecutionWorkspaceMode::IsolatedWorkspace,
        name: "iso".into(),
        status: ExecutionWorkspaceStatus::Active,
    };
    assert!(!is_closed_isolated_execution_workspace(Some(&w)));
}

#[test]
fn r552_is_closed_isolated_archived_is_true() {
    let w = ExecutionWorkspaceGuardTarget {
        closed_at: None,
        mode: ExecutionWorkspaceMode::IsolatedWorkspace,
        name: "iso".into(),
        status: ExecutionWorkspaceStatus::Archived,
    };
    assert!(is_closed_isolated_execution_workspace(Some(&w)));
}

#[test]
fn r552_is_closed_isolated_cleanup_failed_is_true() {
    let w = ExecutionWorkspaceGuardTarget {
        closed_at: None,
        mode: ExecutionWorkspaceMode::IsolatedWorkspace,
        name: "iso".into(),
        status: ExecutionWorkspaceStatus::CleanupFailed,
    };
    assert!(is_closed_isolated_execution_workspace(Some(&w)));
}

#[test]
fn r552_is_closed_with_closed_at_is_true_even_active() {
    let w = ExecutionWorkspaceGuardTarget {
        closed_at: Some("2026-08-11T00:00:00Z".into()),
        mode: ExecutionWorkspaceMode::IsolatedWorkspace,
        name: "iso".into(),
        status: ExecutionWorkspaceStatus::Active,
    };
    assert!(is_closed_isolated_execution_workspace(Some(&w)));
}

#[test]
fn r552_message_contains_workspace_name() {
    let w = ExecutionWorkspaceGuardTarget {
        closed_at: None,
        mode: ExecutionWorkspaceMode::IsolatedWorkspace,
        name: "feature-x".into(),
        status: ExecutionWorkspaceStatus::Active,
    };
    let msg = get_closed_isolated_execution_workspace_message(&w);
    assert!(msg.contains("feature-x"));
    assert!(msg.contains("closed workspace"));
}
