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

/// Set of wake reasons accepted by `is_verified_issue_tree_control_interaction_wake`.
///
/// Node parity: `ISSUE_TREE_CONTROL_INTERACTION_WAKE_REASONS`.
pub const ISSUE_TREE_CONTROL_INTERACTION_WAKE_REASONS: &[&str] = &[
    "issue_commented",
    "issue_reopened_via_comment",
    "issue_comment_mentioned",
];

/// Map from wake reason to the set of allowed `contextSnapshot.source` values.
///
/// Node parity: `ISSUE_TREE_CONTROL_INTERACTION_WAKE_SOURCES`.
pub fn issue_tree_control_interaction_wake_sources(reason: &str) -> &'static [&'static str] {
    match reason {
        "issue_commented" => &["issue.comment"],
        "issue_reopened_via_comment" => &["issue.comment.reopen"],
        "issue_comment_mentioned" => &["comment.mention"],
        _ => &[],
    }
}

/// Read a non-empty trimmed string field from a JSON object snapshot.
///
/// Node parity: `readNonEmptyStringFromRecord`.
pub fn read_non_empty_string_from_record(snapshot: Option<&Value>, key: &str) -> Option<String> {
    let obj = snapshot?.as_object()?;
    let value = obj.get(key)?;
    value.as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Read the interaction wake comment id from a snapshot.
///
/// Prefers the last entry of `wakeCommentIds[]`, falls back to
/// `wakeCommentId` / `commentId`. Returns `None` if none is set.
pub fn read_interaction_wake_comment_id(snapshot: Option<&Value>) -> Option<String> {
    let obj = snapshot?.as_object()?;
    if let Some(arr) = obj.get("wakeCommentIds").and_then(|v| v.as_array()) {
        let latest = arr
            .iter()
            .rev()
            .filter_map(|v| v.as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
            .next();
        if latest.is_some() {
            return latest;
        }
    }
    read_non_empty_string_from_record(snapshot, "wakeCommentId")
        .or_else(|| read_non_empty_string_from_record(snapshot, "commentId"))
}

/// Whether the snapshot's `source` matches the wake reason's allowed source set.
pub fn has_verified_interaction_source(wake_reason: &str, snapshot: &Value) -> bool {
    let Some(source) = read_non_empty_string_from_record(Some(snapshot), "source") else {
        return false;
    };
    issue_tree_control_interaction_wake_sources(wake_reason)
        .iter()
        .any(|s| *s == source)
}

/// Whether the actor type / id matches the comment authorship.
pub fn actor_matches_comment(
    actor_type: Option<&str>,
    actor_id: Option<&str>,
    comment_author_agent_id: Option<&str>,
    comment_author_user_id: Option<&str>,
) -> bool {
    let Some(actor_type) = actor_type else {
        return false;
    };
    if actor_type == "system" {
        return true;
    }
    let Some(actor_id) = actor_id else {
        return false;
    };
    match actor_type {
        "agent" => comment_author_agent_id.map(|s| s == actor_id).unwrap_or(false),
        "user" => comment_author_user_id.map(|s| s == actor_id).unwrap_or(false),
        _ => false,
    }
}

/// Extract the wake reason from a context snapshot, falling back to `reason`.
pub fn read_wake_reason(snapshot: Option<&Value>) -> Option<String> {
    read_non_empty_string_from_record(snapshot, "wakeReason")
        .or_else(|| read_non_empty_string_from_record(snapshot, "reason"))
}

/// Pure helper: validate that a context snapshot describes a verified interaction
/// wake without performing any DB lookup.
///
/// Node parity: first three guards of `isVerifiedIssueTreeControlInteractionWake`.
///
/// Returns `Ok(true)` if the snapshot passes all gates. Returns `Ok(false)` if any
/// gate rejects. The DB-dependent final check (comment + wakeup request) lives in
/// the service layer.
pub fn is_verified_issue_tree_control_wake_snapshot(snapshot: Option<&Value>) -> bool {
    let Some(wake_reason) = read_wake_reason(snapshot) else {
        return false;
    };
    if !ISSUE_TREE_CONTROL_INTERACTION_WAKE_REASONS
        .iter()
        .any(|r| *r == wake_reason)
    {
        return false;
    }
    let Some(snapshot) = snapshot else {
        return false;
    };
    if !has_verified_interaction_source(&wake_reason, snapshot) {
        return false;
    }
    read_interaction_wake_comment_id(Some(snapshot)).is_some()
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
    use serde_json::json;

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

    #[test]
    fn read_non_empty_string_basic() {
        let v = json!({ "wakeReason": "  hi  ", "empty": "   ", "num": 1 });
        assert_eq!(
            read_non_empty_string_from_record(Some(&v), "wakeReason").as_deref(),
            Some("hi")
        );
        assert!(read_non_empty_string_from_record(Some(&v), "empty").is_none());
        assert!(read_non_empty_string_from_record(Some(&v), "num").is_none());
        assert!(read_non_empty_string_from_record(Some(&v), "missing").is_none());
        assert!(read_non_empty_string_from_record(None, "x").is_none());
    }

    #[test]
    fn read_interaction_wake_comment_id_prefers_array_last() {
        let v = json!({ "wakeCommentIds": ["a", "b", "c"] });
        assert_eq!(
            read_interaction_wake_comment_id(Some(&v)).as_deref(),
            Some("c")
        );
    }

    #[test]
    fn read_interaction_wake_comment_id_falls_back_to_string() {
        let v = json!({ "wakeCommentId": "x" });
        assert_eq!(
            read_interaction_wake_comment_id(Some(&v)).as_deref(),
            Some("x")
        );
        let v = json!({ "commentId": "y" });
        assert_eq!(
            read_interaction_wake_comment_id(Some(&v)).as_deref(),
            Some("y")
        );
    }

    #[test]
    fn read_interaction_wake_comment_id_skips_blank_array_entries() {
        let v = json!({ "wakeCommentIds": ["", "  ", "valid"] });
        assert_eq!(
            read_interaction_wake_comment_id(Some(&v)).as_deref(),
            Some("valid")
        );
    }

    #[test]
    fn read_interaction_wake_comment_id_none() {
        let v = json!({});
        assert!(read_interaction_wake_comment_id(Some(&v)).is_none());
        assert!(read_interaction_wake_comment_id(None).is_none());
    }

    #[test]
    fn has_verified_interaction_source_each_reason() {
        let v = json!({ "source": "issue.comment" });
        assert!(has_verified_interaction_source("issue_commented", &v));
        let v = json!({ "source": "issue.comment.reopen" });
        assert!(has_verified_interaction_source(
            "issue_reopened_via_comment",
            &v
        ));
        let v = json!({ "source": "comment.mention" });
        assert!(has_verified_interaction_source("issue_comment_mentioned", &v));
        let v = json!({ "source": "wrong.source" });
        assert!(!has_verified_interaction_source("issue_commented", &v));
    }

    #[test]
    fn has_verified_interaction_source_missing() {
        let v = json!({});
        assert!(!has_verified_interaction_source("issue_commented", &v));
    }

    #[test]
    fn actor_matches_comment_logic() {
        assert!(actor_matches_comment(Some("system"), None, None, None));
        assert!(actor_matches_comment(
            Some("agent"),
            Some("a1"),
            Some("a1"),
            None
        ));
        assert!(!actor_matches_comment(
            Some("agent"),
            Some("a1"),
            Some("a2"),
            None
        ));
        assert!(actor_matches_comment(
            Some("user"),
            Some("u1"),
            None,
            Some("u1")
        ));
        assert!(!actor_matches_comment(Some("agent"), Some("a1"), None, Some("u1")));
        assert!(!actor_matches_comment(None, Some("a1"), Some("a1"), None));
        assert!(!actor_matches_comment(Some("agent"), None, Some("a1"), None));
        assert!(!actor_matches_comment(Some("other"), Some("x"), None, None));
    }

    #[test]
    fn read_wake_reason_fallback() {
        let v = json!({ "wakeReason": "issue_commented" });
        assert_eq!(
            read_wake_reason(Some(&v)).as_deref(),
            Some("issue_commented")
        );
        let v = json!({ "reason": "issue_commented" });
        assert_eq!(
            read_wake_reason(Some(&v)).as_deref(),
            Some("issue_commented")
        );
        let v = json!({ "wakeReason": "  trimmed  " });
        assert_eq!(
            read_wake_reason(Some(&v)).as_deref(),
            Some("trimmed")
        );
        assert!(read_wake_reason(None).is_none());
    }

    #[test]
    fn verified_wake_snapshot_happy_path() {
        let v = json!({
            "wakeReason": "issue_commented",
            "source": "issue.comment",
            "wakeCommentId": "c1"
        });
        assert!(is_verified_issue_tree_control_wake_snapshot(Some(&v)));
    }

    #[test]
    fn verified_wake_snapshot_rejects_unknown_reason() {
        let v = json!({ "wakeReason": "other", "source": "issue.comment" });
        assert!(!is_verified_issue_tree_control_wake_snapshot(Some(&v)));
    }

    #[test]
    fn verified_wake_snapshot_rejects_source_mismatch() {
        let v = json!({
            "wakeReason": "issue_commented",
            "source": "wrong.source",
            "wakeCommentId": "c1"
        });
        assert!(!is_verified_issue_tree_control_wake_snapshot(Some(&v)));
    }

    #[test]
    fn verified_wake_snapshot_rejects_no_comment_id() {
        let v = json!({
            "wakeReason": "issue_commented",
            "source": "issue.comment"
        });
        assert!(!is_verified_issue_tree_control_wake_snapshot(Some(&v)));
    }

    #[test]
    fn verified_wake_snapshot_rejects_none_snapshot() {
        assert!(!is_verified_issue_tree_control_wake_snapshot(None));
    }
}
