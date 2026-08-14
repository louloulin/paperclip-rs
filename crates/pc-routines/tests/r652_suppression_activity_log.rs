//! R652: routine scheduler suppression activity log 真实 PG E2E 测试
//!
//! 验证 tick_scheduled_triggers 在 skip 时同步写 activity_log 条目。
//! action = routine.run_skipped
//! actor_type = system, actor_id = routine-scheduler
//! entity_type = routine_run, entity_id = <skipped_run_id>
//! details = { routineId, triggerId, source, status, reason, scheduledAt, claimedAt }
//!
//! 覆盖：
//! 1. project paused 抑制
//! 2. worktree execution cutoff (R649)
//! 3. activity gate 抑制 (R650)
//! 4. 同一 trigger 重复 skip → 独立条目

use std::collections::HashMap;
use chrono::Utc;
use pc_routines::{RoutineSchedulerContext, RoutineService};
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

/// 全局锁: 所有 R652 测试串行执行(共享 instance_settings + scheduler 状态)。
static R652_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn try_setup_pool() -> Option<PgPool> {
    sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(TEST_DATABASE_URL).await.ok()
}

async fn setup_company_with_agent(pool: &PgPool) -> (Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let unique = company_id.simple().to_string();
    sqlx::query("INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) VALUES ($1, $2, 'active', $3, now(), now())")
        .bind(company_id)
        .bind(format!("R652-{unique}"))
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

async fn make_routine(pool: &PgPool, company_id: Uuid, agent_id: Uuid, activity_gate_policy: &str) -> Uuid {
    let routine_id = Uuid::new_v4();
    sqlx::query("INSERT INTO routines (id, company_id, title, status, assignee_agent_id, activity_gate_policy, created_at, updated_at) VALUES ($1, $2, $3, 'active', $4, $5, now(), now())")
        .bind(routine_id)
        .bind(company_id)
        .bind(format!("Routine {routine_id}"))
        .bind(agent_id)
        .bind(activity_gate_policy)
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

async fn fetch_run_skipped_entries(pool: &PgPool, company_id: Uuid) -> Vec<(String, String, Option<Uuid>, Option<Value>)> {
    sqlx::query("SELECT actor_id, details->>'reason' AS reason, run_id, details FROM activity_log WHERE company_id = $1 AND action = 'routine.run_skipped' AND entity_type = 'routine_run' ORDER BY created_at ASC")
        .bind(company_id)
        .fetch_all(pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|r| (
                    r.get::<String, _>("actor_id"),
                    r.get::<String, _>("reason"),
                    r.get::<Option<Uuid>, _>("run_id"),
                    r.get::<Option<Value>, _>("details"),
                ))
                .collect()
        })
        .expect("query activity entries")
}

