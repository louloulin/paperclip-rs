//! Round 124 集成测试：SecretRepo 复合事务方法
//! patch_provider_config + rotate_company_secret。

use pc_db::Db;
use pc_repos::secret::{SecretRepo};
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
        .bind(format!("r124-{tag}-{id}"))
        .bind(format!("R124{}", &id.simple().to_string()[..4]))
        .execute(db.pool()).await.expect("insert company");
    id
}

async fn insert_provider(db: &Db, company_id: Uuid, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO company_secret_provider_configs \
            (id, company_id, provider, display_name, status, is_default, config, created_by_user_id) \
         VALUES ($1, $2, 'aws', $3, 'active', false, '{}'::jsonb, 'tester')",
    )
    .bind(id).bind(company_id).bind(name)
    .execute(db.pool()).await.expect("insert provider");
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

/// 1. patch_provider_config — 部分更新 display_name
#[tokio::test(flavor = "current_thread")]
async fn patch_provider_config_updates_display_name() {
    let db = db().await;
    let cid = insert_company(&db, "patchprov-name").await;
    let pid = insert_provider(&db, cid, "old name").await;
    let row = SecretRepo::new(&db)
        .patch_provider_config(pid, Some("new name"), None, None, None)
        .await
        .expect("patch")
        .unwrap();
    assert_eq!(row.display_name, "new name");
}

/// 2. patch_provider_config — 部分更新 status
#[tokio::test(flavor = "current_thread")]
async fn patch_provider_config_updates_status() {
    let db = db().await;
    let cid = insert_company(&db, "patchprov-status").await;
    let pid = insert_provider(&db, cid, "name").await;
    let row = SecretRepo::new(&db)
        .patch_provider_config(pid, None, Some("disabled"), None, None)
        .await
        .expect("patch")
        .unwrap();
    assert_eq!(row.status, "disabled");
}

/// 3. patch_provider_config — 更新 is_default
#[tokio::test(flavor = "current_thread")]
async fn patch_provider_config_updates_is_default() {
    let db = db().await;
    let cid = insert_company(&db, "patchprov-default").await;
    let pid = insert_provider(&db, cid, "name").await;
    let row = SecretRepo::new(&db)
        .patch_provider_config(pid, None, None, None, Some(true))
        .await
        .expect("patch")
        .unwrap();
    assert!(row.is_default);
}

/// 4. patch_provider_config — 不存在返回 None
#[tokio::test(flavor = "current_thread")]
async fn patch_provider_config_missing_returns_none() {
    let db = db().await;
    let row = SecretRepo::new(&db)
        .patch_provider_config(Uuid::new_v4(), Some("x"), None, None, None)
        .await
        .expect("patch");
    assert!(row.is_none());
}

/// 5. rotate_company_secret — 创建新 version + bump latest_version
#[tokio::test(flavor = "current_thread")]
async fn rotate_company_secret_creates_new_version() {
    let db = db().await;
    let cid = insert_company(&db, "rotate").await;
    let sid = insert_secret(&db, cid, "rotate-test").await;
    let row = SecretRepo::new(&db)
        .rotate_company_secret(sid, &json!({"value": "new-material"}), Some("tester"), None)
        .await
        .expect("rotate")
        .unwrap();
    assert_eq!(row.latest_version, 2, "version should bump from 1 to 2");
}

/// 6. rotate_company_secret — 不存在返回 None
#[tokio::test(flavor = "current_thread")]
async fn rotate_company_secret_missing_returns_none() {
    let db = db().await;
    let row = SecretRepo::new(&db)
        .rotate_company_secret(Uuid::new_v4(), &json!({}), None, None)
        .await
        .expect("rotate");
    assert!(row.is_none());
}

/// 7. rotate_company_secret — sha256 计算并保存
#[tokio::test(flavor = "current_thread")]
async fn rotate_company_secret_saves_sha256() {
    let db = db().await;
    let cid = insert_company(&db, "rotate-sha").await;
    let sid = insert_secret(&db, cid, "rotate-sha").await;
    SecretRepo::new(&db)
        .rotate_company_secret(sid, &json!({"k": "v"}), Some("tester"), None)
        .await
        .expect("rotate");
    let version_row: (String,) = sqlx::query_as(
        "SELECT value_sha256 FROM company_secret_versions WHERE secret_id = $1 AND version = 2",
    )
    .bind(sid)
    .fetch_one(db.pool())
    .await
    .expect("version row");
    assert_eq!(version_row.0.len(), 64, "sha256 hex = 64 chars");
}
