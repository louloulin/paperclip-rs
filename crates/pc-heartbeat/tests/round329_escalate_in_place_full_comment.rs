//! Round 329：`escalate_stranded_recovery_issue_in_place` 集成 build_recovery_issue_in_place_escalation_comment 的验证。
//!
//! 与 Node `services/recovery/service.ts:3122` 对齐：
//! - 输入 issue 是 stranded_issue_recovery 时，调用 in-place 升级
//! - 升级时写入的 system comment 必须包含：
//!   1) issue UI 链接（`[PAP-XXX](/PAP/issues/PAP-XXX)`）
//!   2) Latest run UI 链接（`[uuid](/PAP/agents/{uuid}/runs/{uuid})`）
//!   3) Previous status / Latest run status / Retry reason 等 8 行 bullet
//!   4) Guard 行（`recovery issues do not create nested ...`）
//!   5) Next action 段
//!
//! 设计要点：
//! - Round 327/328 提供了 pure builder 和 helper；Round 329 把它们接到 in-place 升级
//! - 验证 comment body 在 DB 中真实写入的内容
//! - 验证 status 从 in_progress → blocked
//! - 验证 latest run 缺失时 comment 仍能正确渲染（占位符 none / unknown）

use pc_heartbeat::recovery::escalate_db::{
    escalate_stranded_recovery_issue_in_place, EscalateDbResult, EscalateOutcome,
};
use pc_repos::Db;
use serde_json::json;
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
    let _ = sqlx::query(
        "DELETE FROM activity_log WHERE company_id = $1;
        let _ = DELETE FROM issue_comments WHERE company_id = $1",
    )
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

async fn fixture(db: &Db) -> (Uuid, String) {
    let company_id = Uuid::new_v4();
    let prefix = format!("R{}", &company_id.simple().to_string()[..8]);
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind(format!("r329-{company_id}"))
        .bind(&prefix)
        .execute(db.pool())
        .await
        .unwrap();
    (company_id, prefix)
}

