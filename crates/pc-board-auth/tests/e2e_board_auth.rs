#![forbid(unsafe_code)]
//! Round 687: pc-board-auth 端到端测试（Postgres 真实环境）。

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use pc_board_auth::service::Clock;
use pc_board_auth::{
    board_auth_service, hash_bearer_token, BoardAccess, BoardApiKeyCreated, BoardAuthService,
    BoardAuthServiceError, ChallengeStatus, CliRequestedAccess,
};
use pc_repos::board_key::BoardKeyRow;
use pc_repos::instance_user_role::InstanceUserRoleRepo;
use pc_repos::Db;
use uuid::Uuid;

const TAG: &str = "r687";

async fn make_db() -> Db {
    let url = std::env::var("PAPERCLIP_TEST_DB_URL").unwrap_or_else(|_| {
        "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos".to_string()
    });
    Db::connect(&url, 4, 1).await.expect("connect to test db")
}

async fn cleanup(db: &Db, tag: &str) {
    sqlx::query("DELETE FROM cli_auth_challenges WHERE pending_key_name LIKE $1")
        .bind(format!("%{}%", tag))
        .execute(db.pool())
        .await
        .ok();
    sqlx::query("DELETE FROM board_api_keys WHERE name LIKE $1")
        .bind(format!("%{}%", tag))
        .execute(db.pool())
        .await
        .ok();
    sqlx::query("DELETE FROM company_memberships WHERE principal_id LIKE $1")
        .bind(format!("%{}%", tag))
        .execute(db.pool())
        .await
        .ok();
    sqlx::query("DELETE FROM \"user\" WHERE id LIKE $1")
        .bind(format!("%{}%", tag))
        .execute(db.pool())
        .await
        .ok();
    sqlx::query("DELETE FROM companies WHERE name LIKE $1")
        .bind(format!("%{}%", tag))
        .execute(db.pool())
        .await
        .ok();
}

async fn make_user(db: &Db, tag: &str) -> String {
    let id = format!("user-{}-{}", tag, Uuid::new_v4());
    sqlx::query("INSERT INTO \"user\" (id, name, email, created_at, updated_at) VALUES ($1, $2, $3, now(), now())")
        .bind(&id)
        .bind(format!("User {}", tag))
        .bind(format!("{tag}@test.com"))
        .execute(db.pool())
        .await
        .expect("create user");
    id
}

async fn make_company(db: &Db, tag: &str) -> Uuid {
    let name = format!("Co {} {}", tag, Uuid::new_v4());
    let prefix = format!(
        "P{:02}{:02}",
        Uuid::new_v4().as_u128() as u32 % 100,
        Uuid::new_v4().as_u128() as u32 % 100
    );
    let row: (Uuid,) =
        sqlx::query_as("INSERT INTO companies (name, issue_prefix) VALUES ($1, $2) RETURNING id")
            .bind(&name)
            .bind(&prefix)
            .fetch_one(db.pool())
            .await
            .expect("create company");
    row.0
}

async fn make_membership(db: &Db, user_id: &str, company_id: Uuid) {
    sqlx::query(
        "INSERT INTO company_memberships (company_id, principal_type, principal_id, status, membership_role) \
         VALUES ($1, 'user', $2, 'active', 'member')",
    )
    .bind(company_id)
    .bind(user_id)
    .execute(db.pool())
    .await
    .expect("create membership");
}

struct FixedClock(Arc<AtomicI64>);

impl Clock for FixedClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn fresh_clock() -> Arc<FixedClock> {
    Arc::new(FixedClock(Arc::new(AtomicI64::new(1_700_000_000_000))))
}

