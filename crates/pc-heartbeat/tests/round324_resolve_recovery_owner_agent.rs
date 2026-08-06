//! Round 324：`resolveStrandedIssueRecoveryOwnerAgentId` + `resolveInvokableRecoveryAgentId`
//! 的 PostgreSQL 验证。
//!
//! 与 Node `services/recovery/service.ts:2524` + `:2564` 对齐：
//! - resolveInvokableRecoveryAgentId：检查指定 agent_id 是否 invokable + 同 company
//! - resolveStrandedIssueRecoveryOwnerAgentId：
//!   1. 收集候选 agents（preferred → assignee 的 reports_to → creator 的 reports_to + creator →
//!      cto / ceo role agents（cto 优先）→ assignee 本人）
//!   2. 按顺序去重，取第一个 invokable 的返回

use pc_heartbeat::recovery::resolve_recovery_owner_agent::{
    resolve_invokable_recovery_agent_id, resolve_stranded_issue_recovery_owner_agent_id,
};
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn cleanup(db: &Db, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM issues WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM agents WHERE company_id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(db.pool())
        .await;
}

async fn fixture(db: &Db) -> Uuid {
    let company_id = Uuid::new_v4();
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(company_id)
        .bind(format!("r324-{company_id}"))
        .bind(format!("R{}", &company_id.simple().to_string()[..8]))
        .execute(db.pool())
        .await
        .unwrap();
    company_id
}

async fn insert_agent(
    db: &Db,
    company_id: Uuid,
    name: &str,
    role: &str,
    reports_to: Option<Uuid>,
    status: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status, reports_to) \
         VALUES ($1, $2, $3, $4, 'process', $5, $6)",
    )
    .bind(id)
    .bind(company_id)
    .bind(name)
    .bind(role)
    .bind(status)
    .bind(reports_to)
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn insert_issue(
    db: &Db,
    company_id: Uuid,
    assignee: Option<Uuid>,
    created_by_agent: Option<Uuid>,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, origin_kind, \
         origin_fingerprint, assignee_agent_id, created_by_agent_id, execution_policy) \
         VALUES ($1, $2, $3, 'todo', 'normal', 'system', $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("r324-issue-{id}"))
    .bind(format!("r324-fp-{id}"))
    .bind(assignee)
    .bind(created_by_agent)
    .bind(json!({"mode":"normal","commentRequired":false,"stages":[]}))
    .execute(db.pool())
    .await
    .unwrap();
    id
}

async fn fetch_issue(db: &Db, issue_id: Uuid) -> pc_repos::issue::IssueRow {
    pc_repos::issue::IssueRepo::new(db)
        .get(issue_id)
        .await
        .unwrap()
        .expect("issue should exist")
}

/// resolveInvokableRecoveryAgentId 基础路径：active agent + same company → 返回
#[tokio::test]
async fn resolve_invokable_returns_active_agent_same_company() {
    let db = connect().await;
    let company_id = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "r324-agent", "general", None, "active").await;
    let issue_id = insert_issue(&db, company_id, Some(agent_id), None).await;
    let issue = fetch_issue(&db, issue_id).await;

    let result = resolve_invokable_recovery_agent_id(&db, &issue, Some(agent_id))
        .await
        .expect("query should succeed");
    assert_eq!(result, Some(agent_id));

    cleanup(&db, company_id).await;
}

/// None 输入 → 直接 None
#[tokio::test]
async fn resolve_invokable_returns_none_for_none_input() {
    let db = connect().await;
    let company_id = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, None, None).await;
    let issue = fetch_issue(&db, issue_id).await;

    let result = resolve_invokable_recovery_agent_id(&db, &issue, None)
        .await
        .expect("query should succeed");
    assert_eq!(result, None);

    cleanup(&db, company_id).await;
}

/// 不存在的 agent → None
#[tokio::test]
async fn resolve_invokable_returns_none_for_missing_agent() {
    let db = connect().await;
    let company_id = fixture(&db).await;
    let issue_id = insert_issue(&db, company_id, None, None).await;
    let issue = fetch_issue(&db, issue_id).await;
    let nonexistent = Uuid::new_v4();

    let result = resolve_invokable_recovery_agent_id(&db, &issue, Some(nonexistent))
        .await
        .expect("query should succeed");
    assert_eq!(result, None);

    cleanup(&db, company_id).await;
}

/// 不同 company 的 agent → None
#[tokio::test]
async fn resolve_invokable_returns_none_for_other_company_agent() {
    let db = connect().await;
    let company_a = fixture(&db).await;
    let company_b = fixture(&db).await;
    let agent_id = insert_agent(&db, company_b, "r324-other", "general", None, "active").await;
    let issue_id = insert_issue(&db, company_a, None, None).await;
    let issue = fetch_issue(&db, issue_id).await;

    let result = resolve_invokable_recovery_agent_id(&db, &issue, Some(agent_id))
        .await
        .expect("query should succeed");
    assert_eq!(result, None);

    cleanup(&db, company_a).await;
    cleanup(&db, company_b).await;
}

