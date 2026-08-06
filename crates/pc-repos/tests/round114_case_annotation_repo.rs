//! Round 114 集成测试：验证 CaseRepo 9 个 case annotation 新方法。

use pc_db::Db;
use pc_repos::case::{
    CaseAnnotationPatch, CaseRepo, NewCaseAnnotationComment, NewCaseAnnotationThread,
};
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r114-{tag}-{id}"))
        .bind(format!("R114{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

async fn insert_case(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO cases (id, company_id, case_number, identifier, case_type, key, title, status) \
         VALUES ($1, $2, 1, 'CASE-001', 'requirement', NULL, 'test', 'draft')",
    )
    .bind(id).bind(company_id)
    .execute(db.pool()).await.expect("insert case");
    id
}

async fn insert_document(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO documents (id, company_id, format, latest_body) \
         VALUES ($1, $2, 'markdown', '# test')",
    )
    .bind(id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .expect("insert document");
    id
}

async fn link_case_document(db: &Db, company_id: Uuid, case_id: Uuid, doc_id: Uuid, key: &str) {
    sqlx::query(
        "INSERT INTO case_documents (id, company_id, case_id, document_id, key) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(Uuid::new_v4())
    .bind(company_id)
    .bind(case_id)
    .bind(doc_id)
    .bind(key)
    .execute(db.pool())
    .await
    .expect("link case_doc");
}

async fn insert_thread(
    db: &Db,
    company_id: Uuid,
    case_id: Uuid,
    document_id: Uuid,
    document_key: &str,
    status: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO document_annotation_threads \
            (id, company_id, case_id, document_id, document_key, status, \
             original_revision_number, current_revision_number, selected_text, \
             prefix_text, suffix_text, normalized_start, normalized_end, \
             markdown_start, markdown_end, anchor_confidence, anchor_selector) \
         VALUES ($1, $2, $3, $4, $5, $6, 1, 1, 'sel', '', '', 0, 3, 0, 3, 'exact', '{}'::jsonb)",
    )
    .bind(id)
    .bind(company_id)
    .bind(case_id)
    .bind(document_id)
    .bind(document_key)
    .bind(status)
    .execute(db.pool())
    .await
    .expect("insert thread");
    id
}

async fn insert_comment(
    db: &Db,
    company_id: Uuid,
    case_id: Uuid,
    thread_id: Uuid,
    document_id: Uuid,
    body: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO document_annotation_comments \
            (id, company_id, case_id, thread_id, document_id, body, author_type) \
         VALUES ($1, $2, $3, $4, $5, $6, 'user')",
    )
    .bind(id)
    .bind(company_id)
    .bind(case_id)
    .bind(thread_id)
    .bind(document_id)
    .bind(body)
    .execute(db.pool())
    .await
    .expect("insert comment");
    id
}

/// 1. get_case_company_id
#[tokio::test(flavor = "current_thread")]
async fn case_get_company_id_round_trip() {
    let db = db().await;
    let cid = insert_company(&db, "cid").await;
    let case_id = insert_case(&db, cid).await;
    let repo = CaseRepo::new(&db);
    assert_eq!(
        repo.get_case_company_id(case_id)
            .await
            .expect("get")
            .expect("present"),
        cid
    );
    assert!(repo
        .get_case_company_id(Uuid::new_v4())
        .await
        .expect("get")
        .is_none());
}

/// 2. resolve_case_document_id
#[tokio::test(flavor = "current_thread")]
async fn case_resolve_document_id_round_trip() {
    let db = db().await;
    let cid = insert_company(&db, "rdi").await;
    let case_id = insert_case(&db, cid).await;
    let did = insert_document(&db, cid).await;
    link_case_document(&db, cid, case_id, did, "design").await;
    let repo = CaseRepo::new(&db);
    let (got_cid, got_did) = repo
        .resolve_case_document_id(case_id, "design")
        .await
        .expect("get")
        .expect("present");
    assert_eq!(got_cid, cid);
    assert_eq!(got_did, did);
    assert!(repo
        .resolve_case_document_id(case_id, "missing")
        .await
        .expect("get")
        .is_none());
}

