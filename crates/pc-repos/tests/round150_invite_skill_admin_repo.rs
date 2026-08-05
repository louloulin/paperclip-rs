//! Round 150-152 集成测试：invite 扩展方法 + instance_user_role + admin helpers。

use pc_db::Db;
use pc_repos::instance_user_role::InstanceUserRoleRepo;
use pc_repos::invite::InviteRepo;
use pc_repos::skill::SkillRepo;
use pc_repos::user_profile::UserProfileRepo;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(format!("r150-c-{tag}-{id}"))
        .bind(format!("R150{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn insert_user(db: &Db, tag: &str) -> String {
    let id = format!("u_r150_{}_{}", tag, Uuid::new_v4().simple());
    sqlx::query("INSERT INTO "user" (id, name, email) VALUES ($1, $2, $3)")
        .bind(&id)
        .bind(format!("r150-{tag}"))
        .bind(format!("r150_{tag}_{id}@x"))
        .execute(db.pool())
        .await
        .expect("user");
    id
}

async fn insert_invite(db: &Db, company_id: Uuid, token_hash: &str, invited_by: Option<&str>) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO invites (id, company_id, token_hash, defaults_payload, expires_at, invited_by_user_id) \
         VALUES ($1, $2, $3, '{}'::jsonb, now() + interval '7 days', $4)",
    )
    .bind(id)
    .bind(company_id)
    .bind(token_hash)
    .bind(invited_by)
    .execute(db.pool())
    .await
    .expect("invite");
    id
}

// ===== InviteRepo::lookup_revoke_info_by_token_hash =====

/// 1. lookup_revoke_info_by_token_hash — 命中返回 (id, company_id, invited_by)。
#[tokio::test(flavor = "current_thread")]
async fn invite_lookup_revoke_info_hit() {
    let db = db().await;
    let cid = insert_company(&db, "lu1").await;
    let user = insert_user(&db, "lu1").await;
    let id = insert_invite(&db, cid, "th-lu1", Some(&user)).await;
    let repo = InviteRepo::new(&db);
    let row = repo
        .lookup_revoke_info_by_token_hash("th-lu1")
        .await
        .expect("ok");
    assert!(row.is_some());
    let (rid, rcid, inviter) = row.unwrap();
    assert_eq!(rid, id);
    assert_eq!(rcid, cid);
    assert_eq!(inviter.as_deref(), Some(user.as_str()));
}

/// 2. lookup_revoke_info_by_token_hash — 不存在返回 None。
#[tokio::test(flavor = "current_thread")]
async fn invite_lookup_revoke_info_miss() {
    let db = db().await;
    let repo = InviteRepo::new(&db);
    let row = repo.lookup_revoke_info_by_token_hash("missing").await.expect("ok");
    assert!(row.is_none());
}

// ===== InviteRepo::revoke_by_id =====

/// 3. revoke_by_id — 撤销未撤销的 invite 返回 affected=1。
#[tokio::test(flavor = "current_thread")]
async fn invite_revoke_by_id_ok() {
    let db = db().await;
    let cid = insert_company(&db, "rb1").await;
    let id = insert_invite(&db, cid, "th-rb1", None).await;
    let repo = InviteRepo::new(&db);
    let affected = repo.revoke_by_id(id).await.expect("ok");
    assert_eq!(affected, 1);
}

/// 4. revoke_by_id — 撤销已撤销的 invite 返回 affected=0。
#[tokio::test(flavor = "current_thread")]
async fn invite_revoke_by_id_already_revoked() {
    let db = db().await;
    let cid = insert_company(&db, "rb2").await;
    let id = insert_invite(&db, cid, "th-rb2", None).await;
    let repo = InviteRepo::new(&db);
    let _ = repo.revoke_by_id(id).await.expect("first");
    let affected = repo.revoke_by_id(id).await.expect("second");
    assert_eq!(affected, 0);
}

// ===== InstanceUserRoleRepo::promote / demote =====

