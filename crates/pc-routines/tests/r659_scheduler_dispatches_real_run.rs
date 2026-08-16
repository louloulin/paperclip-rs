//! R659 -- real PG scheduler tick: cron trigger due -> real routine_run
//!
//! Insert a cron trigger with next_run_at in the past; call tick_scheduled_triggers.
//! Verify a routine_run row was created in routine_runs with source in
//! {schedule, cron}.

use pc_routines::{RoutineSchedulerContext, RoutineService};
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

static R659_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn try_setup_pool() -> Option<PgPool> {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(TEST_DATABASE_URL)
        .await
        .ok()
}

async fn setup(pool: &PgPool) -> (Uuid, Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let unique = company_id.simple().to_string();
    let short = unique.chars().take(5).collect::<String>();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, $$active$$, $3, now(), now())",
    )
    .bind(company_id)
    .bind(format!("R659-{unique}"))
    .bind(format!("R{short}"))
    .execute(pool)
    .await
    .expect("insert company");

    let agent_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status, adapter_config, created_at, updated_at) \
         VALUES ($1, $2, $3, $$general$$, $$process$$, $$idle$$, $${}$$::jsonb, now(), now())",
    )
    .bind(agent_id)
    .bind(company_id)
    .bind(format!("Agent-{unique}"))
    .execute(pool)
    .await
    .expect("insert agent");

    let routine_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO routines (id, company_id, title, assignee_agent_id, status, priority, \
         concurrency_policy, catch_up_policy, created_at, updated_at) \
         VALUES ($1, $2, $$R659 routine$$, $3, $$active$$, $$medium$$, \
         $$always_enqueue$$, $$skip_missed$$, now(), now())",
    )
    .bind(routine_id)
    .bind(company_id)
    .bind(agent_id)
    .execute(pool)
    .await
    .expect("insert routine");

    (company_id, agent_id, routine_id)
}

async fn insert_trigger(pool: &PgPool, company_id: Uuid, routine_id: Uuid) -> Uuid {
    let trigger_id = Uuid::new_v4();
    // next_run_at = past -> tick fires immediately
    sqlx::query(
        "INSERT INTO routine_triggers (id, company_id, routine_id, kind, label, enabled, \
         cron_expression, timezone, next_run_at, created_at, updated_at) \
         VALUES ($1, $2, $3, $$schedule$$, $$R659 cron$$, true, $$* * * * *$$, $$UTC$$, \
         now() - interval $$5 minutes$$, now(), now())",
    )
    .bind(trigger_id)
    .bind(company_id)
    .bind(routine_id)
    .execute(pool)
    .await
    .expect("insert trigger");
    trigger_id
}

async fn cleanup(pool: &PgPool, company_id: Uuid, agent_id: Uuid, routine_id: Uuid, trigger_id: Uuid) {
    let _ = sqlx::query("DELETE FROM routine_runs WHERE trigger_id = $1").bind(trigger_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM routine_triggers WHERE id = $1").bind(trigger_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM routine_revisions WHERE routine_id = $1").bind(routine_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM routines WHERE id = $1").bind(routine_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM agents WHERE id = $1").bind(agent_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1").bind(company_id).execute(pool).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r659_scheduler_tick_dispatches_real_routine_run() {
    let pool = match try_setup_pool().await {
        Some(p) => p,
        None => {
            eprintln!("[skip] postgres unreachable");
            return;
        }
    };
    let _guard = R659_TEST_LOCK.lock().await;
    let db = pc_repos::Db::connect(TEST_DATABASE_URL, 2, 1).await.expect("Db");
    let (company_id, agent_id, routine_id) = setup(&pool).await;
    let trigger_id = insert_trigger(&pool, company_id, routine_id).await;

    let svc = RoutineService::new(db).with_scheduler_context(RoutineSchedulerContext {
        env: HashMap::new(),
        current_instance_id: None,
    });

    let dispatched = svc.tick_scheduled_triggers(chrono::Utc::now(), 10).await.expect("tick");
    eprintln!("R659 tick_scheduled_triggers returned {} runs", dispatched.len());
    assert!(!dispatched.is_empty(), "expected at least one dispatched run for past-due cron trigger");

    let run: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, source FROM routine_runs WHERE trigger_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(trigger_id)
    .fetch_optional(&pool)
    .await
    .expect("query run");
    let (run_id, source) = run.expect("expected routine_run row for past-due cron trigger");
    eprintln!("R659 routine_run: id={}, source={}", run_id, source);
    assert!(source == "schedule" || source == "cron", "run.source = {} (expected schedule or cron)", source);

    cleanup(&pool, company_id, agent_id, routine_id, trigger_id).await;
}
