//! Round 337：`scan_silent_active_runs` 主循环接通 `create_or_update_stale_run_evaluation_full`。
//!
//! 与 Node `services/recovery/service.ts:2277` (`scanSilentActiveRuns`) 完全对齐：
//! - 加载 silent candidate runs (status='running' + silence >= suspicion threshold)
//! - 按 issue_created_at_gte 过滤
//! - 对每个 candidate：
//!   1. snooze 检查 → snoozed++
//!   2. fetch running_agent view + reports_to
//!   3. fetch source_issue view + status + assignee_agent_id
//!   4. resolve_stale_run_owner_agent_id
//!   5. 调 `create_or_update_stale_run_evaluation_full`
//!   6. 按 outcome 更新 result 计数
//!
//! 与 R336 关键差异：scan_silent_active_runs 内部会**自动 fetch** 所有依赖数据，
//! 调用方只需传 `db + options`；不再需要手工构造 agent view / source_issue view。
//!
//! 测试场景：
//! 1. critical + 有 source + 有 owner → Created + source comment + reviewer wake
//! 2. critical + 无 source → Created（仅 description，无 source comment / wake）
//! 3. dismissed_false_positive → Skipped
//! 4. snoozed → Skipped
//! 5. existing + critical → Escalated

use chrono::{Duration, Utc};
use pc_heartbeat::recovery::scan_silent_active_runs_db::{
    scan_silent_active_runs, ScanSilentRunsOptions,
};
use pc_heartbeat::recovery::watchdog_decision_recording::STALE_ACTIVE_RUN_EVALUATION_ORIGIN_KIND;
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM agent_wakeup_requests WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM activity_log WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issue_comments WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM heartbeat_run_watchdog_decisions WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM heartbeat_runs WHERE company_id = $1")
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

async fn fixture(db: &Db) -> (Uuid, String) {
    let company_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind(format!("r337-{company_id}"))
        .bind(&prefix)
        .execute(db.pool())
        .await
        .unwrap();
    (company_id, prefix)
}

async fn insert_agent(
    db: &Db,
    company_id: Uuid,
    name: &str,
    role: &str,
    reports_to: Option<Uuid>,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status, reports_to) \
         VALUES ($1, $2, $3, $4, 'process', 'active', $5)",
    )
    .bind(id)
    .bind(company_id)
    .bind(name)
    .bind(role)
    .bind(reports_to)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_silent_run(
    db: &Db,
    company_id: Uuid,
    agent_id: Uuid,
    silence_min: i64,
    issue_id: Option<Uuid>,
) -> Uuid {
    let id = Uuid::new_v4();
    let last_output_at = Utc::now() - Duration::minutes(silence_min);
    let started_at = Utc::now() - Duration::minutes(silence_min + 5);
    let context_snapshot = issue_id.map(|i| json!({"issueId": i.to_string()}));
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, invocation_source, status, \
                                    started_at, process_started_at, last_output_at, context_snapshot) \
         VALUES ($1, $2, $3, 'manual', 'running', $4, $4, $5, $6)",
    )
    .bind(id)
    .bind(company_id)
    .bind(agent_id)
    .bind(started_at)
    .bind(last_output_at)
    .bind(context_snapshot)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_source_issue(
    db: &Db,
    company_id: Uuid,
    prefix: &str,
    status: &str,
    assignee_agent_id: Option<Uuid>,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, identifier, title, status, origin_kind, assignee_agent_id) \
         VALUES ($1, $2, $3, 'r337-src', $4, 'todo', $5)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("{prefix}-1"))
    .bind(status)
    .bind(assignee_agent_id)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_dismissed(db: &Db, company_id: Uuid, run_id: Uuid) {
    sqlx::query(
        "INSERT INTO heartbeat_run_watchdog_decisions \
         (company_id, run_id, decision, reason) VALUES ($1, $2, 'dismissed_false_positive', 'r337')",
    )
    .bind(company_id)
    .bind(run_id)
    .execute(db.pool())
    .await
    .unwrap();
}

async fn insert_snooze(db: &Db, company_id: Uuid, run_id: Uuid) {
    sqlx::query(
        "INSERT INTO heartbeat_run_watchdog_decisions \
         (company_id, run_id, decision, snoozed_until, reason) \
         VALUES ($1, $2, 'snooze', now() + interval '1 hour', 'r337')",
    )
    .bind(company_id)
    .bind(run_id)
    .execute(db.pool())
    .await
    .unwrap();
}

async fn fetch_evaluation_for_run(
    db: &Db,
    company_id: Uuid,
    run_id: Uuid,
) -> Option<(Uuid, String)> {
    let row: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, priority::text FROM issues \
         WHERE company_id = $1 AND origin_kind = $2 AND origin_id = $3 \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(company_id)
    .bind(STALE_ACTIVE_RUN_EVALUATION_ORIGIN_KIND)
    .bind(run_id.to_string())
    .fetch_optional(db.pool())
    .await
    .unwrap();
    row
}

