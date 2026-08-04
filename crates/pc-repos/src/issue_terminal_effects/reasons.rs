use serde_json::{json, Value};

use super::TerminalEffectIssue;

fn issue_label(issue: &TerminalEffectIssue<'_>) -> String {
    match issue.identifier.filter(|value| !value.trim().is_empty()) {
        Some(identifier) => format!("{identifier}: {}", issue.title),
        None => issue.title.to_owned(),
    }
}

pub fn summary_failure_reason(issue: &TerminalEffectIssue<'_>) -> Option<String> {
    let label = issue_label(issue);
    match issue.status {
        "cancelled" => Some(format!(
            "Summary generation task {label} was cancelled before writing a summary."
        )),
        "done" => Some(format!(
            "Summary generation task {label} finished without writing a summary."
        )),
        _ => None,
    }
}

pub fn status_card_failure_reason(issue: &TerminalEffectIssue<'_>) -> Option<String> {
    let label = issue_label(issue);
    match issue.status {
        "cancelled" => Some(format!(
            "Status-card generation task {label} was cancelled before writing a summary."
        )),
        "blocked" => Some(format!(
            "Status-card generation task {label} was blocked before writing a summary; re-run to retry."
        )),
        "done" => Some(format!(
            "Status-card generation task {label} finished without writing a summary."
        )),
        _ => None,
    }
}

pub(super) fn administrative_result(kind: &str, previous: Option<&Value>) -> Value {
    match kind {
        "ask_user_questions" => json!({
            "version": 1,
            "outcome": "issue_closed",
            "reason": null,
            "answers": [],
            "summaryMarkdown": null,
        }),
        "request_item_verdicts" => json!({
            "version": 1,
            "outcome": "issue_closed",
            "reason": null,
            "complete": false,
            "items": previous.and_then(|value| value.get("items")).cloned().unwrap_or_else(|| json!([])),
        }),
        _ => json!({ "version": 1, "outcome": "issue_closed", "reason": null }),
    }
}
