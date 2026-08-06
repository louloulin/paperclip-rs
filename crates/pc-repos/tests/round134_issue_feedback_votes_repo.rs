//! Round 134 集成测试：FeedbackVoteRepo — issues.rs feedback_votes 子模块仓储化。
//!
//! 覆盖：
//! - list_by_issue / get_by_id / count_by_issue
//! - create / create_for_issue（复合方法：查 company_id + INSERT）
//! - issue_company_id（issue 不存在返回 None）

use pc_db::Db;
use pc_repos::feedback_vote::{FeedbackVoteRepo, NewFeedbackVote};
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r134-{tag}-{id}"))
        .bind(format!("R134{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn insert_issue(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO issues (id, company_id, identifier, title, kind, status, priority) VALUES ($1,$2,$3,'i','task','todo','normal')")
        .bind(id).bind(company_id).bind(format!("ISS-{}", &id.simple().to_string()[..6]))
        .execute(db.pool()).await.expect("issue");
    id
}

async fn insert_vote(db: &Db, issue_id: Uuid, company_id: Uuid, vote: &str) -> Uuid {
    FeedbackVoteRepo::new(db)
        .create(&NewFeedbackVote {
            company_id,
            issue_id,
            target_type: "user".into(),
            target_id: "u1".into(),
            author_user_id: "system".into(),
            vote: vote.into(),
            reason: None,
        })
        .await
        .expect("create vote")
}

// ===== FeedbackVoteRepo::create / list_by_issue / get_by_id / count_by_issue =====

/// 1. create + get_by_id — 正常插入 + 回读。
#[tokio::test(flavor = "current_thread")]
async fn create_and_get_by_id() {
    let db = db().await;
    let cid = insert_company(&db, "c1").await;
    let iid = insert_issue(&db, cid).await;
    let repo = FeedbackVoteRepo::new(&db);
    let vote_id = insert_vote(&db, iid, cid, "up").await;
    let row = repo.get_by_id(vote_id).await.expect("get").expect("row");
    assert_eq!(row.vote, "up");
    assert_eq!(row.issue_id, iid);
    assert_eq!(row.company_id, cid);
    assert_eq!(row.target_type, "user");
    assert_eq!(row.author_user_id, "system");
}

/// 2. list_by_issue — 按 created_at DESC + LIMIT。
#[tokio::test(flavor = "current_thread")]
async fn list_by_issue_orders_by_created_desc() {
    let db = db().await;
    let cid = insert_company(&db, "l1").await;
    let iid = insert_issue(&db, cid).await;
    let a = insert_vote(&db, iid, cid, "up").await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let b = insert_vote(&db, iid, cid, "down").await;
    let list = FeedbackVoteRepo::new(&db)
        .list_by_issue(iid, 10)
        .await
        .expect("list");
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].id, b, "newer first");
    assert_eq!(list[1].id, a);
}

/// 3. list_by_issue — limit 生效。
#[tokio::test(flavor = "current_thread")]
async fn list_by_issue_respects_limit() {
    let db = db().await;
    let cid = insert_company(&db, "l2").await;
    let iid = insert_issue(&db, cid).await;
    for i in 0..5 {
        insert_vote(&db, iid, cid, &format!("v{i}")).await;
    }
    let list = FeedbackVoteRepo::new(&db)
        .list_by_issue(iid, 3)
        .await
        .expect("list");
    assert_eq!(list.len(), 3);
}

/// 4. list_by_issue — 跨 issue 隔离。
#[tokio::test(flavor = "current_thread")]
async fn list_by_issue_isolates() {
    let db = db().await;
    let cid = insert_company(&db, "iso").await;
    let i1 = insert_issue(&db, cid).await;
    let i2 = insert_issue(&db, cid).await;
    insert_vote(&db, i1, cid, "up").await;
    insert_vote(&db, i2, cid, "down").await;
    insert_vote(&db, i2, cid, "neutral").await;
    assert_eq!(
        FeedbackVoteRepo::new(&db)
            .list_by_issue(i1, 100)
            .await
            .expect("a")
            .len(),
        1
    );
    assert_eq!(
        FeedbackVoteRepo::new(&db)
            .list_by_issue(i2, 100)
            .await
            .expect("b")
            .len(),
        2
    );
}

/// 5. count_by_issue。
#[tokio::test(flavor = "current_thread")]
async fn count_by_issue() {
    let db = db().await;
    let cid = insert_company(&db, "cnt").await;
    let iid = insert_issue(&db, cid).await;
    for _ in 0..4 {
        insert_vote(&db, iid, cid, "up").await;
    }
    let n = FeedbackVoteRepo::new(&db)
        .count_by_issue(iid)
        .await
        .expect("n");
    assert_eq!(n, 4);
}

// ===== create_for_issue 复合方法 =====

/// 6. create_for_issue — 自动补齐 company_id。
#[tokio::test(flavor = "current_thread")]
async fn create_for_issue_resolves_company_id() {
    let db = db().await;
    let cid = insert_company(&db, "cfi").await;
    let iid = insert_issue(&db, cid).await;
    let repo = FeedbackVoteRepo::new(&db);
    let id = repo
        .create_for_issue(iid, "agent", "a-1", "system", "up", Some("good"))
        .await
        .expect("create");
    let row = repo.get_by_id(id).await.expect("get").expect("row");
    assert_eq!(row.company_id, cid);
    assert_eq!(row.target_type, "agent");
    assert_eq!(row.target_id, "a-1");
    assert_eq!(row.reason.as_deref(), Some("good"));
}

/// 7. create_for_issue — issue 不存在返回 RowNotFound。
#[tokio::test(flavor = "current_thread")]
async fn create_for_issue_unknown_issue_errors() {
    let db = db().await;
    let repo = FeedbackVoteRepo::new(&db);
    let res = repo
        .create_for_issue(Uuid::new_v4(), "user", "u", "system", "up", None)
        .await;
    assert!(matches!(res, Err(sqlx::Error::RowNotFound)));
}

/// 8. issue_company_id — 存在/不存在。
#[tokio::test(flavor = "current_thread")]
async fn issue_company_id_returns_option() {
    let db = db().await;
    let cid = insert_company(&db, "ici").await;
    let iid = insert_issue(&db, cid).await;
    let repo = FeedbackVoteRepo::new(&db);
    assert_eq!(repo.issue_company_id(iid).await.expect("ok"), Some(cid));
    assert!(repo
        .issue_company_id(Uuid::new_v4())
        .await
        .expect("ok")
        .is_none());
}

/// 9. create — 必填字段校验（text 非空由 DB 约束保证）。
#[tokio::test(flavor = "current_thread")]
async fn create_with_reason_optional() {
    let db = db().await;
    let cid = insert_company(&db, "cr").await;
    let iid = insert_issue(&db, cid).await;
    let repo = FeedbackVoteRepo::new(&db);
    let id = repo
        .create(&NewFeedbackVote {
            company_id: cid,
            issue_id: iid,
            target_type: "user".into(),
            target_id: "u2".into(),
            author_user_id: "system".into(),
            vote: "neutral".into(),
            reason: None,
        })
        .await
        .expect("create");
    let row = repo.get_by_id(id).await.expect("get").expect("row");
    assert!(row.reason.is_none());
}
