//! R727: e2e for `pc-pipeline-conversation-context` against real Postgres.

use pc_pipeline_conversation_context::{
    format_pipeline_conversation_body_document_context_markdown,
    load_pipeline_conversation_body_document_context, LoadPipelineContextInput,
    MAX_CONTEXT_BODY_CHARS,
};
use pc_repos::Db;
use pc_core::source_trust_resolver::{
    build_low_trust_source_trust, LowTrustSourceTrustInput, SourceTrustMetadata,
    LOW_TRUST_QUARANTINED_BODY,
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

async fn insert_company(pool: &PgPool, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    let prefix = format!(
        "R727{}-{}",
        tag,
        Uuid::new_v4().simple().to_string().chars().take(5).collect::<String>()
    );
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R727-{tag}-{id}"))
    .bind(prefix)
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn insert_issue(pool: &PgPool, company_id: Uuid, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    let identifier = format!(
        "R727-{}-{}",
        tag,
        Uuid::new_v4().simple().to_string().chars().take(6).collect::<String>()
    );
    sqlx::query(
        "INSERT INTO issues (id, company_id, identifier, title, status, priority, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, 'todo', 'normal', now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(identifier)
    .bind(format!("R727 issue {tag}"))
    .execute(pool)
    .await
    .expect("insert issue");
    id
}

async fn insert_document(
    pool: &PgPool,
    company_id: Uuid,
    body: &str,
    source_trust: Option<serde_json::Value>,
) -> (Uuid, Uuid) {
    let doc_id = Uuid::new_v4();
    let rev_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO documents (id, company_id, title, format, latest_body, latest_revision_id, latest_revision_number, source_trust, created_at, updated_at) \
         VALUES ($1, $2, $3, 'markdown', $4, $5, 1, $6, now(), now())",
    )
    .bind(doc_id)
    .bind(company_id)
    .bind(format!("R727 doc {doc_id}"))
    .bind(body)
    .bind(rev_id)
    .bind(source_trust)
    .execute(pool)
    .await
    .expect("insert document");
    sqlx::query(
        "INSERT INTO document_revisions (id, company_id, document_id, revision_number, body, format, created_at) \
         VALUES ($1, $2, $3, 1, $4, 'markdown', now())",
    )
    .bind(rev_id)
    .bind(company_id)
    .bind(doc_id)
    .bind(body)
    .execute(pool)
    .await
    .expect("insert document revision");
    (doc_id, rev_id)
}

async fn link_case_document(pool: &PgPool, company_id: Uuid, case_id: Uuid, doc_id: Uuid) {
    sqlx::query(
        "INSERT INTO pipeline_case_documents (company_id, case_id, document_id, key, created_at, updated_at) \
         VALUES ($1, $2, $3, 'body', now(), now())",
    )
    .bind(company_id)
    .bind(case_id)
    .bind(doc_id)
    .execute(pool)
    .await
    .expect("link case document");
}

async fn insert_thread(
    pool: &PgPool,
    company_id: Uuid,
    issue_id: Uuid,
    doc_id: Uuid,
    rev_id: Uuid,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO document_annotation_threads \
            (id, company_id, issue_id, document_id, document_key, status, original_revision_number, current_revision_id, current_revision_number, \
             anchor_state, anchor_confidence, selected_text, prefix_text, suffix_text, \
             normalized_start, normalized_end, markdown_start, markdown_end, anchor_selector, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, 'body', 'open', 1, $5, 1, 'active', 'high', 'selected', 'pre-', 'suf-', 0, 0, 0, 0, '[]'::jsonb, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(issue_id)
    .bind(doc_id)
    .bind(rev_id)
    .execute(pool)
    .await
    .expect("insert thread");
    id
}

async fn insert_comment(
    pool: &PgPool,
    company_id: Uuid,
    issue_id: Uuid,
    doc_id: Uuid,
    thread_id: Uuid,
    body: &str,
    source_trust: Option<serde_json::Value>,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO document_annotation_comments \
            (id, company_id, issue_id, thread_id, document_id, body, author_type, author_user_id, source_trust, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, 'user', $7, $8, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(issue_id)
    .bind(thread_id)
    .bind(doc_id)
    .bind(body)
    .bind(format!("R727u-{id}"))
    .bind(source_trust)
    .execute(pool)
    .await
    .expect("insert comment");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM pipeline_case_documents WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM pipeline_cases WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM pipeline_stages WHERE pipeline_id IN (SELECT id FROM pipelines WHERE company_id = $1)")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM pipelines WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM document_annotation_comments WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM document_annotation_threads WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM pipeline_case_documents WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM document_revisions WHERE document_id IN (SELECT id FROM documents WHERE company_id = $1)")
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
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
}


async fn insert_pipeline(pool: &PgPool, company_id: Uuid, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO pipelines (id, company_id, key, name, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("R727-pl-{tag}-{}", Uuid::new_v4().simple().to_string().chars().take(6).collect::<String>()))
    .bind(format!("R727 pipeline {tag}"))
    .execute(pool)
    .await
    .expect("insert pipeline");
    id
}

async fn insert_pipeline_stage(pool: &PgPool, pipeline_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO pipeline_stages (id, pipeline_id, key, name, kind, position, config, created_at, updated_at) \
         VALUES ($1, $2, 'working', 'Working', 'working', 1, '{}'::jsonb, now(), now())",
    )
    .bind(id)
    .bind(pipeline_id)
    .execute(pool)
    .await
    .expect("insert stage");
    id
}

async fn insert_pipeline_case(pool: &PgPool, company_id: Uuid, pipeline_id: Uuid, stage_id: Uuid, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO pipeline_cases (id, company_id, pipeline_id, stage_id, case_key, title, fields, version, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, '{}'::jsonb, 1, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(pipeline_id)
    .bind(stage_id)
    .bind(format!("R727-case-{tag}-{}", Uuid::new_v4().simple().to_string().chars().take(6).collect::<String>()))
    .bind(format!("R727 case {tag}"))
    .execute(pool)
    .await
    .expect("insert case");
    id
}


async fn setup_case_with_body(
    pool: &PgPool,
    company_id: Uuid,
    tag: &str,
    body: &str,
    source_trust: Option<serde_json::Value>,
) -> (Uuid, Uuid, Uuid) {
    let pipeline_id = insert_pipeline(pool, company_id, tag).await;
    let stage_id = insert_pipeline_stage(pool, pipeline_id).await;
    let case_id = insert_pipeline_case(pool, company_id, pipeline_id, stage_id, tag).await;
    let (doc_id, rev_id) = insert_document(pool, company_id, body, source_trust).await;
    link_case_document(pool, company_id, case_id, doc_id).await;
    (case_id, doc_id, rev_id)
}

#[tokio::test(flavor = "current_thread")]
async fn returns_empty_context_when_no_body_document() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "nobody").await;

    let ctx = load_pipeline_conversation_body_document_context(
        &db,
        LoadPipelineContextInput {
            company_id: company_id.to_string(),
            case_id: Uuid::new_v4().to_string(),
            conversation_issue_id: None,
        },
    )
    .await
    .expect("load");

    assert!(ctx.body_document.is_none());
    assert!(ctx.open_annotation_threads.is_empty());
    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn loads_body_document_with_high_trust() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "high").await;
    let (case_id, doc_id, rev_id) = setup_case_with_body(&pool, company_id, "high", "safe body content", None).await;

    let ctx = load_pipeline_conversation_body_document_context(
        &db,
        LoadPipelineContextInput {
            company_id: company_id.to_string(),
            case_id: case_id.to_string(),
            conversation_issue_id: None,
        },
    )
    .await
    .expect("load");

    let body = ctx.body_document.expect("body document");
    assert_eq!(body.id, doc_id.to_string());
    assert_eq!(body.latest_body, "safe body content");
    assert_eq!(body.latest_revision_id, Some(rev_id.to_string()));
    assert!(!body.latest_body_truncated);
    assert!(body.source_trust.is_none());
    assert!(ctx.open_annotation_threads.is_empty());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn redacts_low_trust_body_on_load() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "low").await;
    let trust = build_low_trust_source_trust(LowTrustSourceTrustInput {
        issue_id: "i-src".to_string(),
        run_id: None,
        agent_id: None,
    });
    let trust_json = serde_json::to_value(&trust).unwrap();
    let (case_id, _, _) = setup_case_with_body(
        &pool,
        company_id,
        "low",
        "secret content",
        Some(trust_json.clone()),
    )
    .await;

    let ctx = load_pipeline_conversation_body_document_context(
        &db,
        LoadPipelineContextInput {
            company_id: company_id.to_string(),
            case_id: case_id.to_string(),
            conversation_issue_id: None,
        },
    )
    .await
    .expect("load");

    let body = ctx.body_document.expect("body document");
    assert_eq!(body.latest_body, LOW_TRUST_QUARANTINED_BODY);
    assert_eq!(body.source_trust.as_ref().map(|t| &t.preset), Some(&trust.preset));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn loads_threads_and_comments_for_conversation_issue() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "thr").await;
    let issue_id = insert_issue(&pool, company_id, "thr").await;
    let (case_id, doc_id, rev_id) = setup_case_with_body(&pool, company_id, "thr", "body", None).await;

    let t1 = insert_thread(&pool, company_id, issue_id, doc_id, rev_id).await;
    let _t2 = insert_thread(&pool, company_id, issue_id, doc_id, rev_id).await;
    insert_comment(&pool, company_id, issue_id, doc_id, t1, "first", None).await;
    insert_comment(&pool, company_id, issue_id, doc_id, t1, "second", None).await;

    let ctx = load_pipeline_conversation_body_document_context(
        &db,
        LoadPipelineContextInput {
            company_id: company_id.to_string(),
            case_id: case_id.to_string(),
            conversation_issue_id: Some(issue_id.to_string()),
        },
    )
    .await
    .expect("load");

    assert_eq!(ctx.open_annotation_threads.len(), 2);
    let t1_ctx = ctx
        .open_annotation_threads
        .iter()
        .find(|t| t.id == t1.to_string())
        .expect("t1");
    assert_eq!(t1_ctx.comments.len(), 2);
    assert_eq!(t1_ctx.comments[0].body, "first");
    assert_eq!(t1_ctx.comments[1].body, "second");
    // Anchor text is high trust, so it survives.
    assert_eq!(t1_ctx.selected_text, "selected");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn low_trust_body_redacts_anchor_text_but_keeps_high_trust_comment() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "anchor").await;
    let issue_id = insert_issue(&pool, company_id, "anchor").await;
    let trust = build_low_trust_source_trust(LowTrustSourceTrustInput {
        issue_id: "i-src".to_string(),
        run_id: None,
        agent_id: None,
    });
    let trust_json = serde_json::to_value(&trust).unwrap();
    let (case_id, doc_id, rev_id) = setup_case_with_body(
        &pool,
        company_id,
        "anchor",
        "secret body",
        Some(trust_json.clone()),
    )
    .await;

    let thread_id = insert_thread(&pool, company_id, issue_id, doc_id, rev_id).await;
    insert_comment(&pool, company_id, issue_id, doc_id, thread_id, "high trust comment", None).await;

    let ctx = load_pipeline_conversation_body_document_context(
        &db,
        LoadPipelineContextInput {
            company_id: company_id.to_string(),
            case_id: case_id.to_string(),
            conversation_issue_id: Some(issue_id.to_string()),
        },
    )
    .await
    .expect("load");

    let thread = &ctx.open_annotation_threads[0];
    // Anchor text is redacted because the body is low-trust.
    assert_eq!(thread.selected_text, LOW_TRUST_QUARANTINED_BODY);
    assert_eq!(thread.prefix_text, "");
    assert_eq!(thread.suffix_text, "");
    // The high-trust comment survives.
    assert_eq!(thread.comments[0].body, "high trust comment");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn truncates_long_body() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "long").await;
    let long_body = "x".repeat(MAX_CONTEXT_BODY_CHARS + 1_000);
    let (case_id, _, _) = setup_case_with_body(&pool, company_id, "long", &long_body, None).await;

    let ctx = load_pipeline_conversation_body_document_context(
        &db,
        LoadPipelineContextInput {
            company_id: company_id.to_string(),
            case_id: case_id.to_string(),
            conversation_issue_id: None,
        },
    )
    .await
    .expect("load");

    let body = ctx.body_document.expect("body document");
    assert!(body.latest_body_truncated);
    assert_eq!(body.latest_body.chars().count(), MAX_CONTEXT_BODY_CHARS);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn markdown_rendering_for_empty_context() {
    let md = format_pipeline_conversation_body_document_context_markdown(None);
    assert!(md.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn markdown_rendering_for_loaded_context() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "md").await;
    let (case_id, _, _) = setup_case_with_body(&pool, company_id, "md", "hello world", None).await;

    let ctx = load_pipeline_conversation_body_document_context(
        &db,
        LoadPipelineContextInput {
            company_id: company_id.to_string(),
            case_id: case_id.to_string(),
            conversation_issue_id: None,
        },
    )
    .await
    .expect("load");

    let md = format_pipeline_conversation_body_document_context_markdown(Some(&ctx))
        .expect("markdown");
    assert!(md.contains("## Pipeline Item Body Document"));
    assert!(md.contains(&case_id.to_string()));
    assert!(md.contains("hello world"));
    // No body document (None) branch
    let ctx2 = pc_pipeline_conversation_context::PipelineConversationBodyDocumentContext {
        case_id: case_id.to_string(),
        body_document: None,
        open_annotation_threads: Vec::new(),
    };
    let md2 = format_pipeline_conversation_body_document_context_markdown(Some(&ctx2)).unwrap();
    assert!(md2.contains("No body document exists yet"));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn preserves_threads_ordering() {
    // Ensures threads come back in the order the SQL query produces (most-recently-updated first).
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "ord").await;
    let issue_id = insert_issue(&pool, company_id, "ord").await;
    let (case_id, doc_id, rev_id) = setup_case_with_body(&pool, company_id, "ord", "body", None).await;

    // Insert threads; SQL orders by updated_at DESC, id DESC.
    let t_early = insert_thread(&pool, company_id, issue_id, doc_id, rev_id).await;
    sqlx::query("UPDATE document_annotation_threads SET updated_at = now() - interval '10 minutes' WHERE id = $1")
        .bind(t_early)
        .execute(&pool)
        .await
        .unwrap();
    let t_late = insert_thread(&pool, company_id, issue_id, doc_id, rev_id).await;
    sqlx::query("UPDATE document_annotation_threads SET updated_at = now() WHERE id = $1")
        .bind(t_late)
        .execute(&pool)
        .await
        .unwrap();

    let ctx = load_pipeline_conversation_body_document_context(
        &db,
        LoadPipelineContextInput {
            company_id: company_id.to_string(),
            case_id: case_id.to_string(),
            conversation_issue_id: Some(issue_id.to_string()),
        },
    )
    .await
    .expect("load");

    let ids: Vec<String> = ctx
        .open_annotation_threads
        .iter()
        .map(|t| t.id.clone())
        .collect();
    assert_eq!(ids, vec![t_late.to_string(), t_early.to_string()]);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn _json_helper_to_silence_unused_imports() {
    // Ensure unused json import doesn't cause a warning.
    let _ = json!({});
    let _ = SourceTrustMetadata::standard();
}
