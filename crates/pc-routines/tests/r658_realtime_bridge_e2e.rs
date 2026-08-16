//! R658 -- realtime bridge 真实 E2E 验证

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use pc_realtime::hooks::RealtimeRoutineHook;
use pc_realtime::{RealtimeHandle, WsState};
use pc_repos::Db;
use pc_routines::{
    RoutineHook, RoutineHookEvent, RoutineSchedulerContext, RoutineService,
};
use sqlx::PgPool;
use uuid::Uuid;

const TEST_DATABASE_URL: &str =
    "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

static R658_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn try_setup_pool() -> Option<PgPool> {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(TEST_DATABASE_URL)
        .await
        .ok()
}

async fn setup(pool: &PgPool) -> (Uuid, Uuid, Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let unique = company_id.simple().to_string();
    let short: String = unique.chars().take(5).collect();

    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at)         VALUES ($1, $2, $$active$$, $3, now(), now())",
    )
    .bind(company_id)
    .bind(format!("R658-{unique}"))
    .bind(format!("R{short}"))
    .execute(pool)
    .await
    .expect("insert company");

    let agent_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status, adapter_config,         created_at, updated_at)         VALUES ($1, $2, $3, $$general$$, $$process$$, $$idle$$, $${}$$::jsonb, now(), now())",
    )
    .bind(agent_id)
    .bind(company_id)
    .bind(format!("Agent-{unique}"))
    .execute(pool)
    .await
    .expect("insert agent");

    let routine_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO routines (id, company_id, title, assignee_agent_id, status, priority,         concurrency_policy, catch_up_policy, created_at, updated_at)         VALUES ($1, $2, $$R658 routine$$, $3, $$active$$, $$medium$$,         $$always_enqueue$$, $$skip_missed$$, now(), now())",
    )
    .bind(routine_id)
    .bind(company_id)
    .bind(agent_id)
    .execute(pool)
    .await
    .expect("insert routine");

    let trigger_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO routine_triggers (id, company_id, routine_id, kind, label, enabled,         cron_expression, timezone, next_run_at, created_at, updated_at)         VALUES ($1, $2, $3, $$schedule$$, $$R658 cron$$, true, $$* * * * *$$, $$UTC$$,         now() - interval $$5 minutes$$, now(), now())",
    )
    .bind(trigger_id)
    .bind(company_id)
    .bind(routine_id)
    .execute(pool)
    .await
    .expect("insert trigger");

    (company_id, agent_id, routine_id, trigger_id)
}

async fn cleanup(
    pool: &PgPool,
    company_id: Uuid,
    agent_id: Uuid,
    routine_id: Uuid,
    trigger_id: Uuid,
) {
    let _ = sqlx::query("DELETE FROM routine_runs WHERE trigger_id = $1")
        .bind(trigger_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM routine_triggers WHERE id = $1")
        .bind(trigger_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM routine_revisions WHERE routine_id = $1")
        .bind(routine_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM routines WHERE id = $1")
        .bind(routine_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM agents WHERE id = $1")
        .bind(agent_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(company_id)
        .execute(pool)
        .await;
}

struct RecordingHook {
    pub events: Mutex<Vec<RoutineHookEvent>>,
}

impl RecordingHook {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait::async_trait]
impl RoutineHook for RecordingHook {
    async fn on_routine_event(&self, event: RoutineHookEvent) -> pc_errors::Result<()> {
        self.events.lock().unwrap().push(event);
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn r658_realtime_hook_publishes_run_skipped_via_ws_hub() {
    let pool = match try_setup_pool().await {
        Some(p) => p,
        None => {
            eprintln!("[skip] postgres unreachable at {}", TEST_DATABASE_URL);
            return;
        }
    };

    let _guard = R658_TEST_LOCK.lock().await;
    let db = Db::connect(TEST_DATABASE_URL, 2, 1)
        .await
        .expect("Db::connect");
    let (company_id, agent_id, routine_id, trigger_id) = setup(&pool).await;

    let realtime = RealtimeHandle::start(64);
    let _ws_state = Arc::new(WsState::new(realtime.clone(), "r658-test"));

    let mut subscriber_rx = realtime.subscribe();
    let pre_subs = realtime.subscriber_count();
    assert!(pre_subs >= 1, "expected >= 1 subscriber, got {pre_subs}");

    let recording = RecordingHook::new();
    let realtime_hook = RealtimeRoutineHook::new(realtime.clone()).into_arc();

    let mut env = HashMap::new();
    env.insert("PAPERCLIP_IN_WORKTREE".to_string(), "true".to_string());
    env.insert(
        "PAPERCLIP_INSTANCE_ID".to_string(),
        "instance-r658".to_string(),
    );

    let svc = RoutineService::new(db)
        .with_scheduler_context(RoutineSchedulerContext {
            env,
            current_instance_id: Some("instance-r658".to_string()),
        })
        .add_hook(realtime_hook)
        .add_hook(recording.clone());

    let outcome = svc
        .tick_scheduled_triggers(chrono::Utc::now(), 10)
        .await
        .expect("tick_scheduled_triggers");
    eprintln!(
        "R658 tick outcome: dispatched={}",
        outcome.len(),
    );

    let events = recording.events.lock().unwrap();
    let run_skipped_count = events
        .iter()
        .filter(|e| matches!(e, RoutineHookEvent::RunSkipped { .. }))
        .count();
    assert!(
        run_skipped_count >= 1,
        "expected at least 1 RunSkipped event, got {run_skipped_count}; events={:?}",
        events.iter().map(|e| match e {
            RoutineHookEvent::RunSkipped { reason, source, .. } =>
                format!("RunSkipped(reason={reason}, source={source})"),
            other => format!("{other:?}"),
        }).collect::<Vec<_>>()
    );
    drop(events);

    let mut received_routine_run_skipped = false;
    let mut tries = 0;
    while tries < 10 {
        match subscriber_rx.try_recv() {
            Ok(ev) => {
                if ev.event == "routine.run_skipped" && ev.resource == "routine_run" {
                    received_routine_run_skipped = true;
                    eprintln!(
                        "R658 realtime subscriber received: event={} resource={} company={:?} actor={:?}",
                        ev.event, ev.resource, ev.company_id, ev.actor
                    );
                    break;
                }
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                tries += 1;
                continue;
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
        }
        tries += 1;
    }
    assert!(
        received_routine_run_skipped,
        "expected at least one routine.run_skipped LiveEvent in subscriber"
    );

    let _ = _ws_state;
    assert!(
        realtime.subscriber_count() >= 1,
        "expected realtime hub still has subscribers"
    );

    cleanup(&pool, company_id, agent_id, routine_id, trigger_id).await;
    eprintln!(
        "R658 PASS: realtime bridge E2E ({} RunSkipped recorded, subscriber received)",
        run_skipped_count
    );
}
