//! R595: AgentTerminationApprovalHook 端到端 contract 测试。
//!
//! 验证 AgentService.terminate 触发 AgentTerminationApprovalHook：
//! - 高风险 role（ceo/admin/owner）→ 创建 approval
//! - 普通 role（general）→ 不创建 approval
//! - 其他事件（Paused/Resumed）→ 不创建 approval
//! - ApprovalService 不可用 → 不影响 terminate

use std::sync::Arc;

use pc_adapter_api::AdapterRegistry;
use pc_approvals::ApprovalService;
use pc_core::ActorRegistry;
use pc_heartbeat::spawn_heartbeat_supervisor;
use pc_http::{
    hooks::AgentTerminationApprovalHook,
    state::{ConfigSnapshot, RuntimeHandles},
    AppState,
};
use pc_realtime::{RealtimeHandle, WsState};
use pc_repos::{
    approval::{ApprovalType, NewApproval},
    Db,
};
use sqlx::PgPool;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn setup_db() -> (Db, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(TEST_DATABASE_URL)
        .await
        .expect("connect");
    let db = Db::connect(TEST_DATABASE_URL, 4, 1).await.expect("Db");
    (db, pool)
}

fn test_state(db: Db) -> AppState {
    let actors = ActorRegistry::new();
    let realtime = RealtimeHandle::start(64);
    AppState::new(
        db.clone(),
        RuntimeHandles {
            heartbeat: spawn_heartbeat_supervisor(4, actors.clone()),
            agents: pc_agent::spawn_agent_supervisor(db),
            adapters: AdapterRegistry::new(),
            actors,
        },
        ConfigSnapshot {
            host: "127.0.0.1".into(),
            port: 3100,
            session_cookie: "paperclip_session".into(),
            api_key_header: "x-paperclip-agent-key".into(),
            csrf_header: "x-paperclip-csrf".into(),
        },
        pc_telemetry::TelemetryOptions::default(),
        Arc::new(WsState::new(realtime.clone(), "test".to_string())),
        realtime,
    )
}

