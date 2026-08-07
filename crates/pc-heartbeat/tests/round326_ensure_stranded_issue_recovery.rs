//! Round 326：`ensureStrandedIssueRecoveryIssue` 顶层 orchestrator 的 PostgreSQL 验证。
//!
//! 与 Node `services/recovery/service.ts::ensureStrandedIssueRecoveryIssue` 对齐：
//! - input.issue 本身是 stranded_issue_recovery → return None
//! - 已有 open stranded_issue_recovery for same source → 返回 existing
//! - 没有 invokable owner → return None
//! - 否则 → 创建 issue + enqueue wake
//!
//! 关键 invariants：
//! - origin_kind = "stranded_issue_recovery"
//! - origin_id = source_issue.id
//! - origin_run_id = latest_run.id (or null)
//! - parent_id = source_issue.id
//! - assignee_agent_id = resolved owner
//! - description 由 build_stranded_issue_recovery_description 生成
//! - unique conflict 后 → 返回 raced recovery issue（不抛错）

use chrono::{Duration, Utc};
use pc_heartbeat::recovery::build_stranded_issue_recovery_description::LatestRunView;
use pc_heartbeat::recovery::ensure_stranded_issue_recovery_issue::{
    ensure_stranded_issue_recovery_issue, EnsureStrandedIssueRecoveryInput,
};
use pc_heartbeat::recovery::source_scoped_recovery_action::StrandedRecoveryCause;
use pc_heartbeat::recovery::STRANDED_ISSUE_RECOVERY_ORIGIN_KIND;
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn cleanup(db: &Db, company_id: Uuid) {
    // 按 FK 反向顺序清理
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

async fn fixture(db: &Db) -> Uuid {
    let company_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind(format!("r326-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    company_id
}

async fn insert_agent(
    db: &Db,
    company_id: Uuid,
    name: &str,
    role: &str,
    reports_to: Option<Uuid>,
    status: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status, reports_to) \
         VALUES ($1, $2, $3, $4, 'process', $5, $6)",
    )
    .bind(id)
    .bind(company_id)
    .bind(name)
    .bind(role)
    .bind(status)
    .bind(reports_to)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_issue(
    db: &Db,
    company_id: Uuid,
    status: &str,
    priority: &str,
    assignee: Option<Uuid>,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, \
         origin_fingerprint, assignee_agent_id, execution_policy) \
         VALUES ($1, $2, $3, $4, $5, 'user', $6, $7, $8)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r326-source-{id}"))
    .bind(status)
    .bind(priority)
    .bind(format!("r326-fp-{id}"))
    .bind(assignee)
    .bind(json!({"mode":"normal","commentRequired":false,"stages":[]}))
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn fetch_issue_row(db: &Db, issue_id: Uuid) -> pc_repos::issue::IssueRow {
    pc_repos::issue::IssueRepo::new(db)
        .get(issue_id)
        .await
        .unwrap()
        .expect("issue should exist")
}

/// 正常路径：创建 recovery issue + wake
#[tokio::test]
async fn creates_recovery_issue_with_owner_and_wake() {
    let db = connect().await;
    let company_id = fixture(&db).await;
    let cto = insert_agent(&db, company_id, "r326-cto", "cto", None, "active").await;
    let assignee = insert_agent(&db, company_id, "r326-asg", "general", None, "active").await;
    let source_id = insert_issue(&db, company_id, "in_progress", "normal", Some(assignee)).await;
    let source = fetch_issue_row(&db, source_id).await;
    let run_id = Uuid::new_v4();
    let latest_run = LatestRunView {
        id: run_id,
        agent_id: assignee,
        status: Some("failed".to_owned()),
        context_snapshot: Some(json!({"retryReason": "issue_continuation_needed"})),
        result_json: None,
    };

    let input = EnsureStrandedIssueRecoveryInput {
        issue: &source,
        latest_run: Some(&latest_run),
        previous_status: "in_progress",
        recovery_cause: Some(StrandedRecoveryCause::RuntimeFailure),
        successful_run_handoff_evidence: None,
    };

    let result = ensure_stranded_issue_recovery_issue(&db, input)
        .await
        .expect("ensure should succeed");
    let recovery = result.expect("should create recovery issue");

    assert_eq!(recovery.origin_kind, STRANDED_ISSUE_RECOVERY_ORIGIN_KIND);
    assert_eq!(recovery.origin_id, Some(source_id.to_string()));
    assert_eq!(recovery.origin_run_id, Some(run_id.to_string()));
    assert_eq!(recovery.parent_id, Some(source_id));
    assert_eq!(recovery.assignee_agent_id, Some(cto));
    assert_eq!(recovery.status, "todo");
    assert!(recovery
        .description
        .as_deref()
        .unwrap_or("")
        .contains("Source"));

    // origin_fingerprint 应包含 source_id + recovery_cause + run_id
    assert!(recovery.origin_fingerprint.contains(&source_id.to_string()));
    assert!(recovery.origin_fingerprint.contains("runtime_failure"));
    assert!(recovery.origin_fingerprint.ends_with(&run_id.to_string()));

    // wake 应创建
    let wake_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*)::bigint FROM agent_wakeup_requests WHERE company_id = $1")
            .bind(company_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(wake_count.0 >= 1, "should have created at least one wake");

    cleanup(&db, company_id).await;
}

/// input.issue 本身已是 stranded_issue_recovery → 返回 None
#[tokio::test]
async fn returns_none_when_input_issue_is_already_recovery() {
    let db = connect().await;
    let company_id = fixture(&db).await;
    let assignee = insert_agent(&db, company_id, "r326-asg", "general", None, "active").await;
    // Insert issue that is itself a stranded_issue_recovery
    let recovery_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, \
         origin_fingerprint, assignee_agent_id, execution_policy) \
         VALUES ($1, $2, $3, 'todo', 'normal', $4, $5, $6, $7)",
    )
    .bind(recovery_id)
    .bind(company_id)
    .bind("already-recovery")
    .bind(STRANDED_ISSUE_RECOVERY_ORIGIN_KIND)
    .bind(format!("r326-fp-{recovery_id}"))
    .bind(Some(assignee))
    .bind(json!({"mode":"normal","commentRequired":false,"stages":[]}))
    .execute(db.pool())
    .await
    .unwrap();
    let recovery = fetch_issue_row(&db, recovery_id).await;

    let run_id = Uuid::new_v4();
    let latest_run = LatestRunView {
        id: run_id,
        agent_id: assignee,
        status: Some("failed".to_owned()),
        context_snapshot: None,
        result_json: None,
    };
    let input = EnsureStrandedIssueRecoveryInput {
        issue: &recovery,
        latest_run: Some(&latest_run),
        previous_status: "in_progress",
        recovery_cause: Some(StrandedRecoveryCause::RuntimeFailure),
        successful_run_handoff_evidence: None,
    };
    let result = ensure_stranded_issue_recovery_issue(&db, input)
        .await
        .expect("ensure should succeed");
    assert!(
        result.is_none(),
        "should return None when input is recovery"
    );

    cleanup(&db, company_id).await;
}