#[tokio::test]
async fn r687_e2e_create_and_find_board_api_key_by_token() {
    let db = make_db().await;
    cleanup(&db, TAG).await;
    let user_id = make_user(&db, TAG).await;
    let svc = board_auth_service(db.clone());

    let created: BoardApiKeyCreated = svc
        .create_named_board_api_key(&user_id, "r687-laptop", None)
        .await
        .expect("create");
    assert!(created.token.starts_with("pcp_board_"));
    assert_eq!(created.name, "r687-laptop");

    // 通过 token 找 key
    let found = svc
        .find_board_api_key_by_token(&created.token)
        .await
        .expect("find")
        .expect("found");
    assert_eq!(found.id, created.id);
    assert_eq!(found.user_id, user_id);

    // touch 一次
    svc.touch_board_api_key(found.id).await.expect("touch");
    let after_touch = svc
        .find_board_api_key_by_token(&created.token)
        .await
        .expect("find")
        .expect("found");
    assert!(after_touch.last_used_at.is_some());

    cleanup(&db, TAG).await;
}

#[tokio::test]
async fn r687_e2e_list_filters_inactive() {
    let db = make_db().await;
    cleanup(&db, TAG).await;
    let user_id = make_user(&db, TAG).await;
    let svc = board_auth_service(db.clone());

    let k1 = svc
        .create_named_board_api_key(&user_id, "r687-active-1", None)
        .await
        .unwrap();
    let k2 = svc
        .create_named_board_api_key(&user_id, "r687-active-2", None)
        .await
        .unwrap();

    let active = svc.list_board_api_keys(&user_id, false).await.unwrap();
    assert_eq!(active.len(), 2);

    // revoke k2
    svc.revoke_board_api_key(k2.id, &user_id).await.unwrap();

    let active2 = svc.list_board_api_keys(&user_id, false).await.unwrap();
    assert_eq!(active2.len(), 1);
    assert_eq!(active2[0].id, k1.id);

    let all = svc.list_board_api_keys(&user_id, true).await.unwrap();
    assert_eq!(all.len(), 2);

    // revoked token 不再可解析
    let still = svc.find_board_api_key_by_token(&k2.token).await.unwrap();
    assert!(still.is_none());

    cleanup(&db, TAG).await;
}

#[tokio::test]
async fn r687_e2e_resolve_board_access_for_member_and_admin() {
    let db = make_db().await;
    cleanup(&db, TAG).await;
    let user_id = make_user(&db, TAG).await;
    let admin_id = make_user(&db, &format!("{TAG}-admin")).await;
    let company_a = make_company(&db, TAG).await;
    let company_b = make_company(&db, &format!("{TAG}-b")).await;
    make_membership(&db, &user_id, company_a).await;

    let svc = board_auth_service(db.clone());

    // 普通成员
    let access: BoardAccess = svc.resolve_board_access(&user_id).await.unwrap();
    assert!(access.user.is_some());
    assert!(!access.is_instance_admin);
    assert_eq!(access.company_ids.len(), 1);
    assert!(access.company_ids.contains(&company_a));

    // instance admin
    InstanceUserRoleRepo::new(&db)
        .promote(&admin_id)
        .await
        .unwrap();
    let admin_access = svc.resolve_board_access(&admin_id).await.unwrap();
    assert!(admin_access.is_instance_admin);
    assert_eq!(admin_access.company_ids.len(), 0); // 没有 membership
    let all = svc
        .resolve_board_activity_company_ids(&admin_id, None, None)
        .await
        .unwrap();
    assert!(all.len() >= 2); // 至少包含 company_a, company_b
    assert!(all.contains(&company_a));
    assert!(all.contains(&company_b));

    cleanup(&db, TAG).await;
}

#[tokio::test]
async fn r687_e2e_resolve_board_activity_falls_back_to_requested_company() {
    let db = make_db().await;
    cleanup(&db, TAG).await;
    let user_id = make_user(&db, TAG).await;
    let co = make_company(&db, TAG).await;
    let svc = board_auth_service(db.clone());

    let ids = svc
        .resolve_board_activity_company_ids(&user_id, Some(&co.to_string()), None)
        .await
        .unwrap();
    assert_eq!(ids, vec![co]);

    cleanup(&db, TAG).await;
}

