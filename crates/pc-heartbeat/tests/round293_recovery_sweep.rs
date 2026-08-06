//! Recovery sweep 的真实 PostgreSQL 集成测试。
//! 验证 `reconcile_stranded_assigned_issues_for_company` 在 DB 上批量调度 source-scoped recovery。
use pc_heartbeat::recovery::reconcile_stranded_assigned_issues_for_company;
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
        .bind(format!("r293-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO agents (id,company_id,name,role,adapter_type,status) VALUES ($1,$2,'r293-agent','general','process','active')")
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
    .bind(format!("r293-fp-{issue_id}"))
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
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, error_code, error, context_snapshot, started_at, created_at) VALUES ($1, $2, $3, 'failed', $4, 'r293-fixture', $5, now(), now())",
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
async fn sweep_dispatches_recovery_for_failed_in_progress_issues() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_with_company(&db).await;

    // Three stranded candidates:
    // 1) process_lost
    let issue_a = insert_issue(&db, company_id, agent_id, "issue A", "in_progress").await;
    let _ = insert_run(&db, company_id, issue_a, agent_id, "process_lost").await;
    // 2) provider_quota
    let issue_b = insert_issue(&db, company_id, agent_id, "issue B", "in_progress").await;
    let _ = insert_run(&db, company_id, issue_b, agent_id, "provider_quota").await;
    // 3) configuration_incomplete
    let issue_c = insert_issue(&db, company_id, agent_id, "issue C", "in_progress").await;
    let _ = insert_run(&db, company_id, issue_c, agent_id, "adapter_failed").await;
    sqlx::query("UPDATE heartbeat_runs SET error = 'missing API key' WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await
        .unwrap();

    let result = reconcile_stranded_assigned_issues_for_company(
        &db,
        company_id,
        None,
        wake_template(company_id, agent_id),
        100,
    )
    .await
    .unwrap();

    assert_eq!(
        result.dispatched, 3,
        "all three stranded issues must dispatch a recovery action"
    );
    assert_eq!(result.skipped, 0);
    assert_eq!(result.failed, 0);

    // Confirm DB-level recovery actions
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*)::bigint FROM issue_recovery_actions WHERE company_id = $1 AND status = 'active'")
        .bind(company_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count.0, 3);

    // Confirm the causes match expectations
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT cause, kind FROM issue_recovery_actions WHERE company_id = $1")
            .bind(company_id)
            .fetch_all(db.pool())
            .await
            .unwrap();
    let kinds: std::collections::HashSet<_> = rows.iter().map(|(_, k)| k.clone()).collect();
    assert!(kinds.contains("stranded_assigned_issue"));
    assert!(kinds.contains("configuration_validation"));

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn sweep_skips_issues_with_active_execution_path() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let issue_id = insert_issue(
        &db,
        company_id,
        agent_id,
        "active path issue",
        "in_progress",
    )
    .await;

    // Insert an ACTIVE heartbeat run — must be skipped by sweep
    let run_id = Uuid::new_v4();
    let context_snapshot = json!({ "issueId": issue_id.to_string() });
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, error_code, error, context_snapshot, started_at, created_at) VALUES ($1, $2, $3, 'running', NULL, NULL, $4, now(), now())",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .bind(context_snapshot)
    .execute(db.pool())
    .await
    .unwrap();

    let result = reconcile_stranded_assigned_issues_for_company(
        &db,
        company_id,
        None,
        wake_template(company_id, agent_id),
        100,
    )
    .await
    .unwrap();

    assert_eq!(result.dispatched, 0);
    assert_eq!(result.skipped, 1);
    assert_eq!(result.failed, 0);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn sweep_handles_empty_company() {
    let db = connect().await;
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id,name,issue_prefix) VALUES ($1,$2,$3)")
        .bind(company_id)
        .bind(format!("r293e-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO agents (id,company_id,name,role,adapter_type,status) VALUES ($1,$2,'r293e','general','process','active')")
        .bind(agent_id)
        .bind(company_id)
        .execute(db.pool())
        .await
        .unwrap();

    let result = reconcile_stranded_assigned_issues_for_company(
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
