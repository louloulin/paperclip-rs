//! Round 218 集成测试：issue_read_state 仓储语义。
//!
//! 覆盖：
//! - `IssueRepo::delete_read_state` 删除已存在记录
//! - `IssueRepo::delete_read_state` 删除不存在记录返回 false
//! - `IssueRepo::get_read_state` 在删除后返回 None
//! - upsert + delete 组合：再次 upsert 应成功

use pc_db::Db;
use pc_repos::issue::IssueRepo;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0)
        .await
        .expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(format!("r218-{tag}-{id}"))
        .bind(format!("R218{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn insert_issue(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, issue_key) \
         VALUES ($1, $2, 'r218-issue', 'todo', 'R218-TEST')",
    )
    .bind(id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .expect("issue");
    id
}

#[tokio::test]
#[ignore]
async fn delete_read_state_removes_existing() {
    let db = db().await;
    let company_id = insert_company(&db, "del_existing").await;
    let issue_id = insert_issue(&db, company_id).await;
    let user_id = "test-user-1";

    let _ = IssueRepo::new(&db)
        .upsert_read_state(company_id, issue_id, user_id, None)
        .await
        .expect("upsert");

    let removed = IssueRepo::new(&db)
        .delete_read_state(issue_id, user_id)
        .await
        .expect("delete");
    assert!(removed);

    let after = IssueRepo::new(&db)
        .get_read_state(issue_id, user_id)
        .await
        .expect("get");
    assert!(after.is_none());
}

#[tokio::test]
#[ignore]
async fn delete_read_state_returns_false_when_missing() {
    let db = db().await;
    let company_id = insert_company(&db, "del_missing").await;
    let issue_id = insert_issue(&db, company_id).await;

    let removed = IssueRepo::new(&db)
        .delete_read_state(issue_id, "never-existed")
        .await
        .expect("delete");
    assert!(!removed);
}

#[tokio::test]
#[ignore]
async fn upsert_after_delete_succeeds() {
    let db = db().await;
    let company_id = insert_company(&db, "upsert_again").await;
    let issue_id = insert_issue(&db, company_id).await;
    let user_id = "test-user-2";

    let _ = IssueRepo::new(&db)
        .upsert_read_state(company_id, issue_id, user_id, None)
        .await
        .expect("first upsert");
    let _ = IssueRepo::new(&db)
        .delete_read_state(issue_id, user_id)
        .await
        .expect("delete");
    let row = IssueRepo::new(&db)
        .upsert_read_state(company_id, issue_id, user_id, None)
        .await
        .expect("second upsert");
    assert_eq!(row.issue_id, issue_id);
}
