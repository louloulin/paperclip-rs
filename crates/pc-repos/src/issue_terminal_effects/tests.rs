use serde_json::json;
use uuid::Uuid;

use super::reasons::administrative_result;
use super::{status_card_failure_reason, summary_failure_reason, TerminalEffectIssue};

fn issue(status: &'static str) -> TerminalEffectIssue<'static> {
    TerminalEffectIssue {
        id: Uuid::nil(),
        company_id: Uuid::nil(),
        identifier: Some("PC-42"),
        title: "Write summary",
        status,
    }
}

#[test]
fn summary_reason_only_applies_to_terminal_statuses() {
    assert!(summary_failure_reason(&issue("done")).unwrap().contains("finished"));
    assert!(summary_failure_reason(&issue("cancelled")).unwrap().contains("cancelled"));
    assert!(summary_failure_reason(&issue("blocked")).is_none());
}

#[test]
fn status_card_reason_includes_blocked_retry_guidance() {
    let reason = status_card_failure_reason(&issue("blocked")).unwrap();
    assert!(reason.contains("blocked"));
    assert!(reason.contains("re-run"));
}

#[test]
fn question_interaction_gets_empty_answers() {
    let result = administrative_result("ask_user_questions", None);
    assert_eq!(result["outcome"], "issue_closed");
    assert_eq!(result["answers"], json!([]));
}

#[test]
fn verdict_interaction_preserves_existing_items() {
    let result = administrative_result(
        "request_item_verdicts",
        Some(&json!({"items":[{"id":"a","verdict":"accept"}]})),
    );
    assert_eq!(result["items"][0]["id"], "a");
    assert_eq!(result["complete"], false);
}
