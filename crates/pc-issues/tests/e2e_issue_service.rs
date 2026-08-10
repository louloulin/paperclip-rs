//! R602 v1: `pc-issues` 业务层 e2e 测试。
//!
//! 验证：
//! - `IssueService` 构造（new / with_hooks / add_hook）
//! - `count_for_company` / `count_by_status` / `list_by_company`
//! - `get` 公司作用域隔离
//! - `create` 业务校验 + hook 触发
//!
//! 数据库：复用现有 `paperclip_repos` Postgres 实例（不引入新 schema）。

use std::sync::Arc;

use pc_issues::{
    AssignKind, AssignTarget, CommentAuthor, CreateIssueMinimalInput, IssueHook, IssueService,
    NoopIssueHook, RecordingIssueHook,
};
use pc_repos::{
    issue::IssueRepo,
    Db,
};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn setup_db() -> (Db, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(TEST_DATABASE_URL)
        .await
        .expect("connect");
    let db = Db::connect(TEST_DATABASE_URL, 4, 1).await.expect("Db");
    (db, pool)
}

async fn insert_company(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R602-{id}"))
    .bind(format!("A6{}", &id.simple().to_string()[..5]))
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM agents WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM company_memberships WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
}

/// 创建 dummy agent — 解决 issues.assignee_agent_id 的 FK 校验问题。
/// 返回的 agent uuid 可安全用作 assignee_agent_id。
async fn insert_agent(pool: &PgPool, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status,          adapter_config, permissions, created_at, updated_at)          VALUES ($1, $2, $3, 'general', 'process', 'idle', '{}'::jsonb, '{}'::jsonb,          now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("Agent-{id}"))
    .execute(pool)
    .await
    .expect("insert agent");
    id
}

async fn insert_issue(
    pool: &PgPool,
    company_id: Uuid,
    title: &str,
    status: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, created_at, updated_at)          VALUES ($1, $2, $3, $4, 'normal', now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(title)
    .bind(status)
    .execute(pool)
    .await
    .expect("insert issue");
    id
}

