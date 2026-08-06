//! scanSilentActiveRuns 真实 PostgreSQL 集成测试。
//! 验证 candidate 扫描、snooze 检查、create_or_update_stale_run_evaluation 行为。
use chrono::Utc;
use pc_heartbeat::recovery::{
    find_closed_stale_run_evaluation, find_open_stale_run_evaluation,
    has_dismissed_false_positive_decision, scan_silent_active_runs, ScanSilentRunsOptions,
};
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn fixture(db: &Db) -> (Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id,name,issue_prefix) VALUES ($1,$2,$3)")
        .bind(company_id)
        .bind(format!("r304-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO agents (id,company_id,name,role,adapter_type,status) VALUES ($1,$2,'r304-agent','general','process','active')")
        .bind(agent_id)
        .bind(company_id)
        .execute(db.pool())
        .await
        .unwrap();
    (company_id, agent_id)
}

/// 插入一个 silent 的 heartbeat_run（last_output_at = 2 hours ago）
async fn insert_silent_run(db: &Db, company_id: Uuid, agent_id: Uuid, issue_id: Uuid) -> Uuid {
    let run_id = Uuid::new_v4();
    let last_output = Utc::now() - chrono::Duration::hours(2);
    let context_snapshot = json!({ "issueId": issue_id.to_string() });
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, last_output_at, started_at, created_at, context_snapshot) \
         VALUES ($1, $2, $3, 'running'::text, $4, $5, now(), $6)",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .bind(last_output)
    .bind(Utc::now() - chrono::Duration::hours(3))
    .bind(context_snapshot)
    .execute(db.pool())
    .await
    .unwrap();
    run_id
}

async fn insert_fresh_run(db: &Db, company_id: Uuid, agent_id: Uuid, issue_id: Uuid) -> Uuid {
    let run_id = Uuid::new_v4();
    let context_snapshot = json!({ "issueId": issue_id.to_string() });
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, last_output_at, started_at, created_at, context_snapshot) \
         VALUES ($1, $2, $3, 'running'::text, now(), now(), now(), $4)",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .bind(context_snapshot)
    .execute(db.pool())
    .await
    .unwrap();
    run_id
}

async fn insert_terminated_run(db: &Db, company_id: Uuid, agent_id: Uuid, issue_id: Uuid) -> Uuid {
    let run_id = Uuid::new_v4();
    let context_snapshot = json!({ "issueId": issue_id.to_string() });
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, last_output_at, started_at, created_at, context_snapshot) \
         VALUES ($1, $2, $3, 'failed'::text, now(), now(), now(), $4)",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .bind(context_snapshot)
    .execute(db.pool())
    .await
    .unwrap();
    run_id
}

async fn insert_dismissed_decision(db: &Db, company_id: Uuid, run_id: Uuid) {
    sqlx::query(
        "INSERT INTO heartbeat_run_watchdog_decisions \
         (company_id, run_id, decision, snoozed_until, reason) \
         VALUES ($1, $2, 'dismissed_false_positive', NULL, 'r304-test')",
    )
    .bind(company_id)
    .bind(run_id)
    .execute(db.pool())
    .await
    .unwrap();
}

async fn insert_evaluation(
    db: &Db,
    company_id: Uuid,
    agent_id: Uuid,
    run_id: Uuid,
    status: &str,
    priority: &str,
) -> Uuid {
    let issue_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, origin_id, origin_run_id, origin_fingerprint, assignee_agent_id) \
         VALUES ($1, $2, 'r304-eval', $3::text, $4::text, 'stale_active_run_evaluation', $5, $5, $6, $7)",
    )
    .bind(issue_id)
    .bind(company_id)
    .bind(status)
    .bind(priority)
    .bind(run_id.to_string())
    .bind(format!("r304-fp-{issue_id}"))
    .bind(agent_id)
    .execute(db.pool())
    .await
    .unwrap();
    issue_id
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM activity_log WHERE company_id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM heartbeat_run_watchdog_decisions WHERE company_id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM heartbeat_runs WHERE company_id=$1")
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

