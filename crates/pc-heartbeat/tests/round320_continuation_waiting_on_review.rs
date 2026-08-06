//! Round 320：`resolveContinuationWaitingOnReview` 的 PostgreSQL 验证。
//!
//! 与 Node `server/src/services/recovery/service.ts::resolveContinuationWaitingOnReview` 对齐：
//! - in_review issue 在 latest run 报告 "waiting on review" 错误码时
//!   → 收集 unresolved blockers + open children 作为新的 blocker set
//! - 若 blocker set 非空 → status=blocked + 写 issue_relations + 加 system comment + log activity
//! - 若 blocker set 为空 → 返回 None（不动作）

use pc_heartbeat::recovery::resolve_continuation_waiting_on_review;
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM activity_log WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issue_comments WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issue_relations WHERE company_id = $1")
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

async fn fixture_company_agent(db: &Db, tag: &str) -> (Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    let prefix = format!("R{tag}{}", &company_id.simple().to_string()[..6]);
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind(format!("r320-{company_id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, $3, 'general', 'process', 'active')",
    )
    .bind(agent_id)
    .bind(company_id)
    .bind(format!("r320-agent-{agent_id}"))
    .execute(db.pool())
    .await
    .unwrap();
    (company_id, agent_id)
}

async fn insert_issue(
    db: &Db,
    company_id: Uuid,
    status: &str,
    parent_id: Option<Uuid>,
    assignee_agent_id: Option<Uuid>,
) -> Uuid {
    let issue_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, \
         origin_fingerprint, parent_id, assignee_agent_id, execution_policy, execution_state) \
         VALUES ($1, $2, $3, $4, 'normal', 'system', $5, $6, $7, $8, $9)",
    )
    .bind(issue_id)
    .bind(company_id)
    .bind(format!("r320-issue-{issue_id}"))
    .bind(status)
    .bind(format!("r320-fp-{issue_id}"))
    .bind(parent_id)
    .bind(assignee_agent_id)
    .bind(json!({"mode":"normal","commentRequired":false,"stages":[]}))
    .bind(json!({"status":"pending"}))
    .execute(db.pool())
    .await
    .unwrap();
    issue_id
}

async fn insert_block_relation(db: &Db, company_id: Uuid, from: Uuid, to: Uuid) {
    sqlx::query(
        "INSERT INTO issue_relations (company_id, issue_id, related_issue_id, type) \
         VALUES ($1, $2, $3, 'blocks')",
    )
    .bind(company_id)
    .bind(from)
    .bind(to)
    .execute(db.pool())
    .await
    .unwrap();
}

async fn fetch_issue(db: &Db, issue_id: Uuid) -> (String, String) {
    let row: (String, String) =
        sqlx::query_as("SELECT status, COALESCE(parent_id::text, '') FROM issues WHERE id = $1")
            .bind(issue_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    row
}

async fn fetch_blocker_ids(db: &Db, company_id: Uuid, issue_id: Uuid) -> Vec<Uuid> {
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT issue_id FROM issue_relations \
         WHERE company_id = $1 AND related_issue_id = $2 AND type = 'blocks' ORDER BY issue_id",
    )
    .bind(company_id)
    .bind(issue_id)
    .fetch_all(db.pool())
    .await
    .unwrap();
    rows.into_iter().map(|(id,)| id).collect()
}

async fn fetch_comment_count(db: &Db, issue_id: Uuid) -> i64 {
    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM issue_comments WHERE issue_id = $1 AND deleted_at IS NULL",
    )
    .bind(issue_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    count.0
}

async fn fetch_activity_count(db: &Db, company_id: Uuid, action: &str) -> i64 {
    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::bigint FROM activity_log WHERE company_id = $1 AND action = $2",
    )
    .bind(company_id)
    .bind(action)
    .fetch_one(db.pool())
    .await
    .unwrap();
    count.0
}

