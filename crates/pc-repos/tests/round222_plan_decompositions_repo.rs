//! Round 222 集成测试：issue_plan_decompositions 仓储语义。
//!
//! 覆盖：
//! - `IssueRepo::list_plan_decompositions` 按 source_issue_id 过滤并倒序
//! - `IssueRepo::find_plan_decomposition_by_revision` 精确查找
//! - `IssueRepo::create_plan_decomposition` 初始 status=in_flight, child_issue_ids=[]
//! - `IssueRepo::update_plan_decomposition_progress` 状态切换 + child 追加

use pc_db::Db;
use pc_repos::issue::IssueRepo;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(format!("r222-{tag}-{id}"))
        .bind(format!("R222{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn insert_issue(db: &Db, company_id: Uuid, key_suffix: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, issue_key) \
         VALUES ($1, $2, $3, 'todo', $4)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r222-issue-{key_suffix}"))
    .bind(format!("R222-TS-{key_suffix}"))
    .execute(db.pool())
    .await
    .expect("issue");
    id
}

#[tokio::test]
#[ignore]
async fn create_plan_decomposition_initializes_in_flight() {
    let db = db().await;
    let company_id = insert_company(&db, "create").await;
    let issue_id = insert_issue(&db, company_id, "create").await;
    let revision_id = Uuid::new_v4();

    let row = IssueRepo::new(&db)
        .create_plan_decomposition(
            company_id,
            issue_id,
            revision_id,
            None,
            "fp-create-1",
            2,
            &serde_json::json!([{"title": "a"}, {"title": "b"}]),
            None,
            None,
            None,
        )
        .await
        .expect("create");

    assert_eq!(row.status, "in_flight");
    assert_eq!(row.requested_child_count, 2);
    assert_eq!(row.request_fingerprint, "fp-create-1");
    assert_eq!(row.completed_at, None);
    assert_eq!(row.accepted_interaction_id, None);
    let empty: Vec<serde_json::Value> =
        serde_json::from_value(row.child_issue_ids.clone()).expect("parse");
    assert!(empty.is_empty(), "初始 child_issue_ids 应当是空数组");
}

#[tokio::test]
#[ignore]
async fn list_plan_decompositions_filters_by_source_issue() {
    let db = db().await;
    let company_id = insert_company(&db, "list").await;
    let issue_id = insert_issue(&db, company_id, "list").await;
    let other_issue = insert_issue(&db, company_id, "list-other").await;

    // 给目标 issue 创建 2 个 decomposition
    for i in 0..2 {
        IssueRepo::new(&db)
            .create_plan_decomposition(
                company_id,
                issue_id,
                Uuid::new_v4(),
                None,
                &format!("fp-list-{i}"),
                1,
                &serde_json::json!([{"title": format!("t{i}")}]),
                None,
                None,
                None,
            )
            .await
            .expect("create target");
    }
    // 给另一个 issue 创建 1 个（应当被过滤掉）
    IssueRepo::new(&db)
        .create_plan_decomposition(
            company_id,
            other_issue,
            Uuid::new_v4(),
            None,
            "fp-list-other",
            1,
            &serde_json::json!([{"title": "other"}]),
            None,
            None,
            None,
        )
        .await
        .expect("create other");

    let rows = IssueRepo::new(&db)
        .list_plan_decompositions(issue_id)
        .await
        .expect("list");
    assert_eq!(rows.len(), 2, "应只返回 2 条属于 issue_id 的记录");
    assert!(rows.iter().all(|r| r.source_issue_id == issue_id));
}

#[tokio::test]
#[ignore]
async fn find_plan_decomposition_by_revision_returns_existing() {
    let db = db().await;
    let company_id = insert_company(&db, "find").await;
    let issue_id = insert_issue(&db, company_id, "find").await;
    let revision_id = Uuid::new_v4();

    let created = IssueRepo::new(&db)
        .create_plan_decomposition(
            company_id,
            issue_id,
            revision_id,
            None,
            "fp-find-1",
            1,
            &serde_json::json!([{"title": "a"}]),
            None,
            None,
            None,
        )
        .await
        .expect("create");

    let found = IssueRepo::new(&db)
        .find_plan_decomposition_by_revision(company_id, issue_id, revision_id)
        .await
        .expect("find")
        .expect("should exist");
    assert_eq!(found.id, created.id);
    assert_eq!(found.request_fingerprint, "fp-find-1");
}

#[tokio::test]
#[ignore]
async fn find_plan_decomposition_by_revision_returns_none_for_missing() {
    let db = db().await;
    let company_id = insert_company(&db, "miss").await;
    let issue_id = insert_issue(&db, company_id, "miss").await;

    let found = IssueRepo::new(&db)
        .find_plan_decomposition_by_revision(company_id, issue_id, Uuid::new_v4())
        .await
        .expect("find");
    assert!(found.is_none());
}

#[tokio::test]
#[ignore]
async fn update_plan_decomposition_progress_appends_child_id_and_marks_completed() {
    let db = db().await;
    let company_id = insert_company(&db, "update").await;
    let issue_id = insert_issue(&db, company_id, "update").await;
    let revision_id = Uuid::new_v4();

    let created = IssueRepo::new(&db)
        .create_plan_decomposition(
            company_id,
            issue_id,
            revision_id,
            None,
            "fp-update-1",
            2,
            &serde_json::json!([{"title": "a"}, {"title": "b"}]),
            None,
            None,
            None,
        )
        .await
        .expect("create");

    // 第一次推进：追加 1 个 child，仍 in_flight
    let new_child = Uuid::new_v4();
    let partial = IssueRepo::new(&db)
        .update_plan_decomposition_progress(
            created.id,
            "in_flight",
            &serde_json::json!([new_child]),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("update")
        .expect("row exists");
    assert_eq!(partial.status, "in_flight");
    assert_eq!(partial.completed_at, None);
    let partial_ids: Vec<String> =
        serde_json::from_value(partial.child_issue_ids.clone()).expect("parse");
    assert_eq!(partial_ids, vec![new_child.to_string()]);

    // 第二次推进：追加到 2 个，标记 completed
    let second_child = Uuid::new_v4();
    let now = pc_core::Timestamp::from_dt(chrono::Utc::now());
    let completed = IssueRepo::new(&db)
        .update_plan_decomposition_progress(
            created.id,
            "completed",
            &serde_json::json!([new_child, second_child]),
            Some(now),
            None,
            None,
            None,
        )
        .await
        .expect("update2")
        .expect("row exists");
    assert_eq!(completed.status, "completed");
    assert!(completed.completed_at.is_some());
    let final_ids: Vec<String> =
        serde_json::from_value(completed.child_issue_ids.clone()).expect("parse2");
    assert_eq!(final_ids.len(), 2);
}
