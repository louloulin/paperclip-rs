//! R647: RoutineService run lifecycle (dispatch + finalize) 真实 PG 端到端测试。

use std::sync::Arc;

use pc_repos::routine::RunRoutineRecord;
use pc_routines::{NoopRoutineHook, RecordingRoutineHook, RoutineService, RoutineHookEvent};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn try_setup_pool() -> Option<PgPool> {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(TEST_DATABASE_URL)
        .await
        .ok()
}

async fn setup_company_with_agent(pool: &PgPool) -> (Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let unique = company_id.simple().to_string();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at)          VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(company_id)
    .bind(format!("R647-{unique}"))
    .bind(format!("R{}", &unique[..5]))
    .execute(pool)
    .await
    .expect("insert company");
    let agent_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status, adapter_config, created_at, updated_at)          VALUES ($1, $2, $3, 'general', 'process', 'idle', '{}'::jsonb, now(), now())",
    )
    .bind(agent_id)
    .bind(company_id)
    .bind(format!("Agent {unique}"))
    .execute(pool)
    .await
    .expect("insert agent");
    (company_id, agent_id)
}

async fn make_routine(pool: &PgPool, company_id: Uuid, agent_id: Uuid) -> Uuid {
    let routine_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO routines (id, company_id, title, status, assignee_agent_id, created_at, updated_at)          VALUES ($1, $2, $3, 'active', $4, now(), now())",
    )
    .bind(routine_id)
    .bind(company_id)
    .bind(format!("Routine {routine_id}"))
    .bind(agent_id)
    .execute(pool)
    .await
    .expect("insert routine");
    routine_id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM routine_runs WHERE company_id = $1")
        .bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM routines WHERE company_id = $1")
        .bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM agents WHERE company_id = $1")
        .bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id).execute(pool).await;
}

fn make_record(agent_id: Uuid, source: &str, idem: String) -> RunRoutineRecord {
    RunRoutineRecord {
        trigger_id: None,
        source: source.to_string(),
        payload: Some(json!({"from": "test"})),
        variables: None,
        project_id: None,
        project_workspace_id: None,
        assignee_agent_id: Some(agent_id),
        idempotency_key: Some(idem),
        execution_workspace_id: None,
        execution_workspace_preference: None,
        execution_workspace_settings: None,
        actor_agent_id: Some(agent_id),
        actor_user_id: None,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn r647_dispatch_run_creates_run_and_triggers_hook() {
    let pool = match try_setup_pool().await { Some(p) => p, None => { eprintln!("[skip] postgres unreachable"); return; } };
    let db = pc_repos::Db::connect(TEST_DATABASE_URL, 2, 1).await.expect("Db");
    let (company_id, agent_id) = setup_company_with_agent(&pool).await;
    let routine_id = make_routine(&pool, company_id, agent_id).await;
    let hook = Arc::new(RecordingRoutineHook::default());
    let svc = RoutineService::with_hooks(db, vec![hook.clone()]);

    let record = make_record(agent_id, "manual", format!("r647-{routine_id}"));
    let dispatched = svc.dispatch_run(routine_id, &record).await.expect("dispatch");
    assert_ne!(dispatched.run.id, Uuid::nil());
    assert_eq!(dispatched.run.routine_id, routine_id);
    assert_eq!(dispatched.run.source, "manual");

    let events = hook.events_snapshot();
    assert!(events.iter().any(|e| matches!(e, RoutineHookEvent::RunDispatched { .. })));

    let fetched = svc.get_run(dispatched.run.id).await.expect("get_run").expect("run exists");
    assert_eq!(fetched.id, dispatched.run.id);

    let finalized = svc.finalize_run(dispatched.run.id, "succeeded", None).await.expect("finalize").expect("exists");
    assert_eq!(finalized.status, "succeeded");
    assert!(finalized.completed_at.is_some());

    let events2 = hook.events_snapshot();
    assert!(events2.iter().any(|e| matches!(e, RoutineHookEvent::RunFinalized { .. })));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r647_finalize_run_with_failure_reason_persists() {
    let pool = match try_setup_pool().await { Some(p) => p, None => return };
    let db = pc_repos::Db::connect(TEST_DATABASE_URL, 2, 1).await.expect("Db");
    let (company_id, agent_id) = setup_company_with_agent(&pool).await;
    let routine_id = make_routine(&pool, company_id, agent_id).await;
    let svc = RoutineService::new(db);

    let record = make_record(agent_id, "cron", format!("r647-fail-{routine_id}"));
    let dispatched = svc.dispatch_run(routine_id, &record).await.expect("dispatch");

    let finalized = svc.finalize_run(dispatched.run.id, "failed", Some("agent timeout")).await.expect("finalize").expect("exists");
    assert_eq!(finalized.status, "failed");
    assert_eq!(finalized.failure_reason.as_deref(), Some("agent timeout"));
    assert!(finalized.completed_at.is_some());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r647_finalize_run_nonexistent_returns_none() {
    let pool = match try_setup_pool().await { Some(p) => p, None => return };
    let db = pc_repos::Db::connect(TEST_DATABASE_URL, 2, 1).await.expect("Db");
    let svc = RoutineService::new(db);
    let fake_id = Uuid::new_v4();
    let result = svc.finalize_run(fake_id, "succeeded", None).await.expect("finalize");
    assert!(result.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn r647_dispatch_run_idempotent_with_same_key() {
    let pool = match try_setup_pool().await { Some(p) => p, None => return };
    let db = pc_repos::Db::connect(TEST_DATABASE_URL, 2, 1).await.expect("Db");
    let (company_id, agent_id) = setup_company_with_agent(&pool).await;
    let routine_id = make_routine(&pool, company_id, agent_id).await;
    let svc = RoutineService::new(db);
    let idem = format!("r647-idem-{routine_id}");
    let record = make_record(agent_id, "webhook", idem);
    let first = svc.dispatch_run(routine_id, &record).await.expect("first");
    let second = svc.dispatch_run(routine_id, &record).await.expect("second");
    assert_eq!(first.run.id, second.run.id, "idempotency: same key should yield same run");

    cleanup(&pool, company_id).await;
}