/// 3. list_case_annotation_threads + status filter
#[tokio::test(flavor = "current_thread")]
async fn case_annotation_threads_list_filters_by_status() {
    let db = db().await;
    let cid = insert_company(&db, "lst").await;
    let case_id = insert_case(&db, cid).await;
    let did = insert_document(&db, cid).await;
    link_case_document(&db, cid, case_id, did, "spec").await;
    insert_thread(&db, cid, case_id, did, "spec", "open").await;
    insert_thread(&db, cid, case_id, did, "spec", "open").await;
    insert_thread(&db, cid, case_id, did, "spec", "resolved").await;
    insert_thread(&db, cid, case_id, did, "other", "open").await;
    let repo = CaseRepo::new(&db);
    let all = repo
        .list_case_annotation_threads(case_id, "spec", None, 200)
        .await
        .expect("all");
    assert_eq!(all.len(), 2);
    let open = repo
        .list_case_annotation_threads(case_id, "spec", Some("open"), 200)
        .await
        .expect("open");
    assert_eq!(open.len(), 2);
}

/// 4. get_case_annotation_thread
#[tokio::test(flavor = "current_thread")]
async fn case_annotation_thread_get() {
    let db = db().await;
    let cid = insert_company(&db, "get").await;
    let case_id = insert_case(&db, cid).await;
    let did = insert_document(&db, cid).await;
    let tid = insert_thread(&db, cid, case_id, did, "spec", "open").await;
    let repo = CaseRepo::new(&db);
    let row = repo
        .get_case_annotation_thread(case_id, tid, "spec")
        .await
        .expect("get")
        .expect("present");
    assert_eq!(row.id, tid);
    assert_eq!(row.document_key, "spec");
    assert_eq!(row.status, "open");
    let none = repo
        .get_case_annotation_thread(case_id, tid, "wrong-key")
        .await
        .expect("get");
    assert!(none.is_none());
}

/// 5. list_case_thread_comments + bulk
#[tokio::test(flavor = "current_thread")]
async fn case_thread_comments_list_and_bulk() {
    let db = db().await;
    let cid = insert_company(&db, "cmt").await;
    let case_id = insert_case(&db, cid).await;
    let did = insert_document(&db, cid).await;
    let t1 = insert_thread(&db, cid, case_id, did, "spec", "open").await;
    let t2 = insert_thread(&db, cid, case_id, did, "spec", "open").await;
    insert_comment(&db, cid, case_id, t1, did, "c1").await;
    insert_comment(&db, cid, case_id, t1, did, "c2").await;
    insert_comment(&db, cid, case_id, t2, did, "c3").await;
    let repo = CaseRepo::new(&db);
    let t1_c = repo.list_case_thread_comments(t1).await.expect("t1");
    assert_eq!(t1_c.len(), 2);
    let bulk = repo
        .list_case_thread_comments_bulk(&[t1, t2])
        .await
        .expect("bulk");
    assert_eq!(bulk.len(), 3);
}

/// 6. create_case_annotation_thread + get
#[tokio::test(flavor = "current_thread")]
async fn case_annotation_thread_create_get() {
    let db = db().await;
    let cid = insert_company(&db, "crt").await;
    let case_id = insert_case(&db, cid).await;
    let did = insert_document(&db, cid).await;
    let repo = CaseRepo::new(&db);
    let input = NewCaseAnnotationThread {
        company_id: cid,
        case_id,
        document_id: did,
        document_key: "spec".to_owned(),
        status: Some("open".to_owned()),
        original_revision_id: None,
        revision_number: 1,
        selected_text: "hello".to_owned(),
        prefix_text: None,
        suffix_text: None,
        normalized_start: 0,
        normalized_end: 5,
        markdown_start: 0,
        markdown_end: 5,
        anchor_confidence: Some("exact".to_owned()),
        anchor_selector: Some(json!({"type": "text"})),
    };
    let tid = repo
        .create_case_annotation_thread(&input)
        .await
        .expect("create");
    let row = repo
        .get_case_annotation_thread(case_id, tid, "spec")
        .await
        .expect("get")
        .expect("present");
    assert_eq!(row.selected_text, "hello");
    assert_eq!(row.anchor_confidence, "exact");
}

