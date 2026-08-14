//! R650: routine scheduler activity gate 真实 PG 端到端测试。
//!
//! 验证 `tick_scheduled_triggers` 在 routine 配置 `require_external_activity` 时:
//! - 自上次 dispatch 后没有外部活动 → 跳过 (reason=no_external_activity)
//! - 自上次 dispatch 后有外部活动 → dispatch
//! - 自上次 dispatch 后只有 ignored activity (read_marked 等) → 跳过
//! - 自上次 dispatch 后只有 self-loop activity (routine-scheduler + own routineId) → 跳过

use chrono::Utc;
use pc_routines::RoutineService;
use sqlx::PgPool;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

static R650_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
    .bind(format!("R650-{unique}"))
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

async fn make_routine_with_gate(
    pool: &PgPool,
    company_id: Uuid,
    agent_id: Uuid,
    policy: &str,
) -> Uuid {
    let routine_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO routines (id, company_id, title, status, assignee_agent_id, activity_gate_policy, created_at, updated_at)          VALUES ($1, $2, $3, 'active', $4, $5, now(), now())",
    )
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
    let _ = sqlx::query("DELETE FROM activity_log WHERE company_id = $1")
        .bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM agents WHERE company_id = $1")
        .bind(company_id).execute(pool).await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id).execute(pool).await;
}

async fn insert_last_run(pool: &PgPool, company_id: Uuid, routine_id: Uuid) {
    let run_id = Uuid::new_v4();
    let past = Utc::now() - chrono::Duration::hours(1);
    sqlx::query(
        "INSERT INTO routine_runs (id, company_id, routine_id, source, status, triggered_at, completed_at, created_at, updated_at)          VALUES ($1, $2, $3, 'manual', 'succeeded', $4, $4, $4, $4)",
    )
    .bind(run_id)
    .bind(company_id)
    .bind(routine_id)
    .bind(past)
    .execute(pool)
    .await
    .expect("insert last run");
}

async fn insert_activity(
    pool: &PgPool,
    company_id: Uuid,
    actor_id: &str,
    action: &str,
    entity_type: &str,
    entity_id: &str,
    details: serde_json::Value,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO activity_log (id, company_id, actor_type, actor_id, action, entity_type, entity_id, details, created_at)          VALUES ($1, $2, 'user', $3, $4, $5, $6, $7, now())",
    )
    .bind(id)
    .bind(company_id)
    .bind(actor_id)
    .bind(action)
    .bind(entity_type)
    .bind(entity_id)
    .bind(details)
    .execute(pool)
    .await
    .expect("insert activity");
    id
}

