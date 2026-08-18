//! R789: pc-work-products 集成测试 (使用 55433 devdb, 验证 R783 pure 函数 + DB 链路).
//!
//! 验证:
//! - R783 pure 函数 import_row_to_create_input 在 DB 上下文正确转换
//! - CreateWorkProductInput + WorkProductService.create_for_issue 端到端 (纯函数 -> DB)
//! - WorkProduct 序列化 (camelCase + type rename) 与 DB roundtrip 一致
//! - is_primary 标志正确写入 + 读取 (同 kind 清空, 跨 kind 保留)

use pc_repos::Db;
use pc_work_products::import_write_types::ImportIssueWorkProductRow;
use pc_work_products::{
    import_row_to_create_input, CreateWorkProductInput, WorkProductService,
};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

// R789: 使用 devdb 55433
const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:55433/paperclip_repos";

static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn setup_db() -> (Db, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(TEST_DATABASE_URL)
        .await
        .expect("connect 55433");
    let db = Db::connect(TEST_DATABASE_URL, 4, 1).await.expect("Db");
    (db, pool)
}

async fn insert_company(pool: &PgPool, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    let suffix = Uuid::new_v4().simple().to_string().chars().take(6).collect::<String>();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R789-{tag}-{id}"))
    .bind(format!("R789{tag}-{suffix}"))
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn insert_issue(pool: &PgPool, company_id: Uuid, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, created_at, updated_at) VALUES ($1, $2, $3, 'todo', 'normal', now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("R789-{tag}-issue"))
    .execute(pool)
    .await
    .expect("insert issue");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM issue_work_products WHERE company_id = $1")
        .bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id = $1")
        .bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id).execute(pool).await;
}

#[tokio::test]
async fn r789_pure_to_db_end_to_end() {
    let _lock = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "p2db").await;
    let issue_id = insert_issue(&pool, company_id, "p2db").await;
    let svc = WorkProductService::new(&db);

    // Step 1: Build ImportIssueWorkProductRow
    let import_row = ImportIssueWorkProductRow {
        company_id,
        issue_id,
        project_id: None,
        kind: "pr".to_string(),
        provider: "github".to_string(),
        external_id: Some("PR-789".to_string()),
        title: "R789 PR".to_string(),
        url: Some("https://github.com/foo/bar/pull/789".to_string()),
        status: "open".to_string(),
        review_state: "pending".to_string(),
        is_primary: true,
        health_status: "ok".to_string(),
        summary: Some("R789 summary".to_string()),
        metadata: Some(json!({"sha": "deadbeef"})),
        execution_workspace_id: None,
        runtime_service_id: None,
        created_by_run_id: None,
        source_trust: Some(json!({"preset": "standard"})),
    };

    // Step 2: Apply R783 pure function
    let input = import_row_to_create_input(&import_row);

    // Step 3: Verify pure function mapped fields
    assert_eq!(input.kind, "pr");
    assert_eq!(input.provider, "github");
    assert_eq!(input.external_id.as_deref(), Some("PR-789"));
    assert_eq!(input.title, "R789 PR");
    assert_eq!(input.url.as_deref(), Some("https://github.com/foo/bar/pull/789"));
    assert_eq!(input.review_state.as_deref(), Some("pending"));
    assert_eq!(input.is_primary, true);
    assert_eq!(input.health_status.as_deref(), Some("ok"));
    assert_eq!(input.metadata.as_ref().unwrap(), &json!({"sha": "deadbeef"}));
    assert_eq!(input.source_trust.as_ref().unwrap(), &json!({"preset": "standard"}));

    // Step 4: Persist via service
    let created = svc.create_for_issue(issue_id, company_id, input).await
        .expect("create");
    assert_eq!(created.kind, "pr");
    assert_eq!(created.title, "R789 PR");
    assert_eq!(created.provider, "github");
    assert!(created.is_primary);

    // Step 5: Read back
    let fetched = svc.get_by_id(created.id).await
        .expect("get");
    assert_eq!(fetched.id, created.id);
    assert_eq!(fetched.kind, "pr");
    assert_eq!(fetched.title, "R789 PR");
    assert_eq!(fetched.review_state, "pending");

    // Step 6: Verify serialization (camelCase + type rename)
    let serialized = serde_json::to_value(&created).expect("serialize");
    assert_eq!(serialized.get("type").and_then(|v| v.as_str()), Some("pr"));
    assert!(serialized.get("companyId").is_some());
    assert!(serialized.get("createdAt").is_some());
    assert!(serialized.get("kind").is_none());

    // Step 7: list_for_issue
    let listed = svc.list_for_issue(issue_id).await.expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, created.id);

    cleanup(&pool, company_id).await;
}

#[tokio::test]
async fn r789_secondary_primary_clears_primary() {
    let _lock = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "sec").await;
    let issue_id = insert_issue(&pool, company_id, "sec").await;
    let svc = WorkProductService::new(&db);

    let first = svc.create_for_issue(issue_id, company_id, CreateWorkProductInput {
        kind: "pr".to_string(),
        provider: "github".to_string(),
        external_id: Some("PR-1".to_string()),
        title: "First".to_string(),
        url: None,
        status: "open".to_string(),
        is_primary: true,
        ..Default::default()
    }).await.expect("create1");
    assert!(first.is_primary);

    let second = svc.create_for_issue(issue_id, company_id, CreateWorkProductInput {
        kind: "pr".to_string(),
        provider: "github".to_string(),
        external_id: Some("PR-2".to_string()),
        title: "Second".to_string(),
        url: None,
        status: "open".to_string(),
        is_primary: true,
        ..Default::default()
    }).await.expect("create2");
    assert!(second.is_primary);

    // First should lose primary
    let first_after = svc.get_by_id(first.id).await
        .expect("get1");
    assert!(!first_after.is_primary, "first should lose primary");

    let second_after = svc.get_by_id(second.id).await
        .expect("get2");
    assert!(second_after.is_primary);

    cleanup(&pool, company_id).await;
}

#[tokio::test]
async fn r789_different_kind_preserves_primary() {
    let _lock = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "diff").await;
    let issue_id = insert_issue(&pool, company_id, "diff").await;
    let svc = WorkProductService::new(&db);

    let pr = svc.create_for_issue(issue_id, company_id, CreateWorkProductInput {
        kind: "pr".to_string(),
        provider: "github".to_string(),
        external_id: None,
        title: "PR".to_string(),
        url: None,
        status: "open".to_string(),
        is_primary: true,
        ..Default::default()
    }).await.expect("create_pr");

    let deploy = svc.create_for_issue(issue_id, company_id, CreateWorkProductInput {
        kind: "deployment".to_string(),
        provider: "vercel".to_string(),
        external_id: None,
        title: "Deploy".to_string(),
        url: None,
        status: "ready".to_string(),
        is_primary: true,
        ..Default::default()
    }).await.expect("create_deploy");

    let pr_after = svc.get_by_id(pr.id).await
        .expect("get_pr");
    let deploy_after = svc.get_by_id(deploy.id).await
        .expect("get_deploy");
    assert!(pr_after.is_primary, "pr should keep primary (different kind)");
    assert!(deploy_after.is_primary, "deploy is primary too");

    cleanup(&pool, company_id).await;
}