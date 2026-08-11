//! R594: AgentLifecycleEvent hook 真实 DB 端到端测试。

use std::sync::Arc;

use pc_agent::{
    AgentHook, AgentLifecycleEvent, AgentService, NoopAgentHook, PauseReason, RecordingAgentHook,
};
use pc_repos::Db;
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

async fn insert_company(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R594-{id}"))
    .bind(format!("A5{}", &id.simple().to_string()[..5]))
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn insert_agent(pool: &PgPool, company_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status, adapter_config, \
         permissions, created_at, updated_at) \
         VALUES ($1, $2, $3, 'general', 'process', 'idle', '{}'::jsonb, '{}'::jsonb, now(), now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("Agent-{id}"))
    .execute(pool)
    .await
    .expect("insert agent");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid, agent_id: Uuid) {
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
async fn r594_terminate_emits_lifecycle_event() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let agent_id = insert_agent(&pool, company_id).await;

    let hook = Arc::new(RecordingAgentHook::default());
    let svc = AgentService::with_hooks(db.clone(), vec![hook.clone()]);

    let terminated = svc.terminate(agent_id).await.expect("terminate");
    assert!(terminated.is_some(), "agent should be terminated");

    let events = hook.events.lock().expect("lock");
    assert_eq!(events.len(), 1);
    match &events[0] {
        AgentLifecycleEvent::Terminated {
            id,
            company_id: cid,
            role,
        } => {
            assert_eq!(*id, agent_id);
            assert_eq!(*cid, company_id);
            assert_eq!(role, "general");
        }
        other => panic!("expected Terminated, got {other:?}"),
    }

    cleanup(&pool, company_id, agent_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r594_pause_emits_lifecycle_event() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let agent_id = insert_agent(&pool, company_id).await;

    let hook = Arc::new(RecordingAgentHook::default());
    let svc = AgentService::with_hooks(db.clone(), vec![hook.clone()]);

    let paused = svc
        .pause(agent_id, PauseReason::Manual)
        .await
        .expect("pause");
    assert!(paused.is_some());

    let events = hook.events.lock().expect("lock");
    assert_eq!(events.len(), 1);
    match &events[0] {
        AgentLifecycleEvent::Paused { id, reason, .. } => {
            assert_eq!(*id, agent_id);
            assert_eq!(reason, "manual");
        }
        other => panic!("expected Paused, got {other:?}"),
    }

    cleanup(&pool, company_id, agent_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r594_resume_emits_lifecycle_event() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let agent_id = insert_agent(&pool, company_id).await;

    let hook = Arc::new(RecordingAgentHook::default());
    let svc = AgentService::with_hooks(db.clone(), vec![hook.clone()]);

    // 先 pause 再 resume
    svc.pause(agent_id, PauseReason::Manual)
        .await
        .expect("pause");
    let _ = hook.events.lock().expect("lock").clear();

    let resumed = svc.resume(agent_id).await.expect("resume");
    assert!(resumed.is_some());

    let events = hook.events.lock().expect("lock");
    assert_eq!(events.len(), 1);
    match &events[0] {
        AgentLifecycleEvent::Resumed { id, .. } => {
            assert_eq!(*id, agent_id);
        }
        other => panic!("expected Resumed, got {other:?}"),
    }

    cleanup(&pool, company_id, agent_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r594_terminate_nonexistent_no_event() {
    let (db, pool) = setup_db().await;
    let hook = Arc::new(RecordingAgentHook::default());
    let svc = AgentService::with_hooks(db.clone(), vec![hook.clone()]);

    let result = svc.terminate(Uuid::new_v4()).await.expect("terminate");
    assert!(result.is_none(), "missing agent should return None");

    let events = hook.events.lock().expect("lock");
    assert!(events.is_empty(), "no event for missing agent");

    let _ = pool;
}

#[tokio::test(flavor = "current_thread")]
async fn r594_noop_hook_does_not_panic() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let agent_id = insert_agent(&pool, company_id).await;

    let svc = AgentService::with_hooks(db.clone(), vec![Arc::new(NoopAgentHook)]);
    let _ = svc.terminate(agent_id).await.expect("terminate");

    cleanup(&pool, company_id, agent_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r594_add_hook_chain() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let agent_id = insert_agent(&pool, company_id).await;

    let hook = Arc::new(RecordingAgentHook::default());
    let svc = AgentService::new(db.clone()).add_hook(hook.clone());

    let _ = svc.terminate(agent_id).await.expect("terminate");

    let events = hook.events.lock().expect("lock");
    assert_eq!(events.len(), 1);

    cleanup(&pool, company_id, agent_id).await;
}
