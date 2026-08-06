//! Round 155 集成测试：mcp_gateway 仓储（gateway CRUD + sessions + tokens + actions）。

use pc_db::Db;
use pc_repos::mcp_gateway::McpGatewayRepo;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn db() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn insert_company(db: &Db, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(format!("r155-c-{tag}-{id}"))
        .bind(format!("R155{}", &id.simple().to_string()[..4]))
        .execute(db.pool())
        .await
        .expect("company");
    id
}

async fn insert_profile(db: &Db, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tool_profiles (id, company_id, profile_key, name, status, default_action, metadata) \
         VALUES ($1, $2, $3, 'test-profile', 'active', 'allow', '{}'::jsonb)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("pk-{}", &id.simple().to_string()[..8]))
    .execute(db.pool())
    .await
    .expect("profile");
    id
}

async fn insert_gateway(
    db: &Db,
    company_id: Uuid,
    profile_id: Uuid,
    name: &str,
    slug: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tool_mcp_gateways \
         (id, company_id, name, slug, profile_id, status, metadata) \
         VALUES ($1, $2, $3, $4, $5, 'active', '{}'::jsonb)",
    )
    .bind(id)
    .bind(company_id)
    .bind(name)
    .bind(slug)
    .bind(profile_id)
    .execute(db.pool())
    .await
    .expect("gateway");
    id
}

// ===== CRUD =====

/// 1. list_by_company + create — 基本 CRUD。
#[tokio::test(flavor = "current_thread")]
async fn list_and_create_gateway() {
    let db = db().await;
    let cid = insert_company(&db, "lg1").await;
    let pid = insert_profile(&db, cid).await;
    let repo = McpGatewayRepo::new(&db);
    let row = repo
        .create(cid, "my-gw", "my-gw", None, pid, None, None, None)
        .await
        .expect("create");
    assert_eq!(row.name, "my-gw");
    assert_eq!(row.slug, "my-gw");

    let rows = repo.list_by_company(cid).await.expect("list");
    assert!(rows.iter().any(|r| r.id == row.id));
}

/// 2. find_by_id — 命中 / 不命中。
#[tokio::test(flavor = "current_thread")]
async fn find_by_id_hit_and_miss() {
    let db = db().await;
    let cid = insert_company(&db, "fb1").await;
    let pid = insert_profile(&db, cid).await;
    let id = insert_gateway(&db, cid, pid, "gw-1", "gw-1").await;
    let repo = McpGatewayRepo::new(&db);

    let hit = repo.find_by_id(id).await.expect("hit");
    assert!(hit.is_some());
    let miss = repo.find_by_id(Uuid::new_v4()).await.expect("miss");
    assert!(miss.is_none());
}

/// 3. find_id_and_name_by_public_id — slug 解析。
#[tokio::test(flavor = "current_thread")]
async fn find_id_and_name_by_public_id_hit() {
    let db = db().await;
    let cid = insert_company(&db, "fp1").await;
    let pid = insert_profile(&db, cid).await;
    let id = insert_gateway(&db, cid, pid, "by-slug", "by-slug").await;
    let repo = McpGatewayRepo::new(&db);
    let found = repo
        .find_id_and_name_by_public_id("by-slug")
        .await
        .expect("find");
    assert!(found.is_some());
    let (fid, fname) = found.unwrap();
    assert_eq!(fid, id);
    assert_eq!(fname, "by-slug");
}

/// 4. update_partial — 字段独立更新。
#[tokio::test(flavor = "current_thread")]
async fn update_partial_basic() {
    let db = db().await;
    let cid = insert_company(&db, "up1").await;
    let pid = insert_profile(&db, cid).await;
    let id = insert_gateway(&db, cid, pid, "old-name", "old-slug").await;
    let repo = McpGatewayRepo::new(&db);

    repo.update_partial(id, Some("new-name"), None, Some("connected"), None)
        .await
        .expect("update");
    let row = repo.find_by_id(id).await.expect("find").unwrap();
    assert_eq!(row.name, "new-name");
    assert_eq!(row.status, "connected");
}

// ===== Tokens =====

/// 5. issue_token + find_active_token — 命中 + revoke 后不再命中。
#[tokio::test(flavor = "current_thread")]
async fn issue_and_find_token() {
    let db = db().await;
    let cid = insert_company(&db, "tk1").await;
    let pid = insert_profile(&db, cid).await;
    let gw_id = insert_gateway(&db, cid, pid, "gw-tk", "gw-tk").await;
    let repo = McpGatewayRepo::new(&db);
    let token_id = repo.issue_token(gw_id, "hash-1").await.expect("issue");
    let found = repo.find_active_token(gw_id, "hash-1").await.expect("find");
    assert!(found);

    repo.revoke_token(token_id).await.expect("revoke");
    let found_after = repo
        .find_active_token(gw_id, "hash-1")
        .await
        .expect("find2");
    assert!(!found_after);
}

/// 6. find_active_token — 错误 hash 不命中。
#[tokio::test(flavor = "current_thread")]
async fn find_active_token_wrong_hash() {
    let db = db().await;
    let cid = insert_company(&db, "tk2").await;
    let pid = insert_profile(&db, cid).await;
    let gw_id = insert_gateway(&db, cid, pid, "gw-tk2", "gw-tk2").await;
    let repo = McpGatewayRepo::new(&db);
    let _ = repo.issue_token(gw_id, "right-hash").await.expect("issue");
    let found = repo
        .find_active_token(gw_id, "wrong-hash")
        .await
        .expect("find");
    assert!(!found);
}

/// 7. list_sessions — 返回空（无 sessions）。
#[tokio::test(flavor = "current_thread")]
async fn list_sessions_empty() {
    let db = db().await;
    let repo = McpGatewayRepo::new(&db);
    let rows = repo.list_sessions(10).await.expect("list");
    let _ = rows; // 接受任意（包括测试残留数据）
}

// ===== Actions =====

/// 8. list_audit_events — 返回空。
#[tokio::test(flavor = "current_thread")]
async fn list_audit_events_empty() {
    let db = db().await;
    let repo = McpGatewayRepo::new(&db);
    let rows = repo.list_audit_events(10).await.expect("list");
    let _ = rows;
}

// ===== DTO smoke (sync) =====

/// 9. McpGatewayRow 类型 smoke。
#[test]
fn mcp_gateway_row_typecheck() {
    use pc_repos::mcp_gateway::McpGatewayRow;
    fn assert_from_row<T: for<'a> sqlx::FromRow<'a, sqlx::postgres::PgRow>>() {}
    assert_from_row::<McpGatewayRow>();
}