async fn fetch_comment_count(db: &Db, issue_id: Uuid) -> i64 {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issue_comments WHERE issue_id = $1 AND deleted_at IS NULL",
    )
    .bind(issue_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    n
}

async fn fetch_wake_count(db: &Db, company_id: Uuid, evaluation_id: Uuid) -> i64 {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM agent_wakeup_requests \
         WHERE company_id = $1 AND payload->>'issueId' = $2",
    )
    .bind(company_id)
    .bind(evaluation_id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    n
}

/// critical + 有 source issue + 有 owner（sourceIssue.assignee.reportsTo）→ Created + source comment + wake
#[tokio::test]
async fn scan_with_critical_owner_and_source_creates_full() {
    let db = connect().await;
    let (company_id, prefix) = fixture(&db).await;

    // Org: cto (root) <- engineer-reports-to (manager) <- engineer (running)
    let cto_id = insert_agent(&db, company_id, "cto", "cto", None).await;
    let manager_id = insert_agent(&db, company_id, "manager", "engineer", Some(cto_id)).await;
    let running_id =
        insert_agent(&db, company_id, "engineer-1", "engineer", Some(manager_id)).await;

    // Source issue 由 manager 负责 → manager.reportsTo = cto 是 owner candidate
    let source_id =
        insert_source_issue(&db, company_id, &prefix, "in_progress", Some(manager_id)).await;
    // silence 250min → critical (>240min)
    let run_id = insert_silent_run(&db, company_id, running_id, 250, Some(source_id)).await;

    let result = scan_silent_active_runs(
        &db,
        ScanSilentRunsOptions {
            now: Some(Utc::now()),
            company_id: Some(company_id),
            issue_created_at_gte: None,
            limit: Some(50),
        },
    )
    .await
    .unwrap();

    assert_eq!(result.scanned, 1);
    assert_eq!(result.created, 1);
    assert_eq!(result.evaluation_issue_ids.len(), 1);

    let (evaluation_id, priority) = fetch_evaluation_for_run(&db, company_id, run_id)
        .await
        .unwrap();
    assert_eq!(priority, "high"); // critical -> high

    // source issue 被写 comment
    assert!(fetch_comment_count(&db, source_id).await >= 1);

    // reviewer (cto) 被 enqueue wake
    assert_eq!(fetch_wake_count(&db, company_id, evaluation_id).await, 1);

    cleanup(&db, company_id).await;
}

/// critical + 无 source issue + 无 owner → Created（仅 description，无 source comment / wake）
#[tokio::test]
async fn scan_critical_no_source_no_owner_creates_only() {
    let db = connect().await;
    let (company_id, _prefix) = fixture(&db).await;
    // 只有 running agent，无 reports_to，无 cto/ceo
    let running_id = insert_agent(&db, company_id, "engineer", "engineer", None).await;
    let run_id = insert_silent_run(&db, company_id, running_id, 250, None).await;

    let result = scan_silent_active_runs(
        &db,
        ScanSilentRunsOptions {
            now: Some(Utc::now()),
            company_id: Some(company_id),
            issue_created_at_gte: None,
            limit: Some(50),
        },
    )
    .await
    .unwrap();

    assert_eq!(result.scanned, 1);
    assert_eq!(result.created, 1);

    let (evaluation_id, priority) = fetch_evaluation_for_run(&db, company_id, run_id)
        .await
        .unwrap();
    assert_eq!(priority, "high");

    // 无 source issue → 无 comment
    // 无 owner 时不应该有 wake
    assert_eq!(fetch_wake_count(&db, company_id, evaluation_id).await, 0);

    cleanup(&db, company_id).await;
}

/// dismissed_false_positive → Skipped
#[tokio::test]
async fn scan_dismissed_false_positive_skips() {
    let db = connect().await;
    let (company_id, _prefix) = fixture(&db).await;
    let cto_id = insert_agent(&db, company_id, "cto", "cto", None).await;
    let running_id = insert_agent(&db, company_id, "engineer", "engineer", Some(cto_id)).await;
    let run_id = insert_silent_run(&db, company_id, running_id, 250, None).await;
    insert_dismissed(&db, company_id, run_id).await;

    let result = scan_silent_active_runs(
        &db,
        ScanSilentRunsOptions {
            now: Some(Utc::now()),
            company_id: Some(company_id),
            issue_created_at_gte: None,
            limit: Some(50),
        },
    )
    .await
    .unwrap();

    assert_eq!(result.scanned, 1);
    assert_eq!(result.skipped, 1);
    assert_eq!(result.created, 0);

    // 不应创建 evaluation issue
    assert!(fetch_evaluation_for_run(&db, company_id, run_id)
        .await
        .is_none());

    cleanup(&db, company_id).await;
}

