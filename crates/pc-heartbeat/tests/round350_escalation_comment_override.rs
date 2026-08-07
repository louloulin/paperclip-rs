//! Round 350：source escalation 支持 Node `input.comment` 覆盖正文。

use pc_heartbeat::recovery::build_execution_review_participant_recovery_comment::build_execution_review_participant_recovery_comment;
use pc_heartbeat::recovery::build_execution_review_participant_unavailable_comment::build_execution_review_participant_unavailable_comment;
use pc_heartbeat::recovery::build_recovery_issue_in_place_escalation_comment::EscalationRunView;
use pc_heartbeat::recovery::escalate_db::{
    escalate_stranded_assigned_issue_with_comment, EscalateDbInput, EscalateOutcome,
};
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
    for table in [
        "activity_log",
        "agent_wakeup_requests",
        "issue_comments",
        "issue_recovery_actions",
        "heartbeat_runs",
        "issues",
        "agents",
        "companies",
    ] {
        let sql = if table == "companies" {
            "DELETE FROM companies WHERE id = $1".to_owned()
        } else {
            format!("DELETE FROM {table} WHERE company_id = $1")
        };
        sqlx::query(&sql)
            .bind(company_id)
            .execute(db.pool())
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn review_recovery_comment_is_persisted_with_recovery_marker() {
    let db = connect().await;
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let issue_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind(format!("r350-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'reviewer', 'reviewer', 'process', 'active')",
    )
    .bind(agent_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, origin_kind, assignee_agent_id) \
         VALUES ($1, $2, 'review target', 'in_review', 'system', $3)",
    )
    .bind(issue_id)
    .bind(company_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO heartbeat_runs \
         (id, company_id, agent_id, invocation_source, status, error, context_snapshot, started_at) \
         VALUES (gen_random_uuid(), $1, $2, 'manual', 'failed', 'review failed', $3, now())",
    )
    .bind(company_id)
    .bind(agent_id)
    .bind(json!({
        "issueId": issue_id.to_string(),
        "retryReason": "execution_review_participant_recovery"
    }))
    .execute(db.pool())
    .await
    .unwrap();

    let comment = build_execution_review_participant_recovery_comment(&EscalationRunView {
        id: Uuid::new_v4(),
        agent_id: Some(agent_id),
        status: "failed".to_owned(),
        error: Some("review failed".to_owned()),
        error_code: None,
        context_snapshot: Some(json!({})),
    });
    let result = escalate_stranded_assigned_issue_with_comment(
        &db,
        EscalateDbInput {
            issue_id,
            previous_status: "in_review".to_owned(),
            recovery_cause_override: Some(
                pc_heartbeat::recovery::source_scoped_recovery_action::StrandedRecoveryCause::ExecutionReviewParticipantRecovery,
            ),
            recovery_owner_agent_id: Some(agent_id),
            successful_run_handoff_evidence: None,
            workspace_validation_fingerprint_override: None,
        },
        Some(comment.clone()),
        None,
        wake_template(company_id, agent_id),
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(result.outcome, EscalateOutcome::SourceEscalated);
    let action_id = result.recovery_action_id.unwrap();
    let body: String = sqlx::query_scalar(
        "SELECT body FROM issue_comments WHERE issue_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(issue_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(body.starts_with(&comment));
    assert!(body.contains(&format!("Recovery action: `{action_id}`")));
    assert!(body.contains("Recovery owner:"));
    assert!(!body.contains("Paperclip exhausted automatic recovery for the assigned issue"));

    cleanup(&db, company_id).await;
}

#[tokio::test]
async fn unavailable_comment_is_persisted_and_repeat_is_idempotent() {
    let db = connect().await;
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let issue_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind(format!("r350b-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'reviewer', 'reviewer', 'process', 'active')",
    )
    .bind(agent_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, origin_kind, assignee_agent_id) \
         VALUES ($1, $2, 'unavailable review', 'in_review', 'system', $3)",
    )
    .bind(issue_id)
    .bind(company_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO heartbeat_runs \
         (id, company_id, agent_id, invocation_source, status, error_code, context_snapshot, started_at) \
         VALUES (gen_random_uuid(), $1, $2, 'manual', 'failed', 'adapter_unavailable', $3, now())",
    )
    .bind(company_id)
    .bind(agent_id)
    .bind(json!({
        "issueId": issue_id.to_string(),
        "retryReason": "execution_review_participant_recovery"
    }))
    .execute(db.pool())
    .await
    .unwrap();

    let latest_run = EscalationRunView {
        id: Uuid::new_v4(),
        agent_id: Some(agent_id),
        status: "failed".to_owned(),
        error: None,
        error_code: Some("adapter_unavailable".to_owned()),
        context_snapshot: Some(json!({})),
    };
    let comment = build_execution_review_participant_unavailable_comment(&latest_run);
    let input = EscalateDbInput {
        issue_id,
        previous_status: "in_review".to_owned(),
        recovery_cause_override: Some(
            pc_heartbeat::recovery::source_scoped_recovery_action::StrandedRecoveryCause::ExecutionReviewParticipantRecovery,
        ),
        recovery_owner_agent_id: Some(agent_id),
        successful_run_handoff_evidence: None,
        workspace_validation_fingerprint_override: None,
    };
    let first = escalate_stranded_assigned_issue_with_comment(
        &db,
        input.clone(),
        Some(comment.clone()),
        None,
        wake_template(company_id, agent_id),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(first.outcome, EscalateOutcome::SourceEscalated);

    let second = escalate_stranded_assigned_issue_with_comment(
        &db,
        EscalateDbInput {
            previous_status: "blocked".to_owned(),
            ..input
        },
        Some(comment.clone()),
        None,
        wake_template(company_id, agent_id),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(second.outcome, EscalateOutcome::Skipped);
    assert!(second.comment_id.is_none());

    let bodies: Vec<String> =
        sqlx::query_scalar("SELECT body FROM issue_comments WHERE issue_id = $1")
            .bind(issue_id)
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(bodies.len(), 1);
    assert!(bodies[0].starts_with(&comment));
    assert!(bodies[0].contains("Latest retry failure details were withheld"));

    cleanup(&db, company_id).await;
}
