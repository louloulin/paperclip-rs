//! R649: Routine scheduler worktree execution cutoff 真实 PG 端到端测试。
//!
//! 验证 `RoutineSchedulerContext` 注入后, `tick_scheduled_triggers` 会:
//! - non-worktree 运行时：直接 dispatch (existing behavior, R647)
//! - worktree 运行时 + DB flag disabled：skipped with worktree_execution_cutoff reason
//! - worktree 运行时 + DB flag armed + 当前 instance id 不匹配：skipped
//! - worktree 运行时 + DB flag armed + cutoff 在过去 + routine 新建：dispatch
//! - worktree 运行时 + DB flag armed + cutoff 在未来：pre_cutoff routine → skipped

use std::sync::Arc;

use chrono::Utc;
use pc_routines::{RoutineSchedulerContext, RoutineService};
use sqlx::PgPool;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

/// 全局锁：所有 R649 测试串行执行（共享 instance_settings singleton）。
static R649_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
    .bind(format!("R649-{unique}"))
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

async fn insert_schedule_trigger(pool: &PgPool, company_id: Uuid, routine_id: Uuid) -> Uuid {
    let trigger_id = Uuid::new_v4();
    let scheduled_at = Utc::now() - chrono::Duration::minutes(2);
    sqlx::query(
        r#"INSERT INTO routine_triggers
         (id, company_id, routine_id, kind, enabled, cron_expression, timezone, next_run_at)
         VALUES ($1, $2, $3, 'schedule', true, $4, $5, $6)"#,
    )
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
    let _ = sqlx::query("DELETE FROM routine_runs WHERE company_id = $1")
        .bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM routine_triggers WHERE company_id = $1")
        .bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM routines WHERE company_id = $1")
        .bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM agents WHERE company_id = $1")
        .bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id).execute(pool).await;
}

async fn reset_instance_settings(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO instance_settings (id, singleton_key, general, experimental, created_at, updated_at)          VALUES (gen_random_uuid(), 'default', '{}'::jsonb, '{}'::jsonb, now(), now())          ON CONFLICT (singleton_key) DO UPDATE SET experimental = EXCLUDED.experimental, updated_at = now()",
    )
    .execute(pool)
    .await
    .expect("reset instance settings");
}

async fn set_experimental(
    pool: &PgPool,
    enable_worktree_run_execution: bool,
    activated_at: Option<&str>,
    activation_instance_id: Option<&str>,
) {
    let mut experimental = serde_json::Map::new();
    experimental.insert(
        "enableWorktreeRunExecution".into(),
        serde_json::Value::Bool(enable_worktree_run_execution),
    );
    if let Some(at) = activated_at {
        experimental.insert(
            "worktreeRunExecutionActivatedAt".into(),
            serde_json::Value::String(at.to_string()),
        );
    }
    if let Some(id) = activation_instance_id {
        experimental.insert(
            "worktreeRunExecutionActivationInstanceId".into(),
            serde_json::Value::String(id.to_string()),
        );
    }
    sqlx::query(
        "INSERT INTO instance_settings (id, singleton_key, general, experimental, created_at, updated_at)          VALUES (gen_random_uuid(), 'default', '{}'::jsonb, $1::jsonb, now(), now())          ON CONFLICT (singleton_key) DO UPDATE SET experimental = EXCLUDED.experimental, updated_at = now()",
    )
    .bind(serde_json::Value::Object(experimental))
    .execute(pool)
    .await
    .expect("upsert instance settings");
}

