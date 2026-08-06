//! Continuation retry summary 的真实 PostgreSQL 集成测试。
//! 验证 `load_continuation_retry_summary` 在 DB 上读取最近 runs 并正确计算 consecutive count。
use chrono::{DateTime, TimeZone, Utc};
use pc_core::Timestamp;
use pc_heartbeat::recovery::{
    load_continuation_retry_summary, should_escalate_due_to_retry_limit,
    should_skip_due_to_backoff, CONTINUATION_RECOVERY_TRANSIENT_BASE_BACKOFF_MS,
    CONTINUATION_RECOVERY_TRANSIENT_MAX_ATTEMPTS, ISSUE_CONTINUATION_NEEDED_RETRY_REASON,
};
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn fixture_with_company(db: &Db) -> (Uuid, Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let issue_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id,name,issue_prefix) VALUES ($1,$2,$3)")
        .bind(company_id)
        .bind(format!("r296-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO agents (id,company_id,name,role,adapter_type,status) VALUES ($1,$2,'r296-agent','general','process','active')")
        .bind(agent_id)
        .bind(company_id)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO issues (id,company_id,title,status,priority,origin_kind,origin_fingerprint) VALUES ($1,$2,'r296-issue','in_progress','normal','system',$3)")
        .bind(issue_id)
        .bind(company_id)
        .bind(format!("r296-fp-{issue_id}"))
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
    status: &str,
    error_code: Option<&str>,
    retry_reason: Option<&str>,
    finished_at: DateTime<Utc>,
) -> Uuid {
    let run_id = Uuid::new_v4();
    let context_snapshot = json!({
        "issueId": issue_id.to_string(),
        "retryReason": retry_reason.unwrap_or(""),
    });
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, error_code, error, context_snapshot, finished_at, started_at, created_at) VALUES ($1, $2, $3, $4::text, $5, 'r296-fixture', $6, $7, $7, $7)",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .bind(status)
    .bind(error_code)
    .bind(context_snapshot)
    .bind(finished_at)
    .execute(db.pool())
    .await
    .unwrap();
    run_id
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM heartbeat_runs WHERE company_id=$1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issue_recovery_actions WHERE company_id=$1")
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