#[tokio::test]
async fn r687_e2e_cli_auth_challenge_full_lifecycle() {
    let db = make_db().await;
    cleanup(&db, TAG).await;
    let user_id = make_user(&db, TAG).await;
    let co = make_company(&db, TAG).await;
    make_membership(&db, &user_id, co).await;
    let svc = board_auth_service(db.clone());

    // 1. 创建 challenge
    let created = svc
        .create_cli_auth_challenge(
            "paperclip login",
            Some("laptop"),
            CliRequestedAccess::Board,
            Some(co),
        )
        .await
        .unwrap();
    assert!(created.challenge_secret.starts_with("pcp_cli_auth_"));
    assert!(created.pending_board_token.starts_with("pcp_board_"));
    assert!(created.challenge.client_name.is_some());
    assert_eq!(created.challenge.requested_company_id, Some(co));

    // 2. describe（用 challenge_secret）
    let desc = svc
        .describe_cli_auth_challenge(created.challenge.id, &created.challenge_secret)
        .await
        .unwrap()
        .expect("described");
    use pc_board_auth::ChallengeStatus;
    assert_eq!(desc.status, ChallengeStatus::Pending);
    assert_eq!(desc.requested_company_id, Some(co));
    assert!(desc.approved_by_user.is_none());

    // 3. approve
    let (status, updated) = svc
        .approve_cli_auth_challenge(created.challenge.id, &created.challenge_secret, &user_id)
        .await
        .unwrap();
    assert_eq!(status, ChallengeStatus::Approved);
    assert!(updated.board_api_key_id.is_some());

    // 4. describe 再次应看到 approved + user
    let desc2 = svc
        .describe_cli_auth_challenge(created.challenge.id, &created.challenge_secret)
        .await
        .unwrap()
        .expect("described");
    assert_eq!(desc2.status, ChallengeStatus::Approved);
    assert!(desc2.approved_at.is_some());
    assert!(desc2.approved_by_user.is_some());

    // 5. 通过 pending_board_token 应可解析 board api key
    let key_row = svc
        .find_board_api_key_by_token(&created.pending_board_token)
        .await
        .unwrap()
        .expect("key found");
    assert_eq!(key_row.user_id, user_id);

    cleanup(&db, TAG).await;
}

#[tokio::test]
async fn r687_e2e_cli_auth_challenge_wrong_secret_returns_none() {
    let db = make_db().await;
    cleanup(&db, TAG).await;
    let user_id = make_user(&db, TAG).await;
    let svc = board_auth_service(db.clone());
    let created = svc
        .create_cli_auth_challenge("x", None, CliRequestedAccess::Board, None)
        .await
        .unwrap();
    let r = svc
        .describe_cli_auth_challenge(created.challenge.id, "wrong-secret")
        .await
        .unwrap();
    assert!(r.is_none());
    cleanup(&db, TAG).await;
}

#[tokio::test]
async fn r687_e2e_cli_auth_instance_admin_required_blocks_normal_user() {
    let db = make_db().await;
    cleanup(&db, TAG).await;
    let user_id = make_user(&db, TAG).await;
    let svc = board_auth_service(db.clone());
    let created = svc
        .create_cli_auth_challenge("x", None, CliRequestedAccess::InstanceAdminRequired, None)
        .await
        .unwrap();
    let err = svc
        .approve_cli_auth_challenge(created.challenge.id, &created.challenge_secret, &user_id)
        .await
        .expect_err("must be forbidden");
    match err {
        BoardAuthServiceError::Forbidden(_) => {}
        other => panic!("expected Forbidden, got {:?}", other),
    }
    cleanup(&db, TAG).await;
}

#[tokio::test]
async fn r687_e2e_cli_auth_cancel_before_approve() {
    let db = make_db().await;
    cleanup(&db, TAG).await;
    let user_id = make_user(&db, TAG).await;
    let svc = board_auth_service(db.clone());
    let created = svc
        .create_cli_auth_challenge("x", None, CliRequestedAccess::Board, None)
        .await
        .unwrap();
    let (status, _) = svc
        .cancel_cli_auth_challenge(created.challenge.id, &created.challenge_secret)
        .await
        .unwrap();
    assert_eq!(status, pc_board_auth::ChallengeStatus::Cancelled);
    // 取消后 approve 应短路返回 Cancelled
    let (status2, _) = svc
        .approve_cli_auth_challenge(created.challenge.id, &created.challenge_secret, &user_id)
        .await
        .unwrap();
    assert_eq!(status2, pc_board_auth::ChallengeStatus::Cancelled);
    cleanup(&db, TAG).await;
}

