//! R726: e2e for `pc-plan-review-context` against real Postgres.

use pc_plan_review_context::{
    build_plan_review_context, get_plan_interaction_context, BuildPlanReviewContextInput,
    GetPlanInteractionInput, PLAN_REVIEW_CONTEXT_LIMITS,
};
use pc_repos::Db;
use serde_json::json;
use sqlx::{PgPool, Row};
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
        "R726{}-{}",
        tag,
        Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .take(5)
            .collect::<String>()
    );
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R726-{tag}-{id}"))
    .bind(prefix)
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn insert_issue(pool: &PgPool, company_id: Uuid, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    let identifier = format!(
        "R726-{}-{}",
        tag,
        Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .take(6)
            .collect::<String>()
    );
    sqlx::query(
        "INSERT INTO issues (id, company_id, identifier, title, status, priority, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, 'todo', 'normal', now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(identifier)
    .bind(format!("R726 issue {tag}"))
    .execute(pool)
    .await
    .expect("insert issue");
    id
}

async fn insert_document(pool: &PgPool, company_id: Uuid) -> (Uuid, Uuid) {
    let doc_id = Uuid::new_v4();
    let rev_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO documents (id, company_id, title, format, latest_body, latest_revision_id, latest_revision_number, created_at, updated_at) \
         VALUES ($1, $2, $3, 'markdown', $4, $5, 1, now(), now())",
    )
    .bind(doc_id)
    .bind(company_id)
    .bind(format!("R726 plan {doc_id}"))
    .bind("R726 plan body")
    .bind(rev_id)
    .execute(pool)
    .await
    .expect("insert document");
    // Also insert the document_revisions row (current_revision_id FK target).
    sqlx::query(
        "INSERT INTO document_revisions (id, company_id, document_id, revision_number, body, format, created_at) \
         VALUES ($1, $2, $3, 1, $4, 'markdown', now())",
    )
    .bind(rev_id)
    .bind(company_id)
    .bind(doc_id)
    .bind("R726 plan body")
    .execute(pool)
    .await
    .expect("insert document revision");
    (doc_id, rev_id)
}

async fn link_issue_document(pool: &PgPool, company_id: Uuid, issue_id: Uuid, doc_id: Uuid) {
    sqlx::query(
        "INSERT INTO issue_documents (company_id, issue_id, document_id, key, created_at, updated_at) \
         VALUES ($1, $2, $3, 'plan', now(), now())",
    )
    .bind(company_id)
    .bind(issue_id)
    .bind(doc_id)
    .execute(pool)
    .await
    .expect("link issue document");
}

async fn insert_user(pool: &PgPool, tag: &str) -> String {
    let id = format!("R726u-{tag}-{}", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO \"user\" (id, name, email, email_verified, created_at, updated_at) \
         VALUES ($1, $2, $3, false, now(), now())",
    )
    .bind(&id)
    .bind(format!("R726 user {tag}"))
    .bind(format!("{id}@paperclip.test"))
    .execute(pool)
    .await
    .expect("insert user");
    id
}

