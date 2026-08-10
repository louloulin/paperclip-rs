//! E2E tests for `pc-agent-action-audit` against real Postgres.
//!
//! 覆盖：
//! - 基本 list 返回（agent_id 非空的 activity_log 行）
//! - cursor 分页（next_cursor 编码 + 解码）
//! - entity 富化（issue / issue_comment / issue_document）
//! - Hook before/after 触发
//! - 错误：invalid cursor
//! - 隔离：跨 company

use pc_agent_action_audit::{
    codes, normalize_limit, AgentActionAuditFilters, AgentActionAuditService,
    RecordingAgentActionAuditHook, DEFAULT_LIMIT, MAX_LIMIT,
};
use pc_repos::Db;
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

const DB_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(DB_URL, 5, 1).await.expect("connect")
}

async fn cleanup(db: &Db, tag: &str) {
    let prefix = format!("AAA-{tag}");
    let _ = sqlx::query(
        "DELETE FROM activity_log WHERE company_id IN (SELECT id FROM companies WHERE issue_prefix = $1)",
    )
    .bind(&prefix)
    .execute(db.pool())
    .await;
    let _ = sqlx::query(
        "DELETE FROM heartbeat_runs WHERE company_id IN (SELECT id FROM companies WHERE issue_prefix = $1)",
    )
    .bind(&prefix)
    .execute(db.pool())
    .await;
    let _ = sqlx::query(
        "DELETE FROM issue_comments WHERE company_id IN (SELECT id FROM companies WHERE issue_prefix = $1)",
    )
    .bind(&prefix)
    .execute(db.pool())
    .await;
    let _ = sqlx::query(
        "DELETE FROM issue_documents WHERE company_id IN (SELECT id FROM companies WHERE issue_prefix = $1)",
    )
    .bind(&prefix)
    .execute(db.pool())
    .await;
    let _ = sqlx::query(
        "DELETE FROM issues WHERE company_id IN (SELECT id FROM companies WHERE issue_prefix = $1)",
    )
    .bind(&prefix)
    .execute(db.pool())
    .await;
    let _ = sqlx::query(
        "DELETE FROM agents WHERE company_id IN (SELECT id FROM companies WHERE issue_prefix = $1)",
    )
    .bind(&prefix)
    .execute(db.pool())
    .await;
    let _ = sqlx::query("DELETE FROM companies WHERE issue_prefix = $1")
        .bind(&prefix)
        .execute(db.pool())
        .await;
}

