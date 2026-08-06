//! Sweep + Continuation Retry Summary 联合端到端集成测试。
//! 验证 `reconcile_and_escalate_stranded_for_company` 在主循环中接入
//! continuation retry 决策：
//! - consecutive >= max_attempts → force escalate（跳过 scheduler）
//! - 距最近 finished_at 过短 → backoff skip（不入 escalate）
//! - 无 retry history → 走原 sweep 路径
use chrono::Utc;
use pc_heartbeat::recovery::{
    reconcile_and_escalate_stranded_for_company, EscalateOutcome,
    ISSUE_CONTINUATION_NEEDED_RETRY_REASON,
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
        .bind(format!("r299-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO agents (id,company_id,name,role,adapter_type,status) VALUES ($1,$2,'r299-agent','general','process','active')")
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
    .bind(format!("r299-fp-{issue_id}"))
    .bind(agent_id)
    .execute(db.pool())
    .await
    .unwrap();
    issue_id
}

async fn insert_run_with_finished_at(
    db: &Db,
    company_id: Uuid,
    issue_id: Uuid,
    agent_id: Uuid,
    error_code: &str,
    retry_reason: Option<&str>,
    finished_at: chrono::DateTime<Utc>,
) -> Uuid {
    let run_id = Uuid::new_v4();
    let context_snapshot = json!({
        "issueId": issue_id.to_string(),
        "retryReason": retry_reason.unwrap_or(""),
    });
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, error_code, error, context_snapshot, finished_at, started_at, created_at) VALUES ($1, $2, $3, 'failed', $4, 'r299-fixture', $5, $6, $6, $6)",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .bind(error_code)
    .bind(context_snapshot)
    .bind(finished_at)
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
async fn sweep_skips_when_recent_failed_run_hits_backoff() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let issue_id = insert_issue(&db, company_id, agent_id, "backoff issue", "in_progress").await;
    // Insert 1 matching run with finished_at = now (just now, way inside base_backoff 60s)
    let now = Utc::now();
    let _ = insert_run_with_finished_at(
        &db,
        company_id,
        issue_id,
        agent_id,
        "process_lost",
        Some(ISSUE_CONTINUATION_NEEDED_RETRY_REASON),
        now,
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

    // Backoff gate should skip this issue entirely
    assert_eq!(result.skipped, 1, "backoff must suppress escalation");
    assert_eq!(result.dispatched, 0);
    assert_eq!(result.failed, 0);

    // Confirm NO recovery_action written (since we skipped before scheduler)
    let count: (i64,) =
        sqlx::query_as("SELECT COUNT(*)::bigint FROM issue_recovery_actions WHERE company_id=$1")
            .bind(company_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(count.0, 0, "backoff skip must not write recovery_action");

    // Confirm issue status still in_progress
    let row: (String,) = sqlx::query_as("SELECT status FROM issues WHERE id=$1")
        .bind(issue_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(row.0, "in_progress");

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn sweep_force_escalates_when_retry_limit_exceeded() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let issue_id = insert_issue(
        &db,
        company_id,
        agent_id,
        "retry-limit issue",
        "in_progress",
    )
    .await;
    // Insert 3 consecutive matching runs spread over 1 day each (well past base_backoff*4 = 4min)
    let base = Utc::now() - chrono::Duration::days(7);
    for offset in 0..3 {
        let finished = base + chrono::Duration::seconds(offset * 60);
        let _ = insert_run_with_finished_at(
            &db,
            company_id,
            issue_id,
            agent_id,
            "process_lost",
            Some(ISSUE_CONTINUATION_NEEDED_RETRY_REASON),
            finished,
        )
        .await;
    }

    let result = reconcile_and_escalate_stranded_for_company(
        &db,
        company_id,
        None,
        wake_template(company_id, agent_id),
        100,
    )
    .await
    .unwrap();

    // consecutive >= 3 ⇒ force escalate. Escalate path will internally invoke
    // ensure_source_scoped_recovery_action_for_issue, so dispatched=1.
    assert_eq!(result.dispatched, 1, "retry limit must force escalation");
    assert_eq!(result.skipped, 0);
    assert_eq!(result.failed, 0);

    // Confirm escalate outcome
    let outcome = result.outcomes.first().expect("at least one outcome");
    assert_eq!(outcome.escalate_outcome, EscalateOutcome::SourceEscalated);
    assert!(
        outcome.recovery_action_id.is_some(),
        "escalate path writes recovery_action"
    );

    // Confirm issue status now blocked
    let row: (String,) = sqlx::query_as("SELECT status FROM issues WHERE id=$1")
        .bind(issue_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(row.0, "blocked");

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn sweep_normal_path_when_no_retry_history() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let issue_id = insert_issue(&db, company_id, agent_id, "fresh issue", "in_progress").await;
    // Insert one failed run WITHOUT retry_reason=issue_continuation_needed
    let run_id = Uuid::new_v4();
    let context_snapshot = json!({
        "issueId": issue_id.to_string(),
        "retryReason": "",
    });
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, error_code, error, context_snapshot, finished_at, started_at, created_at) VALUES ($1, $2, $3, 'failed', 'process_lost', 'r299-fixture', $4, now(), now(), now())",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .bind(context_snapshot)
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

    // No retry history (consecutive=0) → normal sweep path (schedule + escalate)
    assert_eq!(result.dispatched, 1);
    assert_eq!(result.skipped, 0);

    cleanup(&db, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn sweep_backoff_gate_uses_latest_run_error_code() {
    // Even if there are 3 matching old runs, if the LATEST run's error_code is
    // different, summary.consecutive should be 0 (chain breaks on first row's error mismatch).
    // Wait, that's backwards: latest DESC, first row is newest. If newest error_code != match,
    // summary.consecutive=0 → normal path.
    let db = connect().await;
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let issue_id = insert_issue(&db, company_id, agent_id, "mismatch issue", "in_progress").await;
    let now = Utc::now();
    // newest: process_lost BUT not issue_continuation_needed retry_reason
    let _ = insert_run_with_finished_at(
        &db,
        company_id,
        issue_id,
        agent_id,
        "process_lost",
        Some("other_retry_reason"),
        now,
    )
    .await;
    // older runs: matching — but they shouldn't count because newest breaks the chain
    let _ = insert_run_with_finished_at(
        &db,
        company_id,
        issue_id,
        agent_id,
        "process_lost",
        Some(ISSUE_CONTINUATION_NEEDED_RETRY_REASON),
        now - chrono::Duration::seconds(60),
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

    // Chain broken at newest (wrong retry_reason), summary.consecutive=0 → normal sweep
    assert_eq!(result.dispatched, 1, "broken chain should not skip");
    assert_eq!(result.skipped, 0);

    cleanup(&db, company_id).await;
}
