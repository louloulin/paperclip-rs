//! Round 140 集成测试：auth.rs 路由仓储化 — AuthRepo 新增方法。
//!
//! 覆盖：
//! - find_user_id_by_email / user_exists
//! - revoke_api_key / revoke_session_by_token / revoke_all_sessions_for_user
//! - update_user_name / update_user_image
//! - ensure_user (legacy ON CONFLICT DO NOTHING 语义)
//! - create_credential_account
//! - CompanyMemberRepo::list_company_ids_for_user

use pc_db::Db;
use pc_repos::auth::{AuthRepo, NewSession, NewUser};
use pc_repos::company_member::CompanyMemberRepo;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_user(db: &Db, tag: &str) -> String {
    let id = format!("u_r140_{}_{}", tag, Uuid::new_v4().simple());
    sqlx::query("INSERT INTO \"user\" (id, name, email, email_verified, created_at, updated_at) VALUES ($1,$2,$3,false,now(),now())")
        .bind(&id).bind(format!("r140-{tag}")).bind(format!("r140-{tag}@test"))
        .execute(db.pool()).await.expect("user");
    id
}

async fn insert_company(db: &Db) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id).bind(format!("r140-c-{id}")).bind(format!("R140{}", &id.simple().to_string()[..4]))
        .execute(db.pool()).await.expect("company");
    id
}

async fn insert_membership(db: &Db, user_id: &str, company_id: Uuid) {
    sqlx::query("INSERT INTO company_memberships (company_id, principal_type, principal_id, status, membership_role)                  VALUES ($1, 'user', $2, 'active', 'member')")
        .bind(company_id).bind(user_id)
        .execute(db.pool()).await.expect("member");
}

async fn insert_api_key(db: &Db, user_id: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO board_api_keys (id, user_id, name, prefix, hash, scopes)                  VALUES ($1, $2, $3, $4, $5, '[]')")
        .bind(id).bind(user_id).bind("k").bind("pk").bind("h")
        .execute(db.pool()).await.expect("apikey");
    id
}

// ===== AuthRepo::find_user_id_by_email =====

/// 1. find_user_id_by_email — 找到现有 user。
#[tokio::test(flavor = "current_thread")]
async fn find_user_id_by_email_found() {
    let db = db().await;
    let uid = insert_user(&db, "fid").await;
    let id = AuthRepo::new(&db).find_user_id_by_email("r140-fid@test").await.expect("ok");
    assert_eq!(id, Some(uid));
}

/// 2. find_user_id_by_email — 不存在的 email 返回 None。
#[tokio::test(flavor = "current_thread")]
async fn find_user_id_by_email_missing() {
    let db = db().await;
    let id = AuthRepo::new(&db).find_user_id_by_email("nope@test").await.expect("ok");
    assert!(id.is_none());
}

// ===== AuthRepo::user_exists =====

/// 3. user_exists — 真实 id 返回 true。
#[tokio::test(flavor = "current_thread")]
async fn user_exists_true() {
    let db = db().await;
    let uid = insert_user(&db, "ue").await;
    assert!(AuthRepo::new(&db).user_exists(&uid).await.expect("ok"));
}

/// 4. user_exists — 不存在 id 返回 false。
#[tokio::test(flavor = "current_thread")]
async fn user_exists_false() {
    let db = db().await;
    assert!(!AuthRepo::new(&db).user_exists("u_nope_xyz").await.expect("ok"));
}

// ===== AuthRepo::ensure_user =====

/// 5. ensure_user — 新建用户返回 Some(row)。
#[tokio::test(flavor = "current_thread")]
async fn ensure_user_inserts() {
    let db = db().await;
    let uid = format!("u_r140_eu1_{}", Uuid::new_v4().simple());
    let row = AuthRepo::new(&db)
        .ensure_user(&uid, "n", "eu1@test")
        .await
        .expect("ok");
    assert!(row.is_some(), "first ensure_user should return Some");
    assert_eq!(row.unwrap().id, uid);
}

/// 6. ensure_user — 已存在用户返回 None (DO NOTHING 语义)。
#[tokio::test(flavor = "current_thread")]
async fn ensure_user_idempotent() {
    let db = db().await;
    let uid = insert_user(&db, "eui").await;
    let row = AuthRepo::new(&db)
        .ensure_user(&uid, "different_name", "different@test")
        .await
        .expect("ok");
    assert!(row.is_none(), "second ensure_user should be no-op");
    // 验证原数据未被覆盖
    let fetched = AuthRepo::new(&db).find_by_id(&uid).await.expect("ok").unwrap();
    assert_eq!(fetched.name, "r140-eui", "name should NOT be overwritten");
}

// ===== AuthRepo::create_credential_account =====

/// 7. create_credential_account — 创建 credential account 并返回 row。
#[tokio::test(flavor = "current_thread")]
async fn create_credential_account_basic() {
    let db = db().await;
    let uid = insert_user(&db, "cca").await;
    let acct = AuthRepo::new(&db)
        .create_credential_account(&uid, "hash_value")
        .await
        .expect("ok");
    assert_eq!(acct.user_id, uid);
    assert_eq!(acct.provider_id, "credential");
    assert_eq!(acct.password.as_deref(), Some("hash_value"));
}

// ===== AuthRepo::revoke_api_key =====

/// 8. revoke_api_key — 设置 revoked_at。
#[tokio::test(flavor = "current_thread")]
async fn revoke_api_key_basic() {
    let db = db().await;
    let uid = insert_user(&db, "rak").await;
    let kid = insert_api_key(&db, &uid).await;
    let revoked = AuthRepo::new(&db).revoke_api_key(kid, &uid).await.expect("ok");
    assert!(revoked, "first revoke should succeed");
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM board_api_keys WHERE id=$1 AND revoked_at IS NOT NULL")
        .bind(kid).fetch_one(db.pool()).await.expect("q");
    assert_eq!(n, 1);
}

