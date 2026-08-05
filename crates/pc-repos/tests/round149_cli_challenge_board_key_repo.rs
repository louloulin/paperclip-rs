//! Round 149 集成测试：cli_challenge + board_key 仓储。
//!
//! 覆盖：
//! - ChallengeRepo::create / find_by_id / approve / cancel
//! - BoardKeyRepo::list_active_by_user / create / revoke

use pc_db::Db;
use pc_repos::board_key::BoardKeyRepo;
use pc_repos::cli_challenge::ChallengeRepo;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_user(db: &Db, tag: &str) -> String {
    let id = format!("u_r149_{}_{}", tag, Uuid::new_v4().simple());
    sqlx::query(r#"INSERT INTO "user" (id, name, email) VALUES ($1, $2, $3)"#)
        .bind(&id).bind(format!("r149-{tag}")).bind(format!("r149_{tag}_{id}@x"))
        .execute(db.pool()).await.expect("user");
    id
}

// ===== ChallengeRepo::create =====

/// 1. create — 插入并返回完整行。
#[tokio::test(flavor = "current_thread")]
async fn challenge_create_returns_row() {
    let db = db().await;
    let repo = ChallengeRepo::new(&db);
    let expires_at = pc_core::Timestamp::now();
    let row = repo
        .create(
            "secret-hash-1",
            "paperclip login",
            Some("cli-client"),
            "board",
            None,
            "pending-key-hash-1",
            "cli-session",
            expires_at,
        )
        .await
        .expect("ok");
    assert_eq!(row.secret_hash, "secret-hash-1");
    assert_eq!(row.command, "paperclip login");
    assert_eq!(row.client_name.as_deref(), Some("cli-client"));
    assert_eq!(row.requested_access, "board");
    assert_eq!(row.pending_key_name, "cli-session");
    assert!(row.approved_at.is_none());
    assert!(row.cancelled_at.is_none());
}

/// 2. create — client_name 可空。
#[tokio::test(flavor = "current_thread")]
async fn challenge_create_optional_client_name() {
    let db = db().await;
    let repo = ChallengeRepo::new(&db);
    let row = repo
        .create(
            "sh",
            "cmd",
            None,
            "board",
            None,
            "pkh",
            "pkn",
            pc_core::Timestamp::now(),
        )
        .await
        .expect("ok");
    assert!(row.client_name.is_none());
}

// ===== ChallengeRepo::find_by_id =====

/// 3. find_by_id — 存在则返回行。
#[tokio::test(flavor = "current_thread")]
async fn challenge_find_by_id_returns_some() {
    let db = db().await;
    let repo = ChallengeRepo::new(&db);
    let created = repo
        .create("sh2", "cmd2", None, "board", None, "pk2", "pn2", pc_core::Timestamp::now())
        .await
        .expect("create");
    let fetched = repo.find_by_id(created.id).await.expect("find");
    assert!(fetched.is_some());
    assert_eq!(fetched.unwrap().id, created.id);
}

/// 4. find_by_id — 不存在返回 None。
#[tokio::test(flavor = "current_thread")]
async fn challenge_find_by_id_missing_returns_none() {
    let db = db().await;
    let repo = ChallengeRepo::new(&db);
    let fetched = repo.find_by_id(Uuid::new_v4()).await.expect("find");
    assert!(fetched.is_none());
}

// ===== ChallengeRepo::approve =====

/// 5. approve — 设置 approved_by_user_id + approved_at。
#[tokio::test(flavor = "current_thread")]
async fn challenge_approve_sets_fields() {
    let db = db().await;
    let user = insert_user(&db, "approve").await;
    let repo = ChallengeRepo::new(&db);
    let created = repo
        .create("sh3", "cmd3", None, "board", None, "pk3", "pn3", pc_core::Timestamp::now())
        .await
        .expect("create");
    let row = repo.approve(created.id, &user).await.expect("approve");
    assert_eq!(row.approved_by_user_id.as_deref(), Some(user.as_str()));
    assert!(row.approved_at.is_some());
    assert!(row.cancelled_at.is_none());
}

// ===== ChallengeRepo::cancel =====

/// 6. cancel — 写 cancelled_at，不动 approved_*。
#[tokio::test(flavor = "current_thread")]
async fn challenge_cancel_writes_cancelled_at() {
    let db = db().await;
    let repo = ChallengeRepo::new(&db);
    let created = repo
        .create("sh4", "cmd4", None, "board", None, "pk4", "pn4", pc_core::Timestamp::now())
        .await
        .expect("create");
    let row = repo.cancel(created.id).await.expect("cancel");
    assert!(row.cancelled_at.is_some());
    assert!(row.approved_at.is_none());
}

// ===== BoardKeyRepo::create / list / revoke =====

/// 7. board_key create + list — 仅返回未撤销的 key。
#[tokio::test(flavor = "current_thread")]
async fn board_key_create_then_list() {
    let db = db().await;
    let user = insert_user(&db, "bkey").await;
    let repo = BoardKeyRepo::new(&db);
    let _row = repo
        .create(&user, "laptop", "kh1", None)
        .await
        .expect("create");
    let rows = repo.list_active_by_user(&user).await.expect("list");
    assert!(rows.iter().any(|r| r.name == "laptop" && r.key_hash == "kh1"));
}

/// 8. board_key create + revoke — 撤销后从 list 消失。
#[tokio::test(flavor = "current_thread")]
async fn board_key_revoke_hides_from_list() {
    let db = db().await;
    let user = insert_user(&db, "rev").await;
    let repo = BoardKeyRepo::new(&db);
    let row = repo
        .create(&user, "to-delete", "kh2", None)
        .await
        .expect("create");
    let affected = repo.revoke(row.id, &user).await.expect("revoke");
    assert_eq!(affected, 1);
    let rows = repo.list_active_by_user(&user).await.expect("list");
    assert!(!rows.iter().any(|r| r.id == row.id));
}

/// 9. board_key revoke — 跨用户不命中（affected = 0）。
#[tokio::test(flavor = "current_thread")]
async fn board_key_revoke_other_user_noop() {
    let db = db().await;
    let owner = insert_user(&db, "own").await;
    let other = insert_user(&db, "oth").await;
    let repo = BoardKeyRepo::new(&db);
    let row = repo
        .create(&owner, "owner-only", "kh3", None)
        .await
        .expect("create");
    let affected = repo.revoke(row.id, &other).await.expect("revoke");
    assert_eq!(affected, 0);
}

/// 10. board_key create — expires_at 可空。
#[tokio::test(flavor = "current_thread")]
async fn board_key_create_optional_expires_at() {
    let db = db().await;
    let user = insert_user(&db, "noexp").await;
    let repo = BoardKeyRepo::new(&db);
    let row = repo
        .create(&user, "no-exp", "kh4", None)
        .await
        .expect("create");
    assert!(row.expires_at.is_none());
    assert!(row.revoked_at.is_none());
}

// ===== DTO 构造 smoke tests (sync) =====

/// 11. ChallengeRow 结构 — 字段类型 smoke。
#[test]
fn challenge_row_field_types() {
    use pc_repos::cli_challenge::ChallengeRow;
    fn assert_from_row<T: for<'a> sqlx::FromRow<'a, sqlx::postgres::PgRow>>() {}
    assert_from_row::<ChallengeRow>();
}

/// 12. BoardKeyRow 结构 — 字段类型 smoke。
#[test]
fn board_key_row_field_types() {
    use pc_repos::board_key::BoardKeyRow;
    fn assert_from_row<T: for<'a> sqlx::FromRow<'a, sqlx::postgres::PgRow>>() {}
    assert_from_row::<BoardKeyRow>();
}
