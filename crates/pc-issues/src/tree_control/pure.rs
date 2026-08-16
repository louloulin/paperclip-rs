#![forbid(unsafe_code)]

//! Tree control pure helpers \u2014 1:1 port of paperclip/server/src/services/issue-tree-control.ts
//!
//! R723: zero-DB helpers for status coercion, terminal detection, skip-reason
//! derivation, and cancel-snapshot restoration.

use serde_json::Value;

/// Allowed issue status set (Node ISSUE_STATUSES subset).
pub const ISSUE_STATUSES: &[&str] = &[
    "backlog", "todo", "in_progress", "in_review", "blocked", "done", "cancelled", "archived",
];

/// Terminal issue status set.
pub const TERMINAL_ISSUE_STATUSES: &[&str] = &["done", "cancelled", "archived"];

/// Default release policy (Node DEFAULT_RELEASE_POLICY).
pub const DEFAULT_RELEASE_POLICY: &str = "when_clear";

/// Coerce an arbitrary status string into a known IssueStatus.
///
/// Node parity: coerceIssueStatus(status) \u2014 unknown values fall back to "backlog".
pub fn coerce_issue_status(status: &str) -> &'static str {
    if ISSUE_STATUSES.iter().any(|s| *s == status) {
        // Find the static string.
        ISSUE_STATUSES.iter().find(|s| **s == status).copied().unwrap_or("backlog")
    } else {
        "backlog"
    }
}

/// Test whether a status (after coercion) is terminal.
pub fn is_terminal_issue(status: &str) -> bool {
    let coerced = coerce_issue_status(status);
    TERMINAL_ISSUE_STATUSES.iter().any(|s| *s == coerced)
}

/// Normalize a release policy, falling back to the default.
pub fn normalize_release_policy(policy: Option<&str>) -> &'static str {
    match policy {
        Some("manual") => "manual",
        Some("when_clear") => "when_clear",
        Some("scheduled") => "scheduled",
        _ => DEFAULT_RELEASE_POLICY,
    }
}

/// Restore the status that was active when a cancel snapshot was taken.
///
/// Node parity: restoreStatusFromCancelSnapshot(status) \u2014 returns the original
/// status if it was not yet terminal at cancel time, otherwise null.
pub fn restore_status_from_cancel_snapshot(status: &str) -> Option<&'static str> {
    let coerced = coerce_issue_status(status);
    if is_terminal_issue(coerced) { return None; }
    Some(coerced)
}

/// Derive a skip reason for an issue in tree-control mode.
///
/// Node parity: issueSkipReason(input).
pub fn issue_skip_reason(input: IssueSkipReasonInput<'_>) -> Option<&'static str> {
    let status = coerce_issue_status(input.issue_status);
    if input.mode == "restore" {
        if input.active_cancel_member && status != "cancelled" { return Some("changed_after_cancel"); }
        if status != "cancelled" { return Some("not_cancelled"); }
        if !input.active_cancel_member { return Some("not_cancelled_by_tree_control"); }
        let snap_status = match input.active_cancel_snapshot_status {
            Some(s) => coerce_issue_status(s),
            None => status,
        };
        return if is_terminal_issue(snap_status) { Some("terminal_status") } else { None };
    }
    if is_terminal_issue(status) { return Some("terminal_status"); }
    match input.mode {
        "pause" if input.active_pause_hold_count > 0 => Some("already_held"),
        "resume" if input.active_pause_hold_count == 0 => Some("not_held"),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct IssueSkipReasonInput<'a> {
    pub mode: &'a str,
    pub issue_status: &'a str,
    pub active_pause_hold_count: usize,
    pub active_cancel_member: bool,
    pub active_cancel_snapshot_status: Option<&'a str>,
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    #[test]
    fn coerce_status_known() {
        assert_eq!(coerce_issue_status("done"), "done");
        assert_eq!(coerce_issue_status("in_progress"), "in_progress");
    }

    #[test]
    fn coerce_status_unknown_falls_back() {
        assert_eq!(coerce_issue_status("flying"), "backlog");
        assert_eq!(coerce_issue_status(""), "backlog");
    }

    #[test]
    fn is_terminal_issue_basic() {
        assert!(is_terminal_issue("done"));
        assert!(is_terminal_issue("cancelled"));
        assert!(is_terminal_issue("archived"));
        assert!(!is_terminal_issue("in_progress"));
        assert!(!is_terminal_issue("unknown"));
    }

    #[test]
    fn normalize_release_policy_default() {
        assert_eq!(normalize_release_policy(None), DEFAULT_RELEASE_POLICY);
        assert_eq!(normalize_release_policy(Some("manual")), "manual");
    }

    #[test]
    fn restore_status_terminal_returns_none() {
        assert_eq!(restore_status_from_cancel_snapshot("done"), None);
        assert_eq!(restore_status_from_cancel_snapshot("cancelled"), None);
        assert_eq!(restore_status_from_cancel_snapshot("in_progress"), Some("in_progress"));
    }

    #[test]
    fn skip_reason_restore_changed_after_cancel() {
        let inp = IssueSkipReasonInput {
            mode: "restore",
            issue_status: "in_progress",
            active_pause_hold_count: 0,
            active_cancel_member: true,
            active_cancel_snapshot_status: Some("cancelled"),
        };
        assert_eq!(issue_skip_reason(inp), Some("changed_after_cancel"));
    }

    #[test]
    fn skip_reason_restore_terminal_snapshot() {
        let inp = IssueSkipReasonInput {
            mode: "restore",
            issue_status: "cancelled",
            active_pause_hold_count: 0,
            active_cancel_member: true,
            active_cancel_snapshot_status: Some("done"),
        };
        assert_eq!(issue_skip_reason(inp), Some("terminal_status"));
    }

    #[test]
    fn skip_reason_pause_already_held() {
        let inp = IssueSkipReasonInput {
            mode: "pause",
            issue_status: "in_progress",
            active_pause_hold_count: 1,
            active_cancel_member: false,
            active_cancel_snapshot_status: None,
        };
        assert_eq!(issue_skip_reason(inp), Some("already_held"));
    }

    #[test]
    fn skip_reason_resume_not_held() {
        let inp = IssueSkipReasonInput {
            mode: "resume",
            issue_status: "blocked",
            active_pause_hold_count: 0,
            active_cancel_member: false,
            active_cancel_snapshot_status: None,
        };
        assert_eq!(issue_skip_reason(inp), Some("not_held"));
    }

    #[test]
    fn skip_reason_terminal_status() {
        let inp = IssueSkipReasonInput {
            mode: "pause",
            issue_status: "done",
            active_pause_hold_count: 0,
            active_cancel_member: false,
            active_cancel_snapshot_status: None,
        };
        assert_eq!(issue_skip_reason(inp), Some("terminal_status"));
    }
}
