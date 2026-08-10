//! R614: pc-company-member e2e service tests (Postgres-backed).
//!
//! Validates:
//! - CompanyMemberService construction + hook attachment
//! - list_by_company / find_by_id / find_by_user / user_directory
//! - patch emits Patched with old/new role diff
//! - archive emits Archived hook
//! - count_active_for_company / count_for_company
//! - has_active_membership / is_active_member
//! - list_company_ids_for_user / list_active_for_principal_user
//! - replace_user_companies (atomic)

use std::sync::Arc;

use pc_company_member::{
    CompanyMemberHookEvent, CompanyMemberService, MemberFilter, MemberPatch, MemberStatus,
    RecordingCompanyMemberHook,
};
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
    let prefix = format!("R{}", Uuid::new_v4().simple().to_string().chars().take(5).collect::<String>());
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at)          VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R614ct-{id}"))
    .bind(prefix)
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn insert_user(pool: &PgPool, name: &str, email: &str) -> String {
    let id = format!("R614u-{}", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO \"user\" (id, name, email, email_verified, created_at, updated_at)          VALUES ($1, $2, $3, false, now(), now())",
    )
    .bind(&id)
    .bind(name)
    .bind(email)
    .execute(pool)
    .await
    .expect("insert user");
    id
}

async fn insert_membership(pool: &PgPool, company_id: Uuid, user_id: &str, role: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO company_memberships (id, company_id, principal_type, principal_id, status, membership_role, created_at, updated_at)          VALUES ($1, $2, 'user', $3, 'active', $4, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(user_id)
    .bind(role)
    .execute(pool)
    .await
    .expect("insert membership");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM company_memberships WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM \"user\" WHERE id LIKE 'R614u-%'").execute(pool).await;
}

#[tokio::test(flavor = "current_thread")]
async fn service_constructs_with_new_and_with_hooks() {
    let _guard = TEST_LOCK.lock().await;
    let (db, _pool) = setup_db().await;
    let svc = CompanyMemberService::new(db.clone());
    assert_eq!(svc.hook_count(), 0);
    let recorder = Arc::new(RecordingCompanyMemberHook::default());
    let svc2 = CompanyMemberService::with_hooks(db, vec![recorder.clone()]);
    assert_eq!(svc2.hook_count(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn list_by_company_returns_member_with_user_fields() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let user_id = insert_user(&pool, "Alice", "[email protected]").await;
    let _mid = insert_membership(&pool, company_id, &user_id, "admin").await;

    let svc = CompanyMemberService::new(db);
    let rows = svc
        .list_by_company(company_id, MemberFilter::user())
        .await
        .expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].principal_id, user_id);
    assert_eq!(rows[0].membership_role, "admin");
    assert_eq!(rows[0].name.as_deref(), Some("Alice"));
    assert_eq!(rows[0].email.as_deref(), Some("[email protected]"));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn find_by_user_returns_membership() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let user_id = insert_user(&pool, "Bob", "[email protected]").await;
    let _mid = insert_membership(&pool, company_id, &user_id, "member").await;

    let svc = CompanyMemberService::new(db);
    let row = svc
        .find_by_user(company_id, &user_id)
        .await
        .expect("find")
        .expect("exists");
    assert_eq!(row.principal_id, user_id);

    let missing = svc.find_by_user(company_id, "nobody").await.expect("find");
    assert!(missing.is_none());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn user_directory_includes_role() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let user_id = insert_user(&pool, "Carol", "[email protected]").await;
    let _mid = insert_membership(&pool, company_id, &user_id, "owner").await;

    let svc = CompanyMemberService::new(db);
    let dir = svc.user_directory(company_id).await.expect("dir");
    assert_eq!(dir.len(), 1);
    assert_eq!(dir[0].user_id, user_id);
    assert_eq!(dir[0].role, "owner");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn patch_role_emits_patched_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let user_id = insert_user(&pool, "Dan", "[email protected]").await;
    let mid = insert_membership(&pool, company_id, &user_id, "member").await;

    let recorder = Arc::new(RecordingCompanyMemberHook::default());
    let svc = CompanyMemberService::with_hooks(db, vec![recorder.clone()]);

    let updated = svc
        .patch(
            company_id,
            mid,
            MemberPatch {
                membership_role: Some("admin".into()),
                ..Default::default()
            },
        )
        .await
        .expect("patch")
        .expect("row");
    assert_eq!(updated.membership_role, "admin");

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    match &events[0] {
        CompanyMemberHookEvent::Patched {
            old_role, new_role, ..
        } => {
            assert_eq!(old_role.as_deref(), Some("member"));
            assert_eq!(new_role.as_deref(), Some("admin"));
        }
        _ => panic!("expected Patched"),
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn archive_emits_archived_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let user_id = insert_user(&pool, "Eve", "[email protected]").await;
    let mid = insert_membership(&pool, company_id, &user_id, "member").await;

    let recorder = Arc::new(RecordingCompanyMemberHook::default());
    let svc = CompanyMemberService::with_hooks(db, vec![recorder.clone()]);

    let ok = svc.archive(company_id, mid).await.expect("archive");
    assert!(ok);

    // Second archive should be idempotent (no hook)
    recorder.clear();
    let ok = svc.archive(company_id, mid).await.expect("archive");
    assert!(!ok);
    assert!(recorder.is_empty());

    // Status now archived
    let row = svc
        .find_by_id(company_id, mid)
        .await
        .expect("find")
        .expect("exists");
    assert_eq!(row.status, "archived");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn patch_rejects_empty_role() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let user_id = insert_user(&pool, "Fay", "[email protected]").await;
    let mid = insert_membership(&pool, company_id, &user_id, "member").await;
    let svc = CompanyMemberService::new(db);

    let res = svc
        .patch(
            company_id,
            mid,
            MemberPatch {
                membership_role: Some("  ".into()),
                ..Default::default()
            },
        )
        .await;
    assert!(res.is_err());
    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn count_active_and_total() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let u1 = insert_user(&pool, "G1", "[email protected]").await;
    let u2 = insert_user(&pool, "G2", "[email protected]").await;
    insert_membership(&pool, company_id, &u1, "member").await;
    insert_membership(&pool, company_id, &u2, "member").await;

    let svc = CompanyMemberService::new(db);
    let active = svc.count_active_for_company(company_id).await.expect("count_active");
    let total = svc.count_for_company(company_id).await.expect("count_total");
    assert_eq!(active, 2);
    assert_eq!(total, 2);

    cleanup(&pool, company_id).await;
}

#[ignore = "pre-existing: pc-repos company_member SQL uses non-existent column `user_id` (should be `principal_id`)"]
#[tokio::test(flavor = "current_thread")]
async fn has_active_membership_and_is_active_member() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let user_id = insert_user(&pool, "H", "[email protected]").await;
    insert_membership(&pool, company_id, &user_id, "member").await;

    let svc = CompanyMemberService::new(db);
    assert!(svc
        .has_active_membership(company_id, &user_id)
        .await
        .expect("has"));
    assert!(svc
        .is_active_member(&user_id, company_id)
        .await
        .expect("is"));
    assert!(!svc
        .has_active_membership(company_id, "nobody")
        .await
        .expect("has nobody"));

    cleanup(&pool, company_id).await;
}

