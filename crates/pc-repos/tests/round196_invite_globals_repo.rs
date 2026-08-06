//! Round 196 集成测试：invite revoke_by_id（无 company scope）。
//!
//! 覆盖：
//! - `InviteRepo::revoke_by_id` — 全局撤销（不管 company）
//! - 不存在 id → 0 rows affected
//! - 重复 revoke → 幂等（rows affected = 0）

use pc_db::Db;
use pc_repos::invite::{InviteRepo, NewInvite};
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(format!("r196-{tag}-{id}"))
        .bind(format!("R196{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn insert_invite(
    db: &Db,
    company_id: Option<Uuid>,
    token: &str,
    invite_type: &str,
    expires_in_days: i64,
) -> Uuid {
    let id = Uuid::new_v4();
    let now = chrono::Utc::now();
    let expires_at = now + chrono::Duration::days(expires_in_days);
    sqlx::query(
        "INSERT INTO invites \
            (id, company_id, invite_type, allowed_join_types, token_hash, expires_at, status, created_at) \
         VALUES ($1, $2, $3, ARRAY['human']::text[], $4, $5, 'pending', $6)",
    )
    .bind(id)
    .bind(company_id)
    .bind(invite_type)
    .bind(format!("h-{}", token))
    .bind(expires_at)
    .bind(now)
    .execute(db.pool())
    .await
    .expect("invite");
    id
}

// ===== 1) revoke_by_id: pending → revoked =====
#[tokio::test(flavor = "current_thread")]
async fn revoke_by_id_with_company_succeeds() {
    let db = db().await;
    let cid = insert_company(&db, "rev-c").await;
    let iid = insert_invite(&db, Some(cid), "tok-c", "company_member", 7).await;
    let repo = InviteRepo::new(&db);

    let n = repo.revoke_by_id(iid).await.expect("revoke");
    assert_eq!(n, 1);

    // status should now be revoked
    let status: String = sqlx::query_scalar("SELECT status FROM invites WHERE id = $1")
        .bind(iid)
        .fetch_one(db.pool())
        .await
        .expect("status");
    assert_eq!(status, "revoked");
}

// ===== 2) revoke_by_id: company_id NULL (bootstrap_ceo) =====
#[tokio::test(flavor = "current_thread")]
async fn revoke_by_id_global_invite_without_company() {
    let db = db().await;
    let iid = insert_invite(&db, None, "tok-g", "bootstrap_ceo", 7).await;
    let repo = InviteRepo::new(&db);

    let n = repo.revoke_by_id(iid).await.expect("revoke global");
    assert_eq!(n, 1);

    let status: String = sqlx::query_scalar("SELECT status FROM invites WHERE id = $1")
        .bind(iid)
        .fetch_one(db.pool())
        .await
        .expect("status");
    assert_eq!(status, "revoked");
}

// ===== 3) revoke_by_id: missing id → 0 rows =====
#[tokio::test(flavor = "current_thread")]
async fn revoke_by_id_missing_returns_zero() {
    let db = db().await;
    let repo = InviteRepo::new(&db);
    let n = repo.revoke_by_id(Uuid::new_v4()).await.expect("revoke");
    assert_eq!(n, 0);
}

// ===== 4) revoke_by_id: idempotent (re-revoke) =====
#[tokio::test(flavor = "current_thread")]
async fn revoke_by_id_idempotent() {
    let db = db().await;
    let cid = insert_company(&db, "rev-idem").await;
    let iid = insert_invite(&db, Some(cid), "tok-i", "company_member", 7).await;
    let repo = InviteRepo::new(&db);

    let n1 = repo.revoke_by_id(iid).await.expect("revoke 1");
    assert_eq!(n1, 1);

    // Second revoke should affect 0 rows (already revoked)
    let n2 = repo.revoke_by_id(iid).await.expect("revoke 2");
    assert_eq!(n2, 0, "second revoke must be idempotent (no rows affected)");
}
