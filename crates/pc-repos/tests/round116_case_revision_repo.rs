//! Round 116 集成测试：验证 CaseRepo 3 个 case_revisions 新方法。
//! - list_document_revisions
//! - get_document_revision_body
//! - restore_document_revision (复合 tx)

use pc_db::Db;
use pc_repos::case::CaseRepo;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r116-{tag}-{id}"))
        .bind(format!("R116{}", &id.simple().to_string()[..4]))
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

async fn insert_document(db: &Db, company_id: Uuid, body: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO documents (id, company_id, format, latest_body) \
         VALUES ($1, $2, 'markdown', $3)",
    )
    .bind(id)
    .bind(company_id)
    .bind(body)
    .execute(db.pool())
    .await
    .expect("insert document");
    id
}

async fn insert_revision(db: &Db, company_id: Uuid, document_id: Uuid, n: i32, body: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO document_revisions (id, company_id, document_id, revision_number, body, change_summary) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id).bind(company_id).bind(document_id).bind(n).bind(body)
    .bind(format!("rev-{n}"))
    .execute(db.pool()).await.expect("insert revision");
    id
}

/// 1. list_document_revisions: 按 revision_number DESC
#[tokio::test(flavor = "current_thread")]
async fn list_document_revisions_orders_desc() {
    let db = db().await;
    let cid = insert_company(&db, "lst").await;
    let case_id = insert_case(&db, cid).await;
    let did = insert_document(&db, cid, "head").await;
    insert_revision(&db, cid, did, 1, "v1").await;
    insert_revision(&db, cid, did, 2, "v2").await;
    insert_revision(&db, cid, did, 3, "v3").await;
    let _ = case_id;
    let repo = CaseRepo::new(&db);
    let rows = repo
        .list_document_revisions(cid, did, 200)
        .await
        .expect("list");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].revision_number, 3);
    assert_eq!(rows[1].revision_number, 2);
    assert_eq!(rows[2].revision_number, 1);
}

/// 2. list_document_revisions: 跨 document 隔离
#[tokio::test(flavor = "current_thread")]
async fn list_document_revisions_isolates() {
    let db = db().await;
    let cid = insert_company(&db, "iso").await;
    let d1 = insert_document(&db, cid, "d1").await;
    let d2 = insert_document(&db, cid, "d2").await;
    insert_revision(&db, cid, d1, 1, "x").await;
    insert_revision(&db, cid, d1, 2, "y").await;
    insert_revision(&db, cid, d2, 1, "z").await;
    let repo = CaseRepo::new(&db);
    let d1_rows = repo
        .list_document_revisions(cid, d1, 200)
        .await
        .expect("d1");
    let d2_rows = repo
        .list_document_revisions(cid, d2, 200)
        .await
        .expect("d2");
    assert_eq!(d1_rows.len(), 2);
    assert_eq!(d2_rows.len(), 1);
}

/// 3. list_document_revisions: limit 生效
#[tokio::test(flavor = "current_thread")]
async fn list_document_revisions_limit() {
    let db = db().await;
    let cid = insert_company(&db, "lim").await;
    let did = insert_document(&db, cid, "head").await;
    insert_revision(&db, cid, did, 1, "a").await;
    insert_revision(&db, cid, did, 2, "b").await;
    insert_revision(&db, cid, did, 3, "c").await;
    let repo = CaseRepo::new(&db);
    let rows = repo
        .list_document_revisions(cid, did, 2)
        .await
        .expect("list");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].revision_number, 3);
    assert_eq!(rows[1].revision_number, 2);
}

/// 4. get_document_revision_body: 找到 / 找不到
#[tokio::test(flavor = "current_thread")]
async fn get_document_revision_body_round_trip() {
    let db = db().await;
    let cid = insert_company(&db, "get").await;
    let did = insert_document(&db, cid, "head").await;
    let rid = insert_revision(&db, cid, did, 1, "hello body").await;
    let repo = CaseRepo::new(&db);
    let (body, title) = repo
        .get_document_revision_body(cid, did, rid)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(body, "hello body");
    assert!(title.is_none());
    let none = repo
        .get_document_revision_body(cid, did, Uuid::new_v4())
        .await
        .expect("get");
    assert!(none.is_none());
}

/// 5. get_document_revision_body: 跨 company 隔离
#[tokio::test(flavor = "current_thread")]
async fn get_document_revision_body_cross_company() {
    let db = db().await;
    let cid1 = insert_company(&db, "c1").await;
    let cid2 = insert_company(&db, "c2").await;
    let did = insert_document(&db, cid1, "x").await;
    let rid = insert_revision(&db, cid1, did, 1, "y").await;
    let repo = CaseRepo::new(&db);
    // 用错 company 查返 None
    let none = repo
        .get_document_revision_body(cid2, did, rid)
        .await
        .expect("get");
    assert!(none.is_none());
    // 正确 company 能查
    let some = repo
        .get_document_revision_body(cid1, did, rid)
        .await
        .expect("get");
    assert!(some.is_some());
}

/// 6. restore_document_revision: 复合事务（next_no + INSERT + UPDATE + event）
#[tokio::test(flavor = "current_thread")]
async fn restore_document_revision_creates_new_revision() {
    let db = db().await;
    let cid = insert_company(&db, "rst").await;
    let case_id = insert_case(&db, cid).await;
    let did = insert_document(&db, cid, "current body").await;
    let src_rid = insert_revision(&db, cid, did, 1, "original body").await;

    // 把 documents.latest_body 设为 current body, latest_revision_number = 2
    sqlx::query(
        "UPDATE documents SET latest_revision_id = (SELECT id FROM document_revisions WHERE document_id = $1 AND revision_number = 2 LIMIT 1),                 latest_revision_number = 2 WHERE id = $1",
    )
    .bind(did)
    .execute(db.pool())
    .await
    .expect("upd");
    // 插入 revision 2 with current body
    insert_revision(&db, cid, did, 2, "current body").await;

    let repo = CaseRepo::new(&db);
    let (new_rid, next_no) = repo
        .restore_document_revision(
            cid,
            case_id,
            "design",
            did,
            "original body",
            Some("orig title"),
            "restored from rev 1",
            src_rid,
        )
        .await
        .expect("restore");
    assert_eq!(next_no, 3);

    // 验证新 revision 存在
    let (body, title): (String, Option<String>) =
        sqlx::query_as("SELECT body, title FROM document_revisions WHERE id = $1")
            .bind(new_rid)
            .fetch_one(db.pool())
            .await
            .expect("query");
    assert_eq!(body, "original body");
    assert_eq!(title, Some("orig title".to_owned()));

    // 验证 documents 指针更新
    let (latest_body, latest_num): (String, i32) =
        sqlx::query_as("SELECT latest_body, latest_revision_number FROM documents WHERE id = $1")
            .bind(did)
            .fetch_one(db.pool())
            .await
            .expect("query");
    assert_eq!(latest_body, "original body");
    assert_eq!(latest_num, 3);

    // 验证 event 写入
    let (kind, payload): (String, serde_json::Value) = sqlx::query_as(
        "SELECT kind, payload FROM case_events WHERE case_id = $1 AND kind = 'document_revised'",
    )
    .bind(case_id)
    .fetch_one(db.pool())
    .await
    .expect("query");
    assert_eq!(kind, "document_revised");
    assert_eq!(payload["key"], serde_json::json!("design"));
    assert_eq!(
        payload["restoredFromRevisionId"],
        serde_json::json!(src_rid.to_string())
    );
    assert_eq!(
        payload["newRevisionId"],
        serde_json::json!(new_rid.to_string())
    );
}
