//! Round 351：真实 sweep 路由 execution-review participant 特化评论。

use pc_heartbeat::recovery::reconcile_and_escalate_stranded_for_company;
use pc_repos::agent::{
    HeartbeatInvocationSource, NewAgentWakeupRequest, WakeupActorType, WakeupRequestStatus,
    WakeupTriggerDetail,
};
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.unwrap()
}

fn wake_template(company_id: Uuid, agent_id: Uuid) -> NewAgentWakeupRequest {
    NewAgentWakeupRequest {
        company_id,
        agent_id,
        source: HeartbeatInvocationSource::OnDemand,
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

async fn cleanup(db: &Db, company_id: Uuid) {
    let statements = [
        "DELETE FROM agent_wakeup_requests WHERE company_id = $1",
        "DELETE FROM issue_comments WHERE company_id = $1",
        "DELETE FROM issue_recovery_actions WHERE company_id = $1",
        "DELETE FROM heartbeat_runs WHERE company_id = $1",
        "DELETE FROM issues WHERE company_id = $1",
        "DELETE FROM agents WHERE company_id = $1",
        "DELETE FROM companies WHERE id = $1",
    ];
    for statement in statements {
        sqlx::query(statement)
            .bind(company_id)
            .execute(db.pool())
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn failed_review_retry_uses_recovery_specific_comment() {
    let db = connect().await;
    let company_id = Uuid::new_v4();
    let participant_id = Uuid::new_v4();
    let issue_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind(format!("r351-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'reviewer', 'reviewer', 'process', 'active')",
    )
    .bind(participant_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO issues \
         (id, company_id, title, status, priority, origin_kind, origin_fingerprint, assignee_agent_id, execution_state) \
         VALUES ($1, $2, 'review target', 'in_review', 'normal', 'system', $3, $4, $5)",
    )
    .bind(issue_id)
    .bind(company_id)
    .bind(format!("r351-{issue_id}"))
    .bind(participant_id)
    .bind(json!({
        "status": "pending",
        "currentStageId": "review-stage",
        "currentStageType": "execution_review",
        "currentParticipant": {"type": "agent", "agentId": participant_id}
    }))
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO heartbeat_runs \
         (id, company_id, agent_id, invocation_source, status, error, context_snapshot, started_at, created_at) \
         VALUES (gen_random_uuid(), $1, $2, 'manual', 'failed', 'review failed', $3, now(), now())",
    )
    .bind(company_id)
    .bind(participant_id)
    .bind(json!({
        "issueId": issue_id.to_string(),
        "retryReason": "execution_review_participant_recovery"
    }))
    .execute(db.pool())
    .await
    .unwrap();

    let result = reconcile_and_escalate_stranded_for_company(
        &db,
        company_id,
        None,
        wake_template(company_id, participant_id),
        10,
    )
    .await
    .unwrap();
    assert_eq!(result.dispatched, 1);

    let body: String = sqlx::query_scalar(
        "SELECT body FROM issue_comments WHERE issue_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(issue_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(body.starts_with("Paperclip retried the pending execution-review participant once"));
    assert!(body.contains("Latest retry failure details were withheld"));
    assert!(body.contains("Recovery action:"));

    cleanup(&db, company_id).await;
}
async fn insert_agent_with_status(
    db: &Db,
    company_id: Uuid,
    agent_id: Uuid,
    status: &str,
) {
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'reviewer', 'reviewer', 'process', $3)",
    )
    .bind(agent_id)
    .bind(company_id)
    .bind(status)
    .execute(db.pool())
    .await
    .unwrap();
}
async fn inactive_review_participant_writes_unavailable_comment() {
    let db = connect().await;
    let company_id = Uuid::new_v4();
    let participant_id = Uuid::new_v4();
    let issue_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind(format!("r351b-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    insert_agent_with_status(&db, company_id, participant_id, "offline").await;
    sqlx::query(
        "INSERT INTO issues \
         (id, company_id, title, status, priority, origin_kind, origin_fingerprint, assignee_agent_id, execution_state) \
         VALUES ($1, $2, 'review target', 'in_review', 'normal', 'system', $3, $4, $5)",
    )
    .bind(issue_id)
    .bind(company_id)
    .bind(format!("r351b-{issue_id}"))
    .bind(participant_id)
    .bind(json!({
        "status": "pending",
            "currentStageId": "review-stage",
            "currentStageType": "execution_review",
            "currentParticipant": {"type": "agent", "agentId": participant_id}
        }))
        .execute(db.pool())
        .await
        .unwrap();

    let result = reconcile_and_escalate_stranded_for_company(
        &db,
        company_id,
        None,
        wake_template(company_id, participant_id),
        10,
    )
    .await
    .unwrap();
    assert_eq!(result.dispatched, 1);

    let body: String = sqlx::query_scalar(
        "SELECT body FROM issue_comments WHERE issue_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(issue_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(body.starts_with("Paperclip cannot continue the pending execution-review participant"));
    assert!(body.contains("participant is not invokable"));

    cleanup(&db, company_id).await;
}
