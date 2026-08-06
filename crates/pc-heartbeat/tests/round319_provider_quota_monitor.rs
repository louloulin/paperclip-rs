//! Round 319：`scheduleProviderQuotaRecoveryMonitor` 的 PostgreSQL 验证。
use chrono::{Duration, Utc};
use pc_heartbeat::recovery::{
    reconcile_and_escalate_stranded_for_company, schedule_provider_quota_recovery_monitor,
    ScheduleProviderQuotaRecoveryMonitorInput,
};
use pc_repos::agent::{
    NewAgentWakeupRequest, WakeupActorType, WakeupRequestStatus, WakeupTriggerDetail,
};
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn fixture(db: &Db, status: &str) -> (Uuid, Uuid, Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let issue_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind(format!("r319-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'r319-agent', 'general', 'process', 'active')",
    )
    .bind(agent_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, \
         origin_fingerprint, assignee_agent_id, execution_policy, execution_state) \
         VALUES ($1, $2, 'r319-issue', $3, 'normal', 'system', $4, $5, $6, $7)",
    )
    .bind(issue_id)
    .bind(company_id)
    .bind(status)
    .bind(format!("r319-fp-{issue_id}"))
    .bind(agent_id)
    .bind(json!({"mode":"normal","commentRequired":true,"stages":[]}))
    .bind(json!({"status":"pending","currentParticipant":{"type":"agent","agentId":agent_id}}))
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, error_code, \
         context_snapshot, started_at, created_at) \
         VALUES ($1, $2, $3, 'failed', 'adapter_failed', $4, now(), now())",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .bind(json!({"issueId":issue_id}))
    .execute(db.pool())
    .await
    .unwrap();
    (company_id, agent_id, issue_id, run_id)
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM heartbeat_runs WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM agents WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
}

fn input(
    company_id: Uuid,
    agent_id: Uuid,
    issue_id: Uuid,
    run_id: Uuid,
    retry_at: chrono::DateTime<Utc>,
) -> ScheduleProviderQuotaRecoveryMonitorInput {
    ScheduleProviderQuotaRecoveryMonitorInput {
        company_id,
        issue_id,
        latest_run_id: run_id,
        target_agent_id: agent_id,
        retry_at,
        parsed_reset_time: true,
        now: Some(Utc::now()),
    }
}