/// snoozed run → Skipped（不计 snooze 在外）
#[tokio::test]
async fn scan_snoozed_skips() {
    let db = connect().await;
    let (company_id, _prefix) = fixture(&db).await;
    let cto_id = insert_agent(&db, company_id, "cto", "cto", None).await;
    let running_id = insert_agent(&db, company_id, "engineer", "engineer", Some(cto_id)).await;
    let run_id = insert_silent_run(&db, company_id, running_id, 250, None).await;
    insert_snooze(&db, company_id, run_id).await;

    let result = scan_silent_active_runs(
        &db,
        ScanSilentRunsOptions {
            now: Some(Utc::now()),
            company_id: Some(company_id),
            issue_created_at_gte: None,
            limit: Some(50),
        },
    )
    .await
    .unwrap();

    assert_eq!(result.scanned, 1);
    assert_eq!(result.snoozed, 1);
    assert_eq!(result.created, 0);

    cleanup(&db, company_id).await;
}

/// 已有 evaluation + critical → Escalated（升级 priority + source comment）
#[tokio::test]
async fn scan_existing_critical_escalates() {
    let db = connect().await;
    let (company_id, prefix) = fixture(&db).await;
    let cto_id = insert_agent(&db, company_id, "cto", "cto", None).await;
    let manager_id = insert_agent(&db, company_id, "manager", "engineer", Some(cto_id)).await;
    let running_id = insert_agent(&db, company_id, "engineer", "engineer", Some(manager_id)).await;
    let source_id =
        insert_source_issue(&db, company_id, &prefix, "in_progress", Some(manager_id)).await;

    // 先创建已有 medium-priority evaluation issue
    let existing_id: (Uuid,) = sqlx::query_as(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, \
                              origin_id, origin_run_id, origin_fingerprint) \
         VALUES (gen_random_uuid(), $1, 'existing', 'todo', 'medium', $2, $3, $3, $4) \
         RETURNING id",
    )
    .bind(company_id)
    .bind(STALE_ACTIVE_RUN_EVALUATION_ORIGIN_KIND)
    .bind(Uuid::new_v4().to_string())
    .bind(format!("existing-{company_id}"))
    .fetch_one(db.pool())
    .await
    .unwrap();
    let existing_id = existing_id.0;

    // 创建一个 candidate run，并手动把 existing_id 关联到 origin_id
    let run_id = insert_silent_run(&db, company_id, running_id, 250, Some(source_id)).await;
    sqlx::query("UPDATE issues SET origin_id = $1 WHERE id = $2")
        .bind(run_id.to_string())
        .bind(existing_id)
        .execute(db.pool())
        .await
        .unwrap();

    let result = scan_silent_active_runs(
        &db,
        ScanSilentRunsOptions {
            now: Some(Utc::now()),
            company_id: Some(company_id),
            issue_created_at_gte: None,
            limit: Some(50),
        },
    )
    .await
    .unwrap();

    assert_eq!(result.scanned, 1);
    assert_eq!(result.escalated, 1);

    // priority 升级到 high
    let (_, priority) = fetch_evaluation_for_run(&db, company_id, run_id)
        .await
        .unwrap();
    assert_eq!(priority, "high");

    // evaluation issue 写 critical threshold comment
    assert!(fetch_comment_count(&db, existing_id).await >= 1);

    // source issue 写 escalation comment
    assert!(fetch_comment_count(&db, source_id).await >= 1);

    cleanup(&db, company_id).await;
}

/// suspicious level (60-240 min) + 无 owner → Created but medium priority, no wake
#[tokio::test]
async fn scan_suspicious_no_owner_creates_minimal() {
    let db = connect().await;
    let (company_id, _prefix) = fixture(&db).await;
    let running_id = insert_agent(&db, company_id, "engineer-1", "engineer", None).await;
    let run_id = insert_silent_run(&db, company_id, running_id, 90, None).await;

    let result = scan_silent_active_runs(
        &db,
        ScanSilentRunsOptions {
            now: Some(Utc::now()),
            company_id: Some(company_id),
            issue_created_at_gte: None,
            limit: Some(50),
        },
    )
    .await
    .unwrap();

    assert_eq!(result.scanned, 1);
    assert_eq!(result.created, 1);

    let (evaluation_id, priority) = fetch_evaluation_for_run(&db, company_id, run_id)
        .await
        .unwrap();
    assert_eq!(priority, "medium"); // suspicious -> medium
    assert_eq!(fetch_wake_count(&db, company_id, evaluation_id).await, 0);

    cleanup(&db, company_id).await;
}

/// scan 0 candidates → empty result
#[tokio::test]
async fn scan_no_candidates_returns_empty() {
    let db = connect().await;
    let (company_id, _prefix) = fixture(&db).await;

    let result = scan_silent_active_runs(
        &db,
        ScanSilentRunsOptions {
            now: Some(Utc::now()),
            company_id: Some(company_id),
            issue_created_at_gte: None,
            limit: Some(50),
        },
    )
    .await
    .unwrap();

    assert_eq!(result.scanned, 0);
    assert_eq!(result.created, 0);
    assert_eq!(result.existing, 0);
    assert_eq!(result.escalated, 0);
    assert_eq!(result.skipped, 0);
    assert_eq!(result.snoozed, 0);
    assert!(result.evaluation_issue_ids.is_empty());

    cleanup(&db, company_id).await;
}
