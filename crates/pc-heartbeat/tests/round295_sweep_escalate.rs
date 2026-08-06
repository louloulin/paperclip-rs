//! Sweep + Escalate 联合的端到端集成测试。
//! 验证 `reconcile_and_escalate_stranded_for_company` 既写 recovery_action，又把 source issue 切到 blocked。
use pc_heartbeat::recovery::{reconcile_and_escalate_stranded_for_company, EscalateOutcome};
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
        .bind(format!("r295-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO agents (id,company_id,name,role,adapter_type,status) VALUES ($1,$2,'r295-agent','general','process','active')")
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
) -> Uuid {
    let issue_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id,company_id,title,status,priority,origin_kind,origin_fingerprint,assignee_agent_id) VALUES ($1,$2,$3,$4,'normal','system',$5,$6)",
    )
    .bind(issue_id)
    .bind(company_id)
    .bind(title)
    .bind(status)
    .bind(format!("r295-fp-{issue_id}"))
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
        "DELETE FROM issue_comments WHERE issue_id IN (SELECT id FROM issues WHERE company_id=$1)",
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
async fn combined_sweep_monitors_quota_and_blocks_other_failures() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_with_company(&db).await;

    let issue_a = insert_issue(&db, company_id, agent_id, "issue A", "in_progress").await;
    let _ = insert_run(
        &db,
        company_id,
        issue_a,
        agent_id,
        "process_lost",
        "r295-fixture",
    )
    .await;
    let issue_b = insert_issue(&db, company_id, agent_id, "issue B", "in_progress").await;
    let _ = insert_run(
        &db,
        company_id,
        issue_b,
        agent_id,
        "provider_quota",
        "r295-fixture",
    )
    .await;
    let issue_c = insert_issue(&db, company_id, agent_id, "issue C", "in_progress").await;
    let _ = insert_run(
        &db,
        company_id,
        issue_c,
        agent_id,
        "adapter_failed",
        "missing API key",
    )
    .await;

    let result = reconcile_and_escalate_stranded_for_company(
        &db,
        company_id,
        None,
        wake_template(company_id, agent_id),
        100,
    )
    .await
    .unwrap();

    assert_eq!(result.dispatched, 2);
    assert_eq!(result.provider_quota_monitored, 1);
    assert_eq!(result.skipped, 0);
    assert_eq!(result.failed, 0);

    // Provider quota stays in_progress under monitor; the other failures are blocked.
    let rows: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, status FROM issues WHERE company_id=$1")
            .bind(company_id)
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows.iter().find(|(id, _)| *id == issue_a).unwrap().1,
        "blocked"
    );
    assert_eq!(
        rows.iter().find(|(id, _)| *id == issue_b).unwrap().1,
        "in_progress"
    );
    assert_eq!(
        rows.iter().find(|(id, _)| *id == issue_c).unwrap().1,
        "blocked"
    );

    // Only non-quota failures create recovery actions and escalation comments.
    let actions: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issue_recovery_actions WHERE company_id=$1 AND status='active'",
    )
    .bind(company_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(actions.0, 2);

    let comments: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issue_comments WHERE issue_id IN (SELECT id FROM issues WHERE company_id=$1) AND deleted_at IS NULL",
    )
    .bind(company_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(comments.0, 2);

    let quota_next_check: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT monitor_next_check_at FROM issues WHERE id=$1")
            .bind(issue_b)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(quota_next_check.is_some());

    // Verify the escalate outcomes
    let outcomes: Vec<_> = result
        .outcomes
        .iter()
        .map(|o| o.escalate_outcome.clone())
        .collect();
    for outcome in &outcomes {
        assert_eq!(*outcome, EscalateOutcome::SourceEscalated);
    }

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn combined_sweep_idempotent_when_rerun_on_blocked_issues() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let issue_id = insert_issue(&db, company_id, agent_id, "r295-idem", "in_progress").await;
    let _ = insert_run(
        &db,
        company_id,
        issue_id,
        agent_id,
        "process_lost",
        "r295-fixture",
    )
    .await;

    // First run
    let first = reconcile_and_escalate_stranded_for_company(
        &db,
        company_id,
        None,
        wake_template(company_id, agent_id),
        100,
    )
    .await
    .unwrap();
    assert_eq!(first.dispatched, 1);

    // Second run: blocked issues are filtered out by `list_stranded_candidates`
    // which only selects status IN ('todo','in_progress','in_review').
    // So no candidate is even visited.
    let second = reconcile_and_escalate_stranded_for_company(
        &db,
        company_id,
        None,
        wake_template(company_id, agent_id),
        100,
    )
    .await
    .unwrap();
    assert_eq!(second.dispatched, 0);
    assert_eq!(second.skipped, 0);
    assert_eq!(second.failed, 0);
    assert!(second.outcomes.is_empty());

    // Confirm only ONE escalation comment exists (dedup worked)
    let comments: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issue_comments WHERE issue_id=$1 AND deleted_at IS NULL",
    )
    .bind(issue_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(comments.0, 1);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn combined_sweep_handles_empty_company() {
    let db = connect().await;
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id,name,issue_prefix) VALUES ($1,$2,$3)")
        .bind(company_id)
        .bind(format!("r295e-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO agents (id,company_id,name,role,adapter_type,status) VALUES ($1,$2,'r295e','general','process','active')")
        .bind(agent_id)
        .bind(company_id)
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

    assert_eq!(result.dispatched, 0);
    assert_eq!(result.skipped, 0);
    assert_eq!(result.failed, 0);
    assert!(result.outcomes.is_empty());

    cleanup(&db, company_id).await;
}
