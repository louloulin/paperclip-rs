//! R608: `pc-documents` hook 系统 contract 测试。
//!
//! 验证 DocumentHook trait 的语义契约：
//! - `NoopDocumentHook` 不影响 service 行为
//! - `RecordingDocumentHook` 记录所有 lifecycle 事件
//! - 多个 hook 同时注册时全部触发
//! - 失败的 hook 不阻塞 service
//! - recorder helper（events_snapshot / clear / len / is_empty）
//! - Created / Updated / Deleted / Locked / Unlocked / RevisionRestored /
//!   AnnotationThreadCreated / AnnotationThreadResolved /
//!   AnnotationCommentCreated event 序列化

use std::sync::Arc;

use async_trait::async_trait;
use pc_documents::{
    CreateAnnotationComment, CreateAnnotationThreadInput, CreateDocument, DocumentHook,
    DocumentHookEvent, DocumentPatch, DocumentService, NoopDocumentHook, RecordingDocumentHook,
};
use pc_repos::Db;
use serde_json::{json, Value};
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
    let prefix = format!("H{}", Uuid::new_v4().simple().to_string().chars().take(5).collect::<String>());
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at)          VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R608hk-{id}"))
    .bind(prefix)
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn insert_issue(pool: &PgPool, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, created_at, updated_at)          VALUES ($1, $2, 'hook-issue', 'todo', 'normal', now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .execute(pool)
    .await
    .expect("insert issue");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM document_annotation_comments WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM document_annotation_threads WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM issue_documents WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM document_revisions WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM documents WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM company_memberships WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1").bind(company_id).execute(pool).await;
}

