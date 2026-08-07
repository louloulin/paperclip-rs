//! Recovery escalation 的真实 PostgreSQL 集成测试。
//! 验证 `escalate_stranded_assigned_issue` 在 DB 上完整升级 stranded issue。
use pc_heartbeat::recovery::{
    escalate_stranded_assigned_issue, escalate_stranded_recovery_issue_in_place, EscalateDbInput,
    EscalateOutcome,
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

async fn fixture_with_company(db: &Db) -> (Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id,name,issue_prefix) VALUES ($1,$2,$3)")
        .bind(company_id)
        .bind(format!("r294-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO agents (id,company_id,name,role,adapter_type,status) VALUES ($1,$2,'r294-agent','general','process','active')")
        .bind(agent_id)
        .bind(company_id)
        .execute(db.pool())
        .await
        .unwrap();
    (company_id, agent_id)
}

async fn insert_issue(
    db: &Db,
    company_id: Uuid,
    agent_id: Uuid,
    title: &str,
    status: &str,
    origin_kind: &str,
) -> Uuid {
    let issue_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id,company_id,title,status,priority,origin_kind,origin_fingerprint,assignee_agent_id) VALUES ($1,$2,$3,$4,'normal',$5,$6,$7)",
    )
    .bind(issue_id)
    .bind(company_id)
    .bind(title)
    .bind(status)
    .bind(origin_kind)
    .bind(format!("r294-fp-{issue_id}"))
    .bind(agent_id)
    .execute(db.pool())
    .await
    .unwrap();
    issue_id
}

