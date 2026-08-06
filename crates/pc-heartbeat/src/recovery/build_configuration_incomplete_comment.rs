use crate::recovery::build_recovery_issue_in_place_escalation_comment::EscalationRunView;
use crate::recovery::summarize_run_failure::{
    summarize_run_failure_for_issue_comment, RunFailureView,
};

pub fn build_configuration_incomplete_comment(latest_run: &EscalationRunView) -> String {
    let failure_summary = summarize_run_failure_for_issue_comment(Some(&RunFailureView {
        error: latest_run.error.as_deref(),
        error_code: latest_run.error_code.as_deref(),
    }))
    .unwrap_or("");

    [
        "Paperclip classified the active review participant's latest adapter failure as `configuration_incomplete`. Moving the issue to `blocked` with the configuration fix recorded instead of repeatedly requeueing the reviewer.",
        failure_summary,
    ]
    .join("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn view(error: Option<&str>, error_code: Option<&str>) -> EscalationRunView {
        EscalationRunView {
            id: uuid::Uuid::nil(),
            agent_id: Some(uuid::Uuid::nil()),
            status: "failed".to_owned(),
            error: error.map(str::to_owned),
            error_code: error_code.map(str::to_owned),
            context_snapshot: Some(json!({})),
        }
    }

    #[test]
    fn renders_configuration_guidance_and_failure_summary() {
        let body = build_configuration_incomplete_comment(&view(Some("missing API key"), None));
        assert!(body.starts_with(
            "Paperclip classified the active review participant's latest adapter failure as `configuration_incomplete`."
        ));
        assert!(body.contains("withheld"));
        assert!(body.contains("instead of repeatedly requeueing the reviewer"));
    }
}
