#![forbid(unsafe_code)]
#![allow(clippy::doc_markdown)]

//! Closed isolated execution workspace guards.
//!
//! R552: Direct port of `paperclip/packages/shared/src/execution-workspace-guards.ts`
//! (19 LOC). Two small pure helpers over a subset of the execution workspace shape.

pub mod readiness;
pub mod runtime_service_id;

use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExecutionWorkspaceStatus {
    Active,
    Idle,
    InReview,
    Archived,
    CleanupFailed,
}

impl ExecutionWorkspaceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Idle => "idle",
            Self::InReview => "in_review",
            Self::Archived => "archived",
            Self::CleanupFailed => "cleanup_failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "idle" => Some(Self::Idle),
            "in_review" => Some(Self::InReview),
            "archived" => Some(Self::Archived),
            "cleanup_failed" => Some(Self::CleanupFailed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExecutionWorkspaceMode {
    SharedWorkspace,
    IsolatedWorkspace,
    OperatorBranch,
    ReuseExisting,
    Inherit,
    AgentDefault,
}

impl ExecutionWorkspaceMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SharedWorkspace => "shared_workspace",
            Self::IsolatedWorkspace => "isolated_workspace",
            Self::OperatorBranch => "operator_branch",
            Self::ReuseExisting => "reuse_existing",
            Self::Inherit => "inherit",
            Self::AgentDefault => "agent_default",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "shared_workspace" => Some(Self::SharedWorkspace),
            "isolated_workspace" => Some(Self::IsolatedWorkspace),
            "operator_branch" => Some(Self::OperatorBranch),
            "reuse_existing" => Some(Self::ReuseExisting),
            "inherit" => Some(Self::Inherit),
            "agent_default" => Some(Self::AgentDefault),
            _ => None,
        }
    }
}

/// Closed statuses — mirrors `CLOSED_EXECUTION_WORKSPACE_STATUSES`.
pub fn closed_execution_workspace_statuses() -> HashSet<&'static str> {
    ["archived", "cleanup_failed"].into_iter().collect()
}

#[derive(Debug, Clone)]
pub struct ExecutionWorkspaceGuardTarget {
    pub closed_at: Option<String>,
    pub mode: ExecutionWorkspaceMode,
    pub name: String,
    pub status: ExecutionWorkspaceStatus,
}

/// Returns true when `workspace` is an isolated workspace that has been closed
/// (either `closedAt` set or status is `archived` / `cleanup_failed`).
pub fn is_closed_isolated_execution_workspace(
    workspace: Option<&ExecutionWorkspaceGuardTarget>,
) -> bool {
    let Some(w) = workspace else {
        return false;
    };
    if !matches!(w.mode, ExecutionWorkspaceMode::IsolatedWorkspace) {
        return false;
    }
    if w.closed_at.is_some() {
        return true;
    }
    closed_execution_workspace_statuses().contains(w.status.as_str())
}

/// User-facing message rendered when the guard trips.
pub fn get_closed_isolated_execution_workspace_message(
    workspace: &ExecutionWorkspaceGuardTarget,
) -> String {
    format!(
        "This issue is linked to the closed workspace \"{}\". Move it to an open workspace before adding comments or resuming work.",
        workspace.name
    )
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    fn make(
        mode: ExecutionWorkspaceMode,
        status: ExecutionWorkspaceStatus,
        closed: bool,
    ) -> ExecutionWorkspaceGuardTarget {
        ExecutionWorkspaceGuardTarget {
            closed_at: if closed {
                Some("2026-08-11T00:00:00Z".into())
            } else {
                None
            },
            mode,
            name: "ws-1".into(),
            status,
        }
    }

    #[test]
    fn none_is_not_closed() {
        assert!(!is_closed_isolated_execution_workspace(None));
    }

    #[test]
    fn non_isolated_is_not_closed() {
        let w = make(
            ExecutionWorkspaceMode::SharedWorkspace,
            ExecutionWorkspaceStatus::Archived,
            false,
        );
        assert!(!is_closed_isolated_execution_workspace(Some(&w)));
    }

    #[test]
    fn isolated_active_is_not_closed() {
        let w = make(
            ExecutionWorkspaceMode::IsolatedWorkspace,
            ExecutionWorkspaceStatus::Active,
            false,
        );
        assert!(!is_closed_isolated_execution_workspace(Some(&w)));
    }

    #[test]
    fn isolated_archived_is_closed() {
        let w = make(
            ExecutionWorkspaceMode::IsolatedWorkspace,
            ExecutionWorkspaceStatus::Archived,
            false,
        );
        assert!(is_closed_isolated_execution_workspace(Some(&w)));
    }

    #[test]
    fn isolated_cleanup_failed_is_closed() {
        let w = make(
            ExecutionWorkspaceMode::IsolatedWorkspace,
            ExecutionWorkspaceStatus::CleanupFailed,
            false,
        );
        assert!(is_closed_isolated_execution_workspace(Some(&w)));
    }

    #[test]
    fn isolated_with_closed_at_is_closed_even_if_active() {
        let w = make(
            ExecutionWorkspaceMode::IsolatedWorkspace,
            ExecutionWorkspaceStatus::Active,
            true,
        );
        assert!(is_closed_isolated_execution_workspace(Some(&w)));
    }

    #[test]
    fn message_uses_workspace_name() {
        let w = ExecutionWorkspaceGuardTarget {
            closed_at: None,
            mode: ExecutionWorkspaceMode::IsolatedWorkspace,
            name: "feature-x".into(),
            status: ExecutionWorkspaceStatus::Active,
        };
        let msg = get_closed_isolated_execution_workspace_message(&w);
        assert!(msg.contains("feature-x"));
    }
}