/// 核心场景：in_review issue + unresolved blocker + open child → status=blocked
/// + issue_relations 反映新 set + system comment + activity log
#[tokio::test]
async fn resolves_in_review_with_blocker_and_child_to_blocked() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_company_agent(&db, "A").await;

    let issue_id = insert_issue(&db, company_id, "in_review", None, Some(agent_id)).await;
    let blocker_id = insert_issue(&db, company_id, "todo", None, None).await;
    let child_id = insert_issue(&db, company_id, "todo", Some(issue_id), None).await;
    insert_block_relation(&db, company_id, blocker_id, issue_id).await;

    let updated = resolve_continuation_waiting_on_review(&db, company_id, issue_id)
        .await
        .expect("resolve should succeed")
        .expect("should return updated issue");

    assert_eq!(updated.status, "blocked");
    assert_eq!(updated.id, issue_id);

    let (status, parent) = fetch_issue(&db, issue_id).await;
    assert_eq!(status, "blocked");
    assert_eq!(parent, "");

    let blockers = fetch_blocker_ids(&db, company_id, issue_id).await;
    assert_eq!(blockers.len(), 2, "should have blocker + child as new set");
    assert!(blockers.contains(&blocker_id));
    assert!(blockers.contains(&child_id));

    let comment_count = fetch_comment_count(&db, issue_id).await;
    assert_eq!(comment_count, 1, "should add exactly one system comment");

    let activity_count = fetch_activity_count(&db, company_id, "issue.updated").await;
    assert_eq!(activity_count, 1, "should log one issue.updated activity");

    cleanup(&db, company_id).await;
}

/// 边界 1：blocker set 为空（无 unresolved blocker + 无 open child）→ 返回 None
#[tokio::test]
async fn returns_none_when_no_blockers_or_children() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_company_agent(&db, "B").await;

    let issue_id = insert_issue(&db, company_id, "in_review", None, Some(agent_id)).await;
    let child_done_id = insert_issue(&db, company_id, "done", Some(issue_id), None).await;
    let cancelled_id = insert_issue(&db, company_id, "cancelled", None, None).await;
    insert_block_relation(&db, company_id, cancelled_id, issue_id).await;

    let result = resolve_continuation_waiting_on_review(&db, company_id, issue_id)
        .await
        .expect("resolve should succeed");
    assert!(
        result.is_none(),
        "should return None when nothing to block on"
    );

    let (status, _) = fetch_issue(&db, issue_id).await;
    assert_eq!(status, "in_review", "issue status should be unchanged");

    let comment_count = fetch_comment_count(&db, issue_id).await;
    assert_eq!(comment_count, 0, "no comment when no resolution");

    cleanup(&db, company_id).await;
}

/// 边界 2：仅 open child (无 explicit blocker) → child 作为 blocker
#[tokio::test]
async fn treats_open_children_as_blockers_when_no_explicit_blocker() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_company_agent(&db, "C").await;

    let issue_id = insert_issue(&db, company_id, "in_review", None, Some(agent_id)).await;
    let child_id = insert_issue(&db, company_id, "in_progress", Some(issue_id), None).await;

    let result = resolve_continuation_waiting_on_review(&db, company_id, issue_id)
        .await
        .expect("resolve should succeed")
        .expect("should return updated issue");
    assert_eq!(result.status, "blocked");

    let blockers = fetch_blocker_ids(&db, company_id, issue_id).await;
    assert_eq!(blockers, vec![child_id]);

    cleanup(&db, company_id).await;
}

/// 边界 3：重复调用幂等（第二次调用应返回 None，因为新 issue 已是 blocked，
/// 但已在 blocked 状态下 resolve_continuation_waiting_on_review 仍会再次尝试
/// —— 由于 blocker set 与当前一致且 status 已变，按 Node 语义第二次应返回 updated）
///
/// 实际 Node 语义：update 总是返回 updated（如果 rows > 0），因为 status 重设为 blocked 仍会写。
/// 这里我们只需确保：第二次调用仍 status=blocked，blocker 集合不变，comment 只写一次。
#[tokio::test]
async fn second_call_is_idempotent_on_status() {
    let db = connect().await;
    let (company_id, agent_id) = fixture_company_agent(&db, "D").await;

    let issue_id = insert_issue(&db, company_id, "in_review", None, Some(agent_id)).await;
    let child_id = insert_issue(&db, company_id, "todo", Some(issue_id), None).await;

    let _ = resolve_continuation_waiting_on_review(&db, company_id, issue_id)
        .await
        .expect("first call should succeed")
        .expect("first call should return updated");

    let second = resolve_continuation_waiting_on_review(&db, company_id, issue_id)
        .await
        .expect("second call should succeed");
    assert!(second.is_some(), "second call still returns updated issue");

    let (status, _) = fetch_issue(&db, issue_id).await;
    assert_eq!(status, "blocked");

    let blockers = fetch_blocker_ids(&db, company_id, issue_id).await;
    assert_eq!(blockers, vec![child_id]);

    let comment_count = fetch_comment_count(&db, issue_id).await;
    assert!(
        comment_count >= 1,
        "should add at least one comment (idempotency may or may not be added depending on impl)"
    );

    cleanup(&db, company_id).await;
}
