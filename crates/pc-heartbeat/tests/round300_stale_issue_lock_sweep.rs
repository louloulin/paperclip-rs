//! sweepStaleIssueLocks 的真实 PostgreSQL 集成测试。
//! 验证 sweep 正确清理指向 terminal/missing heartbeat_runs 的 lock 列。
use pc_heartbeat::recovery::sweep_stale_issue_locks;
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

// Tests in this file share one PostgreSQL instance. The sweep is a global
// operation (no company filter) and uses a WHERE-guard against concurrent
// UPDATEs, so two tests creating identical-shaped fixtures in parallel can
// race: each sees the other's fixture, the first sweep clears both, and the
// second sweep's WHERE-guard blocks re-clearing. We serialize execution with a
// process-wide mutex so each test gets a deterministic, isolated view of the
// database state.
static TEST_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_tests() -> std::sync::MutexGuard<'static, ()> {
    TEST_MUTEX.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

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
        .bind(format!("r300-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO agents (id,company_id,name,role,adapter_type,status) VALUES ($1,$2,'r300-agent','general','process','active')")
        .bind(agent_id)
        .bind(company_id)
        .execute(db.pool())
        .await
        .unwrap();
    (company_id, agent_id)
}

async fn insert_run_with_status(db: &Db, company_id: Uuid, agent_id: Uuid, status: &str) -> Uuid {
    let run_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, started_at, created_at) \
         VALUES ($1, $2, $3, $4::text, now(), now())",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .bind(status)
    .execute(db.pool())
    .await
    .unwrap();
    run_id
}

