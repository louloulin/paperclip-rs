use serde_json::{json, Value};
use uuid::Uuid;

pub struct RecoveryCommentDisplayInput<'a> {
    pub cause: &'a str,
    pub latest_run_id: Option<Uuid>,
    pub latest_run_status: Option<&'a str>,
    pub recovery_action_id: Uuid,
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
    let mut rows = vec![
        json!({"type": "key_value", "label": "Recovery action", "value": input.recovery_action_id}),
        json!({"type": "key_value", "label": "Cause", "value": input.cause}),
        json!({"type": "key_value", "label": "Previous status", "value": input.previous_status}),
    ];
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
            recovery_action_id: action_id,
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
}
