//! Round 328：`getCompanyIssuePrefix` 的 PostgreSQL 验证。
//!
//! 与 Node `services/recovery/service.ts:1313` 对齐：
//! - 输入：company_id
//! - 输出：company.issue_prefix
//! - company 不存在 → fallback "PAP"
//! - issue_prefix 为空字符串 / 全空白 → fallback "PAP"

use pc_heartbeat::recovery::get_company_issue_prefix::get_company_issue_prefix;
use pc_repos::Db;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
}

async fn insert_company(db: &Db, prefix: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(format!("r328-{id}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .unwrap();
    id
}

#[tokio::test]
async fn returns_issue_prefix_when_company_exists() {
    let db = connect().await;
    let company_id = insert_company(&db, "ACME").await;

    let prefix = get_company_issue_prefix(&db, company_id).await.unwrap();
    assert_eq!(prefix, "ACME");

    cleanup(&db, company_id).await;
}

#[tokio::test]
async fn returns_default_pap_when_company_missing() {
    let db = connect().await;
    let ghost_id = Uuid::new_v4();

    let prefix = get_company_issue_prefix(&db, ghost_id).await.unwrap();
    assert_eq!(prefix, "PAP");
}

#[tokio::test]
async fn returns_default_pap_when_issue_prefix_empty() {
    let db = connect().await;
    let company_id = insert_company(&db, "").await;

    let prefix = get_company_issue_prefix(&db, company_id).await.unwrap();
    assert_eq!(prefix, "PAP");

    cleanup(&db, company_id).await;
}

#[tokio::test]
async fn returns_default_pap_when_issue_prefix_whitespace() {
    let db = connect().await;
    let company_id = insert_company(&db, "   ").await;

    let prefix = get_company_issue_prefix(&db, company_id).await.unwrap();
    assert_eq!(prefix, "PAP");

    cleanup(&db, company_id).await;
}

#[tokio::test]
async fn handles_long_prefix() {
    let db = connect().await;
    let company_id = insert_company(&db, "VERY_LONG_PREFIX_42").await;

    let prefix = get_company_issue_prefix(&db, company_id).await.unwrap();
    assert_eq!(prefix, "VERY_LONG_PREFIX_42");

    cleanup(&db, company_id).await;
}
