//! Round 113 集成测试：验证 CaseRepo 4 个 case_issue_links 新方法。
//! - record_issue_linked_event
//! - record_issue_unlinked_event
//! - list_issue_links_with_issue
//! - delete_issue_link_by_id

use pc_db::Db;
use pc_repos::case::{CaseLinkRole, CaseRepo};
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r113-{tag}-{id}"))
        .bind(format!("R113{}", &id.simple().to_string()[..4]))
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

async fn insert_issue(db: &Db, company_id: Uuid, title: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, identifier, title, status, kind) \
         VALUES ($1, $2, $3, $4, 'open', 'task')",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("ISS-{}", &id.simple().to_string()[..6]))
    .bind(title)
    .execute(db.pool())
    .await
    .expect("insert issue");
    id
}

async fn insert_link(db: &Db, company_id: Uuid, case_id: Uuid, issue_id: Uuid, role: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO case_issue_links (id, company_id, case_id, issue_id, role) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(company_id)
    .bind(case_id)
    .bind(issue_id)
    .bind(role)
    .execute(db.pool())
    .await
    .expect("insert link");
    id
}

/// 1. record_issue_linked_event 写入 + payload 正确
#[tokio::test(flavor = "current_thread")]
async fn record_issue_linked_event_writes() {
    let db = db().await;
    let cid = insert_company(&db, "lnk").await;
    let case_id = insert_case(&db, cid).await;
    let issue_id = insert_issue(&db, cid, "i1").await;

    let repo = CaseRepo::new(&db);
    let event_id = repo
        .record_issue_linked_event(cid, case_id, issue_id, "reference")
        .await
        .expect("record");
    let (kind, payload): (String, serde_json::Value) =
        sqlx::query_as("SELECT kind, payload FROM case_events WHERE id = $1")
            .bind(event_id)
            .fetch_one(db.pool())
            .await
            .expect("query");
    assert_eq!(kind, "issue_linked");
    assert_eq!(payload["issueId"], serde_json::json!(issue_id.to_string()));
    assert_eq!(payload["role"], serde_json::json!("reference"));
}

/// 2. record_issue_unlinked_event 写入
#[tokio::test(flavor = "current_thread")]
async fn record_issue_unlinked_event_writes() {
    let db = db().await;
    let cid = insert_company(&db, "unl").await;
    let case_id = insert_case(&db, cid).await;
    let issue_id = insert_issue(&db, cid, "i2").await;

    let repo = CaseRepo::new(&db);
    let event_id = repo
        .record_issue_unlinked_event(cid, case_id, issue_id)
        .await
        .expect("record");
    let (kind, payload): (String, serde_json::Value) =
        sqlx::query_as("SELECT kind, payload FROM case_events WHERE id = $1")
            .bind(event_id)
            .fetch_one(db.pool())
            .await
            .expect("query");
    assert_eq!(kind, "issue_unlinked");
    assert_eq!(payload["issueId"], serde_json::json!(issue_id.to_string()));
}

/// 3. list_issue_links_with_issue: JOIN 取 issue title/status
#[tokio::test(flavor = "current_thread")]
async fn list_issue_links_with_issue_joins() {
    let db = db().await;
    let cid = insert_company(&db, "lst").await;
    let case_id = insert_case(&db, cid).await;
    let i1 = insert_issue(&db, cid, "Issue 1").await;
    let i2 = insert_issue(&db, cid, "Issue 2").await;
    insert_link(&db, cid, case_id, i1, "origin").await;
    insert_link(&db, cid, case_id, i2, "work").await;

    let repo = CaseRepo::new(&db);
    let rows = repo
        .list_issue_links_with_issue(cid, case_id)
        .await
        .expect("list");
    assert_eq!(rows.len(), 2);
    // 按 created_at ASC，所以 i1 在前
    assert_eq!(rows[0].issue_id, i1);
    assert_eq!(rows[0].role, "origin");
    assert_eq!(rows[0].issue_title, Some("Issue 1".to_owned()));
    assert_eq!(rows[0].issue_status, Some("open".to_owned()));
    assert_eq!(rows[1].issue_id, i2);
    assert_eq!(rows[1].role, "work");
    assert_eq!(rows[1].issue_title, Some("Issue 2".to_owned()));
}

