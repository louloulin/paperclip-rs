//! R615: pc-invite e2e service tests (Postgres-backed).
//!
//! Validates:
//! - InviteService construction + hook attachment
//! - create emits Created hook with token + role
//! - list_by_company returns the invite with status decoration
//! - find_active_by_token / find_by_token_hash
//! - revoke emits Revoked hook and is idempotent
//! - accept_with_token emits Accepted hook
//! - normalize_new rejects empty inputs and past expiry

use std::sync::Arc;

use chrono::{Duration, Utc};
use pc_core::Timestamp;
use pc_invite::{InviteHookEvent, InviteService, NewInvite, RecordingInviteHook};
use pc_repos::Db;
use sqlx::PgPool;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn setup_db() -> (Db, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(TEST_DATABASE_URL)
        .await
        .expect("connect");
    let db = Db::connect(TEST_DATABASE_URL, 4, 1).await.expect("Db");
    (db, pool)
}

async fn insert_company(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    let prefix = format!(
        "R{}",
        Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .take(5)
            .collect::<String>()
    );
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at)          VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R615ct-{id}"))
    .bind(prefix)
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM invites WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
}

fn make_input(company_id: Uuid) -> NewInvite {
    NewInvite {
        company_id,
        invite_type: "company".into(),
        allowed_join_types: "user,agent".into(),
        defaults_payload: Some(serde_json::json!({"role": "member"})),
        expires_at: Timestamp::from_dt(Utc::now() + Duration::days(7)),
        invited_by_user_id: Some("u1".into()),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn service_constructs_with_new_and_with_hooks() {
    let _guard = TEST_LOCK.lock().await;
    let (db, _pool) = setup_db().await;
    let svc = InviteService::new(db.clone());
    assert_eq!(svc.hook_count(), 0);
    let recorder = Arc::new(RecordingInviteHook::default());
    let svc2 = InviteService::with_hooks(db, vec![recorder.clone()]);
    assert_eq!(svc2.hook_count(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn create_emits_created_hook_with_raw_token() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let recorder = Arc::new(RecordingInviteHook::default());
    let svc = InviteService::with_hooks(db, vec![recorder.clone()]);

    let created = svc.create(make_input(company_id)).await.expect("create");
    assert_eq!(created.row.company_id, company_id);
    assert!(!created.token.is_empty(), "token must be returned once");
    assert_eq!(created.role, "member");
    assert_eq!(created.row.invited_by_user_id.as_deref(), Some("u1"));

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], InviteHookEvent::Created { .. }));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn create_rejects_past_expiry() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = InviteService::new(db);
    let mut input = make_input(company_id);
    input.expires_at = Timestamp::from_dt(Utc::now() - Duration::hours(1));
    let res = svc.create(input).await;
    assert!(res.is_err());
    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn create_rejects_empty_invite_type() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = InviteService::new(db);
    let mut input = make_input(company_id);
    input.invite_type = "  ".into();
    let res = svc.create(input).await;
    assert!(res.is_err());
    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn list_by_company_returns_decorated_invites() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = InviteService::new(db);

    svc.create(make_input(company_id)).await.expect("create");
    let invites = svc.list_by_company(company_id).await.expect("list");
    assert_eq!(invites.len(), 1);
    assert_eq!(invites[0].status, pc_invite::InviteStatus::Pending);
    assert_eq!(invites[0].role, "member");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn find_active_by_token_resolves_invite() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = InviteService::new(db);

    let created = svc.create(make_input(company_id)).await.expect("create");
    let token = created.token.clone();

    let row = svc
        .find_active_by_token(&token)
        .await
        .expect("find")
        .expect("exists");
    assert_eq!(row.id, created.row.id);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn revoke_emits_hook_and_is_idempotent() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let recorder = Arc::new(RecordingInviteHook::default());
    let svc = InviteService::with_hooks(db, vec![recorder.clone()]);

    let created = svc.create(make_input(company_id)).await.expect("create");
    recorder.clear();

    let ok = svc
        .revoke(company_id, created.row.id)
        .await
        .expect("revoke");
    assert!(ok);

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], InviteHookEvent::Revoked { .. }));

    recorder.clear();
    let again = svc
        .revoke(company_id, created.row.id)
        .await
        .expect("revoke 2");
    assert!(!again);
    assert!(recorder.is_empty());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn accept_with_token_emits_accepted_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let recorder = Arc::new(RecordingInviteHook::default());
    let svc = InviteService::with_hooks(db, vec![recorder.clone()]);

    let created = svc.create(make_input(company_id)).await.expect("create");
    let token = created.token.clone();
    recorder.clear();

    let row = svc.accept_with_token(&token).await.expect("accept");
    assert_eq!(row.id, created.row.id);

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    match &events[0] {
        InviteHookEvent::Accepted {
            company_id: cid, ..
        } => {
            assert_eq!(*cid, company_id);
        }
        _ => panic!("expected Accepted"),
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn accept_with_invalid_token_returns_error() {
    let _guard = TEST_LOCK.lock().await;
    let (db, _pool) = setup_db().await;
    let svc = InviteService::new(db);
    let res = svc.accept_with_token("not-a-real-token").await;
    assert!(res.is_err());
}
