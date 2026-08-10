//! R608: `pc-documents` 业务层 e2e 测试。
//!
//! 验证：
//! - `DocumentService` 构造（new / with_hooks / add_hook）
//! - `create` 业务校验：format 校验 / body 非空 / company_id 必填
//! - `update` body 写新 revision / locked 文档拒改 / format 校验
//! - `delete` hook Deleted + locked 拒删
//! - `list_revisions` 排序 + `restore_revision` 创建新 revision
//! - `lock_document` / `unlock_document` 状态机
//! - `create_annotation_thread` 校验（normalized range + markdown range）
//! - `resolve_annotation_thread` 单向 transition
//! - `create_annotation_comment` 校验 author_type
//! - `upsert_issue_document` insert + update 路径
//! - 跨 company 隔离
//!
//! 数据库：复用现有 `paperclip_repos` Postgres 实例。

use std::sync::Arc;

use pc_documents::{
    CreateAnnotationComment, CreateAnnotationThreadInput, CreateDocument, DocumentHookEvent,
    DocumentPatch, DocumentService, RecordingDocumentHook, UpsertIssueDocument,
};
use pc_repos::Db;
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
    let prefix = format!("D{}", Uuid::new_v4().simple().to_string().chars().take(5).collect::<String>());
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at)          VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R608-{id}"))
    .bind(prefix)
    .execute(pool)
    .await
    .expect("insert company");
    id
}

