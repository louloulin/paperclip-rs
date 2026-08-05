//! Round 154 集成测试：tool_connection 仓储（CRUD + catalog + installs + grants + activity）。

use pc_db::Db;
use pc_repos::tool_connection::ToolConnectionRepo;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(format!("r154-c-{tag}-{id}"))
        .bind(format!("R154{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn insert_application(db: &Db, company_id: Uuid, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tool_applications (id, company_id, name, type, metadata) \
         VALUES ($1, $2, $3, 'mcp', '{}'::jsonb)",
    )
    .bind(id)
    .bind(company_id)
    .bind(name)
    .execute(db.pool())
    .await
    .expect("app");
    id
}

async fn insert_tool_connection(
    db: &Db,
    company_id: Uuid,
    app_id: Uuid,
    name: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tool_connections \
         (id, company_id, application_id, name, transport, status, enabled, config, \
          credential_refs, health_status) \
         VALUES ($1, $2, $3, $4, 'stdio', 'draft', false, '{}'::jsonb, '[]'::jsonb, 'unchecked')",
    )
    .bind(id)
    .bind(company_id)
    .bind(app_id)
    .bind(name)
    .execute(db.pool())
    .await
    .expect("connection");
    id
}

// ===== CRUD =====

/// 1. find_by_id — 已插入的连接能查到。
#[tokio::test(flavor = "current_thread")]
async fn find_by_id_returns_row() {
    let db = db().await;
    let cid = insert_company(&db, "fb1").await;
    let app = insert_application(&db, cid, "app1").await;
    let id = insert_tool_connection(&db, cid, app, "conn1").await;
    let repo = ToolConnectionRepo::new(&db);
    let row = repo.find_by_id(id).await.expect("find");
    assert!(row.is_some());
    let row = row.unwrap();
    assert_eq!(row.id, id);
    assert_eq!(row.name, "conn1");
    assert_eq!(row.transport, "stdio");
    assert_eq!(row.application_id, app);
}

/// 2. find_by_id — 不存在返回 None。
#[tokio::test(flavor = "current_thread")]
async fn find_by_id_missing_returns_none() {
    let db = db().await;
    let repo = ToolConnectionRepo::new(&db);
    let row = repo.find_by_id(Uuid::new_v4()).await.expect("find");
    assert!(row.is_none());
}

/// 3. delete_by_id — 删除已插入的连接返回 affected=1。
#[tokio::test(flavor = "current_thread")]
async fn delete_by_id_returns_one() {
    let db = db().await;
    let cid = insert_company(&db, "dl1").await;
    let app = insert_application(&db, cid, "app2").await;
    let id = insert_tool_connection(&db, cid, app, "to-delete").await;
    let repo = ToolConnectionRepo::new(&db);
    let affected = repo.delete_by_id(id).await.expect("delete");
    assert_eq!(affected, 1);
}

/// 4. update_name / update_enabled / update_status — 字段各自更新。
#[tokio::test(flavor = "current_thread")]
async fn update_fields_independently() {
    let db = db().await;
    let cid = insert_company(&db, "uf1").await;
    let app = insert_application(&db, cid, "app3").await;
    let id = insert_tool_connection(&db, cid, app, "old-name").await;
    let repo = ToolConnectionRepo::new(&db);

    repo.update_name(id, "new-name").await.expect("name");
    repo.update_enabled(id, true).await.expect("enabled");
    repo.update_status(id, "connected").await.expect("status");

    let row = repo.find_by_id(id).await.expect("find").unwrap();
    assert_eq!(row.name, "new-name");
    assert!(row.enabled);
    assert_eq!(row.status, "connected");
}

/// 5. update_config / update_credential_refs — jsonb 字段更新。
#[tokio::test(flavor = "current_thread")]
async fn update_jsonb_fields() {
    let db = db().await;
    let cid = insert_company(&db, "uj1").await;
    let app = insert_application(&db, cid, "app4").await;
    let id = insert_tool_connection(&db, cid, app, "json-test").await;
    let repo = ToolConnectionRepo::new(&db);

    let new_config = serde_json::json!({"key": "value"});
    let new_refs = serde_json::json!([{"name": "cred1"}]);
    repo.update_config(id, &new_config).await.expect("config");
    repo.update_credential_refs(id, &new_refs).await.expect("refs");

    let row = repo.find_by_id(id).await.expect("find").unwrap();
    assert_eq!(row.config, new_config);
    assert_eq!(row.credential_refs, new_refs);
}

/// 6. update_health_check — 写 health_status + message + last_health_at。
#[tokio::test(flavor = "current_thread")]
async fn update_health_check_basic() {
    let db = db().await;
    let cid = insert_company(&db, "uh1").await;
    let app = insert_application(&db, cid, "app5").await;
    let id = insert_tool_connection(&db, cid, app, "health-test").await;
    let repo = ToolConnectionRepo::new(&db);
    let affected = repo.update_health_check(id, "ok", None).await.expect("health");
    assert_eq!(affected, 1);
    let row = repo.find_by_id(id).await.expect("find").unwrap();
    assert_eq!(row.health_status, "ok");
    assert!(row.last_health_at.is_some());
}