/// 5. promote — 首次 promote 返回 role assignment id。
#[tokio::test(flavor = "current_thread")]
async fn instance_role_promote_returns_id() {
    let db = db().await;
    let user = insert_user(&db, "pr1").await;
    let repo = InstanceUserRoleRepo::new(&db);
    let id = repo.promote(&user).await.expect("promote");
    assert!(!id.is_nil());
}

/// 6. promote — 重复 promote 幂等（ON CONFLICT DO UPDATE）。
#[tokio::test(flavor = "current_thread")]
async fn instance_role_promote_idempotent() {
    let db = db().await;
    let user = insert_user(&db, "pr2").await;
    let repo = InstanceUserRoleRepo::new(&db);
    let id1 = repo.promote(&user).await.expect("first");
    let id2 = repo.promote(&user).await.expect("second");
    // 同 user_id 应返回相同 assignment id (ON CONFLICT)。
    assert_eq!(id1, id2);
}

/// 7. demote — 已 promote 后 demote 返回 affected=1。
#[tokio::test(flavor = "current_thread")]
async fn instance_role_demote_ok() {
    let db = db().await;
    let user = insert_user(&db, "dm1").await;
    let repo = InstanceUserRoleRepo::new(&db);
    let _ = repo.promote(&user).await.expect("promote");
    let affected = repo.demote(&user).await.expect("demote");
    assert_eq!(affected, 1);
}

/// 8. demote — 未 promote 直接 demote 返回 affected=0。
#[tokio::test(flavor = "current_thread")]
async fn instance_role_demote_noop() {
    let db = db().await;
    let user = insert_user(&db, "dm2").await;
    let repo = InstanceUserRoleRepo::new(&db);
    let affected = repo.demote(&user).await.expect("demote");
    assert_eq!(affected, 0);
}

/// 9. list_user_ids_with_any_role — 命中返回 user_id。
#[tokio::test(flavor = "current_thread")]
async fn instance_role_list_user_ids_hit() {
    let db = db().await;
    let user = insert_user(&db, "ls1").await;
    let repo = InstanceUserRoleRepo::new(&db);
    let _ = repo.promote(&user).await.expect("promote");
    let ids = repo
        .list_user_ids_with_any_role(&[user.clone()])
        .await
        .expect("list");
    assert!(ids.contains(&user));
}

/// 10. list_user_ids_with_any_role — 不在 user_ids 中返回空。
#[tokio::test(flavor = "current_thread")]
async fn instance_role_list_user_ids_miss() {
    let db = db().await;
    let repo = InstanceUserRoleRepo::new(&db);
    let ids = repo
        .list_user_ids_with_any_role(&["u_no_such_user_xyz".to_owned()])
        .await
        .expect("list");
    assert!(ids.is_empty());
}

// ===== UserProfileRepo::list_recent =====

/// 11. list_recent — 返回 user 列表（按 updated_at DESC）。
#[tokio::test(flavor = "current_thread")]
async fn user_profile_list_recent() {
    let db = db().await;
    let user = insert_user(&db, "lr1").await;
    let repo = UserProfileRepo::new(&db);
    let rows = repo.list_recent(10).await.expect("list");
    assert!(rows.iter().any(|(id, _, _, _, _)| id == &user));
}

// ===== SkillRepo::find_content_by_key =====

/// 12. find_content_by_key — 不存在返回 None。
#[tokio::test(flavor = "current_thread")]
async fn skill_find_content_by_key_miss() {
    let db = db().await;
    let repo = SkillRepo::new(&db);
    let row = repo.find_content_by_key("__nope__").await.expect("ok");
    assert!(row.is_none());
}

// ===== DTO / 类型 smoke (sync) =====

/// 13. InstanceUserRoleRow 类型 smoke。
#[test]
fn instance_user_role_row_typecheck() {
    use pc_repos::instance_user_role::InstanceUserRoleRow;
    fn assert_from_row<T: sqlx::FromRow<sqlx::postgres::PgRow>>() {}
    assert_from_row::<InstanceUserRoleRow>();
}
