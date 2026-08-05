//! Round 210 集成测试：issues 按 status/priority 聚合仓储语义。
//!
//! 覆盖：
//! - `IssueRepo::count_visible_by_status` 按 status 分组（hidden_at IS NULL）
//! - `IssueRepo::count_visible_by_priority` 按 priority 分组（hidden_at IS NULL）
//! - hidden_at 不为 NULL 的 issue 不计入

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
        .bind(format!("r210-{tag}-{id}"))
        .bind(format!("R210{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn insert_issue_with(
    db: &Db,
    company_id: Uuid,
    status: &str,
    priority: &str,
    hidden: bool,
) -> Uuid {
    let id = Uuid::new_v4();
    if hidden {
        sqlx::query(
            "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, origin_fingerprint, hidden_at) \
             VALUES ($1, $2, 'r210-issue', $3, $4, 'system', $5, now())",
        )
        .bind(id)
        .bind(company_id)
        .bind(status)
        .bind(priority)
        .bind(format!("r210-fp-{id}"))
        .execute(db.pool())
        .await
        .expect("issue hidden");
    } else {
        sqlx::query(
            "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, origin_fingerprint) \
             VALUES ($1, $2, 'r210-issue', $3, $4, 'system', $5)",
        )
        .bind(id)
        .bind(company_id)
        .bind(status)
        .bind(priority)
        .bind(format!("r210-fp-{id}"))
        .execute(db.pool())
        .await
        .expect("issue");
    }
    id
}

// ===== 1) count_visible_by_status: 按 status 聚合 =====
#[tokio::test(flavor = "current_thread")]
async fn count_visible_by_status_groups() {
    let db = db().await;
    let cid = insert_company(&db, "bs").await;

    // 3 个 todo, 2 个 in_progress, 1 个 done, 1 个 hidden（不计入）
    for _ in 0..3 {
        insert_issue_with(&db, cid, "todo", "normal", false).await;
    }
    for _ in 0..2 {
        insert_issue_with(&db, cid, "in_progress", "high", false).await;
    }
    insert_issue_with(&db, cid, "done", "low", false).await;
    insert_issue_with(&db, cid, "todo", "normal", true).await; // hidden

    let groups = IssueRepo::new(&db)
        .count_visible_by_status(cid)
        .await
        .expect("by status");
    let map: std::collections::HashMap<String, i64> = groups.into_iter().collect();
    assert_eq!(map.get("todo").copied().unwrap_or(0), 3);
    assert_eq!(map.get("in_progress").copied().unwrap_or(0), 2);
    assert_eq!(map.get("done").copied().unwrap_or(0), 1);
}

// ===== 2) count_visible_by_priority: 按 priority 聚合 =====
#[tokio::test(flavor = "current_thread")]
async fn count_visible_by_priority_groups() {
    let db = db().await;
    let cid = insert_company(&db, "bp").await;

    // 2 high, 4 normal, 1 low, 1 hidden (不计入)
    for _ in 0..2 {
        insert_issue_with(&db, cid, "todo", "high", false).await;
    }
    for _ in 0..4 {
        insert_issue_with(&db, cid, "todo", "normal", false).await;
    }
    insert_issue_with(&db, cid, "todo", "low", false).await;
    insert_issue_with(&db, cid, "todo", "high", true).await; // hidden

    let groups = IssueRepo::new(&db)
        .count_visible_by_priority(cid)
        .await
        .expect("by priority");
    let map: std::collections::HashMap<String, i64> = groups.into_iter().collect();
    assert_eq!(map.get("high").copied().unwrap_or(0), 2);
    assert_eq!(map.get("normal").copied().unwrap_or(0), 4);
    assert_eq!(map.get("low").copied().unwrap_or(0), 1);
}

// ===== 3) hidden issues 完全不计入 =====
#[tokio::test(flavor = "current_thread")]
async fn hidden_issues_excluded() {
    let db = db().await;
    let cid = insert_company(&db, "hd").await;
    for _ in 0..5 {
        insert_issue_with(&db, cid, "todo", "normal", true).await;
    }
    let by_status = IssueRepo::new(&db)
        .count_visible_by_status(cid)
        .await
        .expect("by status");
    assert!(by_status.is_empty(), "all hidden should yield empty result");
    let by_priority = IssueRepo::new(&db)
        .count_visible_by_priority(cid)
        .await
        .expect("by priority");
    assert!(by_priority.is_empty());
}