async fn insert_thread(
    pool: &PgPool,
    company_id: Uuid,
    issue_id: Uuid,
    doc_id: Uuid,
    rev_id: Uuid,
    user_id: &str,
    selected: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO document_annotation_threads \
            (id, company_id, issue_id, document_id, document_key, status, original_revision_number, current_revision_id, current_revision_number, \
             anchor_state, anchor_confidence, selected_text, prefix_text, suffix_text, \
             normalized_start, normalized_end, markdown_start, markdown_end, anchor_selector, \
             created_by_user_id, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, 'plan', 'open', 1, $5, 1, 'active', 'high', $6, 'pre-', 'suf-', \
                 0, 0, 0, 0, '[]'::jsonb, $7, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(issue_id)
    .bind(doc_id)
    .bind(rev_id)
    .bind(selected)
    .bind(user_id)
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
    user_id: &str,
    body: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO document_annotation_comments \
            (id, company_id, issue_id, thread_id, document_id, body, author_type, author_user_id, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, 'user', $7, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(issue_id)
    .bind(thread_id)
    .bind(doc_id)
    .bind(body)
    .bind(user_id)
    .execute(pool)
    .await
    .expect("insert comment");
    id
}

async fn insert_interaction(
    pool: &PgPool,
    company_id: Uuid,
    issue_id: Uuid,
    doc_id: Uuid,
    rev_id: Uuid,
    payload: serde_json::Value,
    result: serde_json::Value,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issue_thread_interactions \
            (id, company_id, issue_id, kind, status, payload, result, created_at, updated_at) \
         VALUES ($1, $2, $3, 'plan_review', 'pending', $4, $5, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(issue_id)
    .bind(payload)
    .bind(result)
    .execute(pool)
    .await
    .expect("insert interaction");
    let _ = doc_id;
    let _ = rev_id;
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM issue_thread_interactions WHERE company_id = $1")
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
    let _ = sqlx::query("DELETE FROM issue_documents WHERE company_id = $1")
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
    let _ = sqlx::query("DELETE FROM \"user\" WHERE id LIKE 'R726u-%'")
        .execute(pool)
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn returns_none_when_not_in_planning_mode_and_no_hooks() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "nopln").await;
    let issue_id = insert_issue(&pool, company_id, "nopln").await;
    let (doc_id, rev_id) = insert_document(&pool, company_id).await;
    link_issue_document(&pool, company_id, issue_id, doc_id).await;
    let user_id = insert_user(&pool, "nopln").await;
    let _t = insert_thread(
        &pool, company_id, issue_id, doc_id, rev_id, &user_id, "selected",
    )
    .await;

    let ctx = build_plan_review_context(
        &db,
        BuildPlanReviewContextInput {
            company_id: company_id.to_string(),
            issue_id: issue_id.to_string(),
            ..Default::default()
        },
    )
    .await
    .expect("build");

    // Default inputs: no work mode, no hook flags, no interaction_id.
    // Even though threads exist, should_include is false → return None.
    assert!(ctx.is_none(), "expected None when should_include is false");
    let _ = rev_id;
    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn build_returns_threads_and_comments_in_planning_mode() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "pln").await;
    let issue_id = insert_issue(&pool, company_id, "pln").await;
    let (doc_id, rev_id) = insert_document(&pool, company_id).await;
    link_issue_document(&pool, company_id, issue_id, doc_id).await;
    let user_id = insert_user(&pool, "pln").await;

    let thread_id = insert_thread(
        &pool,
        company_id,
        issue_id,
        doc_id,
        rev_id,
        &user_id,
        "Highlighted text",
    )
    .await;
    insert_comment(
        &pool,
        company_id,
        issue_id,
        doc_id,
        thread_id,
        &user_id,
        "first comment",
    )
    .await;
    insert_comment(
        &pool,
        company_id,
        issue_id,
        doc_id,
        thread_id,
        &user_id,
        "second comment",
    )
    .await;

    let ctx = build_plan_review_context(
        &db,
        BuildPlanReviewContextInput {
            company_id: company_id.to_string(),
            issue_id: issue_id.to_string(),
            issue_work_mode: Some("planning".to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("build")
    .expect("some");

    assert_eq!(ctx.document_key, "plan");
    assert_eq!(ctx.issue_id, issue_id.to_string());
    assert_eq!(ctx.latest_revision_id, Some(rev_id.to_string()));
    assert_eq!(ctx.latest_revision_number, Some(1));
    assert_eq!(ctx.threads.len(), 1);
    assert_eq!(ctx.threads[0].id, thread_id.to_string());
    assert_eq!(ctx.threads[0].comment_count, 2);
    assert_eq!(ctx.threads[0].comments.len(), 2);
    assert_eq!(ctx.threads[0].comments[0].body, "first comment");
    assert_eq!(ctx.threads[0].author.author_type, "user");
    assert_eq!(ctx.threads[0].author.id, Some(user_id.clone()));
    assert_eq!(ctx.totals.open_thread_count, 1);
    assert_eq!(ctx.totals.included_thread_count, 1);
    assert_eq!(ctx.totals.omitted_thread_count, 0);
    assert_eq!(ctx.totals.comment_count, 2);
    assert_eq!(ctx.totals.included_comment_count, 2);
    assert_eq!(ctx.totals.omitted_comment_count, 0);
    assert!(!ctx.truncated);
    assert_eq!(
        ctx.limits.max_threads,
        PLAN_REVIEW_CONTEXT_LIMITS.max_threads
    );
    assert!(ctx.interaction.is_none());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn truncated_flag_set_when_total_chars_exhausted() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "trunc").await;
    let issue_id = insert_issue(&pool, company_id, "trunc").await;
    let (doc_id, rev_id) = insert_document(&pool, company_id).await;
    link_issue_document(&pool, company_id, issue_id, doc_id).await;
    let user_id = insert_user(&pool, "trunc").await;

    let thread_id = insert_thread(&pool, company_id, issue_id, doc_id, rev_id, &user_id, "x").await;
    // 11 comments × 1100 chars each = 12_100 > 12_000 total.
    for _ in 0..11 {
        let body = "a".repeat(1_100);
        insert_comment(
            &pool, company_id, issue_id, doc_id, thread_id, &user_id, &body,
        )
        .await;
    }

    let ctx = build_plan_review_context(
        &db,
        BuildPlanReviewContextInput {
            company_id: company_id.to_string(),
            issue_id: issue_id.to_string(),
            issue_work_mode: Some("planning".to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("build")
    .expect("some");

    assert!(ctx.truncated, "expected truncated flag to be set");
    // The last comment should have body_truncated = true (or the loop broke).
    let last = ctx.threads[0].comments.last().unwrap();
    assert!(last.body_truncated);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn interaction_is_picked_up_when_not_in_planning_mode() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "int").await;
    let issue_id = insert_issue(&pool, company_id, "int").await;
    let (doc_id, rev_id) = insert_document(&pool, company_id).await;
    link_issue_document(&pool, company_id, issue_id, doc_id).await;
    let user_id = insert_user(&pool, "int").await;
    let _t = insert_thread(&pool, company_id, issue_id, doc_id, rev_id, &user_id, "sel").await;

    let payload = json!({
        "target": {
            "type": "issue_document",
            "key": "plan",
            "issueId": issue_id.to_string(),
            "documentId": doc_id.to_string(),
            "revisionId": rev_id.to_string(),
            "revisionNumber": 1
        }
    });
    let interaction_id = insert_interaction(
        &pool,
        company_id,
        issue_id,
        doc_id,
        rev_id,
        payload,
        json!({}),
    )
    .await;

    let ctx = build_plan_review_context(
        &db,
        BuildPlanReviewContextInput {
            company_id: company_id.to_string(),
            issue_id: issue_id.to_string(),
            interaction_id: Some(interaction_id.to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("build")
    .expect("some");

    let interaction = ctx.interaction.expect("interaction should be present");
    assert_eq!(interaction.id, interaction_id.to_string());
    assert_eq!(interaction.kind, "plan_review");
    assert_eq!(interaction.target.key, "plan");
    assert_eq!(interaction.target.issue_id, issue_id.to_string());
    // status='pending' → acceptedTargetRevision is null
    assert!(interaction.accepted_target_revision.is_none());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn accepted_interaction_sets_accepted_target_revision() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "acc").await;
    let issue_id = insert_issue(&pool, company_id, "acc").await;
    let (doc_id, rev_id) = insert_document(&pool, company_id).await;
    link_issue_document(&pool, company_id, issue_id, doc_id).await;

    let payload = json!({
        "target": {
            "type": "issue_document",
            "key": "plan",
            "issueId": issue_id.to_string(),
            "documentId": doc_id.to_string(),
            "revisionId": rev_id.to_string(),
            "revisionNumber": 1
        }
    });
    let interaction_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issue_thread_interactions \
            (id, company_id, issue_id, kind, status, payload, result, created_at, updated_at) \
         VALUES ($1, $2, $3, 'plan_review', 'accepted', $4, '{\"outcome\":\"approved\"}', now(), now())",
    )
    .bind(interaction_id)
    .bind(company_id)
    .bind(issue_id)
    .bind(payload)
    .execute(&pool)
    .await
    .expect("insert accepted interaction");

    let interaction = get_plan_interaction_context(
        &db,
        GetPlanInteractionInput {
            company_id: &company_id.to_string(),
            issue_id: &issue_id.to_string(),
            interaction_id: &interaction_id.to_string(),
        },
    )
    .await
    .expect("get")
    .expect("some");

    assert_eq!(interaction.status, "accepted");
    assert!(interaction.accepted_target_revision.is_some());
    let accepted = interaction.accepted_target_revision.unwrap();
    assert_eq!(accepted.revision_number, Some(1));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn interaction_with_wrong_issue_id_is_filtered_out() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "miss").await;
    let issue_id = insert_issue(&pool, company_id, "miss").await;
    let (doc_id, rev_id) = insert_document(&pool, company_id).await;
    link_issue_document(&pool, company_id, issue_id, doc_id).await;

    let payload = json!({
        "target": {
            "type": "issue_document",
            "key": "plan",
            "issueId": "00000000-0000-0000-0000-000000000000",
            "documentId": doc_id.to_string(),
            "revisionId": rev_id.to_string(),
            "revisionNumber": 1
        }
    });
    let interaction_id = insert_interaction(
        &pool,
        company_id,
        issue_id,
        doc_id,
        rev_id,
        payload,
        json!({}),
    )
    .await;

    let interaction = get_plan_interaction_context(
        &db,
        GetPlanInteractionInput {
            company_id: &company_id.to_string(),
            issue_id: &issue_id.to_string(),
            interaction_id: &interaction_id.to_string(),
        },
    )
    .await
    .expect("get");
    // Target's issueId doesn't match → read_plan_target returns None → interaction is None.
    assert!(interaction.is_none());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn returns_none_when_no_plan_document_attached() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "nodoc").await;
    let issue_id = insert_issue(&pool, company_id, "nodoc").await;
    // no link_issue_document

    let ctx = build_plan_review_context(
        &db,
        BuildPlanReviewContextInput {
            company_id: company_id.to_string(),
            issue_id: issue_id.to_string(),
            issue_work_mode: Some("planning".to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("build");
    assert!(ctx.is_none());

    cleanup(&pool, company_id).await;
}