#[cfg(test)]
mod internal_tests_r770 {
    use super::*;

    // ---- Round 770: pc-execution-workspace-guards::lib 派生方法 ----

    /// ExecutionWorkspaceStatus::as_str 5 个变体。
    #[test]
    fn r770_workspace_status_as_str() {
        assert_eq!(ExecutionWorkspaceStatus::Active.as_str(), "active");
        assert_eq!(ExecutionWorkspaceStatus::Idle.as_str(), "idle");
        assert_eq!(ExecutionWorkspaceStatus::InReview.as_str(), "in_review");
        assert_eq!(ExecutionWorkspaceStatus::Archived.as_str(), "archived");
        assert_eq!(ExecutionWorkspaceStatus::CleanupFailed.as_str(), "cleanup_failed");
    }

    /// ExecutionWorkspaceStatus::parse 全部 5 个 + 未知。
    #[test]
    fn r770_workspace_status_parse() {
        assert_eq!(ExecutionWorkspaceStatus::parse("active"), Some(ExecutionWorkspaceStatus::Active));
        assert_eq!(ExecutionWorkspaceStatus::parse("idle"), Some(ExecutionWorkspaceStatus::Idle));
        assert_eq!(ExecutionWorkspaceStatus::parse("in_review"), Some(ExecutionWorkspaceStatus::InReview));
        assert_eq!(ExecutionWorkspaceStatus::parse("archived"), Some(ExecutionWorkspaceStatus::Archived));
        assert_eq!(ExecutionWorkspaceStatus::parse("cleanup_failed"), Some(ExecutionWorkspaceStatus::CleanupFailed));
        assert_eq!(ExecutionWorkspaceStatus::parse("unknown"), None);
    }

    /// ExecutionWorkspaceMode::as_str 3 个变体。
    #[test]
    fn r770_workspace_mode_as_str() {
        assert_eq!(ExecutionWorkspaceMode::SharedWorkspace.as_str(), "shared_workspace");
        assert_eq!(ExecutionWorkspaceMode::IsolatedWorkspace.as_str(), "isolated_workspace");
        assert_eq!(ExecutionWorkspaceMode::OperatorBranch.as_str(), "operator_branch");
    }

    /// ExecutionWorkspaceMode::parse round trip.
    #[test]
    fn r770_workspace_mode_parse() {
        assert_eq!(ExecutionWorkspaceMode::parse("shared_workspace"), Some(ExecutionWorkspaceMode::SharedWorkspace));
        assert_eq!(ExecutionWorkspaceMode::parse("isolated_workspace"), Some(ExecutionWorkspaceMode::IsolatedWorkspace));
        assert_eq!(ExecutionWorkspaceMode::parse("operator_branch"), Some(ExecutionWorkspaceMode::OperatorBranch));
        assert_eq!(ExecutionWorkspaceMode::parse("unknown"), None);
    }

    /// closed_execution_workspace_statuses: archived + cleanup_failed.
    #[test]
    fn r770_closed_statuses_count() {
        let s = closed_execution_workspace_statuses();
        assert_eq!(s.len(), 2);
        assert!(s.contains("archived"));
        assert!(s.contains("cleanup_failed"));
    }

    /// is_closed_isolated_execution_workspace: 4 种场景.
    #[test]
    fn r770_is_closed_isolated_workspace_combinations() {
        // None -> false
        assert!(!is_closed_isolated_execution_workspace(None));

        // isolated + active -> false
        let active = ExecutionWorkspaceGuardTarget {
            closed_at: None,
            mode: ExecutionWorkspaceMode::IsolatedWorkspace,
            name: "ws1".into(),
            status: ExecutionWorkspaceStatus::Active,
        };
        assert!(!is_closed_isolated_execution_workspace(Some(&active)));

        // isolated + archived -> true
        let archived = ExecutionWorkspaceGuardTarget {
            closed_at: None,
            mode: ExecutionWorkspaceMode::IsolatedWorkspace,
            name: "ws1".into(),
            status: ExecutionWorkspaceStatus::Archived,
        };
        assert!(is_closed_isolated_execution_workspace(Some(&archived)));

        // isolated + closed_at -> true 即使 status = active
        let closed_at = ExecutionWorkspaceGuardTarget {
            closed_at: Some("2026-01-01T00:00:00Z".into()),
            mode: ExecutionWorkspaceMode::IsolatedWorkspace,
            name: "ws1".into(),
            status: ExecutionWorkspaceStatus::Active,
        };
        assert!(is_closed_isolated_execution_workspace(Some(&closed_at)));

        // shared + archived -> false (mode 必须 isolated)
        let shared_archived = ExecutionWorkspaceGuardTarget {
            closed_at: None,
            mode: ExecutionWorkspaceMode::SharedWorkspace,
            name: "ws1".into(),
            status: ExecutionWorkspaceStatus::Archived,
        };
        assert!(!is_closed_isolated_execution_workspace(Some(&shared_archived)));
    }

    /// get_closed_isolated_execution_workspace_message 包含必要字段。
    #[test]
    fn r770_message_contains_required_fields() {
        let ws = ExecutionWorkspaceGuardTarget {
            closed_at: None,
            mode: ExecutionWorkspaceMode::IsolatedWorkspace,
            name: "ws-NAME".into(),
            status: ExecutionWorkspaceStatus::Archived,
        };
        let msg = get_closed_isolated_execution_workspace_message(&ws);
        assert!(msg.contains("ws-NAME"));
        // mode not in message
    }
}