/// terminated agent → None
#[tokio::test]
async fn resolve_invokable_returns_none_for_terminated_agent() {
    let db = connect().await;
    let company_id = fixture(&db).await;
    let agent_id = insert_agent(&db, company_id, "r324-term", "general", None, "terminated").await;
    let issue_id = insert_issue(&db, company_id, Some(agent_id), None).await;
    let issue = fetch_issue(&db, issue_id).await;

    let result = resolve_invokable_recovery_agent_id(&db, &issue, Some(agent_id))
        .await
        .expect("query should succeed");
    assert_eq!(result, None);

    cleanup(&db, company_id).await;
}

/// resolveStrandedIssueRecoveryOwnerAgentId 基础路径：preferred → 返回
#[tokio::test]
async fn resolve_owner_returns_preferred_when_invokable() {
    let db = connect().await;
    let company_id = fixture(&db).await;
    let preferred = insert_agent(&db, company_id, "r324-pref", "general", None, "active").await;
    let issue_id = insert_issue(&db, company_id, None, None).await;
    let issue = fetch_issue(&db, issue_id).await;

    let result = resolve_stranded_issue_recovery_owner_agent_id(&db, &issue, Some(preferred))
        .await
        .expect("query should succeed");
    assert_eq!(result, Some(preferred));

    cleanup(&db, company_id).await;
}

/// preferred 不可 invoke → fallback 到 CTO
#[tokio::test]
async fn resolve_owner_falls_through_to_cto_when_preferred_uninvokable() {
    let db = connect().await;
    let company_id = fixture(&db).await;
    let preferred = insert_agent(&db, company_id, "r324-pref", "general", None, "terminated").await;
    let cto = insert_agent(&db, company_id, "r324-cto", "cto", None, "active").await;
    let _ceo = insert_agent(&db, company_id, "r324-ceo", "ceo", None, "active").await;
    let issue_id = insert_issue(&db, company_id, None, None).await;
    let issue = fetch_issue(&db, issue_id).await;

    let result = resolve_stranded_issue_recovery_owner_agent_id(&db, &issue, Some(preferred))
        .await
        .expect("query should succeed");
    // CTO 应优先于 CEO
    assert_eq!(result, Some(cto));

    cleanup(&db, company_id).await;
}

/// 没有 preferred → assignee 的 manager（reports_to）优先
#[tokio::test]
async fn resolve_owner_prefers_assignee_manager() {
    let db = connect().await;
    let company_id = fixture(&db).await;
    let manager = insert_agent(&db, company_id, "r324-mgr", "general", None, "active").await;
    let assignee = insert_agent(
        &db,
        company_id,
        "r324-asg",
        "general",
        Some(manager),
        "active",
    )
    .await;
    let issue_id = insert_issue(&db, company_id, Some(assignee), None).await;
    let issue = fetch_issue(&db, issue_id).await;

    let result = resolve_stranded_issue_recovery_owner_agent_id(&db, &issue, None)
        .await
        .expect("query should succeed");
    assert_eq!(result, Some(manager));

    cleanup(&db, company_id).await;
}

/// 都没有 invokable → 返回 None
#[tokio::test]
async fn resolve_owner_returns_none_when_no_invokable_candidate() {
    let db = connect().await;
    let company_id = fixture(&db).await;
    let _assignee = insert_agent(&db, company_id, "r324-asg", "general", None, "terminated").await;
    let _cto = insert_agent(&db, company_id, "r324-cto", "cto", None, "terminated").await;
    let _ceo = insert_agent(&db, company_id, "r324-ceo", "ceo", None, "terminated").await;
    let issue_id = insert_issue(&db, company_id, Some(_assignee), None).await;
    let issue = fetch_issue(&db, issue_id).await;

    let result = resolve_stranded_issue_recovery_owner_agent_id(&db, &issue, None)
        .await
        .expect("query should succeed");
    assert_eq!(result, None);

    cleanup(&db, company_id).await;
}

/// created_by_agent 的 manager 也算候选
#[tokio::test]
async fn resolve_owner_considers_creator_manager() {
    let db = connect().await;
    let company_id = fixture(&db).await;
    let creator_manager = insert_agent(&db, company_id, "r324-cm", "general", None, "active").await;
    let creator = insert_agent(
        &db,
        company_id,
        "r324-creator",
        "general",
        Some(creator_manager),
        "active",
    )
    .await;
    let issue_id = insert_issue(&db, company_id, None, Some(creator)).await;
    let issue = fetch_issue(&db, issue_id).await;

    let result = resolve_stranded_issue_recovery_owner_agent_id(&db, &issue, None)
        .await
        .expect("query should succeed");
    // creator_manager 应优先（因为它出现在 candidate list 前面）
    assert_eq!(result, Some(creator_manager));

    cleanup(&db, company_id).await;
}

/// 去重：同一 agent 多次出现在 candidate list 只算一次（仍返回）
#[tokio::test]
async fn resolve_owner_deduplicates_candidates() {
    let db = connect().await;
    let company_id = fixture(&db).await;
    let cto = insert_agent(&db, company_id, "r324-cto", "cto", None, "active").await;
    // 把 cto 同时设为 preferred 和 issue.assignee
    let issue_id = insert_issue(&db, company_id, Some(cto), None).await;
    let issue = fetch_issue(&db, issue_id).await;

    let result = resolve_stranded_issue_recovery_owner_agent_id(&db, &issue, Some(cto))
        .await
        .expect("query should succeed");
    assert_eq!(result, Some(cto));

    cleanup(&db, company_id).await;
}
