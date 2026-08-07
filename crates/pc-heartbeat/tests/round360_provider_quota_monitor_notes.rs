//! Round 360：`ensure_provider_quota_wait_recovery_monitor` 路径
//! 写 `issues.monitor_notes`，与 `schedule_provider_quota_recovery_monitor` 文案对齐。
//!
//! 业务背景：
//! - `monitor_notes` 是 issue 表上的人类可读备注字段（schema 已预留）
//! - `schedule_provider_quota_recovery_monitor`（in-place 路径）R319 已写 monitor_notes：
//!   * `in_review`: "Provider usage quota reached; retry the active review participant
//!                   at the provider reset time." / "...after the default recovery backoff."
//!   * `in_progress`（非 in_review）: "Provider usage quota reached; retry the original
//!                   assignee at the provider reset time." / "...after the default recovery backoff."
//! - `ensure_provider_quota_wait_recovery_monitor`（wait_recovery 路径）R355 接线，
//!   但**只更新** `issue_recovery_actions.monitor_policy`，**从未更新** `issues.monitor_notes` →
//!   用户在前端 dashboard 看到的是空 monitor_notes。
//!
//! 本轮闭合：wait_recovery monitor 路径也写 monitor_notes，文案与 in-place 路径对齐。
//!
//! Node 参考：缺失（service.ts 不在当前仓库结构中），文案按 Rust schedule 路径的契约对齐。

use pc_heartbeat::recovery::provider_quota_recovery_monitor::{
    ensure_provider_quota_wait_recovery_monitor, EnsureProviderQuotaMonitorInput,
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
        .bind(format!("r360-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO agents (id,company_id,name,role,adapter_type,status) VALUES ($1,$2,'r360-agent','general','process','active')")
        .bind(agent_id)
        .bind(company_id)
        .execute(db.pool())
        .await
        .unwrap();
    (company_id, agent_id)
}

async fn insert_issue(db: &Db, company_id: Uuid, status: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id,company_id,title,status,priority,origin_kind,origin_fingerprint) \
         VALUES ($1,$2,$3,$4,'normal','system',$5)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r360-issue-{id}"))
    .bind(status)
    .bind(format!("r360-fp-{id}"))
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_run(db: &Db, company_id: Uuid, issue_id: Uuid, agent_id: Uuid) -> Uuid {
    let run_id = Uuid::new_v4();
    let context_snapshot = json!({ "issueId": issue_id.to_string() });
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, context_snapshot, started_at, created_at) VALUES ($1, $2, $3, 'failed', $4, now(), now())",
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

async fn insert_recovery_action(db: &Db, company_id: Uuid, issue_id: Uuid, agent_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issue_recovery_actions (id, company_id, source_issue_id, kind, owner_type, owner_agent_id, cause, fingerprint, next_action) \
         VALUES ($1, $2, $3, 'wait_recovery', 'system', $4, 'provider_quota', $5, 'Wait for provider quota recovery')",
    )
    .bind(id)
    .bind(company_id)
    .bind(issue_id)
    .bind(agent_id)
    .bind(format!("r360-action-fp-{id}"))
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM activity_log WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM agent_wakeup_requests WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issue_recovery_actions WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM heartbeat_runs WHERE company_id = $1")
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

async fn fetch_issue_monitor_notes(db: &Db, issue_id: Uuid) -> Option<String> {
    sqlx::query_scalar::<_, Option<String>>("SELECT monitor_notes FROM issues WHERE id = $1")
        .bind(issue_id)
        .fetch_one(db.pool())
        .await
        .expect("fetch monitor_notes")
}

/// wait_recovery monitor 路径 (issue status = `in_review`): monitor_notes
/// 必须是 review-participant 路径文案（与 schedule_provider_quota_recovery_monitor 对齐）。
#[tokio::test]
async fn wait_recovery_monitor_writes_monitor_notes_for_review_participant() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let issue_id = insert_issue(&db, company_id, "in_review").await;
    let run_id = insert_run(&db, company_id, issue_id, agent_id).await;
    let action_id = insert_recovery_action(&db, company_id, issue_id, agent_id).await;

    let result = ensure_provider_quota_wait_recovery_monitor(
        &db,
        EnsureProviderQuotaMonitorInput {
            company_id,
            issue_id,
            agent_id,
            action_id,
            latest_run_id: Some(run_id),
            now: Some(chrono::Utc::now()),
        },
    )
    .await
    .expect("ensure monitor")
    .expect("some result");
    assert!(!result.scheduled_run_id.is_nil());

    let notes = fetch_issue_monitor_notes(&db, issue_id)
        .await
        .expect("monitor_notes must be Some");
    assert!(
        notes.contains("review participant"),
        "in_review monitor notes must reference review participant, got: {notes}"
    );
    assert!(
        notes.starts_with("Provider usage quota reached"),
        "monitor notes must start with provider quota prefix, got: {notes}"
    );

    cleanup(&db, company_id).await;
}