#[tokio::test]
async fn r687_e2e_cli_auth_expired_blocks_approve() {
    let db = make_db().await;
    cleanup(&db, TAG).await;
    let user_id = make_user(&db, TAG).await;
    let clock = fresh_clock();
    let svc = BoardAuthService::with_clock(db.clone(), clock.clone());
    let created = svc
        .create_cli_auth_challenge("x", None, CliRequestedAccess::Board, None)
        .await
        .unwrap();
    // 时间推进 11 分钟（> 10min TTL）
    clock
        .0
        .store(1_700_000_000_000 + 11 * 60 * 1000, Ordering::SeqCst);
    let (status, _) = svc
        .approve_cli_auth_challenge(created.challenge.id, &created.challenge_secret, &user_id)
        .await
        .unwrap();
    assert_eq!(status, pc_board_auth::ChallengeStatus::Expired);
    cleanup(&db, TAG).await;
}

#[tokio::test]
async fn r687_e2e_assert_current_board_key() {
    let db = make_db().await;
    cleanup(&db, TAG).await;
    let user_id = make_user(&db, TAG).await;
    let svc = board_auth_service(db.clone());
    let created = svc
        .create_named_board_api_key(&user_id, "r687-assert", None)
        .await
        .unwrap();
    let key = svc
        .assert_current_board_key(Some(created.id), Some(&user_id))
        .await
        .unwrap();
    assert_eq!(key.id, created.id);
    assert!(key.revoked_at.is_none());

    // revoked 后再断言应报错
    svc.revoke_board_api_key(created.id, &user_id)
        .await
        .unwrap();
    let err = svc
        .assert_current_board_key(Some(created.id), Some(&user_id))
        .await
        .expect_err("must be not found");
    assert!(matches!(err, BoardAuthServiceError::NotFound(_)));

    // 缺 user_id 应报 conflict
    let err2 = svc
        .assert_current_board_key(Some(created.id), None)
        .await
        .expect_err("must be conflict");
    assert!(matches!(err2, BoardAuthServiceError::Conflict(_)));

    cleanup(&db, TAG).await;
}

#[tokio::test]
async fn r687_e2e_expired_token_not_resolved() {
    let db = make_db().await;
    cleanup(&db, TAG).await;
    let user_id = make_user(&db, TAG).await;
    let clock = fresh_clock();
    let svc = BoardAuthService::with_clock(db.clone(), clock.clone());
    // 创建一个过期时间已过的 key
    let past = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(
        clock.0.load(Ordering::SeqCst) - 1000,
    )
    .unwrap();
    let created = svc
        .create_named_board_api_key(&user_id, "r687-expired", Some(past))
        .await
        .unwrap();
    // 推进 clock 让 now > expires_at
    clock.0.fetch_add(2000, Ordering::SeqCst);
    let found = svc
        .find_board_api_key_by_token(&created.token)
        .await
        .unwrap();
    assert!(found.is_none());
    cleanup(&db, TAG).await;
}

#[tokio::test]
async fn r687_e2e_get_board_api_key_for_user_returns_none_for_other_user() {
    let db = make_db().await;
    cleanup(&db, TAG).await;
    let user_a = make_user(&db, &format!("{TAG}-a")).await;
    let user_b = make_user(&db, &format!("{TAG}-b")).await;
    let svc = board_auth_service(db.clone());
    let created = svc
        .create_named_board_api_key(&user_a, "r687-other-user", None)
        .await
        .unwrap();
    // user_b 查 user_a 的 key 应 None
    let other: Option<BoardKeyRow> = svc
        .get_board_api_key_for_user(created.id, &user_b)
        .await
        .unwrap();
    assert!(other.is_none());
    // user_a 查自己的应 Some
    let own = svc
        .get_board_api_key_for_user(created.id, &user_a)
        .await
        .unwrap();
    assert!(own.is_some());
    cleanup(&db, TAG).await;
}
