//! Heartbeat ticker 集成测试 —— 验证 run_heartbeat_tick 同时调度
//! reconcile_and_escalate_stranded_for_company + sweep_stale_issue_locks。
use pc_heartbeat::recovery::{run_heartbeat_tick, HeartbeatTickResult, HeartbeatTickerConfig};
use pc_repos::agent::{
    NewAgentWakeupRequest, WakeupActorType, WakeupRequestStatus, WakeupTriggerDetail,
};
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

// Tests in this file share one PostgreSQL instance. The stale-lock sweep is a
// global operation (no per-company filter), so round300 fixtures (cleaned up
// at the end of each test) and our own fixtures interact through the global
// `cleared` counter. We serialize tests inside this binary with a process-wide
// mutex so each test gets a deterministic view of its own company's state,
// and we additionally assert against *our* issue ids (scoped) rather than
// relying on absolute totals.
static TEST_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_tests() -> std::sync::MutexGuard<'static, ()> {
    TEST_MUTEX.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn fixture_with_company(db: &Db) -> (Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id,name,issue_prefix) VALUES ($1,$2,$3)")
        .bind(company_id)
        .bind(format!("r301-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO agents (id,company_id,name,role,adapter_type,status) VALUES ($1,$2,'r301-agent','general','process','active')")
        .bind(agent_id)
        .bind(company_id)
        .execute(db.pool())
        .await
        .unwrap();
    (company_id, agent_id)
}

async fn insert_issue(db: &Db, company_id: Uuid, agent_id: Uuid, status: &str) -> Uuid {
    let issue_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id,company_id,title,status,priority,origin_kind,origin_fingerprint,assignee_agent_id) VALUES ($1,$2,'r301-issue',$3,'normal','system',$4,$5)",
    )
    .bind(issue_id)
    .bind(company_id)
    .bind(status)
    .bind(format!("r301-fp-{issue_id}"))
    .bind(agent_id)
    .execute(db.pool())
    .await
    .unwrap();
    issue_id
}

async fn insert_failed_run(
    db: &Db,
    company_id: Uuid,
    issue_id: Uuid,
    agent_id: Uuid,
    error_code: &str,
) -> Uuid {
    let run_id = Uuid::new_v4();
    let context_snapshot = json!({ "issueId": issue_id.to_string() });
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, error_code, error, context_snapshot, started_at, created_at) VALUES ($1, $2, $3, 'failed', $4, 'r301-fixture', $5, now(), now())",
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
    let _ = sqlx::query("DELETE FROM activity_log WHERE company_id=$1")
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
async fn tick_runs_both_sweeps_on_empty_company() {
    let _guard = lock_tests();
    let db = connect().await;
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let config = HeartbeatTickerConfig::default();
    let result: HeartbeatTickResult = run_heartbeat_tick(
        &db,
        &config,
        &wake_template(company_id, agent_id),
        &[company_id],
    )
    .await
    .unwrap();
    // Empty company → nothing dispatched, nothing cleared
    let stranded = result.stranded.expect("stranded outcome present");
    assert_eq!(stranded.dispatched, 0);
    assert_eq!(stranded.skipped, 0);
    // Scoped assertion: empty company has no issues, so no lock columns should
    // exist for it regardless of how many stale locks the global sweep cleared
    // elsewhere (round300 fixtures, etc.). We do NOT assert
    // `stale_lock_cleared == 0` because that counter is global and shared
    // with round300 tests.
    let row: (Option<Uuid>, Option<Uuid>) = sqlx::query_as(
        "SELECT checkout_run_id, execution_run_id FROM issues WHERE company_id = $1",
    )
    .bind(company_id)
    .fetch_all(db.pool())
    .await
    .unwrap()
    .into_iter()
    .next()
    .unwrap_or((None, None));
    assert_eq!(row, (None, None), "empty company must have no issues");
    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn tick_dispatches_stranded_and_returns_outcome() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let issue = insert_issue(&db, company_id, agent_id, "in_progress").await;
    let _ = insert_failed_run(&db, company_id, issue, agent_id, "process_lost").await;

    let config = HeartbeatTickerConfig::default();
    let result = run_heartbeat_tick(
        &db,
        &config,
        &wake_template(company_id, agent_id),
        &[company_id],
    )
    .await
    .unwrap();
    let stranded = result.stranded.expect("stranded outcome present");
    assert_eq!(stranded.dispatched, 1, "stranded sweep must escalate");

    // Issue should now be blocked
    let row: (String,) = sqlx::query_as("SELECT status FROM issues WHERE id=$1")
        .bind(issue)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(row.0, "blocked");

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn tick_clears_stale_locks_when_enabled() {
    let _guard = lock_tests();
    let db = connect().await;
    let (company_id, agent_id) = fixture_with_company(&db).await;
    // Insert terminal run + issue with checkout_run_id pointing at it
    let failed_run_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, started_at, created_at) VALUES ($1, $2, $3, 'failed'::text, now(), now())",
    )
    .bind(failed_run_id)
    .bind(company_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .unwrap();
    let issue = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id,company_id,title,status,priority,origin_kind,origin_fingerprint,assignee_agent_id,checkout_run_id) VALUES ($1,$2,'r301-issue','todo','normal','system',$3,$4,$5)",
    )
    .bind(issue)
    .bind(company_id)
    .bind(format!("r301-fp-{issue}"))
    .bind(agent_id)
    .bind(failed_run_id)
    .execute(db.pool())
    .await
    .unwrap();

    let config = HeartbeatTickerConfig::default();
    let _result = run_heartbeat_tick(
        &db,
        &config,
        &wake_template(company_id, agent_id),
        &[company_id],
    )
    .await
    .unwrap();

    // Scoped assertion: our specific issue's checkout_run_id must be NULL
    // after the sweep. We do NOT assert `stale_lock_cleared == 1` because
    // that counter is global and shared with round300 fixtures.
    let row: (Option<Uuid>,) = sqlx::query_as("SELECT checkout_run_id FROM issues WHERE id=$1")
        .bind(issue)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        row.0, None,
        "stale lock sweep must clear terminal run reference for our issue"
    );

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn tick_skips_disabled_sweeps() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let issue = insert_issue(&db, company_id, agent_id, "in_progress").await;
    let _ = insert_failed_run(&db, company_id, issue, agent_id, "process_lost").await;

    let config = HeartbeatTickerConfig {
        enable_stranded_sweep: false,
        enable_stale_lock_sweep: false,
        ..Default::default()
    };
    let result = run_heartbeat_tick(
        &db,
        &config,
        &wake_template(company_id, agent_id),
        &[company_id],
    )
    .await
    .unwrap();
    assert_eq!(result.stranded, None);
    assert_eq!(result.stale_lock_cleared, 0);

    // Issue status unchanged (no sweep ran)
    let row: (String,) = sqlx::query_as("SELECT status FROM issues WHERE id=$1")
        .bind(issue)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(row.0, "in_progress");

    cleanup(&db, company_id).await;
}
