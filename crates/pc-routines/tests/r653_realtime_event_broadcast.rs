//! R653: realtime event broadcast for routine.run_skipped.
//!
//! 验证 tick_scheduled_triggers 在以下 3 个抑制路径中通过 RoutineHook 发出
//! RunSkipped event，供 realtime hub 订阅端接收：
//! - project paused
//! - worktree execution cutoff
//! - activity gate
//!
//! 不依赖 pc-realtime crate — 直接通过 RoutineHook trait 验证，
//! 保持 pc-routines 与 realtime 解耦。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::Utc;
use pc_routines::{RoutineHook, RoutineHookEvent, RoutineSchedulerContext, RoutineService};
use sqlx::PgPool;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

/// 全局锁：所有 R653 测试串行执行。
static R653_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn try_setup_pool() -> Option<PgPool> {
    sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(TEST_DATABASE_URL).await.ok()
}

/// 简单 hook：记录所有 RunSkipped event。
struct RecordingRealtimeHook {
    pub skipped_events: Mutex<Vec<(Uuid, Uuid, Uuid, String, String)>>,
}

impl RecordingRealtimeHook {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            skipped_events: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl RoutineHook for RecordingRealtimeHook {
    async fn on_routine_event(&self, event: RoutineHookEvent) -> pc_errors::Result<()> {
        if let RoutineHookEvent::RunSkipped {
            run_id,
            routine_id,
            company_id,
            source: _,
            reason,
            details: _,
            trigger_id: _,
        } = event
        {
            self.skipped_events.lock().unwrap().push((run_id, routine_id, company_id, String::new(), reason));
        }
        Ok(())
    }
}

async fn setup_company_with_agent(pool: &PgPool) -> (Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let unique = company_id.simple().to_string();
    sqlx::query("INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) VALUES ($1, $2, 'active', $3, now(), now())")
        .bind(company_id)
        .bind(format!("R653-{unique}"))
        .bind(format!("R{}", &unique[..5]))
        .execute(pool)
        .await
        .expect("insert company");
    let agent_id = Uuid::new_v4();
    sqlx::query("INSERT INTO agents (id, company_id, name, role, adapter_type, status, adapter_config, created_at, updated_at) VALUES ($1, $2, $3, 'general', 'process', 'idle', '{}'::jsonb, now(), now())")
        .bind(agent_id)
        .bind(company_id)
        .bind(format!("Agent {unique}"))
        .execute(pool)
        .await
        .expect("insert agent");
    (company_id, agent_id)
}

async fn make_routine(pool: &PgPool, company_id: Uuid, agent_id: Uuid, policy: &str) -> Uuid {
    let routine_id = Uuid::new_v4();
    sqlx::query("INSERT INTO routines (id, company_id, title, status, assignee_agent_id, activity_gate_policy, created_at, updated_at) VALUES ($1, $2, $3, 'active', $4, $5, now(), now())")
        .bind(routine_id)
        .bind(company_id)
        .bind(format!("Routine {routine_id}"))
        .bind(agent_id)
        .bind(policy)
        .execute(pool)
        .await
        .expect("insert routine");
    routine_id
}

async fn insert_schedule_trigger(pool: &PgPool, company_id: Uuid, routine_id: Uuid) -> Uuid {
    let trigger_id = Uuid::new_v4();
    let scheduled_at = Utc::now() - chrono::Duration::minutes(2);
    sqlx::query(r#"INSERT INTO routine_triggers (id, company_id, routine_id, kind, enabled, cron_expression, timezone, next_run_at) VALUES ($1, $2, $3, 'schedule', true, $4, $5, $6)"#)
        .bind(trigger_id)
        .bind(company_id)
        .bind(routine_id)
        .bind("*/5 * * * *")
        .bind("UTC")
        .bind(scheduled_at)
        .execute(pool)
        .await
        .expect("insert schedule trigger");
    trigger_id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM activity_log WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM routine_runs WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM routine_triggers WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM routines WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM projects WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM agents WHERE company_id = $1").bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1").bind(company_id).execute(pool).await;
}

async fn reset_instance_settings(pool: &PgPool) {
    sqlx::query("INSERT INTO instance_settings (id, singleton_key, general, experimental, created_at, updated_at) VALUES (gen_random_uuid(), 'default', '{}'::jsonb, '{}'::jsonb, now(), now()) ON CONFLICT (singleton_key) DO UPDATE SET experimental = EXCLUDED.experimental, updated_at = now()")
        .execute(pool)
        .await
        .expect("reset instance settings");
}

async fn insert_last_run(pool: &PgPool, company_id: Uuid, routine_id: Uuid) {
    let run_id = Uuid::new_v4();
    let past = Utc::now() - chrono::Duration::hours(1);
    sqlx::query("INSERT INTO routine_runs (id, company_id, routine_id, source, status, triggered_at, completed_at, created_at, updated_at) VALUES ($1, $2, $3, 'manual', 'succeeded', $4, $4, $4, $4)")
        .bind(run_id)
        .bind(company_id)
        .bind(routine_id)
        .bind(past)
        .execute(pool)
        .await
        .expect("insert last run");
}

#[tokio::test(flavor = "current_thread")]
async fn r653_paused_project_broadcasts_skipped_event() {
    let pool = match try_setup_pool().await { Some(p) => p, None => { eprintln!("[skip] postgres unreachable"); return; } };
    let _guard = R653_TEST_LOCK.lock().await;
    reset_instance_settings(&pool).await;
    let db = pc_repos::Db::connect(TEST_DATABASE_URL, 2, 1).await.expect("Db");
    let (company_id, agent_id) = setup_company_with_agent(&pool).await;
    let routine_id = make_routine(&pool, company_id, agent_id, "coalesce_if_active").await;

    let project_id = Uuid::new_v4();
    sqlx::query("INSERT INTO projects (id, company_id, name, status, paused_at, created_at, updated_at) VALUES ($1, $2, $3, 'paused', now(), now(), now())")
        .bind(project_id)
        .bind(company_id)
        .bind(format!("Paused {project_id}"))
        .execute(&pool)
        .await
        .expect("insert paused project");
    sqlx::query(r"UPDATE routines SET project_id = $1 WHERE id = $2")
        .bind(project_id)
        .bind(routine_id)
        .execute(&pool)
        .await
        .expect("attach project");
    let _trigger_id = insert_schedule_trigger(&pool, company_id, routine_id).await;

    let hook = RecordingRealtimeHook::new();
    let mut svc = RoutineService::with_hooks(db.clone(), vec![hook.clone()]);
    svc = svc.with_scheduler_context(RoutineSchedulerContext { env: HashMap::new(), current_instance_id: None });
    let _ = svc.tick_scheduled_triggers(Utc::now(), 10).await.expect("tick");

    let events = hook.skipped_events.lock().unwrap();
    let paused: Vec<_> = events.iter().filter(|(_, _, cid, _, r)| r == "paused" && *cid == company_id).collect();
    assert_eq!(paused.len(), 1, "expected exactly one paused RunSkipped event");
    let (run_id, rid, cid, _, reason) = paused[0];
    assert_eq!(reason, "paused");
    assert_eq!(*rid, routine_id);
    assert_eq!(*cid, company_id);
    assert_ne!(*run_id, Uuid::nil());

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r653_worktree_cutoff_broadcasts_skipped_event() {
    let pool = match try_setup_pool().await { Some(p) => p, None => { eprintln!("[skip] postgres unreachable"); return; } };
    let _guard = R653_TEST_LOCK.lock().await;
    sqlx::query(r#"INSERT INTO instance_settings (id, singleton_key, general, experimental, created_at, updated_at) VALUES (gen_random_uuid(), 'default', '{}'::jsonb, '{"enableWorktreeRunExecution": true, "worktreeRunExecutionActivatedAt": "2024-01-01T00:00:00Z", "worktreeRunExecutionActivationInstanceId": "instance-other"}'::jsonb, now(), now()) ON CONFLICT (singleton_key) DO UPDATE SET experimental = EXCLUDED.experimental, updated_at = now()"#)
        .execute(&pool)
        .await
        .expect("set worktree flag");
    let db = pc_repos::Db::connect(TEST_DATABASE_URL, 2, 1).await.expect("Db");
    let (company_id, agent_id) = setup_company_with_agent(&pool).await;
    let routine_id = make_routine(&pool, company_id, agent_id, "coalesce_if_active").await;
    let _trigger_id = insert_schedule_trigger(&pool, company_id, routine_id).await;

    let hook = RecordingRealtimeHook::new();
    let mut env = HashMap::new();
    env.insert("PAPERCLIP_IN_WORKTREE".to_string(), "true".to_string());
    let mut svc = RoutineService::with_hooks(db.clone(), vec![hook.clone()]);
    svc = svc.with_scheduler_context(RoutineSchedulerContext { env, current_instance_id: Some("instance-x".to_string()) });
    let _ = svc.tick_scheduled_triggers(Utc::now(), 10).await.expect("tick");

    let events = hook.skipped_events.lock().unwrap();
    let worktree: Vec<_> = events.iter().filter(|(_, _, cid, _, r)| r == "worktree_execution_cutoff" && *cid == company_id).collect();
    assert_eq!(worktree.len(), 1, "expected one worktree RunSkipped event");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r653_activity_gate_broadcasts_skipped_event() {
    let pool = match try_setup_pool().await { Some(p) => p, None => { eprintln!("[skip] postgres unreachable"); return; } };
    let _guard = R653_TEST_LOCK.lock().await;
    reset_instance_settings(&pool).await;
    let db = pc_repos::Db::connect(TEST_DATABASE_URL, 2, 1).await.expect("Db");
    let (company_id, agent_id) = setup_company_with_agent(&pool).await;
    let routine_id = make_routine(&pool, company_id, agent_id, "require_external_activity").await;
    insert_last_run(&pool, company_id, routine_id).await;
    let _trigger_id = insert_schedule_trigger(&pool, company_id, routine_id).await;

    let hook = RecordingRealtimeHook::new();
    let mut svc = RoutineService::with_hooks(db.clone(), vec![hook.clone()]);
    svc = svc.with_scheduler_context(RoutineSchedulerContext { env: HashMap::new(), current_instance_id: None });
    let _ = svc.tick_scheduled_triggers(Utc::now(), 10).await.expect("tick");

    let events = hook.skipped_events.lock().unwrap();
    let gate: Vec<_> = events.iter().filter(|(_, _, cid, _, r)| r == "no_external_activity" && *cid == company_id).collect();
    assert_eq!(gate.len(), 1, "expected one activity_gate RunSkipped event");

    cleanup(&pool, company_id).await;
}
