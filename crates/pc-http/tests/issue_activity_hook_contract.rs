//! R602: IssueActivityHook 端到端 contract 测试。
//!
//! 验证 IssueService 通过 IssueActivityHook 自动触发 ActivityLog + Realtime + PluginEventBus。
//!
//! 注：PluginEventBus 的 subscribe API 是注册式（无 receiver stream），
//! 因此本套测试聚焦 activity log + realtime 两路 fanout。
//! plugin_event_bus 集成由 company_activity_hook_contract 覆盖（同样 pattern）。

use std::sync::Arc;

use pc_activity::{ActivityKind, ActivityLog, InMemoryActivityLog, SharedActivitySink};
use pc_adapter_api::AdapterRegistry;
use pc_core::ActorRegistry;
use pc_heartbeat::spawn_heartbeat_supervisor;
use pc_http::{
    hooks::IssueActivityHook,
    state::{ConfigSnapshot, RuntimeHandles},
    AppState,
};
use pc_issues::{CreateIssueMinimalInput, IssueHook, IssueService};
use pc_realtime::{RealtimeHandle, WsState};
use pc_repos::Db;
use sqlx::PgPool;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
    .bind(format!("R602-{id}"))
    .bind(format!("A6{}", &id.simple().to_string()[..5]))
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query("DELETE FROM issues WHERE company_id = $1")
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

#[tokio::test(flavor = "current_thread")]
async fn r602_create_emits_activity_log_event() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let (state, in_mem) = test_state_with_recording(db.clone());
    let state_arc = Arc::new(state);
    let hook = IssueActivityHook::new(state_arc.clone());
    let hook: Arc<dyn IssueHook> = Arc::new(hook);
    let svc = IssueService::with_hooks(&db, vec![hook]);

    let company_id = insert_company(&pool).await;
    let input = CreateIssueMinimalInput {
        title: "Activity test".into(),
        description: Some("d".into()),
        status: Some("todo".into()),
        priority: Some("high".into()),
        created_by_user_id: Some("user-1".into()),
    };
    let row = svc.create(company_id, &input).await.expect("create");

    let snapshot = in_mem.snapshot();
    assert!(
        snapshot.iter().any(|e| matches!(e.kind, ActivityKind::IssueCreated)),
        "expected at least one IssueCreated activity event, got {snapshot:?}"
    );
    // payload 应该含 issue_id / title / status / priority
    let created_ev = snapshot
        .iter()
        .find(|e| matches!(e.kind, ActivityKind::IssueCreated))
        .expect("at least one");
    let payload_json = serde_json::to_value(&created_ev.payload).unwrap_or(serde_json::json!({}));
    assert_eq!(payload_json["issue_id"].as_str(), Some(row.id.to_string().as_str()));
    assert_eq!(payload_json["title"].as_str(), Some("Activity test"));
    assert_eq!(payload_json["status"].as_str(), Some("todo"));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_create_publishes_live_event() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let (state, _in_mem) = test_state_with_recording(db.clone());
    let realtime = state.realtime.clone();
    let state_arc = Arc::new(state);
    let hook = IssueActivityHook::new(state_arc.clone());
    let hook: Arc<dyn IssueHook> = Arc::new(hook);
    let svc = IssueService::with_hooks(&db, vec![hook]);

    let company_id = insert_company(&pool).await;
    let mut rx = realtime.subscribe();

    let input = CreateIssueMinimalInput {
        title: "Live event test".into(),
        description: None,
        status: None,
        priority: None,
        created_by_user_id: None,
    };
    svc.create(company_id, &input).await.expect("create");

    let mut got_issue_created = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
            Ok(Ok(ev)) => {
                if ev.event == "issue.created" {
                    got_issue_created = true;
                    break;
                }
            }
            _ => continue,
        }
    }
    assert!(
        got_issue_created,
        "expected to receive at least one issue.created live event"
    );

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_no_activity_without_hook() {
    // 反向验证：没有 hook 时不写 activity log
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let (state, in_mem) = test_state_with_recording(db.clone());
    let state_arc = Arc::new(state);
    let svc = IssueService::new(&db); // 无 hook
    let _ = state_arc; // 防 unused

    let company_id = insert_company(&pool).await;
    let input = CreateIssueMinimalInput {
        title: "no-hook test".into(),
        description: None,
        status: Some("todo".into()),
        priority: None,
        created_by_user_id: None,
    };
    let _ = svc.create(company_id, &input).await.expect("create");

    let snapshot = in_mem.snapshot();
    let issue_events: Vec<_> = snapshot
        .iter()
        .filter(|e| matches!(e.kind, ActivityKind::IssueCreated))
        .collect();
    assert!(
        issue_events.is_empty(),
        "no IssueCreated event should be emitted without hook, got {snapshot:?}"
    );

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_hook_count_propagation() {
    use pc_issues::RecordingIssueHook;
    let (db, _pool) = setup_db().await;
    let recorder1 = Arc::new(RecordingIssueHook::default());
    let recorder2 = Arc::new(RecordingIssueHook::default());
    let svc = IssueService::with_hooks(
        &db,
        vec![
            recorder1.clone() as Arc<dyn IssueHook>,
            recorder2.clone() as Arc<dyn IssueHook>,
        ],
    );
    assert_eq!(svc.hook_count(), 2);
}