/// 4. list_issue_links_with_issue: 隔离跨 case
#[tokio::test(flavor = "current_thread")]
async fn list_issue_links_with_issue_isolates() {
    let db = db().await;
    let cid = insert_company(&db, "iso").await;
    let case_a = insert_case(&db, cid).await;
    let case_b = insert_case(&db, cid).await;
    let issue = insert_issue(&db, cid, "shared").await;
    insert_link(&db, cid, case_a, issue, "origin").await;
    insert_link(&db, cid, case_b, issue, "work").await;

    let repo = CaseRepo::new(&db);
    let a = repo
        .list_issue_links_with_issue(cid, case_a)
        .await
        .expect("a");
    let b = repo
        .list_issue_links_with_issue(cid, case_b)
        .await
        .expect("b");
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].role, "origin");
    assert_eq!(b.len(), 1);
    assert_eq!(b[0].role, "work");
}

/// 5. delete_issue_link_by_id: 返回 issue_id + 真删
#[tokio::test(flavor = "current_thread")]
async fn delete_issue_link_by_id_returns_issue() {
    let db = db().await;
    let cid = insert_company(&db, "del").await;
    let case_id = insert_case(&db, cid).await;
    let issue_id = insert_issue(&db, cid, "x").await;
    let link_id = insert_link(&db, cid, case_id, issue_id, "origin").await;

    let repo = CaseRepo::new(&db);
    let back = repo
        .delete_issue_link_by_id(cid, link_id)
        .await
        .expect("del")
        .expect("present");
    assert_eq!(back, issue_id);

    // 验证真删
    let n: i64 = sqlx::query_scalar("SELECT count(*)::bigint FROM case_issue_links WHERE id = $1")
        .bind(link_id)
        .fetch_one(db.pool())
        .await
        .expect("q");
    assert_eq!(n, 0);
}

/// 6. delete_issue_link_by_id: 未知 link_id 返 None
#[tokio::test(flavor = "current_thread")]
async fn delete_issue_link_by_id_missing() {
    let db = db().await;
    let cid = insert_company(&db, "miss").await;
    let repo = CaseRepo::new(&db);
    let none = repo
        .delete_issue_link_by_id(cid, Uuid::new_v4())
        .await
        .expect("del");
    assert!(none.is_none());
}

/// 7. delete_issue_link_by_id: 跨 company 隔离
#[tokio::test(flavor = "current_thread")]
async fn delete_issue_link_by_id_cross_company() {
    let db = db().await;
    let cid1 = insert_company(&db, "c1").await;
    let cid2 = insert_company(&db, "c2").await;
    let case_id = insert_case(&db, cid1).await;
    let issue_id = insert_issue(&db, cid1, "x").await;
    let link_id = insert_link(&db, cid1, case_id, issue_id, "origin").await;

    let repo = CaseRepo::new(&db);
    // 用错 company 删除应失败
    let none = repo
        .delete_issue_link_by_id(cid2, link_id)
        .await
        .expect("del");
    assert!(none.is_none());
    // 真实 company 仍可删
    let some = repo
        .delete_issue_link_by_id(cid1, link_id)
        .await
        .expect("del");
    assert!(some.is_some());
}

/// 8. link_issue + list_issue_links_with_issue 集成（验证已存在方法继续可用）
#[tokio::test(flavor = "current_thread")]
async fn link_issue_then_list_with_issue() {
    let db = db().await;
    let cid = insert_company(&db, "ink").await;
    let case_id = insert_case(&db, cid).await;
    let issue_id = insert_issue(&db, cid, "ink").await;

    let repo = CaseRepo::new(&db);
    let _link = repo
        .link_issue(cid, case_id, issue_id, CaseLinkRole::Work, None)
        .await
        .expect("link");

    let rows = repo
        .list_issue_links_with_issue(cid, case_id)
        .await
        .expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].role, "work");
    assert_eq!(rows[0].issue_title, Some("ink".to_owned()));
}