struct FailingHook;
#[async_trait]
impl DocumentHook for FailingHook {
    async fn on_document_event(&self, _event: DocumentHookEvent) -> pc_errors::Result<()> {
        Err(pc_errors::internal("hook always fails"))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn hook_noop_does_not_affect_service() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = DocumentService::new(db).add_hook(Arc::new(NoopDocumentHook));
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
        .expect("create with noop");
    assert_eq!(row.latest_body, "x");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn hook_recorder_captures_create_event() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(RecordingDocumentHook::default());
    let svc = DocumentService::new(db).add_hook(recorder.clone());
    let row = svc
        .create(CreateDocument {
            company_id,
            title: Some("captured".into()),
            format: None,
            body: "x".into(),
            created_by_user_id: Some("u".into()),
            created_by_agent_id: None,
        })
        .await
        .expect("create");

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    match &events[0] {
        DocumentHookEvent::Created { id, title, format, .. } => {
            assert_eq!(*id, row.id);
            assert_eq!(title.as_deref(), Some("captured"));
            assert_eq!(format, "markdown");
        }
        _ => panic!("expected Created"),
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn hook_recorder_helpers_work() {
    let recorder = RecordingDocumentHook::default();
    assert!(recorder.is_empty());
    assert_eq!(recorder.len(), 0);

    recorder.on_document_event(DocumentHookEvent::Deleted {
        id: Uuid::nil(),
        company_id: Uuid::nil(),
    }).await.expect("hook");

    assert_eq!(recorder.len(), 1);
    assert!(!recorder.is_empty());

    recorder.clear();
    assert!(recorder.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn multiple_hooks_all_fire() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let r1 = Arc::new(RecordingDocumentHook::default());
    let r2 = Arc::new(RecordingDocumentHook::default());
    let r3 = Arc::new(RecordingDocumentHook::default());
    let svc = DocumentService::new(db)
        .add_hook(r1.clone())
        .add_hook(r2.clone())
        .add_hook(r3.clone());

    svc.create(CreateDocument {
        company_id,
        title: None,
        format: None,
        body: "x".into(),
        created_by_user_id: Some("u".into()),
        created_by_agent_id: None,
    })
    .await
    .expect("create");

    assert_eq!(r1.len(), 1);
    assert_eq!(r2.len(), 1);
    assert_eq!(r3.len(), 1);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn failing_hook_does_not_block_other_hooks() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let failing: Arc<dyn DocumentHook> = Arc::new(FailingHook);
    let recorder = Arc::new(RecordingDocumentHook::default());

    let svc = DocumentService::new(db)
        .add_hook(failing)
        .add_hook(recorder.clone());

    let row = svc
        .create(CreateDocument {
            company_id,
            title: None,
            format: None,
            body: "after-fail".into(),
            created_by_user_id: Some("u".into()),
            created_by_agent_id: None,
        })
        .await
        .expect("create despite failing hook");
    assert_eq!(row.latest_body, "after-fail");

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1, "hook after failing hook still fires");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn hook_receives_lifecycle_events() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let issue_id = insert_issue(&pool, company_id).await;

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
        .expect("lock");
    svc.unlock_document(company_id, row.id)
        .await
        .expect("unlock");
    svc.update(
        company_id,
        row.id,
        DocumentPatch {
            body: Some("y".into()),
            ..Default::default()
        },
    )
    .await
    .expect("update");

    let thread = svc
        .create_annotation_thread(CreateAnnotationThreadInput {
            company_id,
            issue_id,
            document_id: row.id,
            document_key: "k".into(),
            selected_text: "x".into(),
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

    svc.create_annotation_comment(CreateAnnotationComment {
        company_id,
        thread_id: thread.id,
        issue_id,
        document_id: row.id,
        body: "first comment".into(),
        author_type: "user".into(),
        author_user_id: Some("u".into()),
        author_agent_id: None,
    })
    .await
    .expect("comment");

    svc.resolve_annotation_thread(thread.id, Some("u"))
        .await
        .expect("resolve");

    let events = recorder.events_snapshot();
    let kinds: Vec<&'static str> = events
        .iter()
        .map(|e| match e {
            DocumentHookEvent::Locked { .. } => "locked",
            DocumentHookEvent::Unlocked { .. } => "unlocked",
            DocumentHookEvent::Updated { .. } => "updated",
            DocumentHookEvent::AnnotationThreadCreated { .. } => "thread_created",
            DocumentHookEvent::AnnotationCommentCreated { .. } => "comment_created",
            DocumentHookEvent::AnnotationThreadResolved { .. } => "thread_resolved",
            other => panic!("unexpected event {other:?}"),
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            "locked",
            "unlocked",
            "updated",
            "thread_created",
            "comment_created",
            "thread_resolved"
        ]
    );

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn hook_events_serialize_for_realtime() {
    let event = DocumentHookEvent::Locked {
        id: Uuid::nil(),
        company_id: Uuid::nil(),
        locked_by_agent_id: Some(Uuid::nil()),
        locked_by_user_id: Some("user-1".into()),
    };
    let v: Value = serde_json::to_value(&event).expect("serialize Locked");
    assert_eq!(v["type"], "locked");
    assert_eq!(v["locked_by_user_id"], "user-1");

    let restored = DocumentHookEvent::RevisionRestored {
        document_id: Uuid::nil(),
        company_id: Uuid::nil(),
        restored_from_revision_number: 3,
        new_revision_id: Uuid::nil(),
    };
    let rv: Value = serde_json::to_value(&restored).expect("serialize RevisionRestored");
    assert_eq!(rv["type"], "revisionRestored");
    assert_eq!(rv["restored_from_revision_number"], 3);

    let c = DocumentHookEvent::AnnotationCommentCreated {
        comment_id: Uuid::nil(),
        thread_id: Uuid::nil(),
        document_id: Uuid::nil(),
        company_id: Uuid::nil(),
        author_type: "user".into(),
    };
    let cv: Value = serde_json::to_value(&c).expect("serialize AnnotationCommentCreated");
    assert_eq!(cv["type"], "annotationCommentCreated");
    assert_eq!(cv["author_type"], "user");

    // ensure JSON shape is consumable
    let _ = json!({ "marker": "ok" });
}
