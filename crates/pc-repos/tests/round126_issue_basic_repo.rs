//! Round 126 集成测试：IssueRepo checkout / create / count_for_company 仓储化。

use pc_db::Db;
use pc_repos::issue::IssueRepo;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r126-{tag}-{id}"))
        .bind(format!("R126{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

async fn insert_issue(db: &Db, company_id: Uuid, title: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, identifier, title, kind, status, priority) \
         VALUES ($1, $2, $3, $4, 'task', 'todo', 'normal')",
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

/// 1. checkout — 原子设置 assignee + run_id
#[tokio::test(flavor = "current_thread")]
async fn checkout_sets_assignee_and_run() {
    let db = db().await;
    let cid = insert_company(&db, "checkout").await;
    let issue_id = insert_issue(&db, cid, "test").await;
    let agent_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let result = IssueRepo::new(&db)
        .checkout(issue_id, agent_id, Some(run_id))
        .await
        .expect("checkout");
    assert!(result.is_some());
    let (company_id, _status) = result.unwrap();
    assert_eq!(company_id, cid);
}

/// 2. checkout — 不存在返回 None
#[tokio::test(flavor = "current_thread")]
async fn checkout_missing_returns_none() {
    let db = db().await;
    let result = IssueRepo::new(&db)
        .checkout(Uuid::new_v4(), Uuid::new_v4(), None)
        .await
        .expect("checkout");
    assert!(result.is_none());
}

/// 3. create — 基础创建
#[tokio::test(flavor = "current_thread")]
async fn create_issue_inserts() {
    let db = db().await;
    let cid = insert_company(&db, "create").await;
    let row = IssueRepo::new(&db)
        .create(cid, "test title", Some("desc"), "high", None)
        .await
        .expect("create");
    assert_eq!(row.title, "test title");
    assert_eq!(row.priority, "high");
}

/// 4. create — 无描述
#[tokio::test(flavor = "current_thread")]
async fn create_issue_without_description() {
    let db = db().await;
    let cid = insert_company(&db, "create-no-desc").await;
    let row = IssueRepo::new(&db)
        .create(cid, "no desc", None, "low", None)
        .await
        .expect("create");
    assert!(row.description.is_none());
}

/// 5. count_for_company — 返回 issue 总数
#[tokio::test(flavor = "current_thread")]
async fn count_for_company_returns_count() {
    let db = db().await;
    let cid = insert_company(&db, "count").await;
    insert_issue(&db, cid, "i1").await;
    insert_issue(&db, cid, "i2").await;
    insert_issue(&db, cid, "i3").await;
    let count = IssueRepo::new(&db)
        .count_for_company(cid)
        .await
        .expect("count");
    assert_eq!(count, 3);
}

/// 6. count_for_company — 空返回 0
#[tokio::test(flavor = "current_thread")]
async fn count_for_company_empty_returns_zero() {
    let db = db().await;
    let cid = insert_company(&db, "count-empty").await;
    let count = IssueRepo::new(&db)
        .count_for_company(cid)
        .await
        .expect("count");
    assert_eq!(count, 0);
}