#[tokio::test(flavor = "current_thread")]
async fn r602_service_constructors_and_hook_count() {
    let (db, _pool) = setup_db().await;
    let svc = IssueService::new(&db);
    assert_eq!(svc.hook_count(), 0, "default no hooks");

    let noop: Arc<dyn IssueHook> = Arc::new(NoopIssueHook);
    let svc2 = IssueService::with_hooks(&db, vec![noop]);
    assert_eq!(svc2.hook_count(), 1);

    let recorder = Arc::new(RecordingIssueHook::default());
    let svc3 = IssueService::new(&db).add_hook(recorder.clone());
    assert_eq!(svc3.hook_count(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn r602_count_for_company_returns_total() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    insert_issue(&pool, company_id, "a", "todo").await;
    insert_issue(&pool, company_id, "b", "in_progress").await;
    insert_issue(&pool, company_id, "c", "done").await;

    let svc = IssueService::new(&db);
    let count = svc.count_for_company(company_id).await.expect("count");
    assert_eq!(count, 3, "should return total visible issues for company");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_count_by_status_returns_breakdown() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    insert_issue(&pool, company_id, "a", "todo").await;
    insert_issue(&pool, company_id, "b", "todo").await;
    insert_issue(&pool, company_id, "c", "done").await;

    let svc = IssueService::new(&db);
    let counts = svc.count_by_status(company_id).await.expect("status_counts");
    let map: std::collections::HashMap<String, i64> = counts.into_iter().collect();
    assert_eq!(map.get("todo").copied().unwrap_or(0), 2);
    assert_eq!(map.get("done").copied().unwrap_or(0), 1);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_list_by_company_filters_correctly() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_a = insert_company(&pool).await;
    let company_b = insert_company(&pool).await;
    insert_issue(&pool, company_a, "a1", "todo").await;
    insert_issue(&pool, company_a, "a2", "done").await;
    insert_issue(&pool, company_b, "b1", "todo").await;

    let svc = IssueService::new(&db);
    let all_a = svc
        .list_by_company(company_a, None)
        .await
        .expect("list a");
    assert_eq!(all_a.len(), 2);
    assert!(all_a.iter().all(|r| r.company_id == company_a));

    let only_done_a = svc
        .list_by_company(company_a, Some("done"))
        .await
        .expect("list done");
    assert_eq!(only_done_a.len(), 1);
    assert_eq!(only_done_a[0].status, "done");

    cleanup(&pool, company_a).await;
    cleanup(&pool, company_b).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_get_enforces_company_scope() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_a = insert_company(&pool).await;
    let company_b = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_a, "x", "todo").await;

    let svc = IssueService::new(&db);
    // 正确公司应该能看到
    let found = svc.get(company_a, issue_id).await.expect("get a");
    assert!(found.is_some(), "issue visible to its own company");
    // 错误公司不应该看到（即使 id 真实存在）
    let hidden = svc.get(company_b, issue_id).await.expect("get b");
    assert!(hidden.is_none(), "issue not visible across companies");

    cleanup(&pool, company_a).await;
    cleanup(&pool, company_b).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_create_minimal_inputs_and_returns_row() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = IssueService::new(&db);
    let input = CreateIssueMinimalInput {
        title: "First issue".into(),
        description: Some("desc".into()),
        status: Some("todo".into()),
        priority: Some("high".into()),
        created_by_user_id: Some("user-1".into()),
    };
    let row = svc.create(company_id, &input).await.expect("create");
    assert_eq!(row.title, "First issue");
    assert_eq!(row.company_id, company_id);
    assert_eq!(row.status, "todo");
    assert_eq!(row.priority, "high");

    let count = svc.count_for_company(company_id).await.expect("count");
    assert_eq!(count, 1);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_create_fires_on_created_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(RecordingIssueHook::default());
    let svc = IssueService::new(&db).add_hook(recorder.clone());

    let input = CreateIssueMinimalInput {
        title: "Hook test".into(),
        description: None,
        status: None,
        priority: None,
        created_by_user_id: None,
    };
    let row = svc.create(company_id, &input).await.expect("create");
    let recorded = recorder.created.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0], row.id);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_create_rejects_empty_title() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = IssueService::new(&db);
    let input = CreateIssueMinimalInput {
        title: "   ".into(),
        description: None,
        status: None,
        priority: None,
        created_by_user_id: None,
    };
    let err = svc.create(company_id, &input).await.expect_err("rejected");
    assert!(matches!(err, pc_issues::IssueServiceError::InvalidInput(_)));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_create_rejects_invalid_status_and_priority() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = IssueService::new(&db);
    let bad_status = CreateIssueMinimalInput {
        title: "x".into(),
        description: None,
        status: Some("bogus".into()),
        priority: None,
        created_by_user_id: None,
    };
    let err = svc.create(company_id, &bad_status).await.expect_err("rejected");
    assert!(matches!(err, pc_issues::IssueServiceError::InvalidInput(_)));

    let bad_priority = CreateIssueMinimalInput {
        title: "x".into(),
        description: None,
        status: None,
        priority: Some("z9".into()),
        created_by_user_id: None,
    };
    let err = svc.create(company_id, &bad_priority).await.expect_err("rejected");
    assert!(matches!(err, pc_issues::IssueServiceError::InvalidInput(_)));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_get_returns_none_for_unknown_id() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = IssueService::new(&db);
    let maybe = svc.get(company_id, Uuid::new_v4()).await.expect("get");
    assert!(maybe.is_none(), "random uuid should not exist");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_update_status_changes_status_and_triggers_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_id, "lifecycle", "todo").await;

    let recorder = Arc::new(RecordingIssueHook::default());
    let svc = IssueService::new(&db).add_hook(recorder.clone());

    let updated = svc
        .update_status(company_id, issue_id, "in_progress")
        .await
        .expect("status");
    assert_eq!(updated.status, "in_progress");
    assert!(updated.started_at.is_some(), "started_at should be set");

    let logged = recorder.status_changed.lock().unwrap();
    assert_eq!(logged.len(), 1);
    assert_eq!(logged[0].0, issue_id);
    assert_eq!(logged[0].1, "todo");
    assert_eq!(logged[0].2, "in_progress");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_update_status_to_done_sets_completed_at() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_id, "to-done", "in_progress").await;

    let svc = IssueService::new(&db);
    let updated = svc
        .update_status(company_id, issue_id, "done")
        .await
        .expect("status");
    assert_eq!(updated.status, "done");
    assert!(updated.completed_at.is_some(), "completed_at should be set");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_update_status_to_cancelled_sets_cancelled_at() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_id, "to-cancel", "todo").await;

    let svc = IssueService::new(&db);
    let updated = svc
        .update_status(company_id, issue_id, "cancelled")
        .await
        .expect("status");
    assert_eq!(updated.status, "cancelled");
    assert!(updated.cancelled_at.is_some(), "cancelled_at should be set");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_update_status_same_status_is_noop() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_id, "stays-todo", "todo").await;

    let recorder = Arc::new(RecordingIssueHook::default());
    let svc = IssueService::new(&db).add_hook(recorder.clone());

    // 同状态 — 应为 no-op：不写库、不触发 hook
    let returned = svc
        .update_status(company_id, issue_id, "todo")
        .await
        .expect("noop");
    assert_eq!(returned.status, "todo");
    assert_eq!(recorder.status_changed.lock().unwrap().len(), 0);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_update_status_leave_done_clears_terminal_timestamps() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_id, "reopen", "done").await;

    // 先手动写 completed_at
    sqlx::query("UPDATE issues SET completed_at = now() WHERE id = $1")
        .bind(issue_id)
        .execute(&pool)
        .await
        .expect("set completed_at");

    let svc = IssueService::new(&db);
    // done -> todo：清空 completed_at
    let updated = svc
        .update_status(company_id, issue_id, "todo")
        .await
        .expect("reopen");
    assert_eq!(updated.status, "todo");
    assert!(
        updated.completed_at.is_none(),
        "completed_at should be cleared on leave done"
    );

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_update_status_invalid_status_rejected() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_id, "x", "todo").await;

    let svc = IssueService::new(&db);
    let err = svc
        .update_status(company_id, issue_id, "bogus_status")
        .await
        .expect_err("rejected");
    assert!(matches!(err, pc_issues::IssueServiceError::InvalidInput(_)));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_update_status_unknown_issue_returns_not_found() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = IssueService::new(&db);
    let err = svc
        .update_status(company_id, Uuid::new_v4(), "done")
        .await
        .expect_err("not found");
    assert!(matches!(err, pc_issues::IssueServiceError::NotFound(_)));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_update_status_cross_company_returns_not_found() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_a = insert_company(&pool).await;
    let company_b = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_a, "x", "todo").await;

    let svc = IssueService::new(&db);
    // 用错误 company_id — 应 404
    let err = svc
        .update_status(company_b, issue_id, "done")
        .await
        .expect_err("not found cross-company");
    assert!(matches!(err, pc_issues::IssueServiceError::NotFound(_)));

    cleanup(&pool, company_a).await;
    cleanup(&pool, company_b).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_update_status_multiple_hooks_all_called() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_id, "multi", "todo").await;

    let r1 = Arc::new(RecordingIssueHook::default());
    let r2 = Arc::new(RecordingIssueHook::default());
    let svc = IssueService::with_hooks(
        &db,
        vec![r1.clone() as Arc<dyn IssueHook>, r2.clone() as Arc<dyn IssueHook>],
    );
    let _ = svc
        .update_status(company_id, issue_id, "in_progress")
        .await
        .expect("status");
    assert_eq!(r1.status_changed.lock().unwrap().len(), 1);
    assert_eq!(r2.status_changed.lock().unwrap().len(), 1);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_v3_assign_to_agent_sets_assignee_and_triggers_hook() {
    use pc_issues::AssignTarget;

    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_id, "to-assign", "todo").await;

    let recorder = Arc::new(RecordingIssueHook::default());
    let svc = IssueService::new(&db).add_hook(recorder.clone());

    let agent_id = insert_agent(&pool, company_id).await;
    let updated = svc
        .assign(company_id, issue_id, AssignTarget::Agent(agent_id))
        .await
        .expect("assign");
    assert_eq!(updated.assignee_agent_id, Some(agent_id));
    assert_eq!(updated.assignee_user_id, None);

    let logged = recorder.assigned.lock().unwrap();
    assert_eq!(logged.len(), 1);
    assert_eq!(logged[0].0, issue_id);
    assert!(matches!(logged[0].1, AssignKind::Agent(_)));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_v3_assign_to_user_clears_agent() {
    use pc_issues::AssignTarget;

    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_id, "user-assign", "todo").await;

    // 先指派给 agent
    let agent_id = insert_agent(&pool, company_id).await;
    sqlx::query("UPDATE issues SET assignee_agent_id = $1 WHERE id = $2")
        .bind(agent_id)
        .bind(issue_id)
        .execute(&pool)
        .await
        .expect("pre-assign");

    let recorder = Arc::new(RecordingIssueHook::default());
    let svc = IssueService::new(&db).add_hook(recorder.clone());

    let updated = svc
        .assign(company_id, issue_id, AssignTarget::User("alice".to_string()))
        .await
        .expect("user assign");
    assert_eq!(updated.assignee_user_id.as_deref(), Some("alice"));
    assert_eq!(updated.assignee_agent_id, None, "agent must be cleared");

    let logged = recorder.assigned.lock().unwrap();
    assert_eq!(logged.len(), 1);
    assert!(matches!(&logged[0].1, AssignKind::User(n) if n == "alice"));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_v3_unassign_clears_both_fields() {
    use pc_issues::AssignTarget;

    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_id, "to-unassign", "todo").await;
    let agent_id = insert_agent(&pool, company_id).await;
    sqlx::query(
        "UPDATE issues SET assignee_agent_id = $1, assignee_user_id = $2 WHERE id = $3",
    )
    .bind(agent_id)
    .bind("bob")
    .bind(issue_id)
    .execute(&pool)
    .await
    .expect("pre-assign");

    let svc = IssueService::new(&db);
    let updated = svc
        .assign(company_id, issue_id, AssignTarget::Unassign)
        .await
        .expect("unassign");
    assert_eq!(updated.assignee_agent_id, None);
    assert_eq!(updated.assignee_user_id, None);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_v3_same_assignment_is_noop() {
    use pc_issues::AssignTarget;

    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_id, "noop-assign", "todo").await;
    let agent_id = insert_agent(&pool, company_id).await;
    sqlx::query("UPDATE issues SET assignee_agent_id = $1 WHERE id = $2")
        .bind(agent_id)
        .bind(issue_id)
        .execute(&pool)
        .await
        .expect("pre-assign");

    let recorder = Arc::new(RecordingIssueHook::default());
    let svc = IssueService::new(&db).add_hook(recorder.clone());

    // 同样的 agent → no-op
    let returned = svc
        .assign(company_id, issue_id, AssignTarget::Agent(agent_id))
        .await
        .expect("noop");
    assert_eq!(returned.assignee_agent_id, Some(agent_id));
    assert_eq!(recorder.assigned.lock().unwrap().len(), 0);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_v3_assign_unknown_issue_returns_not_found() {
    use pc_issues::AssignTarget;

    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = IssueService::new(&db);
    let err = svc
        .assign(company_id, Uuid::new_v4(), AssignTarget::Agent(Uuid::new_v4()))
        .await
        .expect_err("not found");
    assert!(matches!(err, pc_issues::IssueServiceError::NotFound(_)));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_v3_assign_cross_company_returns_not_found() {
    use pc_issues::AssignTarget;

    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_a = insert_company(&pool).await;
    let company_b = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_a, "x", "todo").await;

    let svc = IssueService::new(&db);
    let err = svc
        .assign(company_b, issue_id, AssignTarget::Agent(Uuid::new_v4()))
        .await
        .expect_err("cross company");
    assert!(matches!(err, pc_issues::IssueServiceError::NotFound(_)));

    cleanup(&pool, company_a).await;
    cleanup(&pool, company_b).await;
}

