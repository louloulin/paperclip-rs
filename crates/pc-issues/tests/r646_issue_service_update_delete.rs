//! R646: IssueService update/delete/update_comment/remove_comment 端到端测试。

use std::sync::Arc;

use pc_issues::{
    CommentAuthor, CreateIssueMinimalInput, IssueService, IssueUpdatePatch,
    RecordingIssueHook,
};
use pc_repos::Db;
use sqlx::PgPool;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn try_setup_pool() -> Option<(Db, PgPool)> {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(TEST_DATABASE_URL)
        .await
        .ok()?;
    let db = Db::connect(TEST_DATABASE_URL, 2, 1).await.ok()?;
    Some((db, pool))
}

async fn insert_company(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    let unique = id.simple().to_string();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at)          VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R646-{unique}"))
    .bind(format!("I{}", &unique[..5]))
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM issue_comments WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
}

async fn make_issue(db: &Db, company_id: Uuid, title: &str) -> Uuid {
    let svc = IssueService::new(db);
    let row = svc
        .create(
            company_id,
            &CreateIssueMinimalInput {
                title: title.into(),
                description: None,
                status: Some("todo".into()),
                priority: Some("normal".into()),
                created_by_user_id: Some("r646-user".into()),
            },
        )
        .await
        .expect("create issue");
    row.id
}

#[tokio::test(flavor = "current_thread")]
async fn r646_update_changes_fields_and_triggers_on_updated_hook() {
    let (db, pool) = match try_setup_pool().await {
        Some(x) => x,
        None => {
            eprintln!("[skip] postgres unreachable");
            return;
        }
    };
    let company_id = insert_company(&pool).await;
    let issue_id = make_issue(&db, company_id, "Original title").await;

    let hook = Arc::new(RecordingIssueHook::default());
    let svc = IssueService::with_hooks(&db, vec![hook.clone()]);
    let patch = IssueUpdatePatch {
        title: Some("Updated title".into()),
        description: Some(Some("new body".into())),
        status: None,
        priority: Some("high".into()),
        assignee_agent_id: None,
    };
    let updated = svc
        .update(company_id, issue_id, patch)
        .await
        .expect("update")
        .expect("row exists");
    assert_eq!(updated.title, "Updated title");
    assert_eq!(updated.description.as_deref(), Some("new body"));
    assert_eq!(updated.priority, "high");

    // hook on_updated 应该被触发
    assert_eq!(hook.updated.lock().unwrap().len(), 1);
    assert_eq!(hook.updated.lock().unwrap()[0].0, issue_id);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r646_update_with_status_runs_status_machine_and_triggers_on_status_changed() {
    let (db, pool) = match try_setup_pool().await {
        Some(x) => x,
        None => {
            eprintln!("[skip] postgres unreachable");
            return;
        }
    };
    let company_id = insert_company(&pool).await;
    let issue_id = make_issue(&db, company_id, "Status transition test").await;

    let hook = Arc::new(RecordingIssueHook::default());
    let svc = IssueService::with_hooks(&db, vec![hook.clone()]);
    let patch = IssueUpdatePatch {
        title: None,
        description: None,
        status: Some("in_progress".into()),
        priority: None,
        assignee_agent_id: None,
    };
    let updated = svc.update(company_id, issue_id, patch).await.expect("update").unwrap();
    assert_eq!(updated.status, "in_progress");
    assert!(updated.started_at.is_some(), "started_at should be set");

    // on_status_changed 由 update_status 内部触发
    assert_eq!(hook.status_changed.lock().unwrap().len(), 1);
    // on_updated 也应触发
    assert_eq!(hook.updated.lock().unwrap().len(), 1);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r646_delete_removes_issue_and_triggers_on_deleted() {
    let (db, pool) = match try_setup_pool().await {
        Some(x) => x,
        None => return,
    };
    let company_id = insert_company(&pool).await;
    let issue_id = make_issue(&db, company_id, "To delete").await;

    let hook = Arc::new(RecordingIssueHook::default());
    let svc = IssueService::with_hooks(&db, vec![hook.clone()]);
    let ok = svc.delete(company_id, issue_id).await.expect("delete");
    assert!(ok);
    assert_eq!(hook.deleted.lock().unwrap().as_slice(), &[issue_id]);

    // 二次 delete 返回 false
    let ok2 = svc.delete(company_id, issue_id).await.expect("delete again");
    assert!(!ok2);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r646_update_comment_changes_body_and_triggers_on_comment_updated() {
    let (db, pool) = match try_setup_pool().await {
        Some(x) => x,
        None => return,
    };
    let company_id = insert_company(&pool).await;
    let issue_id = make_issue(&db, company_id, "Comment test").await;
    let hook = Arc::new(RecordingIssueHook::default());
    let svc = IssueService::with_hooks(&db, vec![hook.clone()]);

    let comment = svc
        .create_comment(
            company_id,
            issue_id,
            CommentAuthor::User("alice"),
            "Original comment",
        )
        .await
        .expect("create_comment");
    assert_eq!(hook.commented.lock().unwrap().len(), 1);

    let updated_comment = svc
        .update_comment(company_id, issue_id, comment.id, "Edited comment")
        .await
        .expect("update_comment")
        .expect("comment exists");
    assert_eq!(updated_comment.body, "Edited comment");
    assert_eq!(hook.comment_updated.lock().unwrap().len(), 1);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r646_remove_comment_deletes_and_triggers_on_comment_removed() {
    let (db, pool) = match try_setup_pool().await {
        Some(x) => x,
        None => return,
    };
    let company_id = insert_company(&pool).await;
    let issue_id = make_issue(&db, company_id, "Comment delete test").await;
    let hook = Arc::new(RecordingIssueHook::default());
    let svc = IssueService::with_hooks(&db, vec![hook.clone()]);

    let comment = svc
        .create_comment(
            company_id,
            issue_id,
            CommentAuthor::User("alice"),
            "To remove",
        )
        .await
        .expect("create_comment");
    let ok = svc.remove_comment(company_id, issue_id, comment.id).await.expect("remove");
    assert!(ok);
    assert_eq!(hook.comment_removed.lock().unwrap().len(), 1);

    // 二次 remove 返回 false
    let ok2 = svc.remove_comment(company_id, issue_id, comment.id).await.expect("remove again");
    assert!(!ok2);

    cleanup(&pool, company_id).await;
}
