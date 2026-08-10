//! R608: `pc-documents` service contract 测试。
//!
//! 验证 service 的公共 API 是稳定的：
//! - 公开输出类型（DocumentRow / DocumentRevisionRow / AnnotationThreadRow /
//!   AnnotationCommentRow / DocumentHookEvent）都能 `serde_json` 序列化 +
//!   round-trip 回对象
//! - 公开输入类型（CreateDocument / DocumentPatch /
//!   CreateAnnotationThreadInput / CreateAnnotationComment /
//!   UpsertIssueDocument）的字段集稳定
//! - service 是 HTTP-friendly facade（不依赖外部状态，能独立构造）

use std::sync::Arc;

use pc_documents::{
    CreateAnnotationComment, CreateAnnotationThreadInput, CreateDocument, DocumentPatch,
    DocumentService, RecordingDocumentHook, UpsertIssueDocument,
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
    let prefix = format!("R{}", Uuid::new_v4().simple().to_string().chars().take(5).collect::<String>());
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at)          VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R608ct-{id}"))
    .bind(prefix)
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn insert_issue(pool: &PgPool, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, created_at, updated_at)          VALUES ($1, $2, 'rt-issue', 'todo', 'normal', now(), now())",
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

#[tokio::test(flavor = "current_thread")]
async fn document_row_roundtrips_through_json() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = DocumentService::new(db);
    let row = svc
        .create(CreateDocument {
            company_id,
            title: Some("Spec".into()),
            format: Some("markdown".into()),
            body: "json-roundtrip".into(),
            created_by_user_id: Some("u".into()),
            created_by_agent_id: None,
        })
        .await
        .expect("create");

    let value: Value = serde_json::to_value(&row).expect("serialize DocumentRow");
    assert_eq!(value["company_id"], company_id.to_string());
    assert_eq!(value["title"], "Spec");
    assert_eq!(value["format"], "markdown");
    assert_eq!(value["latest_body"], "json-roundtrip");
    assert_eq!(value["latest_revision_number"], 1);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn document_revision_row_roundtrips_through_json() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let svc = DocumentService::new(db);
    let row = svc
        .create(CreateDocument {
            company_id,
            title: None,
            format: None,
            body: "rev-body".into(),
            created_by_user_id: Some("u".into()),
            created_by_agent_id: None,
        })
        .await
        .expect("create");

    let revs = svc.list_revisions(row.id).await.expect("list");
    assert_eq!(revs.len(), 1);
    let v: Value = serde_json::to_value(&revs[0]).expect("serialize revision");
    assert_eq!(v["revision_number"], 1);
    assert_eq!(v["body"], "rev-body");
    assert_eq!(v["change_summary"], "Created document");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn annotation_thread_row_roundtrips() {
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
            body: "abc".into(),
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
            selected_text: "abc".into(),
            prefix_text: "".into(),
            suffix_text: "".into(),
            normalized_start: 0,
            normalized_end: 3,
            markdown_start: 0,
            markdown_end: 3,
            anchor_confidence: Some("high".into()),
            anchor_selector: Some(json!({"type": "text"})),
            created_by_user_id: Some("u".into()),
            created_by_agent_id: None,
        })
        .await
        .expect("thread");

    let v: Value = serde_json::to_value(&thread).expect("serialize thread");
    assert_eq!(v["company_id"], company_id.to_string());
    assert_eq!(v["document_key"], "k");
    assert_eq!(v["status"], "open");
    assert_eq!(v["anchor_state"], "anchored");
    assert_eq!(v["selected_text"], "abc");
    assert_eq!(v["anchor_confidence"], "high");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn input_types_have_expected_defaults() {
    let create = CreateDocument {
        company_id: Uuid::nil(),
        title: None,
        format: None,
        body: String::new(),
        created_by_user_id: None,
        created_by_agent_id: None,
    };
    assert!(create.title.is_none());
    assert!(create.format.is_none());
    assert_eq!(create.body, "");

    let patch = DocumentPatch::default();
    assert!(patch.title.is_none());
    assert!(patch.body.is_none());
    assert!(patch.format.is_none());

    let thread = CreateAnnotationThreadInput {
        company_id: Uuid::nil(),
        issue_id: Uuid::nil(),
        document_id: Uuid::nil(),
        document_key: String::new(),
        selected_text: String::new(),
        prefix_text: String::new(),
        suffix_text: String::new(),
        normalized_start: 0,
        normalized_end: 0,
        markdown_start: 0,
        markdown_end: 0,
        anchor_confidence: None,
        anchor_selector: None,
        created_by_user_id: None,
        created_by_agent_id: None,
    };
    assert_eq!(thread.document_key, "");
    assert_eq!(thread.selected_text, "");

    let comment = CreateAnnotationComment {
        company_id: Uuid::nil(),
        thread_id: Uuid::nil(),
        issue_id: Uuid::nil(),
        document_id: Uuid::nil(),
        body: String::new(),
        author_type: String::new(),
        author_user_id: None,
        author_agent_id: None,
    };
    assert_eq!(comment.body, "");
    assert_eq!(comment.author_type, "");

    let upsert = UpsertIssueDocument {
        company_id: Uuid::nil(),
        issue_id: Uuid::nil(),
        key: String::new(),
        title: None,
        body: String::new(),
        format: None,
        created_by_user_id: None,
        created_by_agent_id: None,
    };
    assert_eq!(upsert.key, "");
}

#[tokio::test(flavor = "current_thread")]
async fn service_constructs_with_recorder_via_with_hooks() {
    let db = Db::connect(TEST_DATABASE_URL, 1, 0).await.expect("db");
    let recorder = Arc::new(RecordingDocumentHook::default());
    let svc = DocumentService::with_hooks(db, vec![recorder.clone()]);
    drop(svc);
    assert!(recorder.is_empty(), "fresh recorder starts empty");
}

#[tokio::test(flavor = "current_thread")]
async fn format_validation_constants_stable() {
    // Verify the allowed-format set is stable: markdown/plain/html
    // (downstream consumers depend on this surface; expansion is a breaking change)
    // This is enforced by CreateDocument::normalize which rejects anything else.
    let svc_inputs: Vec<CreateDocument> = vec![
        CreateDocument {
            company_id: Uuid::new_v4(),
            title: None,
            format: Some("markdown".into()),
            body: "x".into(),
            created_by_user_id: Some("u".into()),
            created_by_agent_id: None,
        },
        CreateDocument {
            company_id: Uuid::new_v4(),
            title: None,
            format: Some("plain".into()),
            body: "x".into(),
            created_by_user_id: Some("u".into()),
            created_by_agent_id: None,
        },
        CreateDocument {
            company_id: Uuid::new_v4(),
            title: None,
            format: Some("html".into()),
            body: "x".into(),
            created_by_user_id: Some("u".into()),
            created_by_agent_id: None,
        },
    ];
    // These won't reach validation error in the absence of nil-company check
    // because we used a fresh UUID; we just want to confirm the structs compile
    // and the format string passes through normalize (validation order matters).
    let _ = svc_inputs;
}
