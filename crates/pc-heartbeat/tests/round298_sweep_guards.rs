//! Sweep + Guards 联合端到端集成测试。
//! 验证 `reconcile_and_escalate_stranded_for_company` 在 pause-hold 下正确跳过。
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
        .bind(format!("r298-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO agents (id,company_id,name,role,adapter_type,status) VALUES ($1,$2,'r298-agent','general','process','active')")
        .bind(agent_id)
        .bind(company_id)
        .execute(db.pool())
        .await
        .unwrap();
    (company_id, agent_id)
}

async fn insert_issue_with_parent(
    db: &Db,
    company_id: Uuid,
    agent_id: Uuid,
    parent_id: Option<Uuid>,
    status: &str,
) -> Uuid {
    let issue_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id,company_id,parent_id,title,status,priority,origin_kind,origin_fingerprint,assignee_agent_id) VALUES ($1,$2,$3,'r298-issue',$4,'normal','system',$5,$6)",
    )
    .bind(issue_id)
    .bind(company_id)
    .bind(parent_id)
    .bind(status)
    .bind(format!("r298-fp-{issue_id}"))
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
) -> Uuid {
    let run_id = Uuid::new_v4();
    let context_snapshot = json!({ "issueId": issue_id.to_string() });
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, error_code, error, context_snapshot, started_at, created_at) VALUES ($1, $2, $3, 'failed', $4, 'r298-fixture', $5, now(), now())",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .bind(error_code)
    .bind(context_snapshot)
    .execute(db.pool())
    .await
    .unwrap();
    run_id
}

async fn insert_pause_hold(db: &Db, company_id: Uuid, root_issue_id: Uuid) -> Uuid {
    let hold_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issue_tree_holds (id, company_id, root_issue_id, mode, status, reason, release_policy) VALUES ($1, $2, $3, 'pause', 'active', 'r298-fixture', $4)",
    )
    .bind(hold_id)
    .bind(company_id)
    .bind(root_issue_id)
    .bind(json!({}))
    .execute(db.pool())
    .await
    .unwrap();
    hold_id
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
    let _ = sqlx::query("DELETE FROM issue_tree_holds WHERE company_id=$1")
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
async fn sweep_skips_issues_with_active_pause_hold_on_self() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let paused_issue =
        insert_issue_with_parent(&db, company_id, agent_id, None, "in_progress").await;
    let _ = insert_run(&db, company_id, paused_issue, agent_id, "process_lost").await;
    let _hold_id = insert_pause_hold(&db, company_id, paused_issue).await;

    let result = reconcile_and_escalate_stranded_for_company(
        &db,
        company_id,
        None,
        wake_template(company_id, agent_id),
        100,
    )
    .await
    .unwrap();

    assert_eq!(result.skipped, 1, "pause-hold must suppress escalation");
    assert_eq!(result.dispatched, 0);
    assert_eq!(result.failed, 0);

    // Confirm NO recovery_action written
    let count: (i64,) =
        sqlx::query_as("SELECT COUNT(*)::bigint FROM issue_recovery_actions WHERE company_id=$1")
            .bind(company_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(count.0, 0);

    // Confirm issue status still in_progress (not blocked)
    let row: (String,) = sqlx::query_as("SELECT status FROM issues WHERE id=$1")
        .bind(paused_issue)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(row.0, "in_progress");

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn sweep_skips_grandchild_when_ancestor_has_pause_hold() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let parent = insert_issue_with_parent(&db, company_id, agent_id, None, "todo").await;
    let child =
        insert_issue_with_parent(&db, company_id, agent_id, Some(parent), "in_progress").await;
    let grandchild =
        insert_issue_with_parent(&db, company_id, agent_id, Some(child), "in_progress").await;
    let _ = insert_run(&db, company_id, grandchild, agent_id, "process_lost").await;
    let _hold_id = insert_pause_hold(&db, company_id, parent).await;

    let result = reconcile_and_escalate_stranded_for_company(
        &db,
        company_id,
        None,
        wake_template(company_id, agent_id),
        100,
    )
    .await
    .unwrap();

    // Both grandchild and (if selected) child must be skipped due to ancestor pause-hold
    assert!(result.dispatched == 0);
    assert!(
        result.skipped >= 1,
        "at least the grandchild must be skipped"
    );

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn sweep_processes_unpaused_siblings_normally() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_with_company(&db).await;
    // Paused issue
    let paused = insert_issue_with_parent(&db, company_id, agent_id, None, "in_progress").await;
    let _ = insert_pause_hold(&db, company_id, paused).await;
    // Normal stranded issue (separate, no parent)
    let active = insert_issue_with_parent(&db, company_id, agent_id, None, "in_progress").await;
    let _ = insert_run(&db, company_id, active, agent_id, "process_lost").await;

    let result = reconcile_and_escalate_stranded_for_company(
        &db,
        company_id,
        None,
        wake_template(company_id, agent_id),
        100,
    )
    .await
    .unwrap();

    assert_eq!(result.dispatched, 1, "active sibling must be processed");
    assert_eq!(result.skipped, 1, "paused sibling must be skipped");
    assert_eq!(result.failed, 0);

    // Confirm active is now blocked
    let row: (String,) = sqlx::query_as("SELECT status FROM issues WHERE id=$1")
        .bind(active)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(row.0, "blocked");

    // Confirm paused is still in_progress
    let row: (String,) = sqlx::query_as("SELECT status FROM issues WHERE id=$1")
        .bind(paused)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(row.0, "in_progress");

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn sweep_processes_when_pause_hold_is_released() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let issue_id = insert_issue_with_parent(&db, company_id, agent_id, None, "in_progress").await;
    let _ = insert_run(&db, company_id, issue_id, agent_id, "process_lost").await;
    let hold_id = insert_pause_hold(&db, company_id, issue_id).await;
    // Release the hold
    sqlx::query("UPDATE issue_tree_holds SET status='released', released_at=now() WHERE id=$1")
        .bind(hold_id)
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

    assert_eq!(result.dispatched, 1, "released hold must allow escalation");
    assert_eq!(result.skipped, 0);
    assert_eq!(result.failed, 0);

    // Verify outcome is SourceEscalated
    let outcome = result.outcomes.first().expect("at least one outcome");
    assert_eq!(outcome.escalate_outcome, EscalateOutcome::SourceEscalated);

    cleanup(&db, company_id).await;
}