// ============================================================
// R602 v4 — comments
// ============================================================

#[tokio::test(flavor = "current_thread")]
async fn r602_v4_create_comment_persists_and_fires_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_id, "commentable", "todo").await;
    let agent_id = insert_agent(&pool, company_id).await;

    let recorder = Arc::new(RecordingIssueHook::default());
    let svc = IssueService::new(&db).add_hook(recorder.clone());

    let comment = svc
        .create_comment(company_id, issue_id, CommentAuthor::Agent(agent_id), "first comment")
        .await
        .expect("create comment");
    assert_eq!(comment.body, "first comment");
    assert_eq!(comment.author_agent_id, Some(agent_id));
    assert_eq!(comment.author_user_id, None);
    assert_eq!(comment.issue_id, issue_id);

    let logged = recorder.commented.lock().unwrap();
    assert_eq!(logged.len(), 1);
    assert_eq!(logged[0].0, issue_id);
    assert_eq!(logged[0].1, comment.id);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_v4_list_comments_returns_in_created_order() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_id, "comments-order", "todo").await;
    let agent_id = insert_agent(&pool, company_id).await;

    let svc = IssueService::new(&db);
    svc.create_comment(company_id, issue_id, CommentAuthor::Agent(agent_id), "alpha")
        .await
        .expect("alpha");
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    svc.create_comment(company_id, issue_id, CommentAuthor::Agent(agent_id), "beta")
        .await
        .expect("beta");
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    svc.create_comment(company_id, issue_id, CommentAuthor::User("user-1"), "gamma")
        .await
        .expect("gamma");

    let comments = svc.list_comments(company_id, issue_id).await.expect("list");
    assert_eq!(comments.len(), 3);
    assert_eq!(comments[0].body, "alpha");
    assert_eq!(comments[1].body, "beta");
    assert_eq!(comments[2].body, "gamma");
    assert_eq!(comments[2].author_user_id.as_deref(), Some("user-1"));
    assert_eq!(comments[2].author_agent_id, None);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_v4_list_comments_empty_for_new_issue() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_id, "no-comments", "todo").await;

    let svc = IssueService::new(&db);
    let comments = svc.list_comments(company_id, issue_id).await.expect("list");
    assert!(comments.is_empty());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_v4_create_comment_empty_body_rejected() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_id, "empty-comment", "todo").await;
    let agent_id = insert_agent(&pool, company_id).await;

    let svc = IssueService::new(&db);
    let err = svc
        .create_comment(company_id, issue_id, CommentAuthor::Agent(agent_id), "   ")
        .await
        .expect_err("rejected");
    assert!(matches!(err, pc_issues::IssueServiceError::InvalidInput(_)));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_v4_create_comment_unknown_issue_returns_not_found() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = IssueService::new(&db);
    let err = svc
        .create_comment(company_id, Uuid::new_v4(), CommentAuthor::User("u1"), "x")
        .await
        .expect_err("not found");
    assert!(matches!(err, pc_issues::IssueServiceError::NotFound(_)));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_v4_create_comment_cross_company_returns_not_found() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_a = insert_company(&pool).await;
    let company_b = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_a, "x", "todo").await;

    let svc = IssueService::new(&db);
    let err = svc
        .create_comment(company_b, issue_id, CommentAuthor::User("u1"), "x")
        .await
        .expect_err("cross company");
    assert!(matches!(err, pc_issues::IssueServiceError::NotFound(_)));

    cleanup(&pool, company_a).await;
    cleanup(&pool, company_b).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_v4_list_comments_cross_company_returns_not_found() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_a = insert_company(&pool).await;
    let company_b = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_a, "x", "todo").await;

    let svc = IssueService::new(&db);
    let err = svc
        .list_comments(company_b, issue_id)
        .await
        .expect_err("cross company list");
    assert!(matches!(err, pc_issues::IssueServiceError::NotFound(_)));

    cleanup(&pool, company_a).await;
    cleanup(&pool, company_b).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_v4_create_comment_multiple_hooks_all_called() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_id, "multi-hook", "todo").await;
    let agent_id = insert_agent(&pool, company_id).await;

    let r1 = Arc::new(RecordingIssueHook::default());
    let r2 = Arc::new(RecordingIssueHook::default());
    let svc = IssueService::with_hooks(
        &db,
        vec![r1.clone() as Arc<dyn IssueHook>, r2.clone() as Arc<dyn IssueHook>],
    );
    let _ = svc
        .create_comment(company_id, issue_id, CommentAuthor::Agent(agent_id), "hi")
        .await
        .expect("comment");
    assert_eq!(r1.commented.lock().unwrap().len(), 1);
    assert_eq!(r2.commented.lock().unwrap().len(), 1);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_create_with_default_status_when_omitted() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = IssueService::new(&db);
    let input = CreateIssueMinimalInput {
        title: "no-status".into(),
        description: None,
        status: None,
        priority: None,
        created_by_user_id: None,
    };
    let row = svc.create(company_id, &input).await.expect("create");
    // repo 层默认 status = "todo"
    assert_eq!(row.status, "todo");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_get_via_repo_returns_full_row_for_completeness() {
    // sanity: IssueRepo::get 不带 company scope — service 层是唯一 scope gate
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let _ = insert_issue(&pool, company_id, "raw", "todo").await;
    let raw_repo = IssueRepo::new(&db);
    let svc = IssueService::new(&db);
    let direct = raw_repo
        .list_by_company(company_id, None)
        .await
        .expect("repo list");
    let via_service = svc.list_by_company(company_id, None).await.expect("svc list");
    assert_eq!(direct.len(), via_service.len());
    cleanup(&pool, company_id).await;
    let _ = json!({});
}
