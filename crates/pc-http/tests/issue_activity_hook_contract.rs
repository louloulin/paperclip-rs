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

#[tokio::test(flavor = "current_thread")]
async fn r602_v2_update_status_emits_activity_and_live_event() {
    use pc_issues::ALL_ISSUE_STATUSES;

    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let (state, in_mem) = test_state_with_recording(db.clone());
    let realtime = state.realtime.clone();
    let state_arc = Arc::new(state);
    let hook = IssueActivityHook::new(state_arc.clone());
    let hook: Arc<dyn IssueHook> = Arc::new(hook);
    let svc = IssueService::with_hooks(&db, vec![hook]);

    let company_id = insert_company(&pool).await;

    // 先创建一个 issue（这一步会触发 on_created activity，记录 1 条）
    let input = CreateIssueMinimalInput {
        title: "status change test".into(),
        description: None,
        status: Some("todo".into()),
        priority: None,
        created_by_user_id: None,
    };
    let row = svc.create(company_id, &input).await.expect("create");

    let mut rx = realtime.subscribe();

    // 用合法 status 切换：todo → in_progress
    let target = ALL_ISSUE_STATUSES
        .iter()
        .find(|s| **s == "in_progress")
        .copied()
        .unwrap();
    let updated = svc
        .update_status(company_id, row.id, target)
        .await
        .expect("status");
    assert_eq!(updated.status, "in_progress");

    // 验证 activity log 出现至少一条 IssueUpdated（status_changed 路径）
    let snapshot = in_mem.snapshot();
    let updated_events: Vec<_> = snapshot
        .iter()
        .filter(|e| matches!(e.kind, ActivityKind::IssueUpdated))
        .collect();
    assert!(
        !updated_events.is_empty(),
        "expected at least one IssueUpdated activity after status change, got {snapshot:?}"
    );

    // 验证 realtime 收到 issue.status_changed
    let mut got_status_changed = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
            Ok(Ok(ev)) => {
                if ev.event == "issue.status_changed" {
                    got_status_changed = true;
                    break;
                }
            }
            _ => continue,
        }
    }
    assert!(
        got_status_changed,
        "expected to receive at least one issue.status_changed live event"
    );

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_v2_same_status_noop_does_not_trigger_activity() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let (state, in_mem) = test_state_with_recording(db.clone());
    let state_arc = Arc::new(state);
    let hook = IssueActivityHook::new(state_arc.clone());
    let hook: Arc<dyn IssueHook> = Arc::new(hook);
    let svc = IssueService::with_hooks(&db, vec![hook]);

    let company_id = insert_company(&pool).await;
    let input = CreateIssueMinimalInput {
        title: "noop".into(),
        description: None,
        status: Some("todo".into()),
        priority: None,
        created_by_user_id: None,
    };
    let row = svc.create(company_id, &input).await.expect("create");

    // 记录"调用 update_status 之前"的 activity 数；同状态 no-op 不应新增任何 activity
    let baseline_count = in_mem.snapshot().len();

    // 同状态更新 → no-op → 不触发 hook → 不增加 activity
    let _ = svc
        .update_status(company_id, row.id, "todo")
        .await
        .expect("noop");

    let after_count = in_mem.snapshot().len();
    assert_eq!(
        after_count, baseline_count,
        "same-status no-op should not emit any activity (was {baseline_count}, now {after_count})"
    );

    cleanup(&pool, company_id).await;
}
#[tokio::test(flavor = "current_thread")]
async fn r602_v3_assign_emits_activity_and_live_event() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let (state, in_mem) = test_state_with_recording(db.clone());
    let realtime = state.realtime.clone();
    let state_arc = Arc::new(state);
    let hook = IssueActivityHook::new(state_arc.clone());
    let hook: Arc<dyn IssueHook> = Arc::new(hook);
    let svc = IssueService::with_hooks(&db, vec![hook]);

    let company_id = insert_company(&pool).await;
    let agent_id = Uuid::new_v4();
    // 插入 agent 满足 FK 校验
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status,          adapter_config, permissions, created_at, updated_at)          VALUES ($1, $2, $3, 'general', 'process', 'idle', '{}'::jsonb, '{}'::jsonb,          now(), now())",
    )
    .bind(agent_id)
    .bind(company_id)
    .bind(format!("Agent-{agent_id}"))
    .execute(&pool)
    .await
    .expect("insert agent");

    let input = CreateIssueMinimalInput {
        title: "assign activity test".into(),
        description: None,
        status: Some("todo".into()),
        priority: None,
        created_by_user_id: None,
    };
    let row = svc.create(company_id, &input).await.expect("create");

    let mut rx = realtime.subscribe();

    let updated = svc
        .assign(company_id, row.id, pc_issues::AssignTarget::Agent(agent_id))
        .await
        .expect("assign");
    assert_eq!(updated.assignee_agent_id, Some(agent_id));

    // 验证 activity log 出现至少一条 IssueAssigned
    let snapshot = in_mem.snapshot();
    let assigned_events: Vec<_> = snapshot
        .iter()
        .filter(|e| matches!(e.kind, ActivityKind::IssueAssigned))
        .collect();
    assert!(
        !assigned_events.is_empty(),
        "expected IssueAssigned activity event, got {snapshot:?}"
    );

    // 验证 realtime 收到 issue.assigned
    let mut got_assigned = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
            Ok(Ok(ev)) => {
                if ev.event == "issue.assigned" {
                    got_assigned = true;
                    break;
                }
            }
            _ => continue,
        }
    }
    assert!(
        got_assigned,
        "expected to receive at least one issue.assigned live event"
    );

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_v3_unassign_emits_activity() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let (state, in_mem) = test_state_with_recording(db.clone());
    let state_arc = Arc::new(state);
    let hook = IssueActivityHook::new(state_arc.clone());
    let hook: Arc<dyn IssueHook> = Arc::new(hook);
    let svc = IssueService::with_hooks(&db, vec![hook]);

    let company_id = insert_company(&pool).await;
    let agent_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status,          adapter_config, permissions, created_at, updated_at)          VALUES ($1, $2, $3, 'general', 'process', 'idle', '{}'::jsonb, '{}'::jsonb,          now(), now())",
    )
    .bind(agent_id)
    .bind(company_id)
    .bind(format!("Agent-{agent_id}"))
    .execute(&pool)
    .await
    .expect("insert agent");

    let input = CreateIssueMinimalInput {
        title: "unassign test".into(),
        description: None,
        status: Some("todo".into()),
        priority: None,
        created_by_user_id: None,
    };
    let row = svc.create(company_id, &input).await.expect("create");

    // 先指派 + 记录 baseline
    let _ = svc
        .assign(company_id, row.id, pc_issues::AssignTarget::Agent(agent_id))
        .await
        .expect("assign");

    let baseline = in_mem.snapshot().len();

    // unassign
    let updated = svc
        .assign(company_id, row.id, pc_issues::AssignTarget::Unassign)
        .await
        .expect("unassign");
    assert_eq!(updated.assignee_agent_id, None);
    assert_eq!(updated.assignee_user_id, None);

    // 应出现至少多一条 IssueAssigned 事件（unassign 也是 assign 语义）
    let after = in_mem.snapshot().len();
    assert!(
        after > baseline,
        "unassign should produce an additional activity event (baseline={baseline}, after={after})"
    );

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_v3_same_assignment_noop_does_not_emit() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let (state, in_mem) = test_state_with_recording(db.clone());
    let state_arc = Arc::new(state);
    let hook = IssueActivityHook::new(state_arc.clone());
    let hook: Arc<dyn IssueHook> = Arc::new(hook);
    let svc = IssueService::with_hooks(&db, vec![hook]);

    let company_id = insert_company(&pool).await;
    let agent_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status,          adapter_config, permissions, created_at, updated_at)          VALUES ($1, $2, $3, 'general', 'process', 'idle', '{}'::jsonb, '{}'::jsonb,          now(), now())",
    )
    .bind(agent_id)
    .bind(company_id)
    .bind(format!("Agent-{agent_id}"))
    .execute(&pool)
    .await
    .expect("insert agent");

    let input = CreateIssueMinimalInput {
        title: "noop assign".into(),
        description: None,
        status: Some("todo".into()),
        priority: None,
        created_by_user_id: None,
    };
    let row = svc.create(company_id, &input).await.expect("create");

    let _ = svc
        .assign(company_id, row.id, pc_issues::AssignTarget::Agent(agent_id))
        .await
        .expect("assign");

    // 记录 baseline（assign 已发出 1 条 activity）
    let baseline = in_mem
        .snapshot()
        .iter()
        .filter(|e| matches!(e.kind, ActivityKind::IssueAssigned))
        .count();

    // 同样 assign → no-op → 不发新 activity
    let _ = svc
        .assign(company_id, row.id, pc_issues::AssignTarget::Agent(agent_id))
        .await
        .expect("noop");

    let after = in_mem
        .snapshot()
        .iter()
        .filter(|e| matches!(e.kind, ActivityKind::IssueAssigned))
        .count();
    assert_eq!(
        after, baseline,
        "no-op assign should not emit additional IssueAssigned events (baseline={baseline}, after={after})"
    );

    cleanup(&pool, company_id).await;
}
#[tokio::test(flavor = "current_thread")]
async fn r602_v4_create_comment_emits_activity_and_live_event() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let (state, in_mem) = test_state_with_recording(db.clone());
    let realtime = state.realtime.clone();
    let state_arc = Arc::new(state);
    let hook = IssueActivityHook::new(state_arc.clone());
    let hook: Arc<dyn IssueHook> = Arc::new(hook);
    let svc = IssueService::with_hooks(&db, vec![hook]);

    let company_id = insert_company(&pool).await;
    let agent_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status,          adapter_config, permissions, created_at, updated_at)          VALUES ($1, $2, $3, 'general', 'process', 'idle', '{}'::jsonb, '{}'::jsonb,          now(), now())",
    )
    .bind(agent_id)
    .bind(company_id)
    .bind(format!("Agent-{agent_id}"))
    .execute(&pool)
    .await
    .expect("insert agent");

    let input = CreateIssueMinimalInput {
        title: "comment contract test".into(),
        description: None,
        status: Some("todo".into()),
        priority: None,
        created_by_user_id: None,
    };
    let row = svc.create(company_id, &input).await.expect("create");

    let mut rx = realtime.subscribe();

    let comment = svc
        .create_comment(
            company_id,
            row.id,
            pc_issues::CommentAuthor::Agent(agent_id),
            "looks good",
        )
        .await
        .expect("comment");

    // 验证 activity log
    let snapshot = in_mem.snapshot();
    let commented_events: Vec<_> = snapshot
        .iter()
        .filter(|e| matches!(e.kind, ActivityKind::IssueCommented))
        .collect();
    assert!(
        !commented_events.is_empty(),
        "expected IssueCommented activity event, got {snapshot:?}"
    );

    // 验证 realtime 收到 issue.commented
    let mut got_commented = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
            Ok(Ok(ev)) => {
                if ev.event == "issue.commented" {
                    got_commented = true;
                    break;
                }
            }
            _ => continue,
        }
    }
    assert!(
        got_commented,
        "expected to receive at least one issue.commented live event"
    );

    // 验证 comment id 落进 payload
    let ev_payload = serde_json::to_value(&commented_events[0].payload).unwrap_or(serde_json::json!({}));
    assert_eq!(
        ev_payload["comment_id"].as_str(),
        Some(comment.id.to_string().as_str())
    );
    assert_eq!(ev_payload["author_kind"].as_str(), Some("agent"));

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r602_v4_user_author_comment_routes_correctly() {
    // 反向验证：User author 在 hook payload 里走 "user" 分支
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let (state, in_mem) = test_state_with_recording(db.clone());
    let state_arc = Arc::new(state);
    let hook = IssueActivityHook::new(state_arc.clone());
    let hook: Arc<dyn IssueHook> = Arc::new(hook);
    let svc = IssueService::with_hooks(&db, vec![hook]);

    let company_id = insert_company(&pool).await;

    let input = CreateIssueMinimalInput {
        title: "user-comment test".into(),
        description: None,
        status: Some("todo".into()),
        priority: None,
        created_by_user_id: None,
    };
    let row = svc.create(company_id, &input).await.expect("create");

    let _ = svc
        .create_comment(
            company_id,
            row.id,
            pc_issues::CommentAuthor::User("alice"),
            "human says hi",
        )
        .await
        .expect("comment");

    let snapshot = in_mem.snapshot();
    let ev = snapshot
        .iter()
        .find(|e| matches!(e.kind, ActivityKind::IssueCommented))
        .expect("commented event");
    let payload_json = serde_json::to_value(&ev.payload).unwrap_or(serde_json::json!({}));
    assert_eq!(payload_json["author_kind"].as_str(), Some("user"));
    assert_eq!(payload_json["author_id"].as_str(), Some("alice"));

    cleanup(&pool, company_id).await;
}
