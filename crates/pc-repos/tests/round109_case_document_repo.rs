//! Round 109 集成测试：验证 CaseRepo::list_documents / get_document /
//! lock_document / unlock_document 全部走真实 schema 路径。

use pc_db::Db;
use pc_repos::case::{CaseActor, CaseEventKind, CaseRepo, NewCaseRecord};
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r109-{tag}-{id}"))
        .bind(format!("R109{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

async fn insert_case(db: &Db, company_id: Uuid) -> Uuid {
    let case_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO cases (id, company_id, case_number, identifier, case_type, key, title, status) \
         VALUES ($1, $2, 1, 'CASE-001', 'requirement', NULL, 'test', 'draft')",
    )
    .bind(case_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .expect("insert case");
    case_id
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

async fn link_case_document(
    db: &Db,
    company_id: Uuid,
    case_id: Uuid,
    doc_id: Uuid,
    key: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO case_documents (id, company_id, case_id, document_id, key) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(company_id)
    .bind(case_id)
    .bind(doc_id)
    .bind(key)
    .execute(db.pool())
    .await
    .expect("link");
    id
}

/// 1. list_documents：按 key ASC
#[tokio::test(flavor = "current_thread")]
async fn case_documents_list_orders_by_key_asc() {
    let db = db().await;
    let cid = insert_company(&db, "list").await;
    let case_id = insert_case(&db, cid).await;
    let doc_a = insert_document(&db, cid).await;
    let doc_b = insert_document(&db, cid).await;
    let doc_c = insert_document(&db, cid).await;
    link_case_document(&db, cid, case_id, doc_a, "zebra").await;
    link_case_document(&db, cid, case_id, doc_b, "alpha").await;
    link_case_document(&db, cid, case_id, doc_c, "mike").await;

    let repo = CaseRepo::new(&db);
    let rows = repo.list_documents(cid, case_id).await.expect("list");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].key, "alpha");
    assert_eq!(rows[1].key, "mike");
    assert_eq!(rows[2].key, "zebra");
}

/// 2. get_document：精确 (company_id, case_id, key)
#[tokio::test(flavor = "current_thread")]
async fn case_documents_get_by_key() {
    let db = db().await;
    let cid = insert_company(&db, "get").await;
    let case_id = insert_case(&db, cid).await;
    let doc_id = insert_document(&db, cid).await;
    link_case_document(&db, cid, case_id, doc_id, "design").await;

    let repo = CaseRepo::new(&db);
    let row = repo
        .get_document(cid, case_id, "design")
        .await
        .expect("get")
        .expect("present");
    assert_eq!(row.document_id, doc_id);
    assert_eq!(row.key, "design");

    // 不存在 key 返 None
    let none = repo
        .get_document(cid, case_id, "missing")
        .await
        .expect("get");
    assert!(none.is_none());
}

/// 3. lock_document：UPDATE + 发 event
#[tokio::test(flavor = "current_thread")]
async fn case_documents_lock_emits_event() {
    let db = db().await;
    let cid = insert_company(&db, "lock").await;
    let case_id = insert_case(&db, cid).await;
    let doc_id = insert_document(&db, cid).await;
    link_case_document(&db, cid, case_id, doc_id, "design").await;

    let repo = CaseRepo::new(&db);
    let n = repo
        .lock_document(cid, case_id, "design")
        .await
        .expect("lock");
    assert!(n, "should affect 1 row");

    // 验证 case_event 已写入
    let (kind,): (String,) = sqlx::query_as(
        "SELECT kind FROM case_events WHERE case_id=$1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(case_id)
    .fetch_one(db.pool())
    .await
    .expect("query event");
    assert_eq!(kind, "document_locked");
}

/// 4. lock_document：未存在的 key 返 Ok(false)
#[tokio::test(flavor = "current_thread")]
async fn case_documents_lock_missing_key_returns_false() {
    let db = db().await;
    let cid = insert_company(&db, "lock-miss").await;
    let case_id = insert_case(&db, cid).await;
    let repo = CaseRepo::new(&db);
    let n = repo
        .lock_document(cid, case_id, "ghost")
        .await
        .expect("lock");
    assert!(!n);
}

/// 5. unlock_document：发 event
#[tokio::test(flavor = "current_thread")]
async fn case_documents_unlock_emits_event() {
    let db = db().await;
    let cid = insert_company(&db, "unlock").await;
    let case_id = insert_case(&db, cid).await;
    let doc_id = insert_document(&db, cid).await;
    link_case_document(&db, cid, case_id, doc_id, "design").await;

    let repo = CaseRepo::new(&db);
    let n = repo
        .unlock_document(cid, case_id, "design")
        .await
        .expect("unlock");
    assert!(n);

    let (kind,): (String,) = sqlx::query_as(
        "SELECT kind FROM case_events WHERE case_id=$1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(case_id)
    .fetch_one(db.pool())
    .await
    .expect("query event");
    assert_eq!(kind, "document_unlocked");
}

/// 6. lock_document：跨 case 隔离
#[tokio::test(flavor = "current_thread")]
async fn case_documents_lock_isolates_across_cases() {
    let db = db().await;
    let cid = insert_company(&db, "cross").await;
    let case_a = insert_case(&db, cid).await;
    let case_b = insert_case(&db, cid).await;
    let doc_id = insert_document(&db, cid).await;
    link_case_document(&db, cid, case_a, doc_id, "shared").await;
    let repo = CaseRepo::new(&db);

    // 锁 case_a 的 document
    assert!(repo
        .lock_document(cid, case_a, "shared")
        .await
        .expect("lock a"));
    // case_b 锁同样的 (doc_id, "shared") 不应存在
    assert!(!repo
        .lock_document(cid, case_b, "shared")
        .await
        .expect("lock b"));
}