#[tokio::test(flavor = "current_thread")]
async fn scan_creates_evaluation_for_silent_run() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = Uuid::new_v4();
    let _ = insert_silent_run(&db, company_id, agent_id, issue_id).await;

    let result = scan_silent_active_runs(
        &db,
        ScanSilentRunsOptions {
            now: Some(Utc::now()),
            company_id: Some(company_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(result.scanned, 1);
    assert_eq!(result.created, 1);
    assert_eq!(result.snoozed, 0);
    assert_eq!(result.skipped, 0);
    assert_eq!(result.evaluation_issue_ids.len(), 1);

    // Verify evaluation issue exists with origin_kind = 'stale_active_run_evaluation'
    let row: (String, String) = sqlx::query_as(
        "SELECT origin_kind, priority FROM issues WHERE company_id=$1 AND origin_kind='stale_active_run_evaluation'",
    )
    .bind(company_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(row.0, "stale_active_run_evaluation");
    // silence >= 60min (suspicion threshold) but < 4h (critical threshold) → medium
    assert_eq!(row.1, "medium");

    // Verify activity log
    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM activity_log WHERE company_id=$1 AND action='heartbeat.output_stale_detected'",
    )
    .bind(company_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count.0, 1);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn scan_skips_dismissed_false_positive_run() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = Uuid::new_v4();
    let run_id = insert_silent_run(&db, company_id, agent_id, issue_id).await;
    insert_dismissed_decision(&db, company_id, run_id).await;

    let result = scan_silent_active_runs(
        &db,
        ScanSilentRunsOptions {
            now: Some(Utc::now()),
            company_id: Some(company_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(result.scanned, 1);
    assert_eq!(result.created, 0);
    assert_eq!(result.skipped, 1);
    assert_eq!(result.evaluation_issue_ids.len(), 0);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn scan_returns_existing_evaluation_without_recreate() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = Uuid::new_v4();
    let run_id = insert_silent_run(&db, company_id, agent_id, issue_id).await;
    let existing_id = insert_evaluation(&db, company_id, agent_id, run_id, "todo", "medium").await;

    let result = scan_silent_active_runs(
        &db,
        ScanSilentRunsOptions {
            now: Some(Utc::now()),
            company_id: Some(company_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(result.scanned, 1);
    assert_eq!(result.existing, 1);
    assert_eq!(result.created, 0);
    assert_eq!(result.evaluation_issue_ids, vec![existing_id]);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn scan_escalates_existing_to_critical_priority() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = Uuid::new_v4();
    // Insert silent run with last_output_at = 5 hours ago (above critical threshold)
    let run_id = Uuid::new_v4();
    let last_output = Utc::now() - chrono::Duration::hours(5);
    let context_snapshot = json!({ "issueId": issue_id.to_string() });
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, last_output_at, started_at, created_at, context_snapshot) \
         VALUES ($1, $2, $3, 'running'::text, $4, $5, now(), $6)",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .bind(last_output)
    .bind(Utc::now() - chrono::Duration::hours(6))
    .bind(context_snapshot)
    .execute(db.pool())
    .await
    .unwrap();
    // Insert existing evaluation with medium priority
    insert_evaluation(&db, company_id, agent_id, run_id, "todo", "medium").await;

    let result = scan_silent_active_runs(
        &db,
        ScanSilentRunsOptions {
            now: Some(Utc::now()),
            company_id: Some(company_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(result.scanned, 1);
    assert_eq!(result.escalated, 1);

    // Verify priority updated to high
    let row: (String,) = sqlx::query_as(
        "SELECT priority FROM issues WHERE company_id=$1 AND origin_kind='stale_active_run_evaluation'",
    )
    .bind(company_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(row.0, "high");

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn scan_ignores_fresh_runs() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = Uuid::new_v4();
    let _ = insert_fresh_run(&db, company_id, agent_id, issue_id).await;

    let result = scan_silent_active_runs(
        &db,
        ScanSilentRunsOptions {
            now: Some(Utc::now()),
            company_id: Some(company_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(result.scanned, 0);
    assert_eq!(result.created, 0);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn scan_ignores_terminated_runs() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = Uuid::new_v4();
    let _ = insert_terminated_run(&db, company_id, agent_id, issue_id).await;

    let result = scan_silent_active_runs(
        &db,
        ScanSilentRunsOptions {
            now: Some(Utc::now()),
            company_id: Some(company_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(result.scanned, 0);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn find_helpers_return_correct_states() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = Uuid::new_v4();
    let run_id = insert_silent_run(&db, company_id, agent_id, issue_id).await;

    // Initially: no evaluation, no decision
    let open = find_open_stale_run_evaluation(&db, company_id, run_id)
        .await
        .unwrap();
    assert!(open.is_none());
    let closed = find_closed_stale_run_evaluation(&db, company_id, run_id)
        .await
        .unwrap();
    assert!(closed.is_none());
    let dismissed = has_dismissed_false_positive_decision(&db, company_id, run_id)
        .await
        .unwrap();
    assert!(!dismissed);

    // Insert open evaluation
    let open_id = insert_evaluation(&db, company_id, agent_id, run_id, "todo", "medium").await;
    let open = find_open_stale_run_evaluation(&db, company_id, run_id)
        .await
        .unwrap();
    assert_eq!(open.unwrap().id, open_id);

    // Dismiss
    insert_dismissed_decision(&db, company_id, run_id).await;
    let dismissed = has_dismissed_false_positive_decision(&db, company_id, run_id)
        .await
        .unwrap();
    assert!(dismissed);

    // Close evaluation
    sqlx::query("UPDATE issues SET status='done' WHERE id=$1")
        .bind(open_id)
        .execute(db.pool())
        .await
        .unwrap();
    let closed = find_closed_stale_run_evaluation(&db, company_id, run_id)
        .await
        .unwrap();
    assert_eq!(closed.unwrap().id, open_id);

    cleanup(&db, company_id).await;
}
