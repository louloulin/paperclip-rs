//! Round 323：`findOpenStrandedIssueRecoveryIssue` + 相关 helpers 的 PostgreSQL 验证。
//!
//! 与 Node `services/recovery/service.ts` 对齐：
//! - `isStrandedIssueRecoveryIssue(issue)` —— origin_kind == "stranded_issue_recovery"
//! - `findOpenStrandedIssueRecoveryIssue(company_id, source_issue_id)` —— 查 open recovery
//!   issue (origin_id = source, origin_kind = stranded_issue_recovery, hidden_at IS NULL,
//!   status NOT IN done/cancelled) — 最多一条，按 created_at DESC
//! - `isUniqueStrandedIssueRecoveryConflict(error)` —— sqlx::Error 来自 PG 23505 + 约束名
//!   `issues_active_stranded_issue_recovery_uq`

use pc_heartbeat::recovery::stranded_issue_recovery_queries::{
    find_open_stranded_issue_recovery_issue, is_stranded_issue_recovery_issue,
};
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";
const STRANDED_ISSUE_RECOVERY_ORIGIN_KIND: &str = "stranded_issue_recovery";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn cleanup(db: &Db, company_id: Uuid) {
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

async fn fixture(db: &Db) -> (Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let agent_id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind(format!("r323-{company_id}"))
        .bind(format!("R{}", &company_id.simple().to_string()[..8]))
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status) \
         VALUES ($1, $2, 'r323-agent', 'general', 'process', 'active')",
    )
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
    assignee: Option<Uuid>,
    status: &str,
    origin_kind: &str,
    origin_id: Option<Uuid>,
    hidden_at: Option<&str>,
) -> Uuid {
    let issue_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, \
         origin_fingerprint, origin_id, assignee_agent_id, hidden_at, execution_policy) \
         VALUES ($1, $2, $3, $4, 'normal', $5, $6, $7, $8, \
         CASE WHEN $9::text IS NULL THEN NULL ELSE $9::timestamp END, $10)",
    )
    .bind(issue_id)
    .bind(company_id)
    .bind(format!("r323-issue-{issue_id}"))
    .bind(status)
    .bind(origin_kind)
    .bind(format!("r323-fp-{issue_id}"))
    .bind(origin_id)
    .bind(assignee)
    .bind(hidden_at)
    .bind(json!({"mode":"normal","commentRequired":false,"stages":[]}))
    .execute(db.pool())
    .await
    .unwrap();
    issue_id
}

async fn fetch_issue_row(db: &Db, issue_id: Uuid) -> pc_repos::issue::IssueRow {
    use pc_repos::issue::IssueRepo;
    IssueRepo::new(db)
        .get(issue_id)
        .await
        .unwrap()
        .expect("issue should exist")
}

/// `is_stranded_issue_recovery_issue`：origin_kind 匹配返回 true，否则 false
#[tokio::test]
async fn is_stranded_issue_recovery_issue_returns_true_when_origin_kind_matches() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(
        &db,
        company_id,
        Some(agent_id),
        "todo",
        STRANDED_ISSUE_RECOVERY_ORIGIN_KIND,
        Some(Uuid::new_v4()),
        None,
    )
    .await;

    let row = fetch_issue_row(&db, issue_id).await;
    assert!(is_stranded_issue_recovery_issue(&row));

    cleanup(&db, company_id).await;
}

#[tokio::test]
async fn is_stranded_issue_recovery_issue_returns_false_for_other_origins() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, Some(agent_id), "todo", "user", None, None).await;
    let row = fetch_issue_row(&db, issue_id).await;
    assert!(!is_stranded_issue_recovery_issue(&row));

    let system_issue_id = insert_issue(
        &db,
        company_id,
        Some(agent_id),
        "todo",
        "system",
        None,
        None,
    )
    .await;
    let system_row = fetch_issue_row(&db, system_issue_id).await;
    assert!(!is_stranded_issue_recovery_issue(&system_row));

    cleanup(&db, company_id).await;
}

