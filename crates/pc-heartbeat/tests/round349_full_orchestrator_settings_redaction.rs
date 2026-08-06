//! Round 349：主编排从 instance_settings 读取用户名脱敏配置。

use chrono::{Duration, Utc};
use pc_heartbeat::recovery::build_stale_run_evaluation_description::StaleAgentView;
use pc_heartbeat::recovery::create_or_update_stale_run_evaluation_full::{
    create_or_update_stale_run_evaluation_full, CreateOrUpdateStaleRunEvaluationInput,
};
use pc_heartbeat::recovery::scan_silent_active_runs_db::{
    SilentRunCandidate, StaleRunEvaluationOutcome,
};
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 2, 0).await.unwrap()
}

async fn replace_settings(db: &Db, general: serde_json::Value) {
    sqlx::query(
        "INSERT INTO instance_settings (singleton_key, general, experimental) \
         VALUES ('singleton', $1, '{}'::jsonb) \
         ON CONFLICT (singleton_key) DO UPDATE SET general = EXCLUDED.general",
    )
    .bind(general)
    .execute(db.pool())
    .await
    .unwrap();
}

async fn cleanup(db: &Db, company_id: Uuid) {
    sqlx::query("DELETE FROM activity_log WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM issues WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM heartbeat_run_events WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM heartbeat_runs WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM agents WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await
        .unwrap();
    replace_settings(db, json!({})).await;
}

#[tokio::test]
async fn full_orchestrator_redacts_evidence_from_instance_settings() {
    let db = connect().await;
    replace_settings(
        &db,
        json!({
            "censorUsernameInLogs": true,
            "usernames": ["alice"],
            "homeDirs": ["/Users/alice"]
        }),
    )
    .await;

    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let issue_prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind(format!("r349-{company_id}"))
        .bind(issue_prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'engineer-1', 'engineer', 'process', 'active')",
    )
    .bind(agent_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .unwrap();
    let last_output_at = Utc::now() - Duration::minutes(20);
    sqlx::query(
        "INSERT INTO heartbeat_runs \
         (id, company_id, agent_id, invocation_source, status, started_at, process_started_at, last_output_at) \
         VALUES ($1, $2, $3, 'manual', 'running', now() - interval '1 hour', now() - interval '1 hour', $4)",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .bind(last_output_at)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO heartbeat_run_events (run_id, company_id, agent_id, seq, event_type, level, message) \
         VALUES ($1, $2, $3, 1, 'stdout', 'info', 'alice opened /Users/alice/work')",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .unwrap();

    let outcome = create_or_update_stale_run_evaluation_full(
        &db,
        &CreateOrUpdateStaleRunEvaluationInput {
            run: SilentRunCandidate {
                id: run_id,
                company_id,
                agent_id,
                status: "running".to_owned(),
                last_output_at: Some(last_output_at),
                started_at: Some(Utc::now() - Duration::hours(1)),
                process_started_at: Some(Utc::now() - Duration::hours(1)),
                created_at: Utc::now() - Duration::hours(1),
                context_snapshot: None,
            },
            running_agent: StaleAgentView {
                id: agent_id,
                name: "engineer-1".to_owned(),
                adapter_type: "process".to_owned(),
            },
            source_issue: None,
            source_issue_row: None,
            source_issue_origin_kind: None,
            evaluation_owner_agent_id: None,
            now: Utc::now(),
        },
    )
    .await
    .unwrap();
    let evaluation_id = match outcome {
        StaleRunEvaluationOutcome::Created(id) => id,
        other => panic!("expected Created, got {other:?}"),
    };
    let description: String = sqlx::query_scalar("SELECT description FROM issues WHERE id = $1")
        .bind(evaluation_id)
        .fetch_one(db.pool())
        .await
        .unwrap();

    assert!(description.contains("a*****"));
    assert!(description.contains("/Users/a*****/work"));
    assert!(!description.contains("alice"));

    cleanup(&db, company_id).await;
}