/// 7. create_case_thread_comment
#[tokio::test(flavor = "current_thread")]
async fn case_thread_comment_create() {
    let db = db().await;
    let cid = insert_company(&db, "ccrt").await;
    let case_id = insert_case(&db, cid).await;
    let did = insert_document(&db, cid).await;
    let tid = insert_thread(&db, cid, case_id, did, "spec", "open").await;
    let repo = CaseRepo::new(&db);
    let input = NewCaseAnnotationComment {
        company_id: cid,
        case_id,
        thread_id: tid,
        document_id: did,
        body: "first".to_owned(),
        author_type: "user".to_owned(),
        author_user_id: Some("u1".to_owned()),
        author_agent_id: None,
    };
    let cid_new = repo
        .create_case_thread_comment(&input)
        .await
        .expect("create");
    let (body, author): (String, String) =
        sqlx::query_as("SELECT body, author_type FROM document_annotation_comments WHERE id = $1")
            .bind(cid_new)
            .fetch_one(db.pool())
            .await
            .expect("query");
    assert_eq!(body, "first");
    assert_eq!(author, "user");
}

/// 8. update_case_annotation_thread: resolved 触发 resolved_at
#[tokio::test(flavor = "current_thread")]
async fn case_annotation_thread_update_resolved() {
    let db = db().await;
    let cid = insert_company(&db, "upd").await;
    let case_id = insert_case(&db, cid).await;
    let did = insert_document(&db, cid).await;
    let tid = insert_thread(&db, cid, case_id, did, "spec", "open").await;
    let repo = CaseRepo::new(&db);
    let patch = CaseAnnotationPatch {
        status: Some("resolved".to_owned()),
        ..Default::default()
    };
    let n = repo
        .update_case_annotation_thread(case_id, tid, "spec", &patch)
        .await
        .expect("upd");
    assert_eq!(n, 1);
    let resolved_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT resolved_at FROM document_annotation_threads WHERE id = $1")
            .bind(tid)
            .fetch_one(db.pool())
            .await
            .expect("query");
    assert!(resolved_at.is_some());
}

/// 9. update_case_annotation_thread: open 清除 resolved_at
#[tokio::test(flavor = "current_thread")]
async fn case_annotation_thread_update_open_clears() {
    let db = db().await;
    let cid = insert_company(&db, "clr").await;
    let case_id = insert_case(&db, cid).await;
    let did = insert_document(&db, cid).await;
    let tid = insert_thread(&db, cid, case_id, did, "spec", "resolved").await;
    let repo = CaseRepo::new(&db);
    let patch = CaseAnnotationPatch {
        status: Some("open".to_owned()),
        ..Default::default()
    };
    let n = repo
        .update_case_annotation_thread(case_id, tid, "spec", &patch)
        .await
        .expect("upd");
    assert_eq!(n, 1);
    let resolved_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT resolved_at FROM document_annotation_threads WHERE id = $1")
            .bind(tid)
            .fetch_one(db.pool())
            .await
            .expect("query");
    assert!(resolved_at.is_none());
}

/// 10. get_case_thread_document_id
#[tokio::test(flavor = "current_thread")]
async fn case_thread_document_id_round_trip() {
    let db = db().await;
    let cid = insert_company(&db, "tdid").await;
    let case_id = insert_case(&db, cid).await;
    let did = insert_document(&db, cid).await;
    let tid = insert_thread(&db, cid, case_id, did, "spec", "open").await;
    let repo = CaseRepo::new(&db);
    let back = repo
        .get_case_thread_document_id(case_id, tid, "spec")
        .await
        .expect("get")
        .expect("present");
    assert_eq!(back, did);
    assert!(repo
        .get_case_thread_document_id(case_id, tid, "wrong")
        .await
        .expect("get")
        .is_none());
}
