//! Scheduler DB 接入层的真实 PostgreSQL 集成测试。
//! 验证 `ensure_source_scoped_recovery_action_for_issue` 在 DB 上的端到端行为。
use pc_heartbeat::recovery::{ensure_source_scoped_recovery_action_for_issue, SchedulerDbInput};
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

async fn fixture_with_agent(db: &Db) -> (Uuid, Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let issue_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id,name,issue_prefix) VALUES ($1,$2,$3)")
        .bind(company_id)
        .bind(format!("r291-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO agents (id,company_id,name,role,adapter_type,status) VALUES ($1,$2,'r291-agent','general','process','active')")
        .bind(agent_id)
        .bind(company_id)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO issues (id,company_id,title,status,priority,origin_kind,origin_fingerprint,assignee_agent_id) VALUES ($1,$2,'scheduler fixture','in_progress','normal','system',$3,$4)")
        .bind(issue_id)
        .bind(company_id)
        .bind(format!("r291-fp-{issue_id}"))
        .bind(agent_id)
        .execute(db.pool())
        .await
        .unwrap();
    (company_id, agent_id, issue_id)
}

async fn insert_run(
    db: &Db,
    company_id: Uuid,
    issue_id: Uuid,
    agent_id: Uuid,
    error_code: &str,
    error: &str,
    result_json: Option<serde_json::Value>,
) -> Uuid {
    let run_id = Uuid::new_v4();
    let context_snapshot =
        json!({ "issueId": issue_id.to_string(), "retryReason": "issue_continuation_needed" });
    sqlx::query(
        "INSERT INTO heartbeat_runs \
         (id, company_id, agent_id, status, error_code, error, context_snapshot, result_json, liveness_state, started_at, created_at) \
         VALUES ($1, $2, $3, 'failed', $4, $5, $6, $7, 'active', now(), now())",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .bind(error_code)
    .bind(error)
    .bind(context_snapshot)
    .bind(result_json)
    .execute(db.pool())
    .await
    .unwrap();
    run_id
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM issue_recovery_actions WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM heartbeat_runs WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM agent_wakeup_requests WHERE company_id = $1")
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
async fn scheduler_persists_recovery_action_for_process_lost_run() {
    let db = connect().await;
    let (company_id, agent_id, issue_id) = fixture_with_agent(&db).await;
    let _ = insert_run(
        &db,
        company_id,
        issue_id,
        agent_id,
        "process_lost",
        "process exited",
        None,
    )
    .await;

    let result = ensure_source_scoped_recovery_action_for_issue(
        &db,
        SchedulerDbInput {
            issue_id,
            previous_status: Some("in_progress".into()),
            recovery_cause_override: None,
            recovery_owner_agent_id: None,
            successful_run_handoff_evidence: None,
            workspace_validation_fingerprint_override: None,
        },
        None,
        wake_template(company_id, agent_id),
    )
    .await
    .unwrap();

    let result = result.expect("scheduler returned Some");
    assert_eq!(
        result.cause,
        pc_heartbeat::recovery::StrandedRecoveryCause::ProcessLost
    );
    let action = &result.result.persisted.action;
    assert_eq!(action.source_issue_id, issue_id);
    assert_eq!(action.owner_agent_id, Some(agent_id));
    assert_eq!(action.kind, "stranded_assigned_issue");
    assert_eq!(action.wake_policy.as_ref().unwrap()["type"], "wake_owner");

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn scheduler_uses_monitor_only_for_provider_quota_without_owner() {
    let db = connect().await;
    let company_id = Uuid::new_v4();
    let issue_id = Uuid::new_v4();
    let original_agent_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id,name,issue_prefix) VALUES ($1,$2,$3)")
        .bind(company_id)
        .bind(format!("r291q-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    // Original assignee is PAUSED → not invokable → quota must fall back to monitor_only.
    sqlx::query("INSERT INTO agents (id,company_id,name,role,adapter_type,status) VALUES ($1,$2,'r291-paused','general','process','paused')")
        .bind(original_agent_id)
        .bind(company_id)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO issues (id,company_id,title,status,priority,origin_kind,origin_fingerprint,assignee_agent_id) VALUES ($1,$2,'scheduler quota fixture','in_progress','normal','system',$3,$4)")
        .bind(issue_id)
        .bind(company_id)
        .bind(format!("r291q-fp-{issue_id}"))
        .bind(original_agent_id)
        .execute(db.pool())
        .await
        .unwrap();
    let _ = insert_run(
        &db,
        company_id,
        issue_id,
        original_agent_id,
        "provider_quota",
        "usage limit reached",
        Some(json!({ "retryNotBefore": "2099-01-01T00:00:00Z" })),
    )
    .await;

    let result = ensure_source_scoped_recovery_action_for_issue(
        &db,
        SchedulerDbInput {
            issue_id,
            previous_status: Some("in_progress".into()),
            recovery_cause_override: None,
            recovery_owner_agent_id: None,
            successful_run_handoff_evidence: None,
            workspace_validation_fingerprint_override: None,
        },
        None,
        wake_template(company_id, original_agent_id),
    )
    .await
    .unwrap();

    let result = result.expect("scheduler returned Some");
    assert_eq!(
        result.cause,
        pc_heartbeat::recovery::StrandedRecoveryCause::ProviderQuota
    );
    let action = &result.result.persisted.action;
    assert_eq!(action.kind, "stranded_assigned_issue");
    assert_eq!(action.wake_policy.as_ref().unwrap()["type"], "monitor_only");
    assert!(action.monitor_policy.is_some());
    assert!(
        result.result.wake.is_none(),
        "monitor_only should not dispatch wake"
    );

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn scheduler_routes_configuration_incomplete_to_manual_repair() {
    let db = connect().await;
    let (company_id, agent_id, issue_id) = fixture_with_agent(&db).await;
    let _ = insert_run(
        &db,
        company_id,
        issue_id,
        agent_id,
        "adapter_failed",
        "missing API key",
        None,
    )
    .await;

    let result = ensure_source_scoped_recovery_action_for_issue(
        &db,
        SchedulerDbInput {
            issue_id,
            previous_status: Some("in_progress".into()),
            recovery_cause_override: None,
            recovery_owner_agent_id: None,
            successful_run_handoff_evidence: None,
            workspace_validation_fingerprint_override: None,
        },
        None,
        wake_template(company_id, agent_id),
    )
    .await
    .unwrap();

    let result = result.expect("scheduler returned Some");
    assert_eq!(
        result.cause,
        pc_heartbeat::recovery::StrandedRecoveryCause::ConfigurationIncomplete
    );
    let action = &result.result.persisted.action;
    assert_eq!(action.kind, "configuration_validation");
    assert_eq!(
        action.wake_policy.as_ref().unwrap()["type"],
        "manual_repair_required"
    );
    assert!(
        action.owner_agent_id.is_none(),
        "configuration_incomplete must not auto-wake"
    );
    assert!(result.result.wake.is_none());

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn scheduler_returns_none_when_issue_or_run_missing() {
    let db = connect().await;
    let company_id = Uuid::new_v4();
    let issue_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id,name,issue_prefix) VALUES ($1,$2,$3)")
        .bind(company_id)
        .bind(format!("r291b-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    // No issue inserted, no run inserted -> scheduler must return None
    let result = ensure_source_scoped_recovery_action_for_issue(
        &db,
        SchedulerDbInput {
            issue_id,
            previous_status: None,
            recovery_cause_override: None,
            recovery_owner_agent_id: None,
            successful_run_handoff_evidence: None,
            workspace_validation_fingerprint_override: None,
        },
        None,
        wake_template(company_id, Uuid::new_v4()),
    )
    .await
    .unwrap();
    assert!(result.is_none());

    cleanup(&db, company_id).await;
}