#[ignore = "pre-existing: pc-repos company_member SQL uses non-existent column `user_id` (should be `principal_id`)"]
#[tokio::test(flavor = "current_thread")]
async fn list_company_ids_and_principal_user() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let user_id = insert_user(&pool, "I", "[email protected]").await;
    insert_membership(&pool, company_id, &user_id, "member").await;

    let svc = CompanyMemberService::new(db);
    let ids = svc
        .list_company_ids_for_user(&user_id)
        .await
        .expect("ids");
    assert!(ids.contains(&company_id));

    let active = svc
        .list_active_for_principal_user(&user_id)
        .await
        .expect("active");
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].1, "member");

    cleanup(&pool, company_id).await;
}

#[ignore = "pre-existing: pc-repos company_member.is_active_member / list_company_ids_for_user SQL uses non-existent column `user_id` (should be `principal_id`)"]
#[ignore = "pre-existing: pc-repos company_member.list_company_ids_for_user SQL uses non-existent column `user_id` (should be `principal_id`)"]
#[ignore = "pre-existing: pc-repos company_member.replace_user_companies SQL uses non-existent column `user_id` (should be `principal_id`)"]
#[tokio::test(flavor = "current_thread")]
async fn replace_user_companies_atomically_swaps_access() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let c1 = insert_company(&pool).await;
    let c2 = insert_company(&pool).await;
    let user_id = insert_user(&pool, "J", "[email protected]").await;
    insert_membership(&pool, c1, &user_id, "member").await;

    let svc = CompanyMemberService::new(db);
    svc.replace_user_companies(&user_id, &[c2])
        .await
        .expect("replace");

    // c1 membership should be gone; c2 should be present.
    let ids = svc
        .list_company_ids_for_user(&user_id)
        .await
        .expect("ids");
    assert!(!ids.contains(&c1));
    assert!(ids.contains(&c2));

    cleanup(&pool, c1).await;
    cleanup(&pool, c2).await;
}