/// `find_open_stranded_issue_recovery_issue`：找到 active 的 stranded_issue_recovery
#[tokio::test]
async fn find_open_returns_active_recovery_issue() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let source_id = Uuid::new_v4();

    // 创建一个匹配条件的 recovery issue
    let recovery_id = insert_issue(
        &db,
        company_id,
        Some(agent_id),
        "todo",
        STRANDED_ISSUE_RECOVERY_ORIGIN_KIND,
        Some(source_id),
        None,
    )
    .await;

    let result = find_open_stranded_issue_recovery_issue(&db, company_id, source_id)
        .await
        .expect("query should succeed");
    let row = result.expect("should find the recovery issue");
    assert_eq!(row.id, recovery_id);
    assert_eq!(row.origin_kind, STRANDED_ISSUE_RECOVERY_ORIGIN_KIND);
    assert_eq!(row.origin_id, Some(source_id.to_string()));

    cleanup(&db, company_id).await;
}

/// 不存在 → 返回 Ok(None)
#[tokio::test]
async fn find_open_returns_none_when_no_recovery_issue() {
    let db = connect().await;
    let (company_id, _agent_id) = fixture(&db).await;
    let nonexistent_source = Uuid::new_v4();

    let result = find_open_stranded_issue_recovery_issue(&db, company_id, nonexistent_source)
        .await
        .expect("query should succeed");
    assert!(result.is_none());

    cleanup(&db, company_id).await;
}

/// done / cancelled / hidden 的 recovery issue 不算 open
///
/// 由于 PG unique index 只对 active 行生效，这三个状态不同的 issue 在不同 source 下插入，
/// 然后合并查询同一 source，验证它们都不被返回。
#[tokio::test]
async fn find_open_excludes_done_and_cancelled_recovery_issues() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let source_id_a = Uuid::new_v4();
    let source_id_b = Uuid::new_v4();
    let source_id_c = Uuid::new_v4();

    // done status (active when inserted, but UPDATE to done before query)
    let done_id = insert_issue(
        &db,
        company_id,
        Some(agent_id),
        "todo",
        STRANDED_ISSUE_RECOVERY_ORIGIN_KIND,
        Some(source_id_a),
        None,
    )
    .await;
    sqlx::query("UPDATE issues SET status = 'done', completed_at = now() WHERE id = $1")
        .bind(done_id)
        .execute(db.pool())
        .await
        .unwrap();
    // cancelled status
    let cancelled_id = insert_issue(
        &db,
        company_id,
        Some(agent_id),
        "todo",
        STRANDED_ISSUE_RECOVERY_ORIGIN_KIND,
        Some(source_id_b),
        None,
    )
    .await;
    sqlx::query("UPDATE issues SET status = 'cancelled', cancelled_at = now() WHERE id = $1")
        .bind(cancelled_id)
        .execute(db.pool())
        .await
        .unwrap();
    // hidden 状态
    let hidden_id = insert_issue(
        &db,
        company_id,
        Some(agent_id),
        "todo",
        STRANDED_ISSUE_RECOVERY_ORIGIN_KIND,
        Some(source_id_c),
        Some("2024-01-01 00:00:00"),
    )
    .await;

    // 三个 source 都查不到（因为都不是 open）
    for sid in [source_id_a, source_id_b, source_id_c] {
        let result = find_open_stranded_issue_recovery_issue(&db, company_id, sid)
            .await
            .expect("query should succeed");
        assert!(
            result.is_none(),
            "source {sid} should not have an open recovery issue"
        );
    }

    // 校验三个都还在表中（只是不 open）
    assert_eq!(fetch_issue_row(&db, done_id).await.id, done_id);
    assert_eq!(fetch_issue_row(&db, cancelled_id).await.id, cancelled_id);
    assert_eq!(fetch_issue_row(&db, hidden_id).await.id, hidden_id);

    cleanup(&db, company_id).await;
}

/// 同 source 下多个 recovery issue（前面已 done/cancelled）→ 返回最新的 open（created_at DESC）
///
/// 由于 PG unique index `issues_active_stranded_issue_recovery_uq` 只对 active 行加约束，
/// 我们模拟"旧 recovery 已 done 后新建第二个"的场景。
#[tokio::test]
async fn find_open_returns_most_recent_when_multiple_open() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let source_id = Uuid::new_v4();

    // 第一个 recovery
    let first_id = insert_issue(
        &db,
        company_id,
        Some(agent_id),
        "todo",
        STRANDED_ISSUE_RECOVERY_ORIGIN_KIND,
        Some(source_id),
        None,
    )
    .await;
    // 第一个 done（移出 unique 索引范围）
    sqlx::query("UPDATE issues SET status = 'done', completed_at = now() WHERE id = $1")
        .bind(first_id)
        .execute(db.pool())
        .await
        .unwrap();
    // 等几毫秒确保 created_at 不同
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    // 第二个 recovery（现在允许 active）
    let second_id = insert_issue(
        &db,
        company_id,
        Some(agent_id),
        "in_progress",
        STRANDED_ISSUE_RECOVERY_ORIGIN_KIND,
        Some(source_id),
        None,
    )
    .await;

    let result = find_open_stranded_issue_recovery_issue(&db, company_id, source_id)
        .await
        .expect("query should succeed");
    let row = result.expect("should find the recovery issue");
    assert_eq!(
        row.id, second_id,
        "should return the most recently created one"
    );
    assert_ne!(row.id, first_id);

    cleanup(&db, company_id).await;
}

