//! R599: DecisionActivityHook 端到端 contract 测试。
//!
//! 验证 DecisionService 通过 DecisionActivityHook 自动触发 ActivityLog + Realtime。

use std::sync::Arc;

use pc_activity::{ActivityKind, ActivityLog, InMemoryActivityLog, SharedActivitySink};
use pc_adapter_api::AdapterRegistry;
use pc_core::ActorRegistry;
use pc_decisions::DecisionService;
use pc_heartbeat::spawn_heartbeat_supervisor;
use pc_http::{
    hooks::DecisionActivityHook,
    state::{ConfigSnapshot, RuntimeHandles},
    AppState,
};
use pc_realtime::{RealtimeHandle, WsState};
use pc_repos::Db;
use pc_secrets::DecisionSigningService as SigningService;
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

async fn insert_company_agent_issue_run(db: &Db, pool: &PgPool) -> (Uuid, Uuid, Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(company_id)
    .bind(format!("R599-{company_id}"))
    .bind(format!("DC{}", &company_id.simple().to_string()[..4]))
    .execute(db.pool())
    .await
    .expect("insert company");

    let agent_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status, adapter_config, \
         created_at, updated_at) \
         VALUES ($1, $2, $3, 'general', 'process', 'idle', '{}'::jsonb, now(), now())",
    )
    .bind(agent_id)
    .bind(company_id)
    .bind(format!("Agent {agent_id}"))
    .execute(db.pool())
    .await
    .expect("insert agent");

    let issue_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority, created_at, updated_at) \
         VALUES ($1, $2, 'Decision test', 'todo', 'medium', now(), now())",
    )
    .bind(issue_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .expect("insert issue");

    let run_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO heartbeat_runs (id, company_id, agent_id, status, invocation_source, \
         created_at, updated_at) \
         VALUES ($1, $2, $3, 'queued', 'manual_test', now(), now())",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .expect("insert run");

    let _ = pool;
    (company_id, agent_id, issue_id, run_id)
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM decisions WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM heartbeat_runs WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM agents WHERE company_id = $1")
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

fn test_signing() -> SigningService {
    SigningService::from_secret("0123456789abcdef0123456789abcdef").expect("test signing")
}

#[tokio::test(flavor = "current_thread")]
async fn r599_create_emits_decision_proposed() {
    let (db, pool) = setup_db().await;
    let (state, in_mem) = test_state_with_recording(db.clone());
    let (company_id, agent_id, issue_id, _run_id) =
        insert_company_agent_issue_run(&db, &pool).await;

    let signing = test_signing();
    let hook: Arc<dyn pc_decisions::DecisionHook> =
        Arc::new(DecisionActivityHook::new(Arc::new(state.clone())));
    let svc = DecisionService::with_hooks(&db, &signing, vec![hook]);

    let row = svc
        .create(company_id, "R599 proposal", "Test body")
        .await
        .expect("create");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let events = in_mem.snapshot();
    assert!(
        events
            .iter()
            .any(|e| matches!(e.kind, ActivityKind::DecisionProposed)
                && e.subject_kind == "decision"
                && e.subject_id == row.id),
        "expected DecisionProposed activity, got: {events:?}"
    );

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r599_decide_emits_decision_approved() {
    let (db, pool) = setup_db().await;
    let (state, in_mem) = test_state_with_recording(db.clone());
    let (company_id, agent_id, issue_id, _run_id) =
        insert_company_agent_issue_run(&db, &pool).await;

    let signing = test_signing();
    let hook: Arc<dyn pc_decisions::DecisionHook> =
        Arc::new(DecisionActivityHook::new(Arc::new(state.clone())));
    let svc = DecisionService::with_hooks(&db, &signing, vec![hook]);

    let row = svc
        .create(company_id, "R599 decide", "Decide body")
        .await
        .expect("create");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let _ = svc
        .decide(row.id, "x", Some("test-user"), None, None)
        .await
        .expect("decide");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let events = in_mem.snapshot();
    assert!(
        events
            .iter()
            .any(|e| matches!(e.kind, ActivityKind::DecisionApproved) && e.subject_id == row.id),
        "expected DecisionApproved activity, got: {events:?}"
    );

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r599_dismiss_emits_decision_dismissed() {
    let (db, pool) = setup_db().await;
    let (state, in_mem) = test_state_with_recording(db.clone());
    let (company_id, agent_id, issue_id, _run_id) =
        insert_company_agent_issue_run(&db, &pool).await;

    let signing = test_signing();
    let hook: Arc<dyn pc_decisions::DecisionHook> =
        Arc::new(DecisionActivityHook::new(Arc::new(state.clone())));
    let svc = DecisionService::with_hooks(&db, &signing, vec![hook]);

    let row = svc
        .create(company_id, "R599 dismiss", "Dismiss body")
        .await
        .expect("create");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let _ = svc
        .dismiss(row.id, "test reason", "test-user")
        .await
        .expect("dismiss");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let events = in_mem.snapshot();
    assert!(
        events
            .iter()
            .any(|e| matches!(e.kind, ActivityKind::DecisionDismissed) && e.subject_id == row.id),
        "expected DecisionDismissed activity, got: {events:?}"
    );

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r599_cancel_emits_decision_cancelled() {
    let (db, pool) = setup_db().await;
    let (state, in_mem) = test_state_with_recording(db.clone());
    let (company_id, agent_id, issue_id, _run_id) =
        insert_company_agent_issue_run(&db, &pool).await;

    let signing = test_signing();
    let hook: Arc<dyn pc_decisions::DecisionHook> =
        Arc::new(DecisionActivityHook::new(Arc::new(state.clone())));
    let svc = DecisionService::with_hooks(&db, &signing, vec![hook]);

    let row = svc
        .create(company_id, "R599 cancel", "Cancel body")
        .await
        .expect("create");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let _ = svc.cancel(row.id).await.expect("cancel");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let events = in_mem.snapshot();
    assert!(
        events
            .iter()
            .any(|e| matches!(e.kind, ActivityKind::DecisionCancelled) && e.subject_id == row.id),
        "expected DecisionCancelled activity, got: {events:?}"
    );

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r599_hook_trait_is_object_safe() {
    let (db, _pool) = setup_db().await;
    let (state, _) = test_state_with_recording(db.clone());
    let hook: Arc<dyn pc_decisions::DecisionHook> =
        Arc::new(DecisionActivityHook::new(Arc::new(state)));
    let _ = hook;
}

#[tokio::test(flavor = "current_thread")]
async fn r599_activity_event_payload_carries_metadata() {
    let (db, pool) = setup_db().await;
    let (state, in_mem) = test_state_with_recording(db.clone());
    let (company_id, agent_id, issue_id, _run_id) =
        insert_company_agent_issue_run(&db, &pool).await;

    let signing = test_signing();
    let hook: Arc<dyn pc_decisions::DecisionHook> =
        Arc::new(DecisionActivityHook::new(Arc::new(state.clone())));
    let svc = DecisionService::with_hooks(&db, &signing, vec![hook]);

    let row = svc
        .create(company_id, "R599 metadata", "Metadata body")
        .await
        .expect("create");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let events = in_mem.snapshot();
    let created = events
        .iter()
        .find(|e| matches!(e.kind, ActivityKind::DecisionProposed) && e.subject_id == row.id)
        .expect("DecisionProposed event");
    assert_eq!(created.payload["title"], "R599 metadata");
    assert_eq!(created.payload["decision_id"], row.id.to_string());

    cleanup(&pool, company_id).await;
}
