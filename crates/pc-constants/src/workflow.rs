//! Workflow / Routine / Pipeline 域常量。

/// Pipeline case 状态（与 stage status enum 对齐）。
pub const PIPELINE_CASE_STATUSES: &[&str] = &[
    "pending",
    "in_progress",
    "blocked",
    "completed",
    "failed",
    "cancelled",
    "skipped",
];

/// Pipeline stage kind。
pub const PIPELINE_STAGE_KINDS: &[&str] = &[
    "trigger",
    "llm_prompt",
    "tool_call",
    "human_approval",
    "branch",
    "merge",
    "terminal",
];

/// Pipeline trigger kind。
pub const PIPELINE_TRIGGER_KINDS: &[&str] =
    &["manual", "schedule", "webhook", "issue_change", "agent_run"];

/// Routine 触发类型。
pub const ROUTINE_TRIGGER_KINDS: &[&str] = &[
    "cron",
    "schedule",
    "webhook",
    "manual",
    "issue_change",
    "agent_run",
];

/// Routine 状态。
pub const ROUTINE_STATUSES: &[&str] = &["active", "paused", "archived"];

/// Decision effect type。
pub const DECISION_EFFECT_TYPES: &[&str] = &[
    "create_issue",
    "update_issue",
    "send_message",
    "trigger_pipeline",
    "trigger_routine",
    "assign_agent",
    "no_op",
];

/// Approval request status。
pub const APPROVAL_REQUEST_STATUSES: &[&str] =
    &["pending", "approved", "denied", "expired", "cancelled"];

/// Approval decision。
pub const APPROVAL_DECISIONS: &[&str] = &["approve", "deny", "abstain"];

/// Document annotation thread 状态。
pub const DOCUMENT_ANNOTATION_THREAD_STATUSES: &[&str] = &["open", "resolved"];

/// Document annotation anchor state。
pub const DOCUMENT_ANNOTATION_ANCHOR_STATES: &[&str] = &["active", "stale", "orphaned"];

/// Document annotation anchor confidence。
pub const DOCUMENT_ANNOTATION_ANCHOR_CONFIDENCES: &[&str] = &["high", "medium", "low"];

/// External object status category。
pub const EXTERNAL_OBJECT_STATUS_CATEGORIES: &[&str] =
    &["active", "deprecated", "blocked", "merged", "closed"];

/// External object status tone。
pub const EXTERNAL_OBJECT_STATUS_TONES: &[&str] =
    &["neutral", "info", "success", "warning", "danger"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_case_statuses_match_node() {
        assert!(PIPELINE_CASE_STATUSES.contains(&"pending"));
        assert!(PIPELINE_CASE_STATUSES.contains(&"completed"));
        assert!(PIPELINE_CASE_STATUSES.contains(&"cancelled"));
    }

    #[test]
    fn stage_kinds_contains_terminal() {
        assert!(PIPELINE_STAGE_KINDS.contains(&"terminal"));
        assert!(PIPELINE_STAGE_KINDS.contains(&"trigger"));
    }

    #[test]
    fn trigger_kinds_match_node() {
        assert!(PIPELINE_TRIGGER_KINDS.contains(&"webhook"));
        assert!(PIPELINE_TRIGGER_KINDS.contains(&"issue_change"));
    }

    #[test]
    fn routine_trigger_kinds_superset_pipeline() {
        // Routine triggers are a superset of pipeline triggers (adds cron / manual)
        for kind in PIPELINE_TRIGGER_KINDS {
            assert!(
                ROUTINE_TRIGGER_KINDS.contains(kind),
                "pipeline trigger {kind} should be in routine triggers"
            );
        }
    }

    #[test]
    fn decision_effect_types_includes_no_op() {
        assert!(DECISION_EFFECT_TYPES.contains(&"no_op"));
        assert!(DECISION_EFFECT_TYPES.contains(&"create_issue"));
    }

    #[test]
    fn approval_statuses_includes_pending_and_terminal() {
        assert!(APPROVAL_REQUEST_STATUSES.contains(&"pending"));
        assert!(APPROVAL_REQUEST_STATUSES.contains(&"approved"));
        assert!(APPROVAL_REQUEST_STATUSES.contains(&"denied"));
    }

    #[test]
    fn doc_annotation_thread_statuses_open_or_resolved() {
        assert_eq!(DOCUMENT_ANNOTATION_THREAD_STATUSES, &["open", "resolved"]);
    }
}
