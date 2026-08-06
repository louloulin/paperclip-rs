//! Round 213 集成测试：issue_tree_holds 按公司维度列出仓储语义。
//!
//! 覆盖：
//! - `IssueTreeHoldRepo::list_by_company` 默认仅 active+未释放
//! - include_released=true 包含已释放

use pc_db::Db;
use pc_repos::issue_tree_hold::{IssueTreeHoldRepo, NewIssueTreeHold};
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(format!("r213-{tag}-{id}"))
        .bind(format!("R213{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn insert_issue(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, origin_fingerprint) \
         VALUES ($1, $2, 'r213-issue', 'todo', 'medium', 'system', $3)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r213-fp-{id}"))
    .execute(db.pool())
    .await
    .expect("issue");
    id
}

async fn make_hold(db: &Db, company_id: Uuid, root_id: Uuid, mode: &str) -> Uuid {
    let repo = IssueTreeHoldRepo::new(db);
    let new = NewIssueTreeHold {
        company_id,
        root_issue_id: root_id,
        mode,
        reason: Some("r213 test"),
        release_policy: serde_json::Value::Null,
        created_by_user_id: "system",
    };
    repo.create(&new).await.expect("create hold")
}

// ===== 1) 默认仅返回 active =====
#[tokio::test(flavor = "current_thread")]
async fn list_by_company_default_active_only() {
    let db = db().await;
    let cid = insert_company(&db, "act").await;
    let i1 = insert_issue(&db, cid).await;
    let i2 = insert_issue(&db, cid).await;
    let h1 = make_hold(&db, cid, i1, "rerun").await;
    let h2 = make_hold(&db, cid, i2, "redo").await;

    let rows = IssueTreeHoldRepo::new(&db)
        .list_by_company(cid, false)
        .await
        .expect("list");
    assert_eq!(rows.len(), 2);

    // 释放 h1
    IssueTreeHoldRepo::new(&db)
        .release(i1, h1)
        .await
        .expect("release");

    // 默认（include_released=false）应仅看到 h2
    let active = IssueTreeHoldRepo::new(&db)
        .list_by_company(cid, false)
        .await
        .expect("active");
    assert_eq!(active.len(), 1);
    let _ = h2;
}

// ===== 2) include_released=true 包含已释放 =====
#[tokio::test(flavor = "current_thread")]
async fn list_by_company_include_released() {
    let db = db().await;
    let cid = insert_company(&db, "rel").await;
    let i1 = insert_issue(&db, cid).await;
    let h1 = make_hold(&db, cid, i1, "rerun").await;
    IssueTreeHoldRepo::new(&db)
        .release(i1, h1)
        .await
        .expect("release");

    let all = IssueTreeHoldRepo::new(&db)
        .list_by_company(cid, true)
        .await
        .expect("all");
    assert_eq!(all.len(), 1, "include_released=true 应见 1 条已释放的");
}

// ===== 3) 跨公司隔离 =====
#[tokio::test(flavor = "current_thread")]
async fn list_by_company_isolation() {
    let db = db().await;
    let c1 = insert_company(&db, "iso1").await;
    let c2 = insert_company(&db, "iso2").await;
    let i1 = insert_issue(&db, c1).await;
    let i2 = insert_issue(&db, c2).await;
    make_hold(&db, c1, i1, "rerun").await;
    make_hold(&db, c1, i1, "redo").await;
    make_hold(&db, c2, i2, "merge").await;

    let r1 = IssueTreeHoldRepo::new(&db)
        .list_by_company(c1, false)
        .await
        .expect("c1");
    let r2 = IssueTreeHoldRepo::new(&db)
        .list_by_company(c2, false)
        .await
        .expect("c2");
    assert_eq!(r1.len(), 2);
    assert_eq!(r2.len(), 1);
}