fn wake_template(company_id: Uuid, agent_id: Uuid) -> NewAgentWakeupRequest {
    NewAgentWakeupRequest {
        company_id,
        agent_id,
        source: pc_repos::agent::HeartbeatInvocationSource::OnDemand,
        trigger_detail: Some(WakeupTriggerDetail::Manual),
        reason: None,
        payload: None,
        status: WakeupRequestStatus::Queued,
        coalesced_count: 0,
        requested_by_actor_type: Some(WakeupActorType::System),
        requested_by_actor_id: None,
        idempotency_key: None,
        run_id: None,
        error: None,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn schedules_monitor_without_blocking_issue_or_creating_recovery_action() {
    let db = connect().await;
    let (company_id, agent_id, issue_id, run_id) = fixture(&db, "in_progress").await;
    let retry_at = Utc::now() + Duration::hours(2);

    let result = schedule_provider_quota_recovery_monitor(
        &db,
        input(company_id, agent_id, issue_id, run_id, retry_at),
    )
    .await
    .unwrap()
    .expect("monitor should be scheduled");

    assert_eq!(result.issue_id, issue_id);
    let (status, next_check_at, scheduled_by, notes, policy, state): (
        String,
        Option<chrono::DateTime<Utc>>,
        Option<String>,
        Option<String>,
        Option<serde_json::Value>,
        Option<serde_json::Value>,
    ) = sqlx::query_as(
        "SELECT status, monitor_next_check_at, monitor_scheduled_by, monitor_notes, \
         execution_policy, execution_state FROM issues WHERE id = $1",
    )
    .bind(issue_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(status, "in_progress");
    assert_eq!(scheduled_by.as_deref(), Some("assignee"));
    assert!(notes.as_deref().unwrap_or_default().contains("provider"));
    assert!((next_check_at.unwrap() - retry_at).num_seconds().abs() < 2);
    assert_eq!(policy.unwrap()["stages"], json!([]));
    assert_eq!(state.unwrap()["monitor"]["status"], "scheduled");

    let action_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issue_recovery_actions WHERE source_issue_id = $1",
    )
    .bind(issue_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(action_count.0, 0);
    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn does_not_schedule_monitor_for_blocked_issue() {
    let db = connect().await;
    let (company_id, agent_id, issue_id, run_id) = fixture(&db, "blocked").await;
    let result = schedule_provider_quota_recovery_monitor(
        &db,
        input(
            company_id,
            agent_id,
            issue_id,
            run_id,
            Utc::now() + Duration::hours(2),
        ),
    )
    .await
    .unwrap();
    assert!(result.is_none());
    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issues WHERE id = $1 AND monitor_next_check_at IS NOT NULL",
    )
    .bind(issue_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count.0, 0);
    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn returns_existing_pending_monitor_without_restamping() {
    let db = connect().await;
    let (company_id, agent_id, issue_id, run_id) = fixture(&db, "in_progress").await;
    let first_retry = Utc::now() + Duration::hours(2);
    schedule_provider_quota_recovery_monitor(
        &db,
        input(company_id, agent_id, issue_id, run_id, first_retry),
    )
    .await
    .unwrap();
    let second_retry = Utc::now() + Duration::hours(4);
    let result = schedule_provider_quota_recovery_monitor(
        &db,
        input(company_id, agent_id, issue_id, run_id, second_retry),
    )
    .await
    .unwrap();
    assert!(result.is_none());
    let next: chrono::DateTime<Utc> =
        sqlx::query_scalar("SELECT monitor_next_check_at FROM issues WHERE id = $1")
            .bind(issue_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!((next - first_retry).num_seconds().abs() < 2);
    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn sweep_monitors_provider_quota_before_recovery_escalation() {
    let db = connect().await;
    let (company_id, agent_id, issue_id, run_id) = fixture(&db, "in_progress").await;
    sqlx::query(
        "UPDATE heartbeat_runs SET error = 'You have hit your usage limit; try again later', \
         error_code = 'adapter_failed' WHERE id = $1",
    )
    .bind(run_id)
    .execute(db.pool())
    .await
    .unwrap();

    let result = reconcile_and_escalate_stranded_for_company(
        &db,
        company_id,
        None,
        wake_template(company_id, agent_id),
        100,
    )
    .await
    .unwrap();

    assert_eq!(result.provider_quota_monitored, 1);
    assert_eq!(result.dispatched, 0);
    let status: String = sqlx::query_scalar("SELECT status FROM issues WHERE id = $1")
        .bind(issue_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(status, "in_progress");
    let actions: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issue_recovery_actions WHERE source_issue_id = $1",
    )
    .bind(issue_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(actions.0, 0);
    let (run_error_code, result_json): (Option<String>, Option<serde_json::Value>) =
        sqlx::query_as("SELECT error_code, result_json FROM heartbeat_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(run_error_code.as_deref(), Some("provider_quota"));
    assert_eq!(result_json.unwrap()["errorFamily"], "provider_quota");
    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn review_sweep_uses_active_participant_run_for_provider_quota_monitor() {
    let db = connect().await;
    let (company_id, original_assignee_id, issue_id, _old_run_id) = fixture(&db, "in_review").await;
    let participant_id = Uuid::new_v4();
    let participant_run_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'r319-reviewer', 'general', 'process', 'active')",
    )
    .bind(participant_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query("UPDATE issues SET execution_state = $1 WHERE id = $2")
        .bind(json!({
            "status":"pending",
            "currentParticipant":{"type":"agent","agentId":participant_id}
        }))
        .bind(issue_id)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, error, error_code, \
         context_snapshot, started_at, created_at) \
         VALUES ($1, $2, $3, 'failed', 'provider usage limit reached', 'adapter_failed', $4, now(), now())",
    )
    .bind(participant_run_id)
    .bind(company_id)
    .bind(participant_id)
    .bind(json!({"issueId":issue_id}))
    .execute(db.pool())
    .await
    .unwrap();

    let result = reconcile_and_escalate_stranded_for_company(
        &db,
        company_id,
        None,
        wake_template(company_id, original_assignee_id),
        100,
    )
    .await
    .unwrap();

    assert_eq!(result.provider_quota_monitored, 1);
    assert_eq!(result.dispatched, 0);
    let (status, notes, external_ref): (String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT status, monitor_notes, execution_policy->'monitor'->>'externalRef' \
         FROM issues WHERE id = $1",
    )
    .bind(issue_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(status, "in_review");
    assert!(notes.unwrap().contains("active review participant"));
    assert_eq!(
        external_ref.as_deref(),
        Some(participant_run_id.to_string().as_str())
    );
    cleanup(&db, company_id).await;
}