/// 不同 source 的 recovery 不会被匹配
#[tokio::test]
async fn find_open_does_not_match_other_source_recovery_issues() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let other_source = Uuid::new_v4();
    let target_source = Uuid::new_v4();

    insert_issue(
        &db,
        company_id,
        Some(agent_id),
        "todo",
        STRANDED_ISSUE_RECOVERY_ORIGIN_KIND,
        Some(other_source),
        None,
    )
    .await;

    let result = find_open_stranded_issue_recovery_issue(&db, company_id, target_source)
        .await
        .expect("query should succeed");
    assert!(result.is_none(), "should not find other source's recovery");

    cleanup(&db, company_id).await;
}

/// origin_kind 不为 stranded_issue_recovery 的 issue 即使 origin_id 匹配也不返回
#[tokio::test]
async fn find_open_does_not_match_other_origin_kinds() {
    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let source_id = Uuid::new_v4();

    // origin_kind=user 但 origin_id = source_id
    insert_issue(
        &db,
        company_id,
        Some(agent_id),
        "todo",
        "user",
        Some(source_id),
        None,
    )
    .await;

    let result = find_open_stranded_issue_recovery_issue(&db, company_id, source_id)
        .await
        .expect("query should succeed");
    assert!(
        result.is_none(),
        "user origin_kind should not match stranded_issue_recovery"
    );

    cleanup(&db, company_id).await;
}

/// 真实 PG 唯一冲突 → `is_unique_stranded_issue_recovery_conflict` 返回 true
///
/// 通过尝试插入两个 active stranded_issue_recovery 触发 PG 23505 唯一冲突。
#[tokio::test]
async fn real_pg_unique_conflict_is_recognized() {
    use pc_heartbeat::recovery::is_unique_stranded_issue_recovery_conflict;

    let db = connect().await;
    let (company_id, agent_id) = fixture(&db).await;
    let source_id = Uuid::new_v4();

    // 第一个 active recovery（用 SQL 直接 INSERT 以保证不被 helper unwrap）
    let first_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, \
         origin_fingerprint, origin_id, assignee_agent_id, execution_policy) \
         VALUES ($1, $2, $3, 'todo', 'normal', $4, $5, $6, $7, $8)",
    )
    .bind(first_id)
    .bind(company_id)
    .bind(format!("r323-recovery-{first_id}"))
    .bind(STRANDED_ISSUE_RECOVERY_ORIGIN_KIND)
    .bind(format!("r323-fp-{first_id}"))
    .bind(source_id.to_string())
    .bind(agent_id)
    .bind(json!({"mode":"normal","commentRequired":false,"stages":[]}))
    .execute(db.pool())
    .await
    .expect("first insert should succeed");

    // 第二个 active recovery（同 source）应触发 23505 冲突
    let second_id = Uuid::new_v4();
    let result = sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, \
         origin_fingerprint, origin_id, assignee_agent_id, execution_policy) \
         VALUES ($1, $2, $3, 'todo', 'normal', $4, $5, $6, $7, $8)",
    )
    .bind(second_id)
    .bind(company_id)
    .bind(format!("r323-recovery-{second_id}"))
    .bind(STRANDED_ISSUE_RECOVERY_ORIGIN_KIND)
    .bind(format!("r323-fp-{second_id}"))
    .bind(source_id.to_string())
    .bind(agent_id)
    .bind(json!({"mode":"normal","commentRequired":false,"stages":[]}))
    .execute(db.pool())
    .await;

    let err = result.expect_err("expected unique conflict on second insert");
    assert!(
        is_unique_stranded_issue_recovery_conflict(&err),
        "expected recognized unique conflict, got: {err}"
    );

    cleanup(&db, company_id).await;
}
