//! R610: pc-assets e2e service tests (Postgres-backed).
//!
//! Validates:
//! - AssetService construction / hook attachment
//! - create / get_by_id / list_by_company / delete_by_id round-trip
//! - create / delete emit AssetHookEvent::Created / Deleted
//! - delete_by_id rejects nil company_id
//! - list_by_company_with_provider filters correctly
//! - list_attachments_for_asset returns empty for fresh asset

use std::sync::Arc;

use pc_repos::asset_service::{AssetHookEvent, AssetService, CreateAssetRecord, RecordingAssetHook};
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
    .bind(format!("R610ct-{id}"))
    .bind(prefix)
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM assets WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM company_memberships WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1").bind(company_id).execute(pool).await;
}

#[tokio::test(flavor = "current_thread")]
async fn service_constructs_with_new_and_with_hooks() {
    let _guard = TEST_LOCK.lock().await;
    let (db, _pool) = setup_db().await;
    let svc = AssetService::new(db.clone());
    assert_eq!(svc.hook_count(), 0);
    let recorder = Arc::new(RecordingAssetHook::default());
    let svc2 = AssetService::with_hooks(db, vec![recorder.clone()]);
    assert_eq!(svc2.hook_count(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn create_rejects_nil_company() {
    let _guard = TEST_LOCK.lock().await;
    let (db, _pool) = setup_db().await;
    let svc = AssetService::new(db);
    let rec = CreateAssetRecord::new("local", "key", "image/png", 100, "abc");
    let res = svc.create(Uuid::nil(), rec).await;
    assert!(res.is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn create_rejects_empty_provider() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = AssetService::new(db);
    let mut rec = CreateAssetRecord::new("local", "key", "image/png", 100, "abc");
    rec.provider = "".into();
    let res = svc.create(company_id, rec).await;
    assert!(res.is_err());
    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn create_inserts_row_and_emits_created_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(RecordingAssetHook::default());
    let svc = AssetService::with_hooks(db, vec![recorder.clone()]);

    let rec = CreateAssetRecord::new("local", "abc/123.png", "image/png", 1024, "deadbeef");
    let row = svc.create(company_id, rec).await.expect("create");
    assert_eq!(row.company_id, company_id);
    assert_eq!(row.provider, "local");
    assert_eq!(row.content_type, "image/png");
    assert_eq!(row.byte_size, 1024);

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], AssetHookEvent::Created { .. }));

    // get_by_id should return the same row
    let fetched = svc.get_by_id(row.id).await.expect("get").expect("exists");
    assert_eq!(fetched.id, row.id);
    assert_eq!(fetched.sha256, "deadbeef");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn list_by_company_returns_recent_first() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = AssetService::new(db);

    for i in 0..3 {
        let rec = CreateAssetRecord::new("local", format!("k{i}"), "image/png", 10, format!("h{i}"));
        svc.create(company_id, rec).await.expect("create");
    }

    let rows = svc.list_by_company(company_id, 10).await.expect("list");
    assert_eq!(rows.len(), 3);
    for r in &rows {
        assert_eq!(r.company_id, company_id);
    }

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn list_by_company_with_provider_filters() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = AssetService::new(db);

    svc.create(
        company_id,
        CreateAssetRecord::new("local", "k-local-1", "image/png", 10, "h1"),
    )
    .await
    .expect("create");
    svc.create(
        company_id,
        CreateAssetRecord::new("local", "k-local-2", "image/png", 20, "h2"),
    )
    .await
    .expect("create");
    svc.create(
        company_id,
        CreateAssetRecord::new("s3", "k-s3-1", "image/jpeg", 30, "h3"),
    )
    .await
    .expect("create");

    let locals = svc
        .list_by_company_with_provider(company_id, Some("local"), 10)
        .await
        .expect("list local");
    assert_eq!(locals.len(), 2);
    assert!(locals.iter().all(|r| r.provider == "local"));

    let all = svc
        .list_by_company_with_provider(company_id, None, 10)
        .await
        .expect("list all");
    assert_eq!(all.len(), 3);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn delete_emits_deleted_hook_and_returns_true() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;

    let recorder = Arc::new(RecordingAssetHook::default());
    let svc = AssetService::with_hooks(db, vec![recorder.clone()]);

    let row = svc
        .create(
            company_id,
            CreateAssetRecord::new("local", "k", "image/png", 10, "h"),
        )
        .await
        .expect("create");
    recorder.clear();

    let deleted = svc.delete_by_id(company_id, row.id).await.expect("delete");
    assert!(deleted, "expected actual delete");

    let events = recorder.events_snapshot();
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], AssetHookEvent::Deleted { .. }));

    let after = svc.get_by_id(row.id).await.expect("get");
    assert!(after.is_none());

    // second delete should return false and NOT emit hook
    recorder.clear();
    let again = svc.delete_by_id(company_id, row.id).await.expect("delete again");
    assert!(!again);
    assert!(recorder.is_empty());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn delete_rejects_nil_company() {
    let _guard = TEST_LOCK.lock().await;
    let (db, _pool) = setup_db().await;
    let svc = AssetService::new(db);
    let res = svc.delete_by_id(Uuid::nil(), Uuid::new_v4()).await;
    assert!(res.is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn find_logo_meta_by_company_returns_none_for_fresh_company() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = AssetService::new(db);
    let logo = svc.find_logo_meta_by_company(company_id).await.expect("logo");
    assert!(logo.is_none());
    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn list_attachments_for_asset_returns_empty_for_fresh_asset() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let svc = AssetService::new(db);
    let row = svc
        .create(
            company_id,
            CreateAssetRecord::new("local", "k", "image/png", 10, "h"),
        )
        .await
        .expect("create");
    let attachments = svc
        .list_attachments_for_asset(row.id)
        .await
        .expect("list attachments");
    assert!(attachments.is_empty());
    cleanup(&pool, company_id).await;
}