/// wait_recovery monitor 路径 (issue status = `in_progress` 非 review): monitor_notes
/// 必须是 original-assignee 路径文案（与 schedule_provider_quota_recovery_monitor 对齐）。
#[tokio::test]
async fn wait_recovery_monitor_writes_monitor_notes_for_original_assignee() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let issue_id = insert_issue(&db, company_id, "in_progress").await;
    let run_id = insert_run(&db, company_id, issue_id, agent_id).await;
    let action_id = insert_recovery_action(&db, company_id, issue_id, agent_id).await;

    let result = ensure_provider_quota_wait_recovery_monitor(
        &db,
        EnsureProviderQuotaMonitorInput {
            company_id,
            issue_id,
            agent_id,
            action_id,
            latest_run_id: Some(run_id),
            now: Some(chrono::Utc::now()),
        },
    )
    .await
    .expect("ensure monitor")
    .expect("some result");
    assert!(!result.scheduled_run_id.is_nil());

    let notes = fetch_issue_monitor_notes(&db, issue_id)
        .await
        .expect("monitor_notes must be Some");
    assert!(
        notes.contains("original assignee"),
        "in_progress monitor notes must reference original assignee, got: {notes}"
    );
    assert!(
        !notes.contains("review participant"),
        "in_progress monitor notes must NOT reference review participant, got: {notes}"
    );

    cleanup(&db, company_id).await;
}

/// 幂等：重复调用 ensure_provider_quota_wait_recovery_monitor 不会覆盖 monitor_notes
/// （避免破坏 review-participant 路径已写的 notes）。
#[tokio::test]
async fn repeated_wait_recovery_monitor_does_not_overwrite_monitor_notes() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_with_company(&db).await;
    let issue_id = insert_issue(&db, company_id, "in_review").await;
    let run_id = insert_run(&db, company_id, issue_id, agent_id).await;
    let action_id = insert_recovery_action(&db, company_id, issue_id, agent_id).await;

    let _ = ensure_provider_quota_wait_recovery_monitor(
        &db,
        EnsureProviderQuotaMonitorInput {
            company_id,
            issue_id,
            agent_id,
            action_id,
            latest_run_id: Some(run_id),
            now: Some(chrono::Utc::now()),
        },
    )
    .await
    .expect("first")
    .expect("some");
    let first_notes = fetch_issue_monitor_notes(&db, issue_id).await.unwrap();
    assert!(first_notes.contains("review participant"));

    // 第二次调用 → 走 early-return 路径（已有 scheduled_retry run），不应再覆写 monitor_notes
    let _second = ensure_provider_quota_wait_recovery_monitor(
        &db,
        EnsureProviderQuotaMonitorInput {
            company_id,
            issue_id,
            agent_id,
            action_id,
            latest_run_id: Some(run_id),
            now: Some(chrono::Utc::now() + chrono::Duration::seconds(1)),
        },
    )
    .await
    .expect("second")
    .expect("some");
    let second_notes = fetch_issue_monitor_notes(&db, issue_id).await.unwrap();
    assert_eq!(
        first_notes, second_notes,
        "second call must not overwrite monitor_notes"
    );

    cleanup(&db, company_id).await;
}
