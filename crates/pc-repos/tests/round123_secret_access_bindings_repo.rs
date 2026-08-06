//! Round 123 集成测试：SecretRepo bindings + access_events + patch_company_secret。

use pc_db::Db;
use pc_repos::secret::{NewCompanySecret, SecretRepo, SecretScope};
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r123-{tag}-{id}"))
        .bind(format!("R123{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

async fn insert_secret(db: &Db, company_id: Uuid, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO company_secrets (id, company_id, scope, key, name, provider, status, latest_version) \
         VALUES ($1, $2, 'company', $3, $4, 'manual', 'active', 1)",
    )
    .bind(id).bind(company_id).bind(format!("k-{id}")).bind(name)
    .execute(db.pool()).await.expect("insert secret");
    id
}

async fn insert_binding(db: &Db, company_id: Uuid, secret_id: Uuid) {
    sqlx::query(
        "INSERT INTO company_secret_bindings (id, company_id, secret_id, target_type, target_id, config_path) \
         VALUES ($1, $2, $3, 'agent', $4, '/env')",
    )
    .bind(Uuid::new_v4()).bind(company_id).bind(secret_id).bind(format!("a-{}", &secret_id.simple().to_string()[..6]))
    .execute(db.pool()).await.expect("insert binding");
}

async fn insert_access_event(db: &Db, secret_id: Uuid, company_id: Uuid) {
    sqlx::query(
        "INSERT INTO secret_access_events (company_id, secret_id, secret_scope, provider, actor_type, consumer_type, consumer_id, outcome) \
         VALUES ($1, $2, 'company', 'manual', 'agent', 'agent_run', $3, 'ok')",
    )
    .bind(company_id).bind(secret_id).bind(format!("r-{}", &Uuid::new_v4().simple().to_string()[..6]))
    .execute(db.pool()).await.expect("insert event");
}

/// 1. list_bindings_for_secret
#[tokio::test(flavor = "current_thread")]
async fn list_bindings_for_secret_returns_bindings() {
    let db = db().await;
    let cid = insert_company(&db, "bindings").await;
    let sid = insert_secret(&db, cid, "test-secret").await;
    insert_binding(&db, cid, sid).await;
    insert_binding(&db, cid, sid).await;
    let rows = SecretRepo::new(&db)
        .list_bindings_for_secret(sid)
        .await
        .expect("list bindings");
    assert_eq!(rows.len(), 2);
}

/// 2. list_access_events_for_secret
#[tokio::test(flavor = "current_thread")]
async fn list_access_events_for_secret_returns_events() {
    let db = db().await;
    let cid = insert_company(&db, "events").await;
    let sid = insert_secret(&db, cid, "test-secret").await;
    insert_access_event(&db, sid, cid).await;
    insert_access_event(&db, sid, cid).await;
    insert_access_event(&db, sid, cid).await;
    let rows = SecretRepo::new(&db)
        .list_access_events_for_secret(sid, 100)
        .await
        .expect("list events");
    assert_eq!(rows.len(), 3);
}

/// 3. patch_company_secret — 仅更新 name
#[tokio::test(flavor = "current_thread")]
async fn patch_company_secret_updates_name() {
    let db = db().await;
    let cid = insert_company(&db, "patch-name").await;
    let sid = insert_secret(&db, cid, "old name").await;
    let row = SecretRepo::new(&db)
        .patch_company_secret(sid, Some("new name"), None)
        .await
        .expect("patch")
        .unwrap();
    assert_eq!(row.name, "new name");
}

/// 4. patch_company_secret — 仅更新 description
#[tokio::test(flavor = "current_thread")]
async fn patch_company_secret_updates_description() {
    let db = db().await;
    let cid = insert_company(&db, "patch-desc").await;
    let sid = insert_secret(&db, cid, "kept").await;
    let row = SecretRepo::new(&db)
        .patch_company_secret(sid, None, Some(Some("new desc".into())))
        .await
        .expect("patch")
        .unwrap();
    assert_eq!(row.description, Some("new desc".to_owned()));
}

/// 5. patch_company_secret — 不存在返回 None
#[tokio::test(flavor = "current_thread")]
async fn patch_company_secret_missing_returns_none() {
    let db = db().await;
    let row = SecretRepo::new(&db)
        .patch_company_secret(Uuid::new_v4(), Some("x"), None)
        .await
        .expect("patch");
    assert!(row.is_none());
}

/// 6. patch_company_secret — None 字段保持原值
#[tokio::test(flavor = "current_thread")]
async fn patch_company_secret_keeps_unchanged() {
    let db = db().await;
    let cid = insert_company(&db, "patch-keep").await;
    let sid = insert_secret(&db, cid, "original").await;
    let row = SecretRepo::new(&db)
        .patch_company_secret(sid, None, None)
        .await
        .expect("patch")
        .unwrap();
    assert_eq!(row.name, "original");
}
