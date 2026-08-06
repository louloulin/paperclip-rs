//! Round 322：`summarize_run_failure_for_issue_comment` 的 PostgreSQL round-trip 验证。
//!
//! 与 Node `services/recovery/service.ts::summarizeRunFailureForIssueComment` 对齐：
//! - 输入是 heartbeat_run 行的 (error, error_code) 字段
//! - 输出是 Option<&'static str>：有 error 时返回固定字符串，否则 None
//!
//! 测试焦点：从真实 DB 读 heartbeat_runs 行 → 构造 RunFailureView → 调函数 → 验证输出。

use pc_heartbeat::recovery::summarize_run_failure_for_issue_comment;
use pc_repos::Db;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn cleanup(db: &Db, company_id: Uuid) {
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

async fn fixture(db: &Db) -> (Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind(format!("r322-{company_id}"))
        .bind(format!("R{}", &company_id.simple().to_string()[..8]))
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'r322-agent', 'general', 'process', 'active')",
    )
    .bind(agent_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .unwrap();
    (company_id, agent_id)
}

async fn insert_run_with_error(
    db: &Db,
    company_id: Uuid,
    agent_id: Uuid,
    error: Option<&str>,
    error_code: Option<&str>,
) -> Uuid {
    let run_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, error, error_code, \
         started_at, created_at) \
         VALUES ($1, $2, $3, 'failed', $4, $5, now(), now())",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .bind(error)
    .bind(error_code)
    .execute(db.pool())
    .await
    .unwrap();
    run_id
}

async fn fetch_error_fields(db: &Db, run_id: Uuid) -> (Option<String>, Option<String>) {
    let row: (Option<String>, Option<String>) =
        sqlx::query_as("SELECT error, error_code FROM heartbeat_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    row
}

/// 真实 DB 路径：run with error → summary should be Some。
#[tokio::test]
async fn db_run_with_error_returns_some_summary() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let run_id = insert_run_with_error(&db, company_id, agent_id, Some("boom"), None).await;

    let (error, error_code) = fetch_error_fields(&db, run_id).await;
    let view = pc_heartbeat::recovery::RunFailureView {
        error: error.as_deref(),
        error_code: error_code.as_deref(),
    };
    let result = summarize_run_failure_for_issue_comment(Some(&view));
    assert!(result.is_some());
    assert!(result.unwrap().contains("withheld"));

    cleanup(&db, company_id).await;
}

/// 真实 DB 路径：run with error_code only → summary should be Some。
#[tokio::test]
async fn db_run_with_only_error_code_returns_some_summary() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let run_id =
        insert_run_with_error(&db, company_id, agent_id, None, Some("adapter_failed")).await;

    let (error, error_code) = fetch_error_fields(&db, run_id).await;
    let view = pc_heartbeat::recovery::RunFailureView {
        error: error.as_deref(),
        error_code: error_code.as_deref(),
    };
    let result = summarize_run_failure_for_issue_comment(Some(&view));
    assert!(result.is_some());

    cleanup(&db, company_id).await;
}

/// 真实 DB 路径：clean run（无 error / error_code）→ summary should be None。
#[tokio::test]
async fn db_clean_run_returns_none_summary() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let run_id = insert_run_with_error(&db, company_id, agent_id, None, None).await;

    let (error, error_code) = fetch_error_fields(&db, run_id).await;
    let view = pc_heartbeat::recovery::RunFailureView {
        error: error.as_deref(),
        error_code: error_code.as_deref(),
    };
    let result = summarize_run_failure_for_issue_comment(Some(&view));
    assert!(result.is_none());

    cleanup(&db, company_id).await;
}

/// DB 路径：clean run + happy summary 直接拼到 escalation comment 模板中。
#[tokio::test]
async fn clean_run_yields_no_failure_section_in_comment() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let run_id = insert_run_with_error(&db, company_id, agent_id, None, None).await;

    let (error, error_code) = fetch_error_fields(&db, run_id).await;
    let view = pc_heartbeat::recovery::RunFailureView {
        error: error.as_deref(),
        error_code: error_code.as_deref(),
    };
    let failure_summary = summarize_run_failure_for_issue_comment(Some(&view)).unwrap_or("");

    // 模拟 Node escalation comment 拼接
    let comment = format!(
        "Paperclip retried the pending execution-review participant once, but the review stage still has no completed decision or live reviewer run.{failure_summary} Moving it to `blocked`."
    );

    assert!(
        !comment.contains("withheld"),
        "clean run must not add withheld section, got: {comment}"
    );
    assert!(comment.contains("Moving it to `blocked`"));

    cleanup(&db, company_id).await;
}
