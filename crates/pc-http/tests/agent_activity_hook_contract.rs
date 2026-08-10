//! R594: AgentActivityHook 端到端 contract 测试。
//!
//! 验证 AgentService 通过 AgentActivityHook 自动触发 ActivityLog + Realtime。
//! 注：actor 路径不经过 hook（actor 直接调 service）；这里测 service 路径。

use std::sync::Arc;

use pc_activity::{ActivityKind, ActivityLog, InMemoryActivityLog, SharedActivitySink};
use pc_adapter_api::AdapterRegistry;
use pc_agent::{AgentHook, AgentLifecycleEvent, AgentService, RecordingAgentHook};
use pc_core::ActorRegistry;
use pc_heartbeat::spawn_heartbeat_supervisor;
use pc_http::{
    hooks::AgentActivityHook,
    state::{ConfigSnapshot, RuntimeHandles},
    AppState,
};
use pc_realtime::{RealtimeHandle, WsState};
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

fn test_state_with_recording(db: Db) -> (AppState, Arc<InMemoryActivityLog>) {
    let actors = ActorRegistry::new();
    let realtime = RealtimeHandle::start(64);
    let in_mem = Arc::new(InMemoryActivityLog::new());
    let activity = ActivityLog::new(SharedActivitySink::new(in_mem.clone()));
    let state = AppState::new(
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
    .with_activity(activity);
    (state, in_mem)
}

async fn insert_company(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R594-Contract-{id}"))
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
async fn r594_terminate_through_service_emits_activity_event() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let agent_id = insert_agent(&pool, company_id).await;

    let (state, in_mem) = test_state_with_recording(db.clone());
    let hook: Arc<dyn AgentHook> = Arc::new(AgentActivityHook::new(Arc::new(state.clone())));
    let svc = AgentService::with_hooks(db.clone(), vec![hook]);

    let _ = svc.terminate(agent_id).await.expect("terminate");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let events = in_mem.snapshot();
    assert!(
        events
            .iter()
            .any(|e| matches!(e.kind, ActivityKind::AgentStopped)
                && e.subject_kind == "agent"
                && e.subject_id == agent_id),
        "expected AgentStopped activity event for agent {agent_id}, got: {events:?}"
    );

    cleanup(&pool, company_id, agent_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r594_recording_hook_captures_all_three_events() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let agent_id = insert_agent(&pool, company_id).await;

    let hook = Arc::new(RecordingAgentHook::default());
    let svc = AgentService::with_hooks(db.clone(), vec![hook.clone()]);

    svc.pause(agent_id, pc_agent::PauseReason::Manual)
        .await
        .expect("pause");
    svc.resume(agent_id).await.expect("resume");
    svc.terminate(agent_id).await.expect("terminate");

    let events = hook.events.lock().expect("lock");
    assert_eq!(events.len(), 3);
    assert!(matches!(events[0], AgentLifecycleEvent::Paused { .. }));
    assert!(matches!(events[1], AgentLifecycleEvent::Resumed { .. }));
    assert!(matches!(events[2], AgentLifecycleEvent::Terminated { .. }));

    cleanup(&pool, company_id, agent_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r594_agent_hook_trait_is_object_safe() {
    // 验证 AgentHook 可以作为 dyn trait — 这是 service 抽象的核心保证
    let (db, _pool) = setup_db().await;
    let hook: Arc<dyn AgentHook> = Arc::new(RecordingAgentHook::default());
    let _svc = AgentService::with_hooks(db, vec![hook]);
}

#[tokio::test(flavor = "current_thread")]
async fn r594_pause_then_resume_emits_correct_kinds() {
    let (db, pool) = setup_db().await;
    let company_id = insert_company(&pool).await;
    let agent_id = insert_agent(&pool, company_id).await;

    let (state, in_mem) = test_state_with_recording(db.clone());
    let hook: Arc<dyn AgentHook> = Arc::new(AgentActivityHook::new(Arc::new(state.clone())));
    let svc = AgentService::with_hooks(db.clone(), vec![hook]);

    svc.pause(agent_id, pc_agent::PauseReason::Manual)
        .await
        .expect("pause");
    svc.resume(agent_id).await.expect("resume");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let events = in_mem.snapshot();
    assert!(
        events
            .iter()
            .any(|e| matches!(e.kind, ActivityKind::AgentStarted) && e.subject_id == agent_id),
        "expected AgentStarted activity event for agent {agent_id}, got: {events:?}"
    );

    cleanup(&pool, company_id, agent_id).await;
}