/// 已有 open recovery → 返回 existing（不创建新的）
#[tokio::test]
async fn returns_existing_when_open_recovery_already_exists() {
    let db = connect().await;
    let company_id = fixture(&db).await;
    let cto = insert_agent(&db, company_id, "r326-cto", "cto", None, "active").await;
    let assignee = insert_agent(&db, company_id, "r326-asg", "general", None, "active").await;
    let source_id = insert_issue(&db, company_id, "in_progress", "normal", Some(assignee)).await;
    let source = fetch_issue_row(&db, source_id).await;

    // 预先创建一个 open recovery（手动插入 origin_id=source_id）
    let existing_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, \
         origin_fingerprint, origin_id, assignee_agent_id, execution_policy) \
         VALUES ($1, $2, $3, 'todo', 'normal', $4, $5, $6, $7, $8)",
    )
    .bind(existing_id)
    .bind(company_id)
    .bind("pre-existing-recovery")
    .bind(STRANDED_ISSUE_RECOVERY_ORIGIN_KIND)
    .bind(format!("r326-fp-{existing_id}"))
    .bind(source_id.to_string())
    .bind(Some(cto))
    .bind(json!({"mode":"normal","commentRequired":false,"stages":[]}))
    .execute(db.pool())
    .await
    .unwrap();

    let run_id = Uuid::new_v4();
    let latest_run = LatestRunView {
        id: run_id,
        agent_id: assignee,
        status: Some("failed".to_owned()),
        context_snapshot: Some(json!({"retryReason": "issue_continuation_needed"})),
        result_json: None,
    };
    let input = EnsureStrandedIssueRecoveryInput {
        issue: &source,
        latest_run: Some(&latest_run),
        previous_status: "in_progress",
        recovery_cause: Some(StrandedRecoveryCause::RuntimeFailure),
        successful_run_handoff_evidence: None,
    };
    let result = ensure_stranded_issue_recovery_issue(&db, input)
        .await
        .expect("ensure should succeed");
    let returned = result.expect("should return existing");
    assert_eq!(
        returned.id, existing_id,
        "should return pre-existing recovery"
    );

    // 应只存在 1 个 stranded_issue_recovery（没有创建新的）
    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issues \
         WHERE company_id = $1 AND origin_kind = $2 AND origin_id = $3",
    )
    .bind(company_id)
    .bind(STRANDED_ISSUE_RECOVERY_ORIGIN_KIND)
    .bind(source_id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count.0, 1);

    cleanup(&db, company_id).await;
}