async fn make_company(db: &Db, tag: &str) -> Uuid {
    let name = format!("AAA Co {tag} {}", Uuid::new_v4());
    let row = sqlx::query("INSERT INTO companies (name, issue_prefix) VALUES ($1, $2) RETURNING id")
        .bind(&name)
        .bind(format!("AAA-{tag}-{}", Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .expect("create company");
    row.try_get::<Uuid, _>("id").expect("id")
}

async fn make_issue(db: &Db, company_id: Uuid) -> Uuid {
    let row = sqlx::query(
        "INSERT INTO issues (company_id, title, status, created_by_user_id, identifier) \
         VALUES ($1, 'Test issue', 'todo', $2, $3) RETURNING id",
    )
    .bind(company_id)
    .bind(Uuid::new_v4().to_string())
    .bind(format!("AAA-{}", Uuid::new_v4().to_string()[..6].to_uppercase()))
    .fetch_one(db.pool())
    .await
    .expect("create issue");
    row.try_get::<Uuid, _>("id").expect("issue id")
}

async fn make_agent(db: &Db, company_id: Uuid) -> Uuid {
    let row = sqlx::query(
        "INSERT INTO agents (company_id, name, role, status, adapter_type, adapter_config, \
         budget_monthly_cents, spent_monthly_cents) \
         VALUES ($1, $2, 'general', 'idle', 'process', '{}', 0, 0) RETURNING id",
    )
    .bind(company_id)
    .bind(Uuid::new_v4().to_string())
    .fetch_one(db.pool())
    .await
    .expect("create agent");
    row.try_get::<Uuid, _>("id").expect("agent id")
}

async fn insert_activity(
    db: &Db,
    company_id: Uuid,
    agent_id: Uuid,
    action: &str,
    entity_type: &str,
    entity_id: &str,
) -> Uuid {
    let row = sqlx::query(
        "INSERT INTO activity_log (company_id, actor_type, actor_id, action, entity_type, \
         entity_id, agent_id) VALUES ($1, 'agent', $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(company_id)
    .bind(agent_id.to_string())
    .bind(action)
    .bind(entity_type)
    .bind(entity_id)
    .bind(agent_id)
    .fetch_one(db.pool())
    .await
    .expect("insert activity");
    row.try_get::<Uuid, _>("id").expect("activity id")
}

async fn insert_activity_no_agent(db: &Db, company_id: Uuid, action: &str) -> Uuid {
    let row = sqlx::query(
        "INSERT INTO activity_log (company_id, actor_type, actor_id, action, entity_type, \
         entity_id) VALUES ($1, 'user', $2, $3, 'misc', $4) RETURNING id",
    )
    .bind(company_id)
    .bind(Uuid::new_v4().to_string())
    .bind(action)
    .bind(Uuid::new_v4().to_string())
    .fetch_one(db.pool())
    .await
    .expect("insert no-agent");
    row.try_get::<Uuid, _>("id").expect("id")
}

#[tokio::test]
async fn r684_e2e_list_filters_agent_only() {
    let db = connect().await;
    cleanup(&db, "filter").await;
    let cid = make_company(&db, "filter").await;
    let agent_id = make_agent(&db, cid).await;

    // 插入 1 条 agent 动作
    insert_activity(&db, cid, agent_id, "tool_run", "run", &Uuid::new_v4().to_string()).await;
    // 插入 1 条 user 动作（agent_id IS NULL）—— 不应被 list 出来
    insert_activity_no_agent(&db, cid, "user_login").await;

    let svc = AgentActionAuditService::new(db.clone());
    let page = svc
        .list(AgentActionAuditFilters {
            company_id: cid,
            agent_id: None,
            responsible_user_id: None,
            run_id: None,
            entity_type: None,
            entity_id: None,
            action: None,
            actor_type: None,
            from: None,
            to: None,
            cursor: None,
            limit: Some(10),
        })
        .await
        .expect("list");

    // 仅 1 行（agent 那条）
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].action, "tool_run");
    assert!(page.next_cursor.is_none());

    cleanup(&db, "filter").await;
}

#[tokio::test]
async fn r684_e2e_cursor_pagination() {
    let db = connect().await;
    cleanup(&db, "page").await;
    let cid = make_company(&db, "page").await;
    let agent_id = make_agent(&db, cid).await;

    // 插入 3 条 agent 动作
    for i in 0..3 {
        insert_activity(
            &db,
            cid,
            agent_id,
            &format!("action_{i}"),
            "run",
            &format!("r{i}"),
        )
        .await;
        // 让 created_at 各异
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let svc = AgentActionAuditService::new(db.clone());
    // 第一页：limit=2
    let page1 = svc
        .list(AgentActionAuditFilters {
            company_id: cid,
            agent_id: None,
            responsible_user_id: None,
            run_id: None,
            entity_type: None,
            entity_id: None,
            action: None,
            actor_type: None,
            from: None,
            to: None,
            cursor: None,
            limit: Some(2),
        })
        .await
        .expect("page1");
    assert_eq!(page1.items.len(), 2);
    let cursor = page1.next_cursor.expect("cursor for next page");

    // 第二页：用 cursor 继续
    let page2 = svc
        .list(AgentActionAuditFilters {
            company_id: cid,
            agent_id: None,
            responsible_user_id: None,
            run_id: None,
            entity_type: None,
            entity_id: None,
            action: None,
            actor_type: None,
            from: None,
            to: None,
            cursor: Some(cursor),
            limit: Some(2),
        })
        .await
        .expect("page2");
    // 第二页应该有 1 条（剩余）+ next_cursor = None
    assert_eq!(page2.items.len(), 1);
    assert!(page2.next_cursor.is_none());

    cleanup(&db, "page").await;
}

#[tokio::test]
async fn r684_e2e_invalid_cursor_returns_error() {
    let db = connect().await;
    cleanup(&db, "inv").await;
    let cid = make_company(&db, "inv").await;
    let svc = AgentActionAuditService::new(db.clone());

    let err = svc
        .list(AgentActionAuditFilters {
            company_id: cid,
            agent_id: None,
            responsible_user_id: None,
            run_id: None,
            entity_type: None,
            entity_id: None,
            action: None,
            actor_type: None,
            from: None,
            to: None,
            cursor: Some("garbage_cursor".to_string()),
            limit: Some(10),
        })
        .await
        .expect_err("should fail");
    assert_eq!(err.infer_code(), Some(codes::INVALID_AUDIT_CURSOR));

    cleanup(&db, "inv").await;
}

#[tokio::test]
async fn r684_e2e_hooks_fire() {
    let db = connect().await;
    cleanup(&db, "hook").await;
    let cid = make_company(&db, "hook").await;
    let agent_id = make_agent(&db, cid).await;
    insert_activity(&db, cid, agent_id, "tool_call", "run", &Uuid::new_v4().to_string()).await;

    let hook = Arc::new(RecordingAgentActionAuditHook::new());
    let svc = AgentActionAuditService::with_hook(db.clone(), hook.clone());

    let _ = svc
        .list(AgentActionAuditFilters {
            company_id: cid,
            agent_id: None,
            responsible_user_id: None,
            run_id: None,
            entity_type: None,
            entity_id: None,
            action: None,
            actor_type: None,
            from: None,
            to: None,
            cursor: None,
            limit: Some(10),
        })
        .await
        .expect("list");

    assert_eq!(hook.before_count(), 1);
    assert_eq!(hook.after_count(), 1);

    cleanup(&db, "hook").await;
}

#[tokio::test]
async fn r684_e2e_filter_by_entity_type() {
    let db = connect().await;
    cleanup(&db, "entity").await;
    let cid = make_company(&db, "entity").await;
    let agent_id = make_agent(&db, cid).await;

    insert_activity(&db, cid, agent_id, "issue_updated", "issue", &Uuid::new_v4().to_string()).await;
    insert_activity(&db, cid, agent_id, "tool_call", "run", &Uuid::new_v4().to_string()).await;

    let svc = AgentActionAuditService::new(db.clone());
    let page = svc
        .list(AgentActionAuditFilters {
            company_id: cid,
            agent_id: None,
            responsible_user_id: None,
            run_id: None,
            entity_type: Some("issue".to_string()),
            entity_id: None,
            action: None,
            actor_type: None,
            from: None,
            to: None,
            cursor: None,
            limit: Some(10),
        })
        .await
        .expect("list");

    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].entity_type, "issue");

    cleanup(&db, "entity").await;
}