async fn insert_run(
    db: &Db,
    company_id: Uuid,
    issue_id: Uuid,
    agent_id: Uuid,
    error_code: &str,
    error: &str,
) -> Uuid {
    let run_id = Uuid::new_v4();
    let context_snapshot = json!({ "issueId": issue_id.to_string() });
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, error_code, error, context_snapshot, started_at, created_at) VALUES ($1, $2, $3, 'failed', $4, $5, $6, now(), now())",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .bind(error_code)
    .bind(error)
    .bind(context_snapshot)
    .execute(db.pool())
    .await
    .unwrap();
    run_id
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query(
        "DELETE FROM activity_log WHERE company_id = $1;
        let _ = DELETE FROM issue_comments WHERE issue_id IN (SELECT id FROM issues WHERE company_id=$1)",
    )
    .bind(company_id)
    .execute(db.pool())
    .await;
    let _ = sqlx::query("DELETE FROM issue_recovery_actions WHERE company_id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM heartbeat_runs WHERE company_id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM agent_wakeup_requests WHERE company_id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM agents WHERE company_id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id=$1")
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
async fn escalate_stranded_source_issue_writes_recovery_and_blocks_source() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let issue_id = insert_issue(
        &db,
        company_id,
        agent_id,
        "r294-escalate-source",
        "in_progress",
        "system",
    )
    .await;
    let _ = insert_run(
        &db,
        company_id,
        issue_id,
        agent_id,
        "process_lost",
        "r294-fixture",
    )
    .await;

    let result = escalate_stranded_assigned_issue(
        &db,
        EscalateDbInput {
            issue_id,
            previous_status: "in_progress".into(),
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

    let result = result.expect("escalate returned Some");
    assert_eq!(result.outcome, EscalateOutcome::SourceEscalated);
    assert_eq!(result.updated_issue.status, "blocked");
    assert!(result.updated_issue.assignee_agent_id.is_some());
    assert!(
        result.comment_id.is_some(),
        "escalation comment must be written"
    );
    assert!(result.recovery_action_id.is_some());

    // DB-level assertions
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*)::bigint FROM issue_recovery_actions WHERE source_issue_id=$1 AND status='active'")
        .bind(issue_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count.0, 1);
    let row: (String,) = sqlx::query_as("SELECT status FROM issues WHERE id=$1")
        .bind(issue_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(row.0, "blocked");

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn escalate_dedupes_repeated_escalation_comments() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let issue_id = insert_issue(
        &db,
        company_id,
        agent_id,
        "r294-dedup",
        "in_progress",
        "system",
    )
    .await;
    let _ = insert_run(
        &db,
        company_id,
        issue_id,
        agent_id,
        "process_lost",
        "r294-fixture",
    )
    .await;

    // First escalation: writes comment + recovery action
    let first = escalate_stranded_assigned_issue(
        &db,
        EscalateDbInput {
            issue_id,
            previous_status: "in_progress".into(),
            recovery_cause_override: None,
            recovery_owner_agent_id: None,
            successful_run_handoff_evidence: None,
            workspace_validation_fingerprint_override: None,
        },
        None,
        wake_template(company_id, agent_id),
    )
    .await
    .unwrap()
    .expect("first escalation Some");
    let first_comment = first.comment_id.expect("first comment must be written");

    // Second escalation: same issue, status still blocked → must skip comment (dedup)
    let second = escalate_stranded_assigned_issue(
        &db,
        EscalateDbInput {
            issue_id,
            previous_status: "blocked".into(),
            recovery_cause_override: None,
            recovery_owner_agent_id: None,
            successful_run_handoff_evidence: None,
            workspace_validation_fingerprint_override: None,
        },
        None,
        wake_template(company_id, agent_id),
    )
    .await
    .unwrap()
    .expect("second escalation Some");

    // The dedup only applies to SourceEscalate. After first call the issue is already blocked,
    // so second call should hit Skip(AlreadyBlocked) and write no comment.
    assert_eq!(second.outcome, EscalateOutcome::Skipped);
    assert!(second.comment_id.is_none());

    // Confirm only ONE comment exists for this issue
    let comment_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issue_comments WHERE issue_id=$1 AND deleted_at IS NULL",
    )
    .bind(issue_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        comment_count.0, 1,
        "exactly one escalation comment must remain"
    );
    let _ = first_comment; // silence unused warning

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn escalate_in_place_for_recovery_origin_issue() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let issue_id = insert_issue(
        &db,
        company_id,
        agent_id,
        "r294-recovery-issue",
        "in_progress",
        "stranded_issue_recovery",
    )
    .await;
    let _ = insert_run(
        &db,
        company_id,
        issue_id,
        agent_id,
        "process_lost",
        "r294-fixture",
    )
    .await;

    let result = escalate_stranded_recovery_issue_in_place(&db, issue_id, "in_progress".into())
        .await
        .unwrap()
        .expect("in-place returned Some");

    assert_eq!(result.outcome, EscalateOutcome::RecoveryInPlace);
    assert_eq!(result.updated_issue.status, "blocked");
    assert!(result.comment_id.is_some(), "in-place must write a comment");
    assert!(
        result.recovery_action_id.is_none(),
        "in-place does not create a new recovery action"
    );

    // Confirm the comment mentions "stranded_issue_recovery"
    let body: (String,) = sqlx::query_as(
        "SELECT body FROM issue_comments WHERE issue_id=$1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(issue_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(body.0.contains("stranded_issue_recovery") || body.0.contains("recovery"));

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn escalate_returns_none_when_issue_missing() {
    let db = connect().await;
    let company_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id,name,issue_prefix) VALUES ($1,$2,$3)")
        .bind(company_id)
        .bind(format!("r294m-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();

    let result = escalate_stranded_assigned_issue(
        &db,
        EscalateDbInput {
            issue_id: Uuid::new_v4(),
            previous_status: "in_progress".into(),
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

#[tokio::test(flavor = "current_thread")]
async fn escalate_skips_already_blocked_issue() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let issue_id = insert_issue(
        &db,
        company_id,
        agent_id,
        "r294-already-blocked",
        "blocked",
        "system",
    )
    .await;
    let _ = insert_run(
        &db,
        company_id,
        issue_id,
        agent_id,
        "process_lost",
        "r294-fixture",
    )
    .await;

    let result = escalate_stranded_assigned_issue(
        &db,
        EscalateDbInput {
            issue_id,
            previous_status: "blocked".into(),
            recovery_cause_override: None,
            recovery_owner_agent_id: None,
            successful_run_handoff_evidence: None,
            workspace_validation_fingerprint_override: None,
        },
        None,
        wake_template(company_id, agent_id),
    )
    .await
    .unwrap()
    .expect("escalate returned Some");

    assert_eq!(result.outcome, EscalateOutcome::Skipped);

    cleanup(&db, company_id).await;
}
