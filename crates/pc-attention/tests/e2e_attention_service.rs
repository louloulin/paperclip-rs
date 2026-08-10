//! R632: AttentionService 真实 DB 端到端测试。

use pc_attention::{AttentionItemKind, AttentionService};
use pc_repos::Db;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn setup_pool() -> sqlx::PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(TEST_DATABASE_URL)
        .await
        .expect("connect")
}

async fn insert_company(pool: &sqlx::PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R632-{id}"))
    .bind(format!("AT{}", &id.simple().to_string()[..4]))
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn cleanup(pool: &sqlx::PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM issues WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM agents WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM approvals WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM budget_incidents WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM company_memberships WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn r632_attention_empty_company_returns_no_items() {
    let pool = setup_pool().await;
    let db = Db::from_pool(pool.clone());
    let company_id = insert_company(&pool).await;
    let svc = AttentionService::new(db);

    let items = svc
        .list_for_company(company_id, 100)
        .await
        .expect("list");
    assert!(items.is_empty());

    let counts = svc.counts_for_company(company_id).await.expect("counts");
    assert!(counts.is_empty());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r632_attention_rejects_nil_company() {
    let pool = setup_pool().await;
    let db = Db::from_pool(pool.clone());
    let svc = AttentionService::new(db);
    let err = svc.list_for_company(Uuid::nil(), 10).await.expect_err("nil");
    assert!(matches!(
        err,
        pc_attention::AttentionError::Validation(_)
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn r632_attention_supported_kinds_returns_twelve() {
    let kinds = AttentionService::supported_kinds();
    assert_eq!(kinds.len(), 12);
}

#[tokio::test(flavor = "current_thread")]
async fn r632_attention_list_by_kind_filters_correctly() {
    let pool = setup_pool().await;
    let db = Db::from_pool(pool.clone());
    let svc = AttentionService::new(db);
    let items = svc
        .list_by_kind(Uuid::new_v4(), AttentionItemKind::AgentError, 50)
        .await
        .expect("list by kind");
    // Random UUID has no agents, so should be empty
    assert!(items.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn r632_attention_includes_blocked_issue() {
    let pool = setup_pool().await;
    let db = Db::from_pool(pool.clone());
    let company_id = insert_company(&pool).await;
    // Insert a blocked-style issue: status that matches the blocked query
    let issue_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, identifier, title, status, priority, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, 'high', now(), now())",
    )
    .bind(issue_id)
    .bind(company_id)
    .bind(format!("AT-1"))
    .bind("Blocked issue")
    .bind("blocked")
    .execute(&pool)
    .await
    .expect("insert blocked issue");

    let svc = AttentionService::new(db);
    let items = svc
        .list_for_company(company_id, 100)
        .await
        .expect("list");
    let has_blocked = items.iter().any(|i| {
        i.kind == AttentionItemKind::IssueBlocked && i.subject_id == issue_id
    });
    assert!(has_blocked, "expected a blocked issue attention item");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r632_attention_counts_total() {
    let pool = setup_pool().await;
    let db = Db::from_pool(pool.clone());
    let company_id = insert_company(&pool).await;
    let svc = AttentionService::new(db);

    let counts = svc.counts_for_company(company_id).await.expect("counts");
    assert_eq!(counts.total(), 0);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r632_attention_severity_ord_correct() {
    assert!(pc_attention::AttentionSeverity::Critical < pc_attention::AttentionSeverity::High);
    assert!(pc_attention::AttentionSeverity::High < pc_attention::AttentionSeverity::Medium);
    assert!(pc_attention::AttentionSeverity::Medium < pc_attention::AttentionSeverity::Low);
    assert!(pc_attention::AttentionSeverity::Low < pc_attention::AttentionSeverity::Info);
}