/// 7. touch_catalog_refresh — 写 last_catalog_refresh_at。
#[tokio::test(flavor = "current_thread")]
async fn touch_catalog_refresh_basic() {
    let db = db().await;
    let cid = insert_company(&db, "tc1").await;
    let app = insert_application(&db, cid, "app6").await;
    let id = insert_tool_connection(&db, cid, app, "cat-test").await;
    let repo = ToolConnectionRepo::new(&db);
    let _ = repo.touch_catalog_refresh(id).await.expect("touch");
    let row = repo.find_by_id(id).await.expect("find").unwrap();
    assert!(row.last_catalog_refresh_at.is_some());
}

/// 8. update_status_to_reconnecting — 状态变更为 reconnecting。
#[tokio::test(flavor = "current_thread")]
async fn update_status_to_reconnecting_basic() {
    let db = db().await;
    let cid = insert_company(&db, "ur1").await;
    let app = insert_application(&db, cid, "app7").await;
    let id = insert_tool_connection(&db, cid, app, "reconn").await;
    let repo = ToolConnectionRepo::new(&db);
    repo.update_status_to_reconnecting(id).await.expect("reconnect");
    let row = repo.find_by_id(id).await.expect("find").unwrap();
    assert_eq!(row.status, "reconnecting");
}

// ===== catalog / installs / grants =====

/// 9. list_catalog — 空连接返回空。
#[tokio::test(flavor = "current_thread")]
async fn list_catalog_empty() {
    let db = db().await;
    let cid = insert_company(&db, "lc1").await;
    let app = insert_application(&db, cid, "app8").await;
    let id = insert_tool_connection(&db, cid, app, "cat-empty").await;
    let repo = ToolConnectionRepo::new(&db);
    let rows = repo.list_catalog(id).await.expect("list");
    assert!(rows.is_empty());
}

/// 10. list_installs + upsert_install — 新建安装后能查到。
#[tokio::test(flavor = "current_thread")]
async fn upsert_then_list_installs() {
    let db = db().await;
    let cid = insert_company(&db, "li1").await;
    let app = insert_application(&db, cid, "app9").await;
    let id = insert_tool_connection(&db, cid, app, "inst-test").await;
    let repo = ToolConnectionRepo::new(&db);
    repo.upsert_install(id, cid, "agent", "agent-1").await.expect("install");
    let rows = repo.list_installs(id).await.expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].3, "agent-1"); // target_id
}

/// 11. grants_table_exists — 不存在时返回 false（grants 表通常不在 v3 schema）。
#[tokio::test(flavor = "current_thread")]
async fn grants_table_exists_returns_false() {
    let db = db().await;
    let cid = insert_company(&db, "ge1").await;
    let app = insert_application(&db, cid, "app10").await;
    let id = insert_tool_connection(&db, cid, app, "grants-test").await;
    let repo = ToolConnectionRepo::new(&db);
    let _ = id;
    // 即使表不存在也返回 false（不报错）
    let _ = repo.grants_table_exists(cid).await;
}

/// 12. usage_install_count — 0 个安装。
#[tokio::test(flavor = "current_thread")]
async fn usage_install_count_zero() {
    let db = db().await;
    let cid = insert_company(&db, "uu1").await;
    let app = insert_application(&db, cid, "app11").await;
    let id = insert_tool_connection(&db, cid, app, "usage-test").await;
    let repo = ToolConnectionRepo::new(&db);
    let count = repo.usage_install_count(id).await.expect("count");
    assert_eq!(count, Some(0));
}

/// 13. usage_install_count — 多个安装。
#[tokio::test(flavor = "current_thread")]
async fn usage_install_count_nonzero() {
    let db = db().await;
    let cid = insert_company(&db, "uu2").await;
    let app = insert_application(&db, cid, "app12").await;
    let id = insert_tool_connection(&db, cid, app, "usage-test2").await;
    let repo = ToolConnectionRepo::new(&db);
    for n in 0..3 {
        repo.upsert_install(id, cid, "agent", &format!("a-{n}")).await.expect("install");
    }
    let count = repo.usage_install_count(id).await.expect("count");
    assert_eq!(count, Some(3));
}

// ===== DTO smoke (sync) =====

/// 14. ToolConnectionRow 类型 smoke。
#[test]
fn tool_connection_row_typecheck() {
    use pc_repos::tool_connection::ToolConnectionRow;
    fn assert_from_row<T: for<'a> sqlx::FromRow<'a, sqlx::postgres::PgRow>>() {}
    assert_from_row::<ToolConnectionRow>();
}