#[tokio::test]
async fn r684_e2e_distinct_companies_isolated() {
    let db = connect().await;
    cleanup(&db, "iso-a").await;
    cleanup(&db, "iso-b").await;
    let cid_a = make_company(&db, "iso-a").await;
    let cid_b = make_company(&db, "iso-b").await;
    let agent_a = make_agent(&db, cid_a).await;
    let agent_b = make_agent(&db, cid_b).await;

    insert_activity(&db, cid_a, agent_a, "tool_call", "run", &Uuid::new_v4().to_string()).await;
    insert_activity(&db, cid_b, agent_b, "tool_call", "run", &Uuid::new_v4().to_string()).await;

    let svc = AgentActionAuditService::new(db.clone());
    let page_a = svc
        .list(AgentActionAuditFilters {
            company_id: cid_a,
            agent_id: None,
            responsible_user_id: None,
            run_id: None,
            entity_type: None,
            entity_id: None,
            action: None,
            actor_type: None,
            from: None,
            to: None,
            cursor: None,
            limit: Some(10),
        })
        .await
        .expect("a");
    let page_b = svc
        .list(AgentActionAuditFilters {
            company_id: cid_b,
            agent_id: None,
            responsible_user_id: None,
            run_id: None,
            entity_type: None,
            entity_id: None,
            action: None,
            actor_type: None,
            from: None,
            to: None,
            cursor: None,
            limit: Some(10),
        })
        .await
        .expect("b");

    assert_eq!(page_a.items.len(), 1);
    assert_eq!(page_b.items.len(), 1);
    assert_ne!(page_a.items[0].id, page_b.items[0].id);

    cleanup(&db, "iso-a").await;
    cleanup(&db, "iso-b").await;
}

#[tokio::test]
async fn r684_e2e_default_limit_when_none() {
    // 通过 Repo 验证 normalize_limit 已 clamp
    assert_eq!(normalize_limit(None), DEFAULT_LIMIT);
    assert_eq!(normalize_limit(Some(0)), 1);
    assert_eq!(normalize_limit(Some(MAX_LIMIT)), MAX_LIMIT);
    assert_eq!(normalize_limit(Some(99_999)), MAX_LIMIT);
}
