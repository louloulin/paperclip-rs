//! Round 119 集成测试：验证 CaseRepo::list_children / list_all_for_tree /
//! list_case_document_annotations / list_issue_cases / link_document upsert 路径。

use pc_db::Db;
use pc_repos::case::CaseRepo;
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
        .bind(format!("r119-{tag}-{id}"))
        .bind(format!("R119{}", &id.simple().to_string()[..4]))
        .execute(db.pool()).await.expect("insert company");
    id
}

async fn insert_case(db: &Db, company_id: Uuid, parent: Option<Uuid>, status: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO cases (id, company_id, case_number, identifier, case_type, key, title, status, parent_case_id) \
         VALUES ($1, $2, 1, 'CASE-001', 'requirement', NULL, 'test', $3, $4)",
    )
    .bind(id).bind(company_id).bind(status).bind(parent)
    .execute(db.pool()).await.expect("insert case");
    id
}

async fn insert_document(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO documents (id, company_id, format, latest_body) VALUES ($1, $2, 'markdown', '# r119')")
        .bind(id).bind(company_id)
        .execute(db.pool()).await.expect("insert document");
    id
}

async fn link_case_document(db: &Db, company_id: Uuid, case_id: Uuid, doc_id: Uuid, key: &str) {
    sqlx::query(
        "INSERT INTO case_documents (id, company_id, case_id, document_id, key) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(Uuid::new_v4()).bind(company_id).bind(case_id).bind(doc_id).bind(key)
    .execute(db.pool()).await.expect("link");
}

async fn insert_doc_annotation(db: &Db, doc_id: Uuid, kind: &str, payload: serde_json::Value) {
    sqlx::query(
        "INSERT INTO document_annotations (id, document_id, kind, payload) \
         VALUES ($1, $2, $3, $4::jsonb)",
    )
    .bind(Uuid::new_v4()).bind(doc_id).bind(kind).bind(payload)
    .execute(db.pool()).await.expect("insert doc_annotation");
}

async fn insert_issue(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, identifier, title, status, kind) \
         VALUES ($1, $2, $3, 'issue', 'open', 'task')",
    )
    .bind(id).bind(company_id)
    .bind(format!("ISS-{}", &id.simple().to_string()[..6]))
    .execute(db.pool()).await.expect("insert issue");
    id
}

async fn link_issue_case(db: &Db, company_id: Uuid, case_id: Uuid, issue_id: Uuid) {
    sqlx::query(
        "INSERT INTO case_issue_links (id, company_id, case_id, issue_id, role) \
         VALUES ($1, $2, $3, $4, 'reference')",
    )
    .bind(Uuid::new_v4()).bind(company_id).bind(case_id).bind(issue_id)
    .execute(db.pool()).await.expect("link");
}

/// 1. list_children — 直系子 case
#[tokio::test(flavor = "current_thread")]
async fn list_children_returns_direct_children() {
    let db = db().await;
    let cid = insert_company(&db, "children").await;
    let root = insert_case(&db, cid, None, "draft").await;
    let child1 = insert_case(&db, cid, Some(root), "in_progress").await;
    let child2 = insert_case(&db, cid, Some(root), "draft").await;
    let grand = insert_case(&db, cid, Some(child1), "draft").await;
    let rows = CaseRepo::new(&db).list_children(cid, root).await.expect("list children");
    assert_eq!(rows.len(), 2);
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    assert!(ids.contains(&child1));
    assert!(ids.contains(&child2));
    assert!(!ids.contains(&grand));
}

/// 2. list_all_for_tree — 公司全部 cases（用于构建 children tree）
#[tokio::test(flavor = "current_thread")]
async fn list_all_for_tree_returns_all_cases() {
    let db = db().await;
    let cid = insert_company(&db, "tree").await;
    let root = insert_case(&db, cid, None, "draft").await;
    let _child = insert_case(&db, cid, Some(root), "in_progress").await;
    let rows = CaseRepo::new(&db).list_all_for_tree(cid).await.expect("list all for tree");
    assert!(rows.len() >= 2);
}

/// 3. list_case_document_annotations — 通过 case_id + key 查批注
#[tokio::test(flavor = "current_thread")]
async fn list_case_document_annotations_filters_by_case_key() {
    let db = db().await;
    let cid = insert_company(&db, "ann").await;
    let case_id = insert_case(&db, cid, None, "draft").await;
    let doc_id = insert_document(&db, cid).await;
    link_case_document(&db, cid, case_id, doc_id, "spec.md").await;
    insert_doc_annotation(&db, doc_id, "highlight", json!({"text": "see here"})).await;
    let rows = CaseRepo::new(&db)
        .list_case_document_annotations(case_id, "spec.md")
        .await
        .expect("list annotations");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, "highlight");
}

/// 4. list_issue_cases — 反向查询 issue 关联的 cases
#[tokio::test(flavor = "current_thread")]
async fn list_issue_cases_returns_linked_cases() {
    let db = db().await;
    let cid = insert_company(&db, "iss-cases").await;
    let issue_id = insert_issue(&db, cid).await;
    let case1 = insert_case(&db, cid, None, "draft").await;
    let case2 = insert_case(&db, cid, None, "in_review").await;
    link_issue_case(&db, cid, case1, issue_id).await;
    link_issue_case(&db, cid, case2, issue_id).await;
    let rows = CaseRepo::new(&db)
        .list_issue_cases(issue_id)
        .await
        .expect("list issue cases");
    assert_eq!(rows.len(), 2);
    let ids: Vec<Uuid> = rows.iter().map(|r| r.case_id).collect();
    assert!(ids.contains(&case1));
    assert!(ids.contains(&case2));
}

/// 5. link_document — 替代 upsert_case_document 的 ON CONFLICT 行为
#[tokio::test(flavor = "current_thread")]
async fn link_document_upserts_on_conflict() {
    let db = db().await;
    let cid = insert_company(&db, "upsert").await;
    let case_id = insert_case(&db, cid, None, "draft").await;
    let doc1 = insert_document(&db, cid).await;
    let doc2 = insert_document(&db, cid).await;
    let row1 = CaseRepo::new(&db)
        .link_document(cid, case_id, doc1, "spec.md")
        .await
        .expect("link 1");
    let row2 = CaseRepo::new(&db)
        .link_document(cid, case_id, doc2, "spec.md")
        .await
        .expect("link 2 (upsert)");
    assert_eq!(row1.id, row2.id, "same id after upsert (ON CONFLICT)");
    assert_eq!(row2.document_id, doc2, "document_id updated");
}

/// 6. list_children — 空 case 返回空数组
#[tokio::test(flavor = "current_thread")]
async fn list_children_empty_when_no_children() {
    let db = db().await;
    let cid = insert_company(&db, "empty-children").await;
    let root = insert_case(&db, cid, None, "draft").await;
    let rows = CaseRepo::new(&db).list_children(cid, root).await.expect("list");
    assert!(rows.is_empty());
}
