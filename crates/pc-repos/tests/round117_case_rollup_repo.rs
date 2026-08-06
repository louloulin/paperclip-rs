//! Round 117 集成测试：验证 CaseRepo::get_case_rollup 复合聚合。

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
        .bind(format!("r117-{tag}-{id}"))
        .bind(format!("R117{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("insert company");
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

async fn insert_issue(db: &Db, company_id: Uuid, status: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, identifier, title, status, kind) \
         VALUES ($1, $2, $3, 'i', $4, 'task')",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("ISS-{}", &id.simple().to_string()[..6]))
    .bind(status)
    .execute(db.pool())
    .await
    .expect("insert issue");
    id
}

async fn link_issue(db: &Db, company_id: Uuid, case_id: Uuid, issue_id: Uuid) {
    sqlx::query(
        "INSERT INTO case_issue_links (id, company_id, case_id, issue_id, role) \
         VALUES ($1, $2, $3, $4, 'reference')",
    )
    .bind(Uuid::new_v4())
    .bind(company_id)
    .bind(case_id)
    .bind(issue_id)
    .execute(db.pool())
    .await
    .expect("link");
}

/// 1. 空 case rollup
#[tokio::test(flavor = "current_thread")]
async fn rollup_empty_case() {
    let db = db().await;
    let cid = insert_company(&db, "empty").await;
    let case_id = insert_case(&db, cid, None, "draft").await;
    let repo = CaseRepo::new(&db);
    let r = repo.get_case_rollup(cid, case_id).await.expect("rollup");
    assert_eq!(r.child_count, 0);
    assert_eq!(r.descendant_count, 0);
    assert_eq!(r.issue_link_count, 0);
    assert_eq!(r.open_issue_count, 0);
    assert!(r.status_breakdown.is_empty());
}

/// 2. child + descendant 计数
#[tokio::test(flavor = "current_thread")]
async fn rollup_child_and_descendant_counts() {
    let db = db().await;
    let cid = insert_company(&db, "tree").await;
    let root = insert_case(&db, cid, None, "active").await;
    let c1 = insert_case(&db, cid, Some(root), "draft").await;
    let c2 = insert_case(&db, cid, Some(root), "active").await;
    let c1_1 = insert_case(&db, cid, Some(c1), "done").await;
    let _ = c2;
    let repo = CaseRepo::new(&db);
    let r = repo.get_case_rollup(cid, root).await.expect("rollup");
    assert_eq!(r.child_count, 2, "root should have 2 direct children");
    assert_eq!(
        r.descendant_count, 3,
        "root should have 3 descendants total"
    );
}

/// 3. status breakdown 包含 self + 直接子
#[tokio::test(flavor = "current_thread")]
async fn rollup_status_breakdown() {
    let db = db().await;
    let cid = insert_company(&db, "brk").await;
    let root = insert_case(&db, cid, None, "active").await;
    insert_case(&db, cid, Some(root), "active").await;
    insert_case(&db, cid, Some(root), "draft").await;
    insert_case(&db, cid, Some(root), "done").await;
    let repo = CaseRepo::new(&db);
    let r = repo.get_case_rollup(cid, root).await.expect("rollup");
    // self=active + 1 child active = 2 active
    let map: std::collections::HashMap<String, i64> = r.status_breakdown.into_iter().collect();
    assert_eq!(map.get("active").copied(), Some(2));
    assert_eq!(map.get("draft").copied(), Some(1));
    assert_eq!(map.get("done").copied(), Some(1));
}

/// 4. issue link 计数
#[tokio::test(flavor = "current_thread")]
async fn rollup_issue_link_count() {
    let db = db().await;
    let cid = insert_company(&db, "ilnk").await;
    let case_id = insert_case(&db, cid, None, "active").await;
    let i1 = insert_issue(&db, cid, "open").await;
    let i2 = insert_issue(&db, cid, "done").await;
    let i3 = insert_issue(&db, cid, "open").await;
    link_issue(&db, cid, case_id, i1).await;
    link_issue(&db, cid, case_id, i2).await;
    link_issue(&db, cid, case_id, i3).await;
    let repo = CaseRepo::new(&db);
    let r = repo.get_case_rollup(cid, case_id).await.expect("rollup");
    assert_eq!(r.issue_link_count, 3);
    assert_eq!(r.open_issue_count, 2, "done 不算 open");
}

/// 5. open_issue_count 排除 cancelled/closed/done
#[tokio::test(flavor = "current_thread")]
async fn rollup_open_issue_excludes_terminal() {
    let db = db().await;
    let cid = insert_company(&db, "term").await;
    let case_id = insert_case(&db, cid, None, "active").await;
    let i1 = insert_issue(&db, cid, "open").await;
    let i2 = insert_issue(&db, cid, "in_progress").await;
    let i3 = insert_issue(&db, cid, "done").await;
    let i4 = insert_issue(&db, cid, "cancelled").await;
    let i5 = insert_issue(&db, cid, "closed").await;
    link_issue(&db, cid, case_id, i1).await;
    link_issue(&db, cid, case_id, i2).await;
    link_issue(&db, cid, case_id, i3).await;
    link_issue(&db, cid, case_id, i4).await;
    link_issue(&db, cid, case_id, i5).await;
    let repo = CaseRepo::new(&db);
    let r = repo.get_case_rollup(cid, case_id).await.expect("rollup");
    assert_eq!(r.issue_link_count, 5);
    assert_eq!(r.open_issue_count, 2, "只 open + in_progress 算 open");
}