#[tokio::test(flavor = "current_thread")]
async fn r650_first_run_with_require_external_activity_fires() {
    let pool = match try_setup_pool().await { Some(p) => p, None => { eprintln!("[skip] postgres unreachable"); return; } };
    let _guard = R650_TEST_LOCK.lock().await;
    let db = pc_repos::Db::connect(TEST_DATABASE_URL, 2, 1).await.expect("Db");
    let (company_id, agent_id) = setup_company_with_agent(&pool).await;
    let routine_id = make_routine_with_gate(&pool, company_id, agent_id, "require_external_activity").await;
    let trigger_id = insert_schedule_trigger(&pool, company_id, routine_id).await;

    // 没有 last dispatched run → fire=true
    let svc = RoutineService::new(db);
    let dispatched = svc.tick_scheduled_triggers(Utc::now(), 10).await.expect("tick");
    assert!(!dispatched.is_empty(), "first run should fire even with require_external_activity");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r650_no_activity_since_last_run_skipped() {
    let pool = match try_setup_pool().await { Some(p) => p, None => { eprintln!("[skip] postgres unreachable"); return; } };
    let _guard = R650_TEST_LOCK.lock().await;
    let db = pc_repos::Db::connect(TEST_DATABASE_URL, 2, 1).await.expect("Db");
    let (company_id, agent_id) = setup_company_with_agent(&pool).await;
    let routine_id = make_routine_with_gate(&pool, company_id, agent_id, "require_external_activity").await;
    let trigger_id = insert_schedule_trigger(&pool, company_id, routine_id).await;
    insert_last_run(&pool, company_id, routine_id).await;

    // 没有外部 activity → skipped
    let svc = RoutineService::new(db);
    let dispatched = svc.tick_scheduled_triggers(Utc::now(), 10).await.expect("tick");
    assert!(dispatched.is_empty(), "no external activity should suppress");

    let skipped = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM routine_runs WHERE trigger_id = $1 AND status = 'skipped' AND failure_reason = 'no_external_activity'",
    )
    .bind(trigger_id)
    .fetch_all(&pool)
    .await
    .expect("query skipped");
    assert_eq!(skipped.len(), 1);

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r650_external_activity_fires_routine() {
    let pool = match try_setup_pool().await { Some(p) => p, None => { eprintln!("[skip] postgres unreachable"); return; } };
    let _guard = R650_TEST_LOCK.lock().await;
    let db = pc_repos::Db::connect(TEST_DATABASE_URL, 2, 1).await.expect("Db");
    let (company_id, agent_id) = setup_company_with_agent(&pool).await;
    let routine_id = make_routine_with_gate(&pool, company_id, agent_id, "require_external_activity").await;
    let trigger_id = insert_schedule_trigger(&pool, company_id, routine_id).await;
    insert_last_run(&pool, company_id, routine_id).await;
    // 添加外部活动
    let issue_id = Uuid::new_v4().to_string();
    insert_activity(
        &pool,
        company_id,
        "user-1",
        "issue.comment_added",
        "issue",
        &issue_id,
        serde_json::json!({"commentId": "c-1"}),
    )
    .await;

    let svc = RoutineService::new(db);
    let dispatched = svc.tick_scheduled_triggers(Utc::now(), 10).await.expect("tick");
    assert!(
        !dispatched.is_empty(),
        "external activity should let the routine fire"
    );

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r650_only_ignored_activities_keeps_skipped() {
    let pool = match try_setup_pool().await { Some(p) => p, None => { eprintln!("[skip] postgres unreachable"); return; } };
    let _guard = R650_TEST_LOCK.lock().await;
    let db = pc_repos::Db::connect(TEST_DATABASE_URL, 2, 1).await.expect("Db");
    let (company_id, agent_id) = setup_company_with_agent(&pool).await;
    let routine_id = make_routine_with_gate(&pool, company_id, agent_id, "require_external_activity").await;
    let trigger_id = insert_schedule_trigger(&pool, company_id, routine_id).await;
    insert_last_run(&pool, company_id, routine_id).await;
    // 只添加 ignored activity (read_marked)
    insert_activity(
        &pool,
        company_id,
        "user-1",
        "issue.read_marked",
        "issue",
        &Uuid::new_v4().to_string(),
        serde_json::json!({}),
    )
    .await;

    let svc = RoutineService::new(db);
    let dispatched = svc.tick_scheduled_triggers(Utc::now(), 10).await.expect("tick");
    assert!(dispatched.is_empty(), "only ignored activities should not satisfy the gate");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r650_only_self_loop_keeps_skipped() {
    let pool = match try_setup_pool().await { Some(p) => p, None => { eprintln!("[skip] postgres unreachable"); return; } };
    let _guard = R650_TEST_LOCK.lock().await;
    let db = pc_repos::Db::connect(TEST_DATABASE_URL, 2, 1).await.expect("Db");
    let (company_id, agent_id) = setup_company_with_agent(&pool).await;
    let routine_id = make_routine_with_gate(&pool, company_id, agent_id, "require_external_activity").await;
    let trigger_id = insert_schedule_trigger(&pool, company_id, routine_id).await;
    insert_last_run(&pool, company_id, routine_id).await;
    // 添加 routine-scheduler 自身的活动（自循环）
    insert_activity(
        &pool,
        company_id,
        "routine-scheduler",
        "routine.run_skipped",
        "routine",
        &routine_id.to_string(),
        serde_json::json!({"routineId": routine_id.to_string()}),
    )
    .await;

    let svc = RoutineService::new(db);
    let dispatched = svc.tick_scheduled_triggers(Utc::now(), 10).await.expect("tick");
    assert!(dispatched.is_empty(), "self-loop activities should not satisfy the gate");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r650_always_policy_ignores_gate() {
    let pool = match try_setup_pool().await { Some(p) => p, None => { eprintln!("[skip] postgres unreachable"); return; } };
    let _guard = R650_TEST_LOCK.lock().await;
    let db = pc_repos::Db::connect(TEST_DATABASE_URL, 2, 1).await.expect("Db");
    let (company_id, agent_id) = setup_company_with_agent(&pool).await;
    let routine_id = make_routine_with_gate(&pool, company_id, agent_id, "always").await;
    let trigger_id = insert_schedule_trigger(&pool, company_id, routine_id).await;
    insert_last_run(&pool, company_id, routine_id).await;
    // 没有外部活动，但 always 策略 → 应该 dispatch

    let svc = RoutineService::new(db);
    let dispatched = svc.tick_scheduled_triggers(Utc::now(), 10).await.expect("tick");
    assert!(
        !dispatched.is_empty(),
        "always policy should ignore the activity gate"
    );

    cleanup(&pool, company_id).await;
}