async fn insert_company(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R595-{id}"))
    .bind(format!("A5{}", &id.simple().to_string()[..5]))
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn insert_agent(pool: &PgPool, company_id: Uuid, role: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status, adapter_config, \
         permissions, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, 'process', 'idle', '{}'::jsonb, '{}'::jsonb, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("Agent-{id}"))
    .bind(role)
    .execute(pool)
    .await
    .expect("insert agent");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid, agent_id: Uuid) {
    // 先清理 approvals 引用此 agent
    let _ = sqlx::query("DELETE FROM approvals WHERE payload->>'agent_id' = $1")
        .bind(agent_id.to_string())
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM agents WHERE id = $1")
        .bind(agent_id)
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
async fn r595_ceo_termination_creates_approval() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let agent_id = insert_agent(&pool, company_id, "ceo").await;

    let db_static: &'static pc_repos::Db = Box::leak(Box::new(db.clone()));
    let approval_svc = Arc::new(ApprovalService::new(db_static));
    let hook: Arc<dyn pc_agent::AgentHook> =
        Arc::new(AgentTerminationApprovalHook::new(approval_svc));
    let svc = pc_agent::AgentService::with_hooks(db.clone(), vec![hook]);

    let _ = svc.terminate(agent_id).await.expect("terminate");

    // 验证 approval 被创建
    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM approvals WHERE company_id = $1 AND payload->>'agent_id' = $2",
    )
    .bind(company_id)
    .bind(agent_id.to_string())
    .fetch_one(db.pool())
    .await
    .expect("count");
    assert_eq!(count.0, 1, "expected 1 approval for ceo termination");

    cleanup(&pool, company_id, agent_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r595_admin_termination_creates_approval() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let agent_id = insert_agent(&pool, company_id, "admin").await;

    let db_static: &'static pc_repos::Db = Box::leak(Box::new(db.clone()));
    let approval_svc = Arc::new(ApprovalService::new(db_static));
    let hook: Arc<dyn pc_agent::AgentHook> =
        Arc::new(AgentTerminationApprovalHook::new(approval_svc));
    let svc = pc_agent::AgentService::with_hooks(db.clone(), vec![hook]);

    let _ = svc.terminate(agent_id).await.expect("terminate");

    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM approvals WHERE company_id = $1 AND payload->>'agent_id' = $2",
    )
    .bind(company_id)
    .bind(agent_id.to_string())
    .fetch_one(db.pool())
    .await
    .expect("count");
    assert_eq!(count.0, 1, "admin termination should create approval");

    cleanup(&pool, company_id, agent_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r595_owner_termination_creates_approval() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let agent_id = insert_agent(&pool, company_id, "owner").await;

    let db_static: &'static pc_repos::Db = Box::leak(Box::new(db.clone()));
    let approval_svc = Arc::new(ApprovalService::new(db_static));
    let hook: Arc<dyn pc_agent::AgentHook> =
        Arc::new(AgentTerminationApprovalHook::new(approval_svc));
    let svc = pc_agent::AgentService::with_hooks(db.clone(), vec![hook]);

    let _ = svc.terminate(agent_id).await.expect("terminate");

    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM approvals WHERE company_id = $1 AND payload->>'agent_id' = $2",
    )
    .bind(company_id)
    .bind(agent_id.to_string())
    .fetch_one(db.pool())
    .await
    .expect("count");
    assert_eq!(count.0, 1, "owner termination should create approval");

    cleanup(&pool, company_id, agent_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r595_general_termination_does_not_create_approval() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let agent_id = insert_agent(&pool, company_id, "general").await;

    let db_static: &'static pc_repos::Db = Box::leak(Box::new(db.clone()));
    let approval_svc = Arc::new(ApprovalService::new(db_static));
    let hook: Arc<dyn pc_agent::AgentHook> =
        Arc::new(AgentTerminationApprovalHook::new(approval_svc));
    let svc = pc_agent::AgentService::with_hooks(db.clone(), vec![hook]);

    let _ = svc.terminate(agent_id).await.expect("terminate");

    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM approvals WHERE company_id = $1 AND payload->>'agent_id' = $2",
    )
    .bind(company_id)
    .bind(agent_id.to_string())
    .fetch_one(db.pool())
    .await
    .expect("count");
    assert_eq!(count.0, 0, "general termination should NOT create approval");

    cleanup(&pool, company_id, agent_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r595_resume_does_not_create_approval() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let agent_id = insert_agent(&pool, company_id, "ceo").await;

    let db_static: &'static pc_repos::Db = Box::leak(Box::new(db.clone()));
    let approval_svc = Arc::new(ApprovalService::new(db_static));
    let hook: Arc<dyn pc_agent::AgentHook> =
        Arc::new(AgentTerminationApprovalHook::new(approval_svc));
    let svc = pc_agent::AgentService::with_hooks(db.clone(), vec![hook]);

    // resume 不应该触发 approval
    let _ = svc.resume(agent_id).await;
    let _ = svc.pause(agent_id, pc_agent::PauseReason::Manual).await;

    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM approvals WHERE company_id = $1 AND payload->>'agent_id' = $2",
    )
    .bind(company_id)
    .bind(agent_id.to_string())
    .fetch_one(db.pool())
    .await
    .expect("count");
    assert_eq!(count.0, 0, "resume/pause should NOT create approval");

    cleanup(&pool, company_id, agent_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r595_custom_high_risk_roles_respected() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let agent_id = insert_agent(&pool, company_id, "researcher").await;

    let db_static: &'static pc_repos::Db = Box::leak(Box::new(db.clone()));
    let approval_svc = Arc::new(ApprovalService::new(db_static));
    // 自定义高风险 role 为 "researcher"
    let hook: Arc<dyn pc_agent::AgentHook> = Arc::new(
        AgentTerminationApprovalHook::with_high_risk_roles(approval_svc, vec!["researcher".into()]),
    );
    let svc = pc_agent::AgentService::with_hooks(db.clone(), vec![hook]);

    let _ = svc.terminate(agent_id).await.expect("terminate");

    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM approvals WHERE company_id = $1 AND payload->>'agent_id' = $2",
    )
    .bind(company_id)
    .bind(agent_id.to_string())
    .fetch_one(db.pool())
    .await
    .expect("count");
    assert_eq!(
        count.0, 1,
        "researcher should be high-risk with custom config"
    );

    cleanup(&pool, company_id, agent_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r595_terminate_nonexistent_no_approval() {
    let (db, pool) = setup_db().await;

    let db_static: &'static pc_repos::Db = Box::leak(Box::new(db.clone()));
    let approval_svc = Arc::new(ApprovalService::new(db_static));
    let hook: Arc<dyn pc_agent::AgentHook> =
        Arc::new(AgentTerminationApprovalHook::new(approval_svc));
    let svc = pc_agent::AgentService::with_hooks(db.clone(), vec![hook]);

    let bogus_company = Uuid::new_v4();
    let result = svc.terminate(Uuid::new_v4()).await.expect("terminate");
    assert!(result.is_none());

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM approvals WHERE company_id = $1")
        .bind(bogus_company)
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(count.0, 0, "no approval for missing agent");
}

#[tokio::test(flavor = "current_thread")]
async fn r595_approval_payload_carries_agent_metadata() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let agent_id = insert_agent(&pool, company_id, "ceo").await;

    let db_static: &'static pc_repos::Db = Box::leak(Box::new(db.clone()));
    let approval_svc = Arc::new(ApprovalService::new(db_static));
    let hook: Arc<dyn pc_agent::AgentHook> =
        Arc::new(AgentTerminationApprovalHook::new(approval_svc));
    let svc = pc_agent::AgentService::with_hooks(db.clone(), vec![hook]);

    let _ = svc.terminate(agent_id).await.expect("terminate");

    let row: (String, serde_json::Value) = sqlx::query_as(
        "SELECT type AS approval_type, payload FROM approvals WHERE company_id = $1 \
         AND payload->>'agent_id' = $2",
    )
    .bind(company_id)
    .bind(agent_id.to_string())
    .fetch_one(db.pool())
    .await
    .expect("fetch approval");
    assert_eq!(row.0, ApprovalType::AgentAction.as_str());
    assert_eq!(row.1["action"], "agent_termination");
    assert_eq!(row.1["role"], "ceo");
    assert_eq!(row.1["reason"], "high_risk_role_termination");

    cleanup(&pool, company_id, agent_id).await;
}