#[tokio::test(flavor = "current_thread")]
async fn r652_paused_project_writes_skipped_activity_log() {
    let pool = match try_setup_pool().await { Some(p) => p, None => { eprintln!("[skip] postgres unreachable"); return; } };
    let _guard = R652_TEST_LOCK.lock().await;
    reset_instance_settings(&pool).await;
    let db = pc_repos::Db::connect(TEST_DATABASE_URL, 2, 1).await.expect("Db");
    let (company_id, agent_id) = setup_company_with_agent(&pool).await;
    let routine_id = make_routine(&pool, company_id, agent_id, "coalesce_if_active").await;

    let project_id = Uuid::new_v4();
    let paused_at = Utc::now();
    sqlx::query("INSERT INTO projects (id, company_id, name, status, paused_at, created_at, updated_at) VALUES ($1, $2, $3, 'paused', $4, now(), now())")
        .bind(project_id)
        .bind(company_id)
        .bind(format!("Paused Project {project_id}"))
        .bind(paused_at)
        .execute(&pool)
        .await
        .expect("insert paused project");
    sqlx::query("UPDATE routines SET project_id = $1 WHERE id = $2")
        .bind(project_id)
        .bind(routine_id)
        .execute(&pool)
        .await
        .expect("attach project to routine");

    let _trigger_id = insert_schedule_trigger(&pool, company_id, routine_id).await;

    let svc = RoutineService::new(db).with_scheduler_context(RoutineSchedulerContext { env: HashMap::new(), current_instance_id: None });
    let dispatched = svc.tick_scheduled_triggers(Utc::now(), 10).await.expect("tick");
        // Scope to OUR trigger only - other test runs may have due triggers too.
    let my_dispatch_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM routine_runs WHERE trigger_id = $1 AND status != 'skipped'"
    )
    .bind(_trigger_id)
    .fetch_one(&pool)
    .await
    .expect("count dispatched");
    assert_eq!(my_dispatch_count, 0, "paused project should suppress dispatch for our trigger");

    let skipped_count: i64 = sqlx::query_scalar("SELECT COUNT(*)::bigint FROM routine_runs WHERE trigger_id = $1 AND status = 'skipped' AND failure_reason = 'paused'")
        .bind(_trigger_id)
        .fetch_one(&pool)
        .await
        .expect("count skipped runs");
    assert_eq!(skipped_count, 1, "expected exactly one paused skipped run");

    let entries = fetch_run_skipped_entries(&pool, company_id).await;
    assert_eq!(entries.len(), 1, "expected exactly one activity log entry");
    let (actor_id, reason, _run_id, details) = &entries[0];
    assert_eq!(actor_id, "routine-scheduler", "actor_id must be routine-scheduler");
    assert_eq!(reason, "paused", "reason must be paused");
    let details = details.as_ref().expect("details must be present");
    assert_eq!(details.get("source").and_then(Value::as_str), Some("schedule"));
    assert_eq!(details.get("status").and_then(Value::as_str), Some("skipped"));
    assert_eq!(details.get("reason").and_then(Value::as_str), Some("paused"));
    assert!(details.get("routineId").is_some(), "details must include routineId");
    assert!(details.get("triggerId").is_some(), "details must include triggerId");
    assert!(details.get("scheduledAt").is_some(), "details must include scheduledAt");
    assert!(details.get("claimedAt").is_some(), "details must include claimedAt");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r652_worktree_cutoff_writes_skipped_activity_log() {
    let pool = match try_setup_pool().await { Some(p) => p, None => { eprintln!("[skip] postgres unreachable"); return; } };
    let _guard = R652_TEST_LOCK.lock().await;
    sqlx::query(r#"INSERT INTO instance_settings (id, singleton_key, general, experimental, created_at, updated_at) VALUES (gen_random_uuid(), 'default', '{}'::jsonb, '{"enableWorktreeRunExecution": true, "worktreeRunExecutionActivatedAt": "2024-01-01T00:00:00Z", "worktreeRunExecutionActivationInstanceId": "instance-other"}'::jsonb, now(), now()) ON CONFLICT (singleton_key) DO UPDATE SET experimental = EXCLUDED.experimental, updated_at = now()"#)
        .execute(&pool)
        .await
        .expect("set worktree flag mismatched");

    let db = pc_repos::Db::connect(TEST_DATABASE_URL, 2, 1).await.expect("Db");
    let (company_id, agent_id) = setup_company_with_agent(&pool).await;
    let routine_id = make_routine(&pool, company_id, agent_id, "coalesce_if_active").await;
    let _trigger_id = insert_schedule_trigger(&pool, company_id, routine_id).await;

    let mut env = HashMap::new();
    env.insert("PAPERCLIP_IN_WORKTREE".to_string(), "true".to_string());
    let svc = RoutineService::new(db).with_scheduler_context(RoutineSchedulerContext { env, current_instance_id: Some("instance-x".to_string()) });
    let dispatched = svc.tick_scheduled_triggers(Utc::now(), 10).await.expect("tick");
        // Scope to OUR trigger only - other test runs may have due triggers too.
    let my_dispatch_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM routine_runs WHERE trigger_id = $1 AND status != 'skipped'"
    )
    .bind(_trigger_id)
    .fetch_one(&pool)
    .await
    .expect("count dispatched");
    assert_eq!(my_dispatch_count, 0, "worktree cutoff should suppress for our trigger");

    let entries = fetch_run_skipped_entries(&pool, company_id).await;
    assert_eq!(entries.len(), 1, "expected exactly one activity log entry");
    let (actor_id, reason, _run_id, details) = &entries[0];
    assert_eq!(actor_id, "routine-scheduler");
    assert_eq!(reason, "worktree_execution_cutoff");
    let details = details.as_ref().expect("details");
    assert_eq!(details.get("source").and_then(Value::as_str), Some("schedule"));
    assert_eq!(details.get("status").and_then(Value::as_str), Some("skipped"));
    assert_eq!(details.get("reason").and_then(Value::as_str), Some("worktree_execution_cutoff"));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r652_activity_gate_writes_skipped_activity_log() {
    let pool = match try_setup_pool().await { Some(p) => p, None => { eprintln!("[skip] postgres unreachable"); return; } };
    let _guard = R652_TEST_LOCK.lock().await;
    reset_instance_settings(&pool).await;
    let db = pc_repos::Db::connect(TEST_DATABASE_URL, 2, 1).await.expect("Db");
    let (company_id, agent_id) = setup_company_with_agent(&pool).await;
    let routine_id = make_routine(&pool, company_id, agent_id, "require_external_activity").await;
    insert_last_run(&pool, company_id, routine_id).await;
    let _trigger_id = insert_schedule_trigger(&pool, company_id, routine_id).await;

    let svc = RoutineService::new(db).with_scheduler_context(RoutineSchedulerContext { env: HashMap::new(), current_instance_id: None });
    let dispatched = svc.tick_scheduled_triggers(Utc::now(), 10).await.expect("tick");
        // Scope to OUR trigger only - other test runs may have due triggers too.
    let my_dispatch_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM routine_runs WHERE trigger_id = $1 AND status != 'skipped'"
    )
    .bind(_trigger_id)
    .fetch_one(&pool)
    .await
    .expect("count dispatched");
    assert_eq!(my_dispatch_count, 0, "no external activity should suppress for our trigger");

    let entries = fetch_run_skipped_entries(&pool, company_id).await;
    assert_eq!(entries.len(), 1);
    let (actor_id, reason, _run_id, details) = &entries[0];
    assert_eq!(actor_id, "routine-scheduler");
    assert_eq!(reason, "no_external_activity");
    let details = details.as_ref().expect("details");
    assert_eq!(details.get("source").and_then(Value::as_str), Some("schedule"));
    assert_eq!(details.get("status").and_then(Value::as_str), Some("skipped"));
    assert_eq!(details.get("reason").and_then(Value::as_str), Some("no_external_activity"));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r652_repeated_skipped_writes_independent_activity_entries() {
    let pool = match try_setup_pool().await { Some(p) => p, None => { eprintln!("[skip] postgres unreachable"); return; } };
    let _guard = R652_TEST_LOCK.lock().await;
    reset_instance_settings(&pool).await;
    let db = pc_repos::Db::connect(TEST_DATABASE_URL, 2, 1).await.expect("Db");
    let (company_id, agent_id) = setup_company_with_agent(&pool).await;
    let routine_id = make_routine(&pool, company_id, agent_id, "require_external_activity").await;
    insert_last_run(&pool, company_id, routine_id).await;
    let trigger_id = insert_schedule_trigger(&pool, company_id, routine_id).await;

    let svc = RoutineService::new(db).with_scheduler_context(RoutineSchedulerContext { env: HashMap::new(), current_instance_id: None });

    let _ = svc.tick_scheduled_triggers(Utc::now(), 10).await.expect("tick1");

    sqlx::query("UPDATE routine_triggers SET next_run_at = $1 WHERE id = $2")
        .bind(Utc::now() - chrono::Duration::minutes(1))
        .bind(trigger_id)
        .execute(&pool)
        .await
        .expect("re-arm trigger");

    let _ = svc.tick_scheduled_triggers(Utc::now(), 10).await.expect("tick2");

    let skipped_count: i64 = sqlx::query_scalar("SELECT COUNT(*)::bigint FROM routine_runs WHERE trigger_id = $1 AND status = 'skipped' AND failure_reason = 'no_external_activity'")
        .bind(trigger_id)
        .fetch_one(&pool)
        .await
        .expect("count skipped");
    assert_eq!(skipped_count, 2, "expected two skipped runs");

    let entries = fetch_run_skipped_entries(&pool, company_id).await;
    assert_eq!(entries.len(), 2, "expected two activity log entries");
    // Verify two distinct entity_ids (each skipped run has its own UUID in entity_id).
    let entity_ids: Vec<String> = sqlx::query_scalar(
        "SELECT entity_id FROM activity_log WHERE action = 'routine.run_skipped' AND entity_type = 'routine_run' AND details->>'triggerId' = $1 ORDER BY created_at ASC"
    )
    .bind(trigger_id.to_string())
    .fetch_all(&pool)
    .await
    .expect("query entity_ids");
    assert_eq!(entity_ids.len(), 2, "each skipped run must produce a distinct activity entry");
    let unique: std::collections::HashSet<_> = entity_ids.iter().collect();
    assert_eq!(unique.len(), 2, "entity_ids must be unique");

    cleanup(&pool, company_id).await;
}