/// 没有 invokable owner → 返回 None
#[tokio::test]
async fn returns_none_when_no_invokable_owner() {
    let db = connect().await;
    let company_id = fixture(&db).await;
    // 所有 agent 都是 terminated
    let assignee = insert_agent(&db, company_id, "r326-asg", "general", None, "terminated").await;
    let cto = insert_agent(&db, company_id, "r326-cto", "cto", None, "terminated").await;
    let ceo = insert_agent(&db, company_id, "r326-ceo", "ceo", None, "terminated").await;
    let source_id = insert_issue(&db, company_id, "in_progress", "normal", Some(assignee)).await;
    let source = fetch_issue_row(&db, source_id).await;

    let run_id = Uuid::new_v4();
    let latest_run = LatestRunView {
        id: run_id,
        agent_id: assignee,
        status: Some("failed".to_owned()),
        context_snapshot: Some(json!({"retryReason": "issue_continuation_needed"})),
        result_json: None,
    };
    let input = EnsureStrandedIssueRecoveryInput {
        issue: &source,
        latest_run: Some(&latest_run),
        previous_status: "in_progress",
        recovery_cause: Some(StrandedRecoveryCause::RuntimeFailure),
        successful_run_handoff_evidence: None,
    };
    let result = ensure_stranded_issue_recovery_issue(&db, input)
        .await
        .expect("ensure should succeed");
    assert!(result.is_none(), "no invokable owner → None");

    // 不应创建任何 stranded_issue_recovery
    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issues \
         WHERE company_id = $1 AND origin_kind = $2 AND origin_id = $3",
    )
    .bind(company_id)
    .bind(STRANDED_ISSUE_RECOVERY_ORIGIN_KIND)
    .bind(source_id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count.0, 0);

    cleanup(&db, company_id).await;
}

/// SuccessfulRunMissingState cause → title 是 "Recover missing next step ..."
#[tokio::test]
async fn successful_run_missing_state_title_differs() {
    let db = connect().await;
    let company_id = fixture(&db).await;
    let cto = insert_agent(&db, company_id, "r326-cto", "cto", None, "active").await;
    let assignee = insert_agent(&db, company_id, "r326-asg", "general", None, "active").await;
    let source_id = insert_issue(&db, company_id, "todo", "normal", Some(assignee)).await;
    let source = fetch_issue_row(&db, source_id).await;

    let run_id = Uuid::new_v4();
    let source_run_id = Uuid::new_v4();
    let evidence = json!({
        "sourceRunId": source_run_id.to_string(),
        "missingDisposition": "blocked",
    });
    let latest_run = LatestRunView {
        id: run_id,
        agent_id: assignee,
        status: Some("succeeded".to_owned()),
        context_snapshot: Some(json!({"retryReason": "successful_run_missing_state"})),
        result_json: None,
    };
    let input = EnsureStrandedIssueRecoveryInput {
        issue: &source,
        latest_run: Some(&latest_run),
        previous_status: "todo",
        recovery_cause: Some(StrandedRecoveryCause::SuccessfulRunMissingState),
        successful_run_handoff_evidence: Some(&evidence),
    };
    let result = ensure_stranded_issue_recovery_issue(&db, input)
        .await
        .expect("ensure should succeed");
    let recovery = result.expect("should create recovery");
    assert!(
        recovery.title.contains("Recover missing next step"),
        "title should be 'Recover missing next step ...', got: {}",
        recovery.title
    );
    assert!(recovery
        .description
        .as_deref()
        .unwrap_or("")
        .contains("Safe Evidence"));

    cleanup(&db, company_id).await;
}

/// title for stranded → "Recover stalled issue ..."
#[tokio::test]
async fn stranded_default_title() {
    let db = connect().await;
    let company_id = fixture(&db).await;
    let cto = insert_agent(&db, company_id, "r326-cto", "cto", None, "active").await;
    let assignee = insert_agent(&db, company_id, "r326-asg", "general", None, "active").await;
    let source_id = insert_issue(&db, company_id, "in_progress", "normal", Some(assignee)).await;
    let source = fetch_issue_row(&db, source_id).await;

    let run_id = Uuid::new_v4();
    let latest_run = LatestRunView {
        id: run_id,
        agent_id: assignee,
        status: Some("failed".to_owned()),
        context_snapshot: Some(json!({"retryReason": "issue_continuation_needed"})),
        result_json: None,
    };
    let input = EnsureStrandedIssueRecoveryInput {
        issue: &source,
        latest_run: Some(&latest_run),
        previous_status: "in_progress",
        recovery_cause: Some(StrandedRecoveryCause::RuntimeFailure),
        successful_run_handoff_evidence: None,
    };
    let result = ensure_stranded_issue_recovery_issue(&db, input)
        .await
        .expect("ensure should succeed");
    let recovery = result.expect("should create recovery");
    assert!(
        recovery.title.contains("Recover stalled issue"),
        "title should be 'Recover stalled issue ...', got: {}",
        recovery.title
    );

    cleanup(&db, company_id).await;
}
