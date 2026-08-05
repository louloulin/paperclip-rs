//! Round 122 集成测试：SecretRepo user_secret_definitions 子模块仓储化。

use pc_db::Db;
use pc_repos::secret::{NewUserSecretDefinition, SecretRepo};
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(format!("r122-{tag}-{id}"))
        .bind(format!("R122{}", &id.simple().to_string()[..4]))
        .execute(db.pool()).await.expect("insert company");
    id
}

async fn insert_def(db: &Db, company_id: Uuid, key: &str, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO user_secret_definitions \
            (id, company_id, key, name, status, provider, managed_mode, created_by_user_id, updated_by_user_id) \
         VALUES ($1, $2, $3, $4, 'active', 'manual', 'user', 'tester', 'tester')",
    )
    .bind(id).bind(company_id).bind(key).bind(name)
    .execute(db.pool()).await.expect("insert def");
    id
}

/// 1. list_user_definitions — 排除 archived
#[tokio::test(flavor = "current_thread")]
async fn list_user_definitions_excludes_archived() {
    let db = db().await;
    let cid = insert_company(&db, "list").await;
    insert_def(&db, cid, "k1", "n1").await;
    insert_def(&db, cid, "k2", "n2").await;
    insert_def(&db, cid, "k3", "n3").await;
    let rows = SecretRepo::new(&db).list_user_definitions(cid).await.expect("list");
    assert_eq!(rows.len(), 3);
}

/// 2. create_user_definition
#[tokio::test(flavor = "current_thread")]
async fn create_user_definition_inserts() {
    let db = db().await;
    let cid = insert_company(&db, "create").await;
    let input = NewUserSecretDefinition {
        company_id: cid,
        key: "my-key".to_owned(),
        name: "My Def".to_owned(),
        description: Some("desc".to_owned()),
        status: "active".to_owned(),
        provider: "manual".to_owned(),
        managed_mode: "user".to_owned(),
        provider_config_id: None,
        provider_metadata: None,
        usage_guidance: Some("use carefully".to_owned()),
        created_by_agent_id: None,
        created_by_user_id: Some("tester".to_owned()),
    };
    let row = SecretRepo::new(&db).create_user_definition(&input).await.expect("create");
    assert_eq!(row.key, "my-key");
    assert_eq!(row.status, "active");
}

/// 3. archive_user_definition
#[tokio::test(flavor = "current_thread")]
async fn archive_user_definition_marks_deleted() {
    let db = db().await;
    let cid = insert_company(&db, "archive").await;
    let id = insert_def(&db, cid, "k1", "n1").await;
    SecretRepo::new(&db).archive_user_definition(id).await.expect("archive");
    let rows = SecretRepo::new(&db).list_user_definitions(cid).await.expect("list");
    assert_eq!(rows.len(), 0);
}

/// 4. patch_user_definition — 部分更新
#[tokio::test(flavor = "current_thread")]
async fn patch_user_definition_updates_partial() {
    let db = db().await;
    let cid = insert_company(&db, "patch").await;
    let id = insert_def(&db, cid, "k1", "old name").await;
    let row = SecretRepo::new(&db)
        .patch_user_definition(cid, id, Some("new name"), None, Some("draft"), None, None)
        .await
        .expect("patch")
        .unwrap();
    assert_eq!(row.name, "new name");
    assert_eq!(row.status, "draft");
}

/// 5. patch_user_definition — 不存在返回 None
#[tokio::test(flavor = "current_thread")]
async fn patch_user_definition_missing_returns_none() {
    let db = db().await;
    let cid = insert_company(&db, "patch-miss").await;
    let row = SecretRepo::new(&db)
        .patch_user_definition(cid, Uuid::new_v4(), None, None, None, None, None)
        .await
        .expect("patch");
    assert!(row.is_none());
}

/// 6. patch_user_definition — None 字段保持不变
#[tokio::test(flavor = "current_thread")]
async fn patch_user_definition_keeps_unchanged() {
    let db = db().await;
    let cid = insert_company(&db, "patch-keep").await;
    let id = insert_def(&db, cid, "k1", "kept name").await;
    let row = SecretRepo::new(&db)
        .patch_user_definition(cid, id, None, None, None, None, None)
        .await
        .expect("patch")
        .unwrap();
    assert_eq!(row.name, "kept name");
}