#[tokio::test(flavor = "current_thread")]
async fn summary_counts_three_consecutive_matching_runs() {
    let db = connect().await;
    let (company_id, agent_id, issue_id) = fixture_with_company(&db).await;
    let t = |epoch: i64| Utc.timestamp_opt(epoch, 0).unwrap();
    // Insert in arbitrary order; SQL orders by created_at DESC
    let _ = insert_run(
        &db,
        company_id,
        issue_id,
        agent_id,
        "failed",
        Some("process_lost"),
        Some(ISSUE_CONTINUATION_NEEDED_RETRY_REASON),
        t(100),
    )
    .await;
    let _ = insert_run(
        &db,
        company_id,
        issue_id,
        agent_id,
        "failed",
        Some("process_lost"),
        Some(ISSUE_CONTINUATION_NEEDED_RETRY_REASON),
        t(80),
    )
    .await;
    let _ = insert_run(
        &db,
        company_id,
        issue_id,
        agent_id,
        "interrupted",
        Some("process_lost"),
        Some(ISSUE_CONTINUATION_NEEDED_RETRY_REASON),
        t(60),
    )
    .await;
    // This one breaks the chain (non-terminal status)
    let _ = insert_run(
        &db,
        company_id,
        issue_id,
        agent_id,
        "running",
        Some("process_lost"),
        Some(ISSUE_CONTINUATION_NEEDED_RETRY_REASON),
        t(40),
    )
    .await;
    // Another matching row after the running one, must not be counted
    let _ = insert_run(
        &db,
        company_id,
        issue_id,
        agent_id,
        "failed",
        Some("process_lost"),
        Some(ISSUE_CONTINUATION_NEEDED_RETRY_REASON),
        t(20),
    )
    .await;

    let summary = load_continuation_retry_summary(
        &db,
        company_id,
        issue_id,
        agent_id,
        Some("process_lost"),
        None,
        10,
    )
    .await
    .unwrap();

    assert_eq!(
        summary.consecutive, 3,
        "first 3 rows must match consecutively"
    );
    assert_eq!(summary.latest_finished_at, Some(Timestamp::from_dt(t(100))));

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn summary_returns_zero_when_no_matching_runs() {
    let db = connect().await;
    let (company_id, agent_id, issue_id) = fixture_with_company(&db).await;
    let t = |epoch: i64| Utc.timestamp_opt(epoch, 0).unwrap();
    // Run with wrong retry_reason
    let _ = insert_run(
        &db,
        company_id,
        issue_id,
        agent_id,
        "failed",
        Some("process_lost"),
        Some("max_turns_continuation"),
        t(100),
    )
    .await;
    let _ = insert_run(
        &db,
        company_id,
        issue_id,
        agent_id,
        "failed",
        Some("process_lost"),
        Some("issue_continuation_needed"),
        t(80),
    )
    .await;
    // The chain starts at the wrong-reason row, but it's not first in DESC... actually let me re-check.

    let summary = load_continuation_retry_summary(
        &db,
        company_id,
        issue_id,
        agent_id,
        Some("process_lost"),
        None,
        10,
    )
    .await
    .unwrap();

    // DESC: row1 (max_turns), row2 (issue_continuation_needed).
    // First row's retry_reason doesn't match → break with consecutive=0.
    assert_eq!(
        summary.consecutive, 0,
        "wrong retry_reason on newest row breaks chain"
    );
    assert!(!summary.matched_retry_reason);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn should_escalate_triggers_at_max_attempts() {
    let db = connect().await;
    let (company_id, agent_id, issue_id) = fixture_with_company(&db).await;
    let t = |epoch: i64| Utc.timestamp_opt(epoch, 0).unwrap();
    for offset in 0..3 {
        let _ = insert_run(
            &db,
            company_id,
            issue_id,
            agent_id,
            "failed",
            Some("process_lost"),
            Some(ISSUE_CONTINUATION_NEEDED_RETRY_REASON),
            t(100 + offset * 100),
        )
        .await;
    }

    let summary = load_continuation_retry_summary(
        &db,
        company_id,
        issue_id,
        agent_id,
        Some("process_lost"),
        None,
        10,
    )
    .await
    .unwrap();

    assert_eq!(summary.consecutive, 3);
    assert!(should_escalate_due_to_retry_limit(
        &summary,
        CONTINUATION_RECOVERY_TRANSIENT_MAX_ATTEMPTS
    ));
    assert!(should_escalate_due_to_retry_limit(&summary, 2));
    assert!(!should_escalate_due_to_retry_limit(&summary, 4));

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn should_skip_backoff_returns_true_when_recent_finish() {
    let db = connect().await;
    let (company_id, agent_id, issue_id) = fixture_with_company(&db).await;
    let now = Utc::now();
    let recent = now - chrono::Duration::seconds(5);
    let _ = insert_run(
        &db,
        company_id,
        issue_id,
        agent_id,
        "failed",
        Some("process_lost"),
        Some(ISSUE_CONTINUATION_NEEDED_RETRY_REASON),
        recent,
    )
    .await;

    let summary = load_continuation_retry_summary(
        &db,
        company_id,
        issue_id,
        agent_id,
        Some("process_lost"),
        None,
        10,
    )
    .await
    .unwrap();

    assert_eq!(summary.consecutive, 1);
    assert!(should_skip_due_to_backoff(
        &summary,
        CONTINUATION_RECOVERY_TRANSIENT_BASE_BACKOFF_MS,
        now
    ));
    // Older base (0) never skips
    assert!(!should_skip_due_to_backoff(&summary, 0, now));

    cleanup(&db, company_id).await;
}