/// 9. revoke_api_key — 重复调用返回 false（已被 revoke 的 key）。
#[tokio::test(flavor = "current_thread")]
async fn revoke_api_key_idempotent() {
    let db = db().await;
    let uid = insert_user(&db, "raki").await;
    let kid = insert_api_key(&db, &uid).await;
    let r1 = AuthRepo::new(&db).revoke_api_key(kid, &uid).await.expect("ok");
    let r2 = AuthRepo::new(&db).revoke_api_key(kid, &uid).await.expect("ok");
    assert!(r1);
    assert!(!r2, "second revoke should return false");
}

// ===== AuthRepo::revoke_session_by_token / revoke_all_sessions_for_user =====

async fn insert_session(db: &Db, user_id: &str, token: &str) -> String {
    let sid = format!("s_r140_{}", Uuid::new_v4().simple());
    sqlx::query("INSERT INTO session (id, user_id, token, expires_at, created_at, updated_at) VALUES ($1,$2,$3, now() + interval '7 days', now(), now())")
        .bind(&sid).bind(user_id).bind(token)
        .execute(db.pool()).await.expect("session");
    sid
}

/// 10. revoke_session_by_token — 删指定 token。
#[tokio::test(flavor = "current_thread")]
async fn revoke_session_by_token_basic() {
    let db = db().await;
    let uid = insert_user(&db, "rst").await;
    insert_session(&db, &uid, "tk_rst_1").await;
    let revoked = AuthRepo::new(&db).revoke_session_by_token("tk_rst_1").await.expect("ok");
    assert!(revoked);
}

/// 11. revoke_all_sessions_for_user — 删除该 user 全部 session。
#[tokio::test(flavor = "current_thread")]
async fn revoke_all_sessions_for_user_basic() {
    let db = db().await;
    let uid = insert_user(&db, "ras").await;
    insert_session(&db, &uid, "tk_ras_1").await;
    insert_session(&db, &uid, "tk_ras_2").await;
    let n = AuthRepo::new(&db).revoke_all_sessions_for_user(&uid).await.expect("ok");
    assert!(n >= 2);
}

// ===== AuthRepo::update_user_name / update_user_image =====

/// 12. update_user_name — 真实更新。
#[tokio::test(flavor = "current_thread")]
async fn update_user_name_basic() {
    let db = db().await;
    let uid = insert_user(&db, "uun").await;
    let updated = AuthRepo::new(&db).update_user_name(&uid, "New Name").await.expect("ok");
    assert!(updated);
    let row = AuthRepo::new(&db).find_by_id(&uid).await.expect("ok").unwrap();
    assert_eq!(row.name, "New Name");
}

/// 13. update_user_image — 真实更新。
#[tokio::test(flavor = "current_thread")]
async fn update_user_image_basic() {
    let db = db().await;
    let uid = insert_user(&db, "uui").await;
    let updated = AuthRepo::new(&db).update_user_image(&uid, "https://img.test/me.png").await.expect("ok");
    assert!(updated);
    let row = AuthRepo::new(&db).find_by_id(&uid).await.expect("ok").unwrap();
    assert_eq!(row.image.as_deref(), Some("https://img.test/me.png"));
}

// ===== CompanyMemberRepo::list_company_ids_for_user =====

/// 14. list_company_ids_for_user — 多家公司。
#[tokio::test(flavor = "current_thread")]
async fn list_company_ids_for_user_basic() {
    let db = db().await;
    let uid = insert_user(&db, "lc").await;
    let c1 = insert_company(&db).await;
    let c2 = insert_company(&db).await;
    insert_membership(&db, &uid, c1).await;
    insert_membership(&db, &uid, c2).await;
    let mut ids = CompanyMemberRepo::new(&db).list_company_ids_for_user(&uid).await.expect("ok");
    ids.sort();
    let mut want = vec![c1, c2];
    want.sort();
    assert_eq!(ids, want);
}

/// 15. list_company_ids_for_user — 用户无公司返回空。
#[tokio::test(flavor = "current_thread")]
async fn list_company_ids_for_user_empty() {
    let db = db().await;
    let uid = insert_user(&db, "lce").await;
    let ids = CompanyMemberRepo::new(&db).list_company_ids_for_user(&uid).await.expect("ok");
    assert!(ids.is_empty());
}

// ===== NewUser / NewSession 构造器（DTO smoke） =====

/// 16. NewUser DTO carries fields。
#[test]
fn new_user_dto_carries_fields() {
    let u = NewUser {
        id: "u_1".into(),
        name: "x".into(),
        email: "x@y".into(),
        email_verified: false,
        image: Some("img".into()),
    };
    assert_eq!(u.id, "u_1");
    assert_eq!(u.image.as_deref(), Some("img"));
}

/// 17. NewSession DTO carries fields。
#[test]
fn new_session_dto_carries_fields() {
    let s = NewSession {
        id: "s_1".into(),
        token: "tk".into(),
        user_id: "u_1".into(),
        expires_at: pc_core::Timestamp::from_dt(chrono::Utc::now() + chrono::Duration::days(7)),
        ip_address: Some("127.0.0.1".into()),
        user_agent: Some("ua".into()),
    };
    assert_eq!(s.token, "tk");
    assert_eq!(s.ip_address.as_deref(), Some("127.0.0.1"));
}