#[tokio::test(flavor = "current_thread")]
async fn r649_non_worktree_runtime_dispatches_normally() {
    let pool = match try_setup_pool().await { Some(p) => p, None => { eprintln!("[skip] postgres unreachable"); return; } };
    let _r649_guard = R649_TEST_LOCK.lock().await;
    reset_instance_settings(&pool).await;
    let db = pc_repos::Db::connect(TEST_DATABASE_URL, 2, 1).await.expect("Db");
    let (company_id, agent_id) = setup_company_with_agent(&pool).await;
    let routine_id = make_routine(&pool, company_id, agent_id).await;
    let _trigger_id = insert_schedule_trigger(&pool, company_id, routine_id).await;

    let svc = RoutineService::new(db).with_scheduler_context(RoutineSchedulerContext {
        env: std::collections::HashMap::new(),
        current_instance_id: None,
    });
    let dispatched = svc.tick_scheduled_triggers(Utc::now(), 10).await.expect("tick");
    assert!(
        !dispatched.is_empty(),
        "non-worktree runtime should still dispatch"
    );

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r649_worktree_runtime_with_disabled_flag_is_skipped() {
    let pool = match try_setup_pool().await { Some(p) => p, None => { eprintln!("[skip] postgres unreachable"); return; } };
    let _r649_guard = R649_TEST_LOCK.lock().await;
    set_experimental(&pool, false, None, None).await;
    let db = pc_repos::Db::connect(TEST_DATABASE_URL, 2, 1).await.expect("Db");
    let (company_id, agent_id) = setup_company_with_agent(&pool).await;
    let routine_id = make_routine(&pool, company_id, agent_id).await;
    let trigger_id = insert_schedule_trigger(&pool, company_id, routine_id).await;

    let mut env = std::collections::HashMap::new();
    env.insert("PAPERCLIP_IN_WORKTREE".into(), "true".into());
    env.insert("PAPERCLIP_INSTANCE_ID".into(), "instance-x".into());
    let svc = RoutineService::new(db).with_scheduler_context(RoutineSchedulerContext {
        env,
        current_instance_id: Some("instance-x".into()),
    });

    let dispatched = svc.tick_scheduled_triggers(Utc::now(), 10).await.expect("tick");
    assert!(
        dispatched.is_empty(),
        "flag_disabled should suppress all dispatch"
    );

    let skipped = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM routine_runs WHERE trigger_id = $1 AND status = 'skipped'",
    )
    .bind(trigger_id)
    .fetch_all(&pool)
    .await
    .expect("query skipped");
    assert_eq!(
        skipped.len(),
        1,
        "expected exactly one skipped run for suppressed trigger"
    );
    let failure_reason: Option<String> = sqlx::query_scalar(
        "SELECT failure_reason FROM routine_runs WHERE id = $1",
    )
    .bind(skipped[0])
    .fetch_one(&pool)
    .await
    .expect("query failure_reason");
    assert_eq!(
        failure_reason.as_deref(),
        Some("worktree_execution_cutoff"),
        "reason must be worktree_execution_cutoff"
    );

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r649_worktree_runtime_with_instance_id_mismatch_is_skipped() {
    let pool = match try_setup_pool().await { Some(p) => p, None => { eprintln!("[skip] postgres unreachable"); return; } };
    let _r649_guard = R649_TEST_LOCK.lock().await;
    set_experimental(&pool, true, Some("2024-01-01T00:00:00Z"), Some("instance-other")).await;
    let db = pc_repos::Db::connect(TEST_DATABASE_URL, 2, 1).await.expect("Db");
    let (company_id, agent_id) = setup_company_with_agent(&pool).await;
    let routine_id = make_routine(&pool, company_id, agent_id).await;
    let trigger_id = insert_schedule_trigger(&pool, company_id, routine_id).await;

    let mut env = std::collections::HashMap::new();
    env.insert("PAPERCLIP_IN_WORKTREE".into(), "true".into());
    let svc = RoutineService::new(db).with_scheduler_context(RoutineSchedulerContext {
        env,
        current_instance_id: Some("instance-x".into()),
    });

    let dispatched = svc.tick_scheduled_triggers(Utc::now(), 10).await.expect("tick");
    assert!(dispatched.is_empty(), "instance mismatch must suppress");

    let skipped = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM routine_runs WHERE trigger_id = $1 AND status = 'skipped'",
    )
    .bind(trigger_id)
    .fetch_all(&pool)
    .await
    .expect("query skipped");
    assert_eq!(skipped.len(), 1);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r649_worktree_runtime_armed_with_post_cutoff_routine_dispatches() {
    let pool = match try_setup_pool().await { Some(p) => p, None => { eprintln!("[skip] postgres unreachable"); return; } };
    let _r649_guard = R649_TEST_LOCK.lock().await;
    set_experimental(&pool, true, Some("2024-01-01T00:00:00Z"), Some("instance-x")).await;
    let db = pc_repos::Db::connect(TEST_DATABASE_URL, 2, 1).await.expect("Db");
    let (company_id, agent_id) = setup_company_with_agent(&pool).await;
    let routine_id = make_routine(&pool, company_id, agent_id).await;
    let _trigger_id = insert_schedule_trigger(&pool, company_id, routine_id).await;

    let mut env = std::collections::HashMap::new();
    env.insert("PAPERCLIP_IN_WORKTREE".into(), "true".into());
    let svc = RoutineService::new(db).with_scheduler_context(RoutineSchedulerContext {
        env,
        current_instance_id: Some("instance-x".into()),
    });

    let dispatched = svc.tick_scheduled_triggers(Utc::now(), 10).await.expect("tick");
    assert!(
        !dispatched.is_empty(),
        "armed + post-cutoff routine should dispatch"
    );

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r649_worktree_runtime_armed_with_pre_cutoff_routine_is_skipped() {
    let pool = match try_setup_pool().await { Some(p) => p, None => { eprintln!("[skip] postgres unreachable"); return; } };
    let _r649_guard = R649_TEST_LOCK.lock().await;
    let future = (Utc::now() + chrono::Duration::days(365))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    set_experimental(&pool, true, Some(&future), Some("instance-x")).await;
    let db = pc_repos::Db::connect(TEST_DATABASE_URL, 2, 1).await.expect("Db");
    let (company_id, agent_id) = setup_company_with_agent(&pool).await;
    let routine_id = make_routine(&pool, company_id, agent_id).await;
    let trigger_id = insert_schedule_trigger(&pool, company_id, routine_id).await;

    let mut env = std::collections::HashMap::new();
    env.insert("PAPERCLIP_IN_WORKTREE".into(), "true".into());
    let svc = RoutineService::new(db).with_scheduler_context(RoutineSchedulerContext {
        env,
        current_instance_id: Some("instance-x".into()),
    });

    let dispatched = svc.tick_scheduled_triggers(Utc::now(), 10).await.expect("tick");
    assert!(
        dispatched.is_empty(),
        "pre-cutoff routine must be suppressed"
    );

    let skipped = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM routine_runs WHERE trigger_id = $1 AND status = 'skipped'",
    )
    .bind(trigger_id)
    .fetch_all(&pool)
    .await
    .expect("query skipped");
    assert_eq!(
        skipped.len(),
        1,
        "expected one skipped run for pre-cutoff routine"
    );

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r649_evaluate_pure_function_supports_post_cutoff() {
    use pc_routines::worktree_eligibility::*;
    use pc_repos::settings::WorktreeRunExecutionActivation;
    use chrono::TimeZone;

    let cutoff = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
    let activation = WorktreeRunExecutionActivation {
        armed: true,
        cutoff: Some(cutoff),
        activation_instance_id: Some("i-1".into()),
        reason: None,
    };
    let post = evaluate_automatic_dispatch_eligibility(
        true,
        &activation,
        Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap(),
    );
    assert!(post.eligible);
    let pre = evaluate_automatic_dispatch_eligibility(
        true,
        &activation,
        Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap(),
    );
    assert!(!pre.eligible);
}
