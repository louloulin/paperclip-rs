//! Round 219 集成测试：issue_thread_interactions CRUD 仓储语义。
//!
//! 覆盖：
//! - `IssueRepo::delete_interaction` 删除已存在记录
//! - `IssueRepo::delete_interaction` 删除不存在记录返回 false
//! - `IssueRepo::create_interaction` + `list_interactions` 完整流程

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
        .bind(format!("r219-{tag}-{id}"))
        .bind(format!("R219{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn insert_issue(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, issue_key) \
         VALUES ($1, $2, 'r219-issue', 'todo', 'R219-TEST')",
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
async fn create_and_list_interaction_round_trip() {
    let db = db().await;
    let company_id = insert_company(&db, "create_list").await;
    let issue_id = insert_issue(&db, company_id).await;

    let row = IssueRepo::new(&db)
        .create_interaction(
            company_id,
            issue_id,
            "suggest_tasks",
            "wake_assignee",
            Some("test title"),
            Some("test summary"),
            &serde_json::json!({"items": []}),
            None,
            Some("test-user"),
        )
        .await
        .expect("create");

    assert_eq!(row.kind, "suggest_tasks");
    assert_eq!(row.status, "pending");

    let listed = IssueRepo::new(&db)
        .list_interactions(issue_id)
        .await
        .expect("list");
    assert!(listed.iter().any(|r| r.id == row.id));
}

#[tokio::test]
#[ignore]
async fn delete_interaction_removes_record() {
    let db = db().await;
    let company_id = insert_company(&db, "delete_existing").await;
    let issue_id = insert_issue(&db, company_id).await;

    let row = IssueRepo::new(&db)
        .create_interaction(
            company_id,
            issue_id,
            "ask_user_questions",
            "wake_assignee",
            None, None,
            &serde_json::json!({}),
            None, None,
        )
        .await
        .expect("create");

    let removed = IssueRepo::new(&db)
        .delete_interaction(row.id)
        .await
        .expect("delete");
    assert!(removed);

    let after = IssueRepo::new(&db)
        .get_interaction(row.id)
        .await
        .expect("get");
    assert!(after.is_none());
}

#[tokio::test]
#[ignore]
async fn delete_interaction_returns_false_when_missing() {
    let db = db().await;
    let removed = IssueRepo::new(&db)
        .delete_interaction(Uuid::new_v4())
        .await
        .expect("delete");
    assert!(!removed);
}
