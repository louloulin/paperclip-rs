use serde_json::{json, Value};
use uuid::Uuid;

pub struct RecoveryCommentDisplayInput<'a> {
    pub cause: &'a str,
    pub latest_run_id: Option<Uuid>,
    pub latest_run_status: Option<&'a str>,
    pub recovery_action_id: Option<Uuid>,
    pub previous_status: &'a str,
    pub recovery_owner_id: Option<Uuid>,
    pub recovery_owner_name: Option<&'a str>,
}

pub fn recovery_cause_title(cause: &str) -> &'static str {
    match cause {
        "process_lost" => "retries exhausted",
        "codex_output_inactivity_monitor" => "output-inactivity retry exhausted",
        "workspace_validation_failed" => "workspace validation failed",
        "configuration_incomplete" => "configuration incomplete",
        "execution_review_participant_recovery" => "reviewer recovery failed",
        "provider_quota" => "provider quota unavailable",
        "successful_run_missing_state" => "missing disposition recovery failed",
        _ => "execution path recovery failed",
    }
}

pub fn build_compact_recovery_presentation(title: &str) -> Value {
    let normalized_title = title.trim();
    let title = if normalized_title.chars().count() > 160 {
        let truncated: String = normalized_title.chars().take(159).collect();
        format!("{truncated}…")
    } else {
        normalized_title.to_owned()
    };
    json!({
        "kind": "system_notice",
        "tone": "warning",
        "title": title,
        "detailsDefaultOpen": false,
        "density": "compact"
    })
}

pub fn build_recovery_notice_metadata(input: &RecoveryCommentDisplayInput<'_>) -> Value {
    let mut rows = Vec::new();
    if let Some(action_id) = input.recovery_action_id {
        rows.push(json!({"type": "key_value", "label": "Recovery action", "value": action_id}));
    }
    rows.extend([
        json!({"type": "key_value", "label": "Cause", "value": input.cause}),
        json!({"type": "key_value", "label": "Previous status", "value": input.previous_status}),
    ]);
    if let (Some(agent_id), Some(name)) = (input.recovery_owner_id, input.recovery_owner_name) {
        rows.push(json!({
            "type": "agent_link",
            "label": "Recovery owner",
            "agentId": agent_id,
            "name": name.chars().take(160).collect::<String>()
        }));
    } else {
        rows.push(json!({
            "type": "key_value",
            "label": "Recovery owner",
            "value": "board"
        }));
    }
    if let (Some(run_id), Some(status)) = (input.latest_run_id, input.latest_run_status) {
        rows.push(json!({
            "type": "run_link",
            "label": "Latest run",
            "runId": run_id,
            "title": status
        }));
    }
    json!({
        "version": 1,
        "sourceRunId": input.latest_run_id,
        "sections": [{"title": "Recovery", "rows": rows}]
    })
}

pub fn metadata_references_recovery_action(
    metadata: Option<&Value>,
    recovery_action_id: Uuid,
) -> bool {
    let expected = recovery_action_id.to_string();
    metadata
        .and_then(|value| value.get("sections"))
        .and_then(Value::as_array)
        .is_some_and(|sections| {
            sections.iter().any(|section| {
                section
                    .get("rows")
                    .and_then(Value::as_array)
                    .is_some_and(|rows| {
                        rows.iter().any(|row| {
                            row.get("type").and_then(Value::as_str) == Some("key_value")
                                && row.get("label").and_then(Value::as_str)
                                    == Some("Recovery action")
                                && row.get("value").and_then(Value::as_str) == Some(&expected)
                        })
                    })
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presentation_matches_compact_system_notice_contract() {
        let value = build_compact_recovery_presentation("  Recovery notice  ");
        assert_eq!(value["kind"], "system_notice");
        assert_eq!(value["tone"], "warning");
        assert_eq!(value["title"], "Recovery notice");
        assert_eq!(value["density"], "compact");
    }

    #[test]
    fn metadata_contains_action_cause_owner_and_run() {
        let action_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        let value = build_recovery_notice_metadata(&RecoveryCommentDisplayInput {
            cause: "configuration_incomplete",
            latest_run_id: Some(run_id),
            latest_run_status: Some("failed"),
            recovery_action_id: Some(action_id),
            previous_status: "in_review",
            recovery_owner_id: Some(Uuid::new_v4()),
            recovery_owner_name: Some("reviewer"),
        });
        assert_eq!(value["version"], 1);
        assert_eq!(value["sourceRunId"], run_id.to_string());
        assert_eq!(
            value["sections"][0]["rows"][0]["value"],
            action_id.to_string()
        );
        assert_eq!(
            value["sections"][0]["rows"][1]["value"],
            "configuration_incomplete"
        );
        assert_eq!(value["sections"][0]["rows"][4]["runId"], run_id.to_string());
    }

    #[test]
    fn metadata_without_action_has_no_action_row() {
        let value = build_recovery_notice_metadata(&RecoveryCommentDisplayInput {
            cause: "recovery_issue_failed",
            latest_run_id: None,
            latest_run_status: None,
            recovery_action_id: None,
            previous_status: "todo",
            recovery_owner_id: None,
            recovery_owner_name: None,
        });
        let rows = value["sections"][0]["rows"].as_array().unwrap();
        // in-place 模式：没有 Recovery action 行，但仍有 Cause + Previous status + Recovery owner(默认 board)
        assert!(!rows.iter().any(|row| row["label"] == "Recovery action"));
        assert!(rows
            .iter()
            .any(|row| row["label"] == "Cause" && row["value"] == "recovery_issue_failed"));
        assert!(rows
            .iter()
            .any(|row| row["label"] == "Previous status" && row["value"] == "todo"));
        assert!(rows
            .iter()
            .any(|row| row["label"] == "Recovery owner" && row["value"] == "board"));
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn metadata_reference_matches_node_shape() {
        let action_id = Uuid::new_v4();
        let value = build_recovery_notice_metadata(&RecoveryCommentDisplayInput {
            cause: "process_lost",
            latest_run_id: None,
            latest_run_status: None,
            recovery_action_id: Some(action_id),
            previous_status: "in_progress",
            recovery_owner_id: None,
            recovery_owner_name: None,
        });
        assert!(metadata_references_recovery_action(Some(&value), action_id));
        assert!(!metadata_references_recovery_action(
            Some(&value),
            Uuid::new_v4()
        ));
    }
}