async fn insert_agent(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status)          VALUES ($1, $2, $3, 'engineer', 'process', 'active')",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r329-{id}"))
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_recovery_issue(
    db: &Db,
    company_id: Uuid,
    agent_id: Uuid,
    identifier: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, identifier, title, status, origin_kind, assignee_agent_id)          VALUES ($1, $2, $3, 'recovery', 'in_progress', 'stranded_issue_recovery', $4)",
    )
    .bind(id)
    .bind(company_id)
    .bind(identifier)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_heartbeat_run(
    db: &Db,
    company_id: Uuid,
    agent_id: Uuid,
    issue_id: Uuid,
    status: &str,
    error: Option<&str>,
    error_code: Option<&str>,
    context_snapshot: Option<serde_json::Value>,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO heartbeat_runs          (id, company_id, agent_id, invocation_source, status, error, error_code, context_snapshot, started_at)          VALUES ($1, $2, $3, 'manual', $4, $5, $6, $7, now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(agent_id)
    .bind(status)
    .bind(error)
    .bind(error_code)
    .bind(context_snapshot)
    .execute(db.pool())
    .await
    .unwrap();
    // 写入 context_snapshot->>'issueId' 关联
    let _ = issue_id;
    id
}

async fn fetch_latest_comment_body(db: &Db, issue_id: Uuid) -> String {
    let row: (String,) = sqlx::query_as(
        "SELECT body FROM issue_comments WHERE issue_id=$1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(issue_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    row.0
}

/// 完整路径：recovery issue + heartbeat run + latest run 含 error → comment 包含所有字段
#[tokio::test]
async fn writes_full_escalation_comment_with_all_sections() {
    let db = connect().await;
    let (company_id, prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id).await;
    let identifier = format!("{prefix}-42");
    let issue_id = insert_recovery_issue(&db, company_id, agent_id, &identifier).await;
    let _run_id = insert_heartbeat_run(
        &db,
        company_id,
        agent_id,
        issue_id,
        "failed",
        Some("boom"),
        None,
        Some(json!({"issueId": issue_id.to_string(), "retryReason": "issue_continuation_needed"})),
    )
    .await;

    let result = escalate_stranded_recovery_issue_in_place(&db, issue_id, "in_progress".to_owned())
        .await
        .unwrap()
        .expect("result Some");

    assert_eq!(result.outcome, EscalateOutcome::RecoveryInPlace);
    assert_eq!(result.updated_issue.status, "blocked");
    assert!(result.comment_id.is_some());

    let body = fetch_latest_comment_body(&db, issue_id).await;
    assert!(body.starts_with(
        "Paperclip stopped automatic stranded-work recovery for this recovery issue."
    ));
    assert!(body.contains(&format!(
        "- Recovery issue: [{identifier}](/{prefix}/issues/{identifier})"
    )));
    assert!(body.contains("- Previous status: `in_progress`"));
    assert!(body.contains("- Retry reason: `issue_continuation_needed`"));
    assert!(body.contains("Latest retry failure details were withheld"));
    assert!(body.contains(
        "- Guard: recovery issues do not create nested `stranded_issue_recovery` issues."
    ));
    assert!(body.contains("Next action:"));

    cleanup(&db, company_id).await;
}

/// 没有 latest run → comment 包含占位符（none / unknown）
#[tokio::test]
async fn writes_escalation_comment_with_placeholders_when_no_run() {
    let db = connect().await;
    let (company_id, prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id).await;
    let identifier = format!("{prefix}-7");
    let issue_id = insert_recovery_issue(&db, company_id, agent_id, &identifier).await;
    // 注意：没有插入 heartbeat_run

    let result = escalate_stranded_recovery_issue_in_place(&db, issue_id, "todo".to_owned())
        .await
        .unwrap()
        .expect("result Some");

    assert_eq!(result.outcome, EscalateOutcome::RecoveryInPlace);

    let body = fetch_latest_comment_body(&db, issue_id).await;
    assert!(body.contains("- Latest run: none"));
    assert!(body.contains("- Latest run status: `unknown`"));
    assert!(body.contains("- Retry reason: `none`"));
    assert!(body.contains("- Failure: none recorded"));
    assert!(body.contains(&format!("[{identifier}](/{prefix}/issues/{identifier})")));

    cleanup(&db, company_id).await;
}

/// recovery issue 是 todo 状态时仍触发 in-place 升级（不限于 in_progress）
#[tokio::test]
async fn escalates_recovery_issue_from_todo_status() {
    let db = connect().await;
    let (company_id, prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id).await;
    let issue_id = insert_recovery_issue(&db, company_id, agent_id, &format!("{prefix}-T1")).await;
    // 修改 status 为 todo
    sqlx::query("UPDATE issues SET status='todo' WHERE id=$1")
        .bind(issue_id)
        .execute(db.pool())
        .await
        .unwrap();

    let result = escalate_stranded_recovery_issue_in_place(&db, issue_id, "todo".to_owned())
        .await
        .unwrap()
        .expect("result Some");

    assert_eq!(result.outcome, EscalateOutcome::RecoveryInPlace);
    assert_eq!(result.updated_issue.status, "blocked");

    cleanup(&db, company_id).await;
}

/// issue 已为 blocked 但 origin=recovery → 仍然走 RecoveryInPlace（核心场景：重新提醒 owner）
#[tokio::test]
async fn recovery_origin_issue_escalates_even_when_already_blocked() {
    let db = connect().await;
    let (company_id, prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id).await;
    let issue_id = insert_recovery_issue(&db, company_id, agent_id, &format!("{prefix}-S1")).await;
    // 改为 blocked
    sqlx::query("UPDATE issues SET status='blocked' WHERE id=$1")
        .bind(issue_id)
        .execute(db.pool())
        .await
        .unwrap();

    let result = escalate_stranded_recovery_issue_in_place(&db, issue_id, "blocked".to_owned())
        .await
        .unwrap()
        .expect("result Some");

    // RecoveryInPlace 是核心场景：blocked 的 recovery issue 也要再次提醒 owner
    assert_eq!(result.outcome, EscalateOutcome::RecoveryInPlace);
    assert!(result.comment_id.is_some());

    cleanup(&db, company_id).await;
}

/// issue 不存在 → 返回 None
#[tokio::test]
async fn returns_none_when_issue_missing() {
    let db = connect().await;
    let ghost = Uuid::new_v4();
    let result = escalate_stranded_recovery_issue_in_place(&db, ghost, "in_progress".to_owned())
        .await
        .unwrap();
    assert!(result.is_none());
}

/// comment 含 latest run link（agent_id + run_id 都在 URL 中）
#[tokio::test]
async fn comment_contains_run_link_with_agent_id() {
    let db = connect().await;
    let (company_id, prefix) = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id).await;
    let issue_id = insert_recovery_issue(&db, company_id, agent_id, &format!("{prefix}-99")).await;
    let run_id = insert_heartbeat_run(
        &db,
        company_id,
        agent_id,
        issue_id,
        "failed",
        Some("err"),
        None,
        Some(json!({"issueId": issue_id.to_string()})),
    )
    .await;

    let result = escalate_stranded_recovery_issue_in_place(&db, issue_id, "in_progress".to_owned())
        .await
        .unwrap()
        .expect("result Some");

    assert_eq!(result.outcome, EscalateOutcome::RecoveryInPlace);

    let body = fetch_latest_comment_body(&db, issue_id).await;
    assert!(body.contains(&format!(
        "- Latest run: [{run_id}](/{prefix}/agents/{agent_id}/runs/{run_id})"
    )));

    cleanup(&db, company_id).await;
}
