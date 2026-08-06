//! Round 348：把 instance_settings.general.censorUsernameInLogs 接入 redaction。

use chrono::{Duration, Utc};
use pc_heartbeat::recovery::build_stale_run_evaluation_description::{
    StaleEvaluationLevel, StaleIssueLinkView, StaleRunEventView, StaleRunEvidenceView,
};
use pc_heartbeat::recovery::redact_watchdog_evidence_text::CurrentUserRedactionOptions;
use pc_repos::Db;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 2, 0).await.unwrap()
}

async fn ensure_instance_settings(db: &Db, general: serde_json::Value) {
    sqlx::query(
        "INSERT INTO instance_settings (singleton_key, general, experimental) \
         VALUES ('singleton', $1::jsonb, '{}'::jsonb) \
         ON CONFLICT (singleton_key) DO UPDATE \
         SET general = EXCLUDED.general, updated_at = now()",
    )
    .bind(general)
    .execute(db.pool())
    .await
    .unwrap();
}

/// 纯单元层：description builder 应在传入 redaction options 后脱敏。
#[test]
fn description_redacts_when_redaction_option_enabled() {
    use pc_heartbeat::recovery::build_stale_run_evaluation_description::{
        build_stale_run_evaluation_description, BuildStaleRunEvaluationDescriptionInput,
        StaleAgentView, StaleRunView, StaleSourceIssueView,
    };
    let run = StaleRunView {
        id: Uuid::new_v4(),
        agent_id: Uuid::new_v4(),
        invocation_source: "manual".to_owned(),
        trigger_detail: None,
        started_at: Some(Utc::now() - Duration::hours(4)),
        process_started_at: Some(Utc::now() - Duration::hours(4)),
        last_output_at: Some(Utc::now() - Duration::minutes(20)),
        last_output_seq: 1,
        process_pid: None,
        process_group_id: None,
    };
    let agent = StaleAgentView {
        id: run.agent_id,
        name: "engineer-1".to_owned(),
        adapter_type: "process".to_owned(),
    };
    let evidence = StaleRunEvidenceView {
        safe_tail: Some("tail mentions alice".to_owned()),
        silence_age_ms: 20 * 60_000,
        recent_events: vec![StaleRunEventView {
            event_type: "lifecycle".to_owned(),
            level: Some("info".to_owned()),
            created_at: "2024-01-01T10:00:00Z".to_owned(),
            message: Some("alice ran the step".to_owned()),
        }],
        child_issues: vec![StaleIssueLinkView {
            id: Uuid::new_v4(),
            identifier: Some("CHILD-1".to_owned()),
            title: "child".to_owned(),
            status: "todo".to_owned(),
        }],
        blockers: vec![],
    };
    let redaction = CurrentUserRedactionOptions {
        enabled: true,
        user_names: vec!["alice".to_owned()],
        home_dirs: vec!["/Users/alice".to_owned()],
        replacement: None,
    };
    let body = build_stale_run_evaluation_description(&BuildStaleRunEvaluationDescriptionInput {
        run: &run,
        running_agent: &agent,
        source_issue: Some(&StaleSourceIssueView {
            id: Uuid::new_v4(),
            identifier: Some("ROOT-1".to_owned()),
        }),
        prefix: "PAP",
        evidence: &evidence,
        level: StaleEvaluationLevel::Critical,
        redaction: Some(redaction),
    });
    assert!(
        body.contains("a*****"),
        "expected redacted name in description"
    );
    assert!(!body.contains("alice"));
}

#[tokio::test]
async fn instance_settings_censor_username_drives_redaction() {
    use pc_heartbeat::recovery::build_stale_run_evaluation_description::{
        build_stale_run_evaluation_description, BuildStaleRunEvaluationDescriptionInput,
        StaleAgentView, StaleRunView, StaleSourceIssueView,
    };
    use pc_repos::settings::InstanceSettingRow;
    let db = connect().await;
    ensure_instance_settings(
        &db,
        serde_json::json!({
            "censorUsernameInLogs": true,
            "usernames": ["alice"],
            "homeDirs": ["/Users/alice"],
        }),
    )
    .await;
    let setting: InstanceSettingRow = sqlx::query_as::<_, InstanceSettingRow>(
        "SELECT id, singleton_key, default_environment_id, general, experimental, created_at, updated_at          FROM instance_settings WHERE singleton_key = 'singleton'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    let general = setting.general.as_object().expect("general object");
    let enabled = general
        .get("censorUsernameInLogs")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    assert!(enabled, "instance settings should expose enabled flag");
    let user_names = general
        .get("usernames")
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_owned()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let home_dirs = general
        .get("homeDirs")
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_owned()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let redaction = if enabled {
        Some(CurrentUserRedactionOptions {
            enabled: true,
            user_names,
            home_dirs,
            replacement: None,
        })
    } else {
        None
    };
    let run = StaleRunView {
        id: Uuid::new_v4(),
        agent_id: Uuid::new_v4(),
        invocation_source: "manual".to_owned(),
        trigger_detail: None,
        started_at: Some(Utc::now() - Duration::hours(4)),
        process_started_at: Some(Utc::now() - Duration::hours(4)),
        last_output_at: Some(Utc::now() - Duration::minutes(20)),
        last_output_seq: 1,
        process_pid: None,
        process_group_id: None,
    };
    let evidence = StaleRunEvidenceView {
        safe_tail: Some("alice tail".to_owned()),
        silence_age_ms: 20 * 60_000,
        recent_events: vec![StaleRunEventView {
            event_type: "lifecycle".to_owned(),
            level: Some("info".to_owned()),
            created_at: "2024-01-01T10:00:00Z".to_owned(),
            message: Some("alice event".to_owned()),
        }],
        child_issues: vec![StaleIssueLinkView {
            id: Uuid::new_v4(),
            identifier: Some("CHILD-1".to_owned()),
            title: "child".to_owned(),
            status: "todo".to_owned(),
        }],
        blockers: vec![],
    };
    let body = build_stale_run_evaluation_description(&BuildStaleRunEvaluationDescriptionInput {
        run: &run,
        running_agent: &StaleAgentView {
            id: run.agent_id,
            name: "engineer-1".to_owned(),
            adapter_type: "process".to_owned(),
        },
        source_issue: Some(&StaleSourceIssueView {
            id: Uuid::new_v4(),
            identifier: Some("ROOT-1".to_owned()),
        }),
        prefix: "PAP",
        evidence: &evidence,
        level: StaleEvaluationLevel::Critical,
        redaction,
    });
    assert!(body.contains("a*****"));
    assert!(!body.contains("alice"));
}
