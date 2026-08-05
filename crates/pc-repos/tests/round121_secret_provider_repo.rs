//! Round 121 集成测试：SecretRepo provider_config + list_secrets 子模块仓储化。

use pc_db::Db;
use pc_repos::secret::{NewProviderConfig, SecretRepo};
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
        .bind(format!("r121-{tag}-{id}"))
        .bind(format!("R121{}", &id.simple().to_string()[..4]))
        .execute(db.pool()).await.expect("insert company");
    id
}

async fn insert_provider(
    db: &Db,
    company_id: Uuid,
    provider: &str,
    is_default: bool,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO company_secret_provider_configs \
            (id, company_id, provider, display_name, status, is_default, config, created_by_user_id) \
         VALUES ($1, $2, $3, $4, 'active', $5, '{}'::jsonb, 'tester')",
    )
    .bind(id).bind(company_id).bind(provider).bind(provider).bind(is_default)
    .execute(db.pool()).await.expect("insert provider");
    id
}

/// 1. list_providers
#[tokio::test(flavor = "current_thread")]
async fn list_providers_returns_company_providers() {
    let db = db().await;
    let cid = insert_company(&db, "listprov").await;
    insert_provider(&db, cid, "aws", false).await;
    insert_provider(&db, cid, "vault", true).await;
    let rows = SecretRepo::new(&db).list_providers(cid).await.expect("list providers");
    assert_eq!(rows.len(), 2);
}

/// 2. get_provider — found
#[tokio::test(flavor = "current_thread")]
async fn get_provider_returns_some_for_existing() {
    let db = db().await;
    let cid = insert_company(&db, "getprov").await;
    let pid = insert_provider(&db, cid, "aws", false).await;
    let row = SecretRepo::new(&db).get_provider(pid).await.expect("get");
    assert!(row.is_some());
    assert_eq!(row.unwrap().provider, "aws");
}

/// 3. get_provider — not found
#[tokio::test(flavor = "current_thread")]
async fn get_provider_returns_none_for_missing() {
    let db = db().await;
    let row = SecretRepo::new(&db).get_provider(Uuid::new_v4()).await.expect("get");
    assert!(row.is_none());
}

/// 4. delete_provider
#[tokio::test(flavor = "current_thread")]
async fn delete_provider_removes_row() {
    let db = db().await;
    let cid = insert_company(&db, "delprov").await;
    let pid = insert_provider(&db, cid, "aws", false).await;
    let deleted = SecretRepo::new(&db).delete_provider(pid).await.expect("delete");
    assert!(deleted);
    let row = SecretRepo::new(&db).get_provider(pid).await.expect("get after delete");
    assert!(row.is_none());
}

/// 5. mark_default_provider — UPDATE RETURNING
#[tokio::test(flavor = "current_thread")]
async fn mark_default_provider_updates_flag() {
    let db = db().await;
    let cid = insert_company(&db, "mkdefault").await;
    let pid = insert_provider(&db, cid, "aws", false).await;
    let row = SecretRepo::new(&db).mark_default_provider(pid).await.expect("mark").unwrap();
    assert!(row.is_default);
}

/// 6. mark_default_provider — 不存在返回 None
#[tokio::test(flavor = "current_thread")]
async fn mark_default_provider_missing_returns_none() {
    let db = db().await;
    let row = SecretRepo::new(&db).mark_default_provider(Uuid::new_v4()).await.expect("mark");
    assert!(row.is_none());
}

/// 7. mark_provider_healthy
#[tokio::test(flavor = "current_thread")]
async fn mark_provider_healthy_updates_health() {
    let db = db().await;
    let cid = insert_company(&db, "healthy").await;
    let pid = insert_provider(&db, cid, "aws", false).await;
    let row = SecretRepo::new(&db).mark_provider_healthy(pid).await.expect("healthy");
    assert_eq!(row.health_status, Some("ok".to_owned()));
    assert!(row.health_checked_at.is_some());
}

/// 8. list_for_company + upsert_provider 联合
#[tokio::test(flavor = "current_thread")]
async fn list_for_company_with_upsert_provider() {
    let db = db().await;
    let cid = insert_company(&db, "upsert-list").await;
    let input = NewProviderConfig {
        company_id: cid,
        provider: "vault".to_owned(),
        display_name: "My Vault".to_owned(),
        status: "active".to_owned(),
        is_default: true,
        config: json!({"url": "https://vault.example.com"}),
        created_by_agent_id: None,
        created_by_user_id: Some("tester".to_owned()),
    };
    let _row = SecretRepo::new(&db).upsert_provider(&input).await.expect("upsert");
    let list = SecretRepo::new(&db).list_providers(cid).await.expect("list");
    assert_eq!(list.len(), 1);
    assert!(list[0].is_default);
}