/// Insert an issue used as FK target for annotation_threads / issue_documents.
async fn insert_issue(pool: &PgPool, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, created_at, updated_at)          VALUES ($1, $2, 'doc-test-issue', 'todo', 'normal', now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .execute(pool)
    .await
    .expect("insert issue");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    // 删除 annotations + threads + revisions + documents（按 FK 顺序）
    let _ = sqlx::query("DELETE FROM document_annotation_comments WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM document_annotation_threads WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM issue_documents WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM document_revisions WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM documents WHERE company_id = $1")
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

#[tokio::test(flavor = "current_thread")]
async fn r608_service_constructors() {
    let (db, _pool) = setup_db().await;
    let _svc = DocumentService::new(db.clone());
    let _svc2 = DocumentService::with_hooks(db.clone(), vec![]);
    let recorder = Arc::new(RecordingDocumentHook::default());
    let _svc3 = DocumentService::new(db).add_hook(recorder);
}

#[tokio::test(flavor = "current_thread")]
async fn r608_create_dispatches_created() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(RecordingDocumentHook::default());
    let svc = DocumentService::new(db).add_hook(recorder.clone());
    let row = svc
        .create(CreateDocument {
            company_id,
            title: Some("Spec".into()),
            format: None,
            body: "# Title

Hello.".into(),
            created_by_user_id: Some("u1".into()),
            created_by_agent_id: None,
        })
        .await
        .expect("create");
    assert_eq!(row.title.as_deref(), Some("Spec"));
    assert_eq!(row.format, "markdown");
    assert_eq!(row.latest_revision_number, 1);

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    match &events[0] {
        DocumentHookEvent::Created { id, .. } => assert_eq!(*id, row.id),
        other => panic!("expected Created, got {other:?}"),
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r608_create_rejects_empty_body() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = DocumentService::new(db);
    let err = svc
        .create(CreateDocument {
            company_id,
            title: None,
            format: None,
            body: String::new(),
            created_by_user_id: Some("u".into()),
            created_by_agent_id: None,
        })
        .await
        .expect_err("empty body");
    assert!(matches!(err, pc_errors::Error::Validation { .. }));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r608_create_rejects_bad_format() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = DocumentService::new(db);
    let err = svc
        .create(CreateDocument {
            company_id,
            title: None,
            format: Some("xml".into()),
            body: "x".into(),
            created_by_user_id: Some("u".into()),
            created_by_agent_id: None,
        })
        .await
        .expect_err("bad format");
    assert!(matches!(err, pc_errors::Error::Validation { .. }));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r608_update_appends_revision() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = DocumentService::new(db);
    let row = svc
        .create(CreateDocument {
            company_id,
            title: Some("v1".into()),
            format: None,
            body: "first".into(),
            created_by_user_id: Some("u".into()),
            created_by_agent_id: None,
        })
        .await
        .expect("create");

    let updated = svc
        .update(
            company_id,
            row.id,
            DocumentPatch {
                title: Some("v2".into()),
                body: Some("second".into()),
                ..Default::default()
            },
        )
        .await
        .expect("update")
        .expect("some");
    assert_eq!(updated.title.as_deref(), Some("v2"));
    assert_eq!(updated.latest_body, "second");
    assert_eq!(updated.latest_revision_number, 2, "should create rev 2");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r608_update_rejects_locked_document() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = DocumentService::new(db);
    let row = svc
        .create(CreateDocument {
            company_id,
            title: None,
            format: None,
            body: "x".into(),
            created_by_user_id: Some("u".into()),
            created_by_agent_id: None,
        })
        .await
        .expect("create");
    svc.lock_document(company_id, row.id, None, Some("editor"))
        .await
        .expect("lock");

    let err = svc
        .update(
            company_id,
            row.id,
            DocumentPatch {
                body: Some("new".into()),
                ..Default::default()
            },
        )
        .await
        .expect_err("locked");
    assert!(matches!(err, pc_errors::Error::Forbidden { .. }));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r608_delete_dispatches_deleted() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(RecordingDocumentHook::default());
    let svc = DocumentService::new(db).add_hook(recorder.clone());
    let row = svc
        .create(CreateDocument {
            company_id,
            title: None,
            format: None,
            body: "x".into(),
            created_by_user_id: Some("u".into()),
            created_by_agent_id: None,
        })
        .await
        .expect("create");

    recorder.clear();
    let removed = svc.delete(company_id, row.id).await.expect("delete");
    assert!(removed);

    let events = recorder.events_snapshot();
    assert!(matches!(events[0], DocumentHookEvent::Deleted { .. }));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r608_list_revisions_returns_descending() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = DocumentService::new(db);
    let row = svc
        .create(CreateDocument {
            company_id,
            title: None,
            format: None,
            body: "v1".into(),
            created_by_user_id: Some("u".into()),
            created_by_agent_id: None,
        })
        .await
        .expect("create");
    svc.update(
        company_id,
        row.id,
        DocumentPatch {
            body: Some("v2".into()),
            ..Default::default()
        },
    )
    .await
    .expect("update");

    let revs = svc.list_revisions(row.id).await.expect("list revs");
    assert_eq!(revs.len(), 2);
    assert_eq!(revs[0].revision_number, 2, "DESC: rev 2 first");
    assert_eq!(revs[1].revision_number, 1);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r608_restore_revision_creates_new_revision() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(RecordingDocumentHook::default());
    let svc = DocumentService::new(db).add_hook(recorder.clone());
    let row = svc
        .create(CreateDocument {
            company_id,
            title: None,
            format: None,
            body: "first".into(),
            created_by_user_id: Some("u".into()),
            created_by_agent_id: None,
        })
        .await
        .expect("create");
    svc.update(
        company_id,
        row.id,
        DocumentPatch {
            body: Some("second".into()),
            ..Default::default()
        },
    )
    .await
    .expect("update");

    recorder.clear();
    let new_rev = svc
        .restore_revision(company_id, row.id, 1, Some("u"))
        .await
        .expect("restore")
        .expect("some");
    assert_eq!(new_rev.revision_number, 3, "create rev 3 from rev 1");
    assert_eq!(new_rev.body, "first", "restored body matches rev 1");

    let after = svc.get_in_company(company_id, row.id).await.expect("get").expect("some");
    assert_eq!(after.latest_body, "first");
    assert_eq!(after.latest_revision_number, 3);

    let events = recorder.events_snapshot();
    assert!(matches!(
        events[0],
        DocumentHookEvent::RevisionRestored { .. }
    ));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r608_lock_and_unlock_dispatch_events() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(RecordingDocumentHook::default());
    let svc = DocumentService::new(db).add_hook(recorder.clone());
    let row = svc
        .create(CreateDocument {
            company_id,
            title: None,
            format: None,
            body: "x".into(),
            created_by_user_id: Some("u".into()),
            created_by_agent_id: None,
        })
        .await
        .expect("create");

    recorder.clear();
    svc.lock_document(company_id, row.id, None, Some("editor"))
        .await
        .expect("lock")
        .expect("some");
    let events = recorder.events_snapshot();
    assert!(matches!(events[0], DocumentHookEvent::Locked { .. }));

    recorder.clear();
    svc.unlock_document(company_id, row.id)
        .await
        .expect("unlock")
        .expect("some");
    let events = recorder.events_snapshot();
    assert!(matches!(events[0], DocumentHookEvent::Unlocked { .. }));

    // double-unlock is idempotent (returns row)
    svc.unlock_document(company_id, row.id)
        .await
        .expect("unlock idempotent")
        .expect("some");

    // double-lock is conflict
    svc.lock_document(company_id, row.id, None, Some("e1"))
        .await
        .expect("lock 1");
    let err = svc
        .lock_document(company_id, row.id, None, Some("e2"))
        .await
        .expect_err("lock 2 conflict");
    assert!(matches!(err, pc_errors::Error::Conflict { .. }));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r608_annotation_thread_lifecycle() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_id).await;

    let svc = DocumentService::new(db);
    let doc = svc
        .create(CreateDocument {
            company_id,
            title: None,
            format: None,
            body: "the quick brown fox jumps".into(),
            created_by_user_id: Some("u".into()),
            created_by_agent_id: None,
        })
        .await
        .expect("doc");

    let thread = svc
        .create_annotation_thread(CreateAnnotationThreadInput {
            company_id,
            issue_id,
            document_id: doc.id,
            document_key: "spec".into(),
            selected_text: "brown fox".into(),
            prefix_text: "quick ".into(),
            suffix_text: " jumps".into(),
            normalized_start: 10,
            normalized_end: 19,
            markdown_start: 10,
            markdown_end: 19,
            anchor_confidence: Some("high".into()),
            anchor_selector: Some(json!({"type": "text"})),
            created_by_user_id: Some("u".into()),
            created_by_agent_id: None,
        })
        .await
        .expect("thread");
    assert_eq!(thread.status, "open");

    let threads = svc
        .list_annotation_threads(doc.id, "spec")
        .await
        .expect("list");
    assert_eq!(threads.len(), 1);

    let resolved = svc
        .resolve_annotation_thread(thread.id, Some("u"))
        .await
        .expect("resolve")
        .expect("some");
    assert_eq!(resolved.status, "resolved");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r608_annotation_thread_validates_range() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_id).await;

    let svc = DocumentService::new(db);
    let doc = svc
        .create(CreateDocument {
            company_id,
            title: None,
            format: None,
            body: "x".into(),
            created_by_user_id: Some("u".into()),
            created_by_agent_id: None,
        })
        .await
        .expect("doc");

    let err = svc
        .create_annotation_thread(CreateAnnotationThreadInput {
            company_id,
            issue_id,
            document_id: doc.id,
            document_key: "k".into(),
            selected_text: "s".into(),
            prefix_text: "p".into(),
            suffix_text: "s".into(),
            normalized_start: 10,
            normalized_end: 5,
            markdown_start: 0,
            markdown_end: 5,
            anchor_confidence: None,
            anchor_selector: None,
            created_by_user_id: Some("u".into()),
            created_by_agent_id: None,
        })
        .await
        .expect_err("normalizedEnd < normalizedStart");
    assert!(matches!(err, pc_errors::Error::Unprocessable { .. }));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r608_annotation_comment_validates_author_type() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_id).await;

    let svc = DocumentService::new(db);
    let doc = svc
        .create(CreateDocument {
            company_id,
            title: None,
            format: None,
            body: "x".into(),
            created_by_user_id: Some("u".into()),
            created_by_agent_id: None,
        })
        .await
        .expect("doc");
    let thread = svc
        .create_annotation_thread(CreateAnnotationThreadInput {
            company_id,
            issue_id,
            document_id: doc.id,
            document_key: "k".into(),
            selected_text: "s".into(),
            prefix_text: "p".into(),
            suffix_text: "s".into(),
            normalized_start: 0,
            normalized_end: 1,
            markdown_start: 0,
            markdown_end: 1,
            anchor_confidence: None,
            anchor_selector: None,
            created_by_user_id: Some("u".into()),
            created_by_agent_id: None,
        })
        .await
        .expect("thread");

    let err = svc
        .create_annotation_comment(CreateAnnotationComment {
            company_id,
            thread_id: thread.id,
            issue_id,
            document_id: doc.id,
            body: "hi".into(),
            author_type: "robot".into(),
            author_user_id: None,
            author_agent_id: None,
        })
        .await
        .expect_err("bad author_type");
    assert!(matches!(err, pc_errors::Error::Validation { .. }));

    let comment = svc
        .create_annotation_comment(CreateAnnotationComment {
            company_id,
            thread_id: thread.id,
            issue_id,
            document_id: doc.id,
            body: "first comment".into(),
            author_type: "user".into(),
            author_user_id: Some("u".into()),
            author_agent_id: None,
        })
        .await
        .expect("create comment");
    assert_eq!(comment.body, "first comment");

    let comments = svc
        .list_annotation_comments(thread.id)
        .await
        .expect("list");
    assert_eq!(comments.len(), 1);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r608_upsert_issue_document_insert_and_update() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_id).await;

    let svc = DocumentService::new(db);
    let inserted = svc
        .upsert_issue_document(UpsertIssueDocument {
            company_id,
            issue_id,
            key: "plan".into(),
            title: Some("Plan".into()),
            body: "v1 plan body".into(),
            format: None,
            created_by_user_id: Some("u".into()),
            created_by_agent_id: None,
        })
        .await
        .expect("upsert insert");
    assert_eq!(inserted.latest_body, "v1 plan body");

    let updated = svc
        .upsert_issue_document(UpsertIssueDocument {
            company_id,
            issue_id,
            key: "plan".into(),
            title: Some("Plan".into()),
            body: "v2 plan body".into(),
            format: None,
            created_by_user_id: Some("u".into()),
            created_by_agent_id: None,
        })
        .await
        .expect("upsert update");
    assert_eq!(updated.latest_body, "v2 plan body");
    assert_eq!(updated.id, inserted.id, "upsert should reuse the document");

    let docs = svc.list_issue_documents(issue_id).await.expect("list");
    assert_eq!(docs.len(), 1);

    let removed = svc
        .delete_issue_document(issue_id, "plan")
        .await
        .expect("delete");
    assert!(removed);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r608_get_returns_none_for_wrong_company() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let co_a = insert_company(&pool).await;
    let co_b = insert_company(&pool).await;

    let svc = DocumentService::new(db);
    let row = svc
        .create(CreateDocument {
            company_id: co_a,
            title: None,
            format: None,
            body: "x".into(),
            created_by_user_id: Some("u".into()),
            created_by_agent_id: None,
        })
        .await
        .expect("create");

    let fetched = svc.get_in_company(co_b, row.id).await.expect("get cross");
    assert!(fetched.is_none());

    cleanup(&pool, co_a).await;
    cleanup(&pool, co_b).await;
}