async fn insert_issue_with_locks(
    db: &Db,
    company_id: Uuid,
    agent_id: Uuid,
    checkout_run_id: Option<Uuid>,
    execution_run_id: Option<Uuid>,
) -> Uuid {
    let issue_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id,company_id,title,status,priority,origin_kind,origin_fingerprint,assignee_agent_id,checkout_run_id,execution_run_id,execution_agent_name_key,execution_locked_at) \
         VALUES ($1,$2,'r300-issue','todo','normal','system',$3,$4,$5,$6,$7,now())",
    )
    .bind(issue_id)
    .bind(company_id)
    .bind(format!("r300-fp-{issue_id}"))
    .bind(agent_id)
    .bind(checkout_run_id)
    .bind(execution_run_id)
    .bind("r300-agent")
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
async fn clears_locks_when_checkout_run_is_terminal() {
    let db = connect().await;
    let _guard = lock_tests();
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let failed_run = insert_run_with_status(&db, company_id, agent_id, "failed").await;
    let issue_id = insert_issue_with_locks(&db, company_id, agent_id, Some(failed_run), None).await;

    let result = sweep_stale_issue_locks(&db).await.unwrap();
    // The sweep runs across the whole shared test database, so other tests'
    // leftover stale-lock rows may also be cleaned. We assert that OUR issue
    // is among the cleared set and that its lock columns are NULL, rather
    // than asserting exact totals that are sensitive to global DB state.
    assert!(
        result.cleared >= 1,
        "sweep must clear at least the test's own stale issue"
    );
    assert!(
        result.issue_ids.contains(&issue_id),
        "test issue {issue_id} must be in the cleared set: {:?}",
        result.issue_ids
    );
    assert!(result.candidates_considered >= 1);

    // Verify lock columns cleared
    let row: (
        Option<Uuid>,
        Option<Uuid>,
        Option<chrono::DateTime<chrono::Utc>>,
    ) = sqlx::query_as(
        "SELECT checkout_run_id, execution_run_id, execution_locked_at FROM issues WHERE id=$1",
    )
    .bind(issue_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(row.0, None);
    assert_eq!(row.1, None);
    assert_eq!(row.2, None);

    // Verify activity log written
    let audit: (String, serde_json::Value) = sqlx::query_as(
        "SELECT action, details FROM activity_log WHERE company_id=$1 AND entity_type='issue' AND entity_id=$2",
    )
    .bind(company_id)
    .bind(issue_id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(audit.0, "issue.stale_lock_cleared");
    assert_eq!(audit.1["source"], "recovery.sweep_stale_issue_locks");
    assert_eq!(audit.1["clearedCheckoutRunId"], json!(failed_run));

    cleanup(&db, company_id).await;
}

// NOTE: missing-run-row scenario is intentionally not covered by integration
// tests — the FK constraint with ON DELETE SET NULL prevents orphan references
// from existing in production. The pure-function `is_cleanable()` branch for
// missing runs is still unit-tested in `recovery::stale_issue_lock_sweep::tests`.

#[tokio::test(flavor = "current_thread")]
async fn preserves_locks_when_run_still_running() {
    let db = connect().await;
    let _guard = lock_tests();
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let running_run = insert_run_with_status(&db, company_id, agent_id, "running").await;
    let issue_id = insert_issue_with_locks(
        &db,
        company_id,
        agent_id,
        Some(running_run),
        Some(running_run),
    )
    .await;

    let result = sweep_stale_issue_locks(&db).await.unwrap();
    // The test's own issue points at a running run, so it must NOT be cleared
    // even when other tests' leftover stale-lock rows are processed.
    assert!(
        !result.issue_ids.contains(&issue_id),
        "running-run issue {issue_id} must not be in the cleared set: {:?}",
        result.issue_ids
    );

    let row: (Option<Uuid>, Option<Uuid>) =
        sqlx::query_as("SELECT checkout_run_id, execution_run_id FROM issues WHERE id=$1")
            .bind(issue_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(row.0, Some(running_run));
    assert_eq!(row.1, Some(running_run));

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn preserves_locks_when_one_lock_terminal_other_running() {
    let db = connect().await;
    let _guard = lock_tests();
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let failed_run = insert_run_with_status(&db, company_id, agent_id, "failed").await;
    let running_run = insert_run_with_status(&db, company_id, agent_id, "running").await;
    let issue_id = insert_issue_with_locks(
        &db,
        company_id,
        agent_id,
        Some(failed_run),
        Some(running_run),
    )
    .await;

    let result = sweep_stale_issue_locks(&db).await.unwrap();
    // Mixed lock (failed + running): only both-cleanable issues are cleared,
    // so the test's own issue must NOT be in the cleared set.
    assert!(
        !result.issue_ids.contains(&issue_id),
        "mixed-lock issue {issue_id} must not be cleared while one run is running: {:?}",
        result.issue_ids
    );

    let row: (Option<Uuid>, Option<Uuid>) =
        sqlx::query_as("SELECT checkout_run_id, execution_run_id FROM issues WHERE id=$1")
            .bind(issue_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(row.0, Some(failed_run));
    assert_eq!(row.1, Some(running_run));

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn idempotent_second_pass_finds_nothing() {
    let db = connect().await;
    let _guard = lock_tests();
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let failed_run = insert_run_with_status(&db, company_id, agent_id, "failed").await;
    let issue_id = insert_issue_with_locks(&db, company_id, agent_id, Some(failed_run), None).await;

    let first = sweep_stale_issue_locks(&db).await.unwrap();
    // First pass must clear the test's issue (it points at a failed run).
    assert!(
        first.issue_ids.contains(&issue_id),
        "first pass must clear the test's own issue {issue_id}: {:?}",
        first.issue_ids
    );

    let second = sweep_stale_issue_locks(&db).await.unwrap();
    // Second pass must not re-clear the test's issue (idempotent).
    assert!(
        !second.issue_ids.contains(&issue_id),
        "second pass must not re-clear the test's issue {issue_id}: {:?}",
        second.issue_ids
    );

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn cleans_locks_when_checkout_terminal_and_execution_null() {
    let db = connect().await;
    let _guard = lock_tests();
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let failed_run = insert_run_with_status(&db, company_id, agent_id, "failed").await;
    // execution_run_id is NULL (the WHERE-clause already required either to be non-null)
    let issue_id = insert_issue_with_locks(&db, company_id, agent_id, Some(failed_run), None).await;

    let result = sweep_stale_issue_locks(&db).await.unwrap();
    // The test's own issue (checkout=failed, execution=null) must be cleared.
    assert!(
        result.issue_ids.contains(&issue_id),
        "test issue {issue_id} must be in the cleared set: {:?}",
        result.issue_ids
    );

    // Both lock columns should now be NULL
    let row: (Option<Uuid>, Option<Uuid>) =
        sqlx::query_as("SELECT checkout_run_id, execution_run_id FROM issues WHERE id=$1")
            .bind(issue_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(row.0.is_none() && row.1.is_none());

    cleanup(&db, company_id).await;
}
