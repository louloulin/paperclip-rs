//! R791: 跨 crate 端到端流程测试 (issue -> work product) using 55433 devdb.
//!
//! 验证:
//! - pc-companies 创建公司
//! - pc-issues 创建 issue (issue_id)
//! - pc-work-products 为 issue 创建 work product
//! - issue 状态变更 (todo -> in_progress) 后 work product 仍可访问
//! - 跨 crate 类型 (IssueRow, WorkProduct) 在同 Db 下正确关联

use pc_issues::{CreateIssueMinimalInput, IssueService};
use pc_repos::Db;
use pc_work_products::{CreateWorkProductInput, WorkProductService};
use sqlx::PgPool;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:55433/paperclip_repos";

static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn setup_db() -> (Db, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(TEST_DATABASE_URL).await.expect("connect");
    let db = Db::connect(TEST_DATABASE_URL, 4, 1).await.expect("Db");
    (db, pool)
}

async fn insert_company(pool: &PgPool, tag: &str) -> Uuid {
    let id = Uuid::new_v4();
    let suffix = Uuid::new_v4().simple().to_string().chars().take(6).collect::<String>();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id).bind(format!("R791-{tag}-{id}")).bind(format!("R791{tag}-{suffix}"))
    .execute(pool).await.expect("insert company");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM issue_work_products WHERE company_id = $1")
        .bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM issue_comments WHERE company_id = $1")
        .bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id = $1")
        .bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id).execute(pool).await;
}

#[tokio::test]
async fn r791_issue_to_work_product_lifecycle() {
    let _lock = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "i2w").await;

    // Step 1: pc-issues 创建 issue (todo)
    let issue_service = IssueService::new(&db);
    let issue = issue_service.create(
        company_id,
        &CreateIssueMinimalInput {
            title: "R791 test issue".to_string(),
            description: Some("Cross-crate test issue".to_string()),
            status: Some("todo".to_string()),
            priority: Some("normal".to_string()),
            created_by_user_id: None,
        },
    ).await.expect("create issue");
    assert_eq!(issue.status, "todo");
    assert_eq!(issue.priority, "normal");

    // Step 2: pc-work-products 为该 issue 创建 work product (PR)
    let wp_service = WorkProductService::new(&db);
    let wp = wp_service.create_for_issue(issue.id, company_id, CreateWorkProductInput {
        kind: "pr".to_string(),
        provider: "github".to_string(),
        external_id: Some("R791-PR".to_string()),
        title: "PR for R791 issue".to_string(),
        url: Some("https://github.com/foo/bar/pull/791".to_string()),
        status: "open".to_string(),
        is_primary: true,
        ..Default::default()
    }).await.expect("create wp");
    assert_eq!(wp.kind, "pr");

    // Step 3: issue 状态变更 todo -> in_progress
    issue_service.update_status(
        company_id, issue.id, "in_progress",
    ).await.expect("update_status");

    // Step 4: 重新查询 issue + work product 验证关联未断
    let issue_after = issue_service.get(company_id, issue.id).await.expect("get issue").expect("some");
    assert_eq!(issue_after.status, "in_progress");

    let wps = wp_service.list_for_issue(issue.id).await.expect("list");
    assert_eq!(wps.len(), 1);
    assert_eq!(wps[0].id, wp.id);
    assert_eq!(wps[0].kind, "pr");
    assert!(wps[0].is_primary);

    cleanup(&pool, company_id).await;
}

#[tokio::test]
async fn r791_issue_close_with_work_product() {
    let _lock = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "close").await;

    let issue_service = IssueService::new(&db);
    let wp_service = WorkProductService::new(&db);

    // Create issue
    let issue = issue_service.create(
        company_id,
        &CreateIssueMinimalInput {
            title: "Issue to close".to_string(),
            description: None,
            status: Some("todo".to_string()),
            priority: None,
            created_by_user_id: None,
        },
    ).await.expect("create");

    // Create work products: PR + deployment
    let pr = wp_service.create_for_issue(issue.id, company_id, CreateWorkProductInput {
        kind: "pr".to_string(),
        provider: "github".to_string(),
        external_id: None,
        title: "PR".to_string(),
        url: None,
        status: "merged".to_string(),
        is_primary: true,
        ..Default::default()
    }).await.expect("pr");

    let deploy = wp_service.create_for_issue(issue.id, company_id, CreateWorkProductInput {
        kind: "deployment".to_string(),
        provider: "vercel".to_string(),
        external_id: None,
        title: "Deploy".to_string(),
        url: None,
        status: "ready".to_string(),
        is_primary: true,
        ..Default::default()
    }).await.expect("deploy");

    // Close the issue
    issue_service.update_status(company_id, issue.id, "done")
        .await.expect("close");

    // Verify work products remain accessible
    let wps = wp_service.list_for_issue(issue.id).await.expect("list");
    assert_eq!(wps.len(), 2);
    let pr_id = pr.id;
    let deploy_id = deploy.id;
    let _ = (pr_id, deploy_id);
    assert!(wps.iter().any(|w| w.id == pr.id));
    assert!(wps.iter().any(|w| w.id == deploy.id));

    cleanup(&pool, company_id).await;
}

#[tokio::test]
async fn r791_multiple_issues_independent_work_products() {
    let _lock = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool, "multi").await;

    let issue_service = IssueService::new(&db);
    let wp_service = WorkProductService::new(&db);

    // Create 2 issues
    let issue_a = issue_service.create(
        company_id,
        &CreateIssueMinimalInput {
            title: "Issue A".to_string(),
            description: None,
            status: Some("todo".to_string()),
            priority: None,
            created_by_user_id: None,
        },
    ).await.expect("a");

    let issue_b = issue_service.create(
        company_id,
        &CreateIssueMinimalInput {
            title: "Issue B".to_string(),
            description: None,
            status: Some("todo".to_string()),
            priority: None,
            created_by_user_id: None,
        },
    ).await.expect("b");

    // Create 1 work product for each issue
    let _wp_a = wp_service.create_for_issue(issue_a.id, company_id, CreateWorkProductInput {
        kind: "pr".to_string(),
        provider: "github".to_string(),
        external_id: None,
        title: "A-PR".to_string(),
        url: None,
        status: "open".to_string(),
        is_primary: true,
        ..Default::default()
    }).await.expect("wp_a");

    let _wp_b = wp_service.create_for_issue(issue_b.id, company_id, CreateWorkProductInput {
        kind: "pr".to_string(),
        provider: "github".to_string(),
        external_id: None,
        title: "B-PR".to_string(),
        url: None,
        status: "open".to_string(),
        is_primary: true,
        ..Default::default()
    }).await.expect("wp_b");

    // Verify each issue only sees its own work products
    let wps_a = wp_service.list_for_issue(issue_a.id).await.expect("list_a");
    let wps_b = wp_service.list_for_issue(issue_b.id).await.expect("list_b");
    assert_eq!(wps_a.len(), 1);
    assert_eq!(wps_b.len(), 1);
    assert_eq!(wps_a[0].title, "A-PR");
    assert_eq!(wps_b[0].title, "B-PR");
    assert_ne!(wps_a[0].id, wps_b[0].id);

    cleanup(&pool, company_id).await;
}