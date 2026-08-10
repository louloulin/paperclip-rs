//! R603 v3: PipelineActivityHook 端到端 contract 测试（pipeline + stage + transition）。

use std::sync::Arc;

use pc_activity::{ActivityKind, ActivityLog, InMemoryActivityLog, SharedActivitySink};
use pc_adapter_api::AdapterRegistry;
use pc_core::ActorRegistry;
use pc_heartbeat::spawn_heartbeat_supervisor;
use pc_http::{
    hooks::PipelineActivityHook,
    state::{ConfigSnapshot, RuntimeHandles},
    AppState,
};
use pc_pipelines::{
    CreatePipelineInput, CreateStageMinimalInput, CreateTransitionInput, PipelineHook,
    PipelineService, StageKind, UpdatePipelinePatch, UpdateStagePatch,
};
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

async fn insert_stage(
    pool: &PgPool,
    pipeline_id: Uuid,
    key: &str,
    name: &str,
    kind: &str,
    position: i32,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO pipeline_stages (id, pipeline_id, key, name, kind, position, config, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, '{}'::jsonb, now(), now())",
    )
    .bind(id)
    .bind(pipeline_id)
    .bind(key)
    .bind(name)
    .bind(kind)
    .bind(position)
    .execute(pool)
    .await
    .expect("insert stage");
    id
}

async fn insert_company(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(id)
    .bind(format!("R603-{id}"))
    .bind(format!("A6{}", &id.simple().to_string()[..5]))
    .execute(pool)
    .await
    .expect("insert company");
    id
}

async fn cleanup(pool: &PgPool, company_id: Uuid) {
    let _ = sqlx::query(
        "DELETE FROM pipeline_transitions WHERE pipeline_id IN          (SELECT id FROM pipelines WHERE company_id = $1)",
    )
    .bind(company_id)
    .execute(pool)
    .await;
    let _ = sqlx::query(
        "DELETE FROM pipeline_stages WHERE pipeline_id IN          (SELECT id FROM pipelines WHERE company_id = $1)",
    )
    .bind(company_id)
    .execute(pool)
    .await;
    let _ = sqlx::query("DELETE FROM pipelines WHERE company_id = $1")
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
async fn r603_create_pipeline_emits_activity_and_live_event() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let (state, in_mem) = test_state_with_recording(db.clone());
    let realtime = state.realtime.clone();
    let state_arc = Arc::new(state);
    let hook = PipelineActivityHook::new(state_arc.clone());
    let hook: Arc<dyn PipelineHook> = Arc::new(hook);
    let svc = PipelineService::with_hooks(&db, vec![hook]);

    let company_id = insert_company(&pool).await;
    let mut rx = realtime.subscribe();

    let input = CreatePipelineInput {
        key: "r603-contract".into(),
        name: "Pipeline Activity Test".into(),
        description: None,
    };
    let row = svc.create(company_id, &input).await.expect("create");

    // 验证 activity log
    let snapshot = in_mem.snapshot();
    let created_events: Vec<_> = snapshot
        .iter()
        .filter(|e| matches!(e.kind, ActivityKind::PipelineCreated))
        .collect();
    assert!(
        !created_events.is_empty(),
        "expected PipelineCreated activity, got {snapshot:?}"
    );

    // 验证 realtime 收到 pipeline.created
    let mut got_created = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
            Ok(Ok(ev)) => {
                if ev.event == "pipeline.created" {
                    got_created = true;
                    break;
                }
            }
            _ => continue,
        }
    }
    assert!(got_created, "expected pipeline.created live event");

    cleanup(&pool, company_id).await;
    let _ = row; // avoid unused
}

#[tokio::test(flavor = "current_thread")]
async fn r603_update_pipeline_emits_activity() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let (state, in_mem) = test_state_with_recording(db.clone());
    let state_arc = Arc::new(state);
    let hook = PipelineActivityHook::new(state_arc.clone());
    let hook: Arc<dyn PipelineHook> = Arc::new(hook);
    let svc = PipelineService::with_hooks(&db, vec![hook]);

    let company_id = insert_company(&pool).await;
    let input = CreatePipelineInput {
        key: "r603-update".into(),
        name: "Original".into(),
        description: None,
    };
    let row = svc.create(company_id, &input).await.expect("create");

    let baseline_count = in_mem.snapshot().len();
    let patch = UpdatePipelinePatch {
        name: Some("Renamed".into()),
        description: None,
    };
    let _ = svc
        .update(company_id, row.id, &patch)
        .await
        .expect("update");

    // 至少多出一条 activity (PipelineUpdated)
    let after = in_mem.snapshot().len();
    assert!(
        after > baseline_count,
        "expected PipelineUpdated activity (was {baseline_count}, now {after})"
    );

    let has_updated = in_mem
        .snapshot()
        .iter()
        .any(|e| matches!(e.kind, ActivityKind::PipelineUpdated));
    assert!(has_updated, "expected PipelineUpdated activity");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603_archive_pipeline_emits_activity() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let (state, in_mem) = test_state_with_recording(db.clone());
    let state_arc = Arc::new(state);
    let hook = PipelineActivityHook::new(state_arc.clone());
    let hook: Arc<dyn PipelineHook> = Arc::new(hook);
    let svc = PipelineService::with_hooks(&db, vec![hook]);

    let company_id = insert_company(&pool).await;
    let input = CreatePipelineInput {
        key: "r603-archive".into(),
        name: "ArchiveMe".into(),
        description: None,
    };
    let row = svc.create(company_id, &input).await.expect("create");

    let _ = svc.archive(company_id, row.id).await.expect("archive");

    let has_archived = in_mem
        .snapshot()
        .iter()
        .any(|e| matches!(e.kind, ActivityKind::PipelineArchived));
    assert!(has_archived, "expected PipelineArchived activity");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603_delete_pipeline_emits_activity() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let (state, in_mem) = test_state_with_recording(db.clone());
    let state_arc = Arc::new(state);
    let hook = PipelineActivityHook::new(state_arc.clone());
    let hook: Arc<dyn PipelineHook> = Arc::new(hook);
    let svc = PipelineService::with_hooks(&db, vec![hook]);

    let company_id = insert_company(&pool).await;
    let input = CreatePipelineInput {
        key: "r603-delete".into(),
        name: "DeleteMe".into(),
        description: None,
    };
    let row = svc.create(company_id, &input).await.expect("create");

    let _ = svc.delete(company_id, row.id).await.expect("delete");

    let has_removed = in_mem
        .snapshot()
        .iter()
        .any(|e| matches!(e.kind, ActivityKind::PipelineRemoved));
    assert!(has_removed, "expected PipelineRemoved activity");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603_no_activity_without_hook() {
    // 反向：service 不带 hook 时不写 activity
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let (state, in_mem) = test_state_with_recording(db.clone());
    let _state_arc = Arc::new(state);
    let _ = _state_arc;

    let svc = PipelineService::new(&db); // no hooks
    let company_id = insert_company(&pool).await;
    let input = CreatePipelineInput {
        key: "r603-no-hook".into(),
        name: "NoHookTest".into(),
        description: None,
    };
    let _ = svc.create(company_id, &input).await.expect("create");

    let snapshot = in_mem.snapshot();
    let pipeline_events: Vec<_> = snapshot
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                ActivityKind::PipelineCreated
                    | ActivityKind::PipelineUpdated
                    | ActivityKind::PipelineArchived
                    | ActivityKind::PipelineRemoved
            )
        })
        .collect();
    assert!(
        pipeline_events.is_empty(),
        "no pipeline activity should be emitted without hook, got {snapshot:?}"
    );

    cleanup(&pool, company_id).await;
}

// ===========================================================================
// R603 v2: stage 子资源 contract 测试
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn r603v2_create_stage_emits_activity_and_live_event() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let (state, in_mem) = test_state_with_recording(db.clone());
    let realtime = state.realtime.clone();
    let state_arc = Arc::new(state);
    let hook = PipelineActivityHook::new(state_arc.clone());
    let hook: Arc<dyn PipelineHook> = Arc::new(hook);
    let svc = PipelineService::with_hooks(&db, vec![hook]);

    let company_id = insert_company(&pool).await;
    let pipe_input = CreatePipelineInput {
        key: "r603v2-create-stage".into(),
        name: "Pipeline".into(),
        description: None,
    };
    let pipe = svc.create(company_id, &pipe_input).await.expect("create pipe");

    let baseline_count = in_mem.snapshot().len();

    let mut rx = realtime.subscribe();

    let input = CreateStageMinimalInput {
        key: "todo".into(),
        name: "To Do".into(),
        kind: StageKind::Working,
        position: 0,
        config: serde_json::json!({}),
    };
    let stage = svc
        .create_stage(company_id, pipe.id, &input)
        .await
        .expect("create stage");

    // activity log: PipelineStageCreated
    let snapshot = in_mem.snapshot();
    let stage_created_events: Vec<_> = snapshot
        .iter()
        .filter(|e| matches!(e.kind, ActivityKind::PipelineStageCreated))
        .collect();
    assert!(
        !stage_created_events.is_empty(),
        "expected PipelineStageCreated activity, got {snapshot:?}"
    );
    assert!(
        snapshot.len() > baseline_count,
        "at least one new activity (PipelineStageCreated) should appear (was {baseline_count}, now {})",
        snapshot.len()
    );

    // realtime: pipeline.stage.created
    let mut got_created = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
            Ok(Ok(ev)) => {
                if ev.event == "pipeline.stage.created" {
                    got_created = true;
                    break;
                }
            }
            _ => continue,
        }
    }
    assert!(got_created, "expected pipeline.stage.created live event");

    cleanup(&pool, company_id).await;
    let _ = stage;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v2_update_stage_emits_activity() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let (state, in_mem) = test_state_with_recording(db.clone());
    let state_arc = Arc::new(state);
    let hook = PipelineActivityHook::new(state_arc.clone());
    let hook: Arc<dyn PipelineHook> = Arc::new(hook);
    let svc = PipelineService::with_hooks(&db, vec![hook]);

    let company_id = insert_company(&pool).await;
    let pipe_input = CreatePipelineInput {
        key: "r603v2-update-stage".into(),
        name: "Pipeline".into(),
        description: None,
    };
    let pipe = svc.create(company_id, &pipe_input).await.expect("create pipe");
    let stage_id = insert_stage(&pool, pipe.id, "tmp", "Tmp", "working", 0).await;

    let baseline_count = in_mem.snapshot().len();
    let patch = UpdateStagePatch {
        name: Some("Renamed".into()),
        kind: None,
        position: None,
        config: None,
    };
    let _ = svc
        .update_stage(company_id, stage_id, &patch)
        .await
        .expect("update stage");

    let snapshot = in_mem.snapshot();
    assert!(
        snapshot.len() > baseline_count,
        "PipelineStageUpdated activity expected (was {baseline_count}, now {})",
        snapshot.len()
    );
    let has_updated = snapshot
        .iter()
        .any(|e| matches!(e.kind, ActivityKind::PipelineStageUpdated));
    assert!(has_updated, "expected PipelineStageUpdated activity");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v2_delete_stage_emits_activity() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let (state, in_mem) = test_state_with_recording(db.clone());
    let state_arc = Arc::new(state);
    let hook = PipelineActivityHook::new(state_arc.clone());
    let hook: Arc<dyn PipelineHook> = Arc::new(hook);
    let svc = PipelineService::with_hooks(&db, vec![hook]);

    let company_id = insert_company(&pool).await;
    let pipe_input = CreatePipelineInput {
        key: "r603v2-delete-stage".into(),
        name: "Pipeline".into(),
        description: None,
    };
    let pipe = svc.create(company_id, &pipe_input).await.expect("create pipe");
    let stage_id = insert_stage(&pool, pipe.id, "tmp", "Tmp", "working", 0).await;

    let baseline_count = in_mem.snapshot().len();
    let deleted = svc
        .delete_stage(company_id, stage_id)
        .await
        .expect("delete stage");
    assert!(deleted);

    let snapshot = in_mem.snapshot();
    assert!(
        snapshot.len() > baseline_count,
        "PipelineStageRemoved activity expected"
    );
    let has_removed = snapshot
        .iter()
        .any(|e| matches!(e.kind, ActivityKind::PipelineStageRemoved));
    assert!(has_removed, "expected PipelineStageRemoved activity");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v2_no_stage_activity_without_hook() {
    // 反向：service 不带 hook 时不写 activity
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let (state, in_mem) = test_state_with_recording(db.clone());
    let _state_arc = Arc::new(state);

    let svc = PipelineService::new(&db); // no hooks
    let company_id = insert_company(&pool).await;
    let pipe_input = CreatePipelineInput {
        key: "r603v2-no-hook".into(),
        name: "Pipeline".into(),
        description: None,
    };
    let pipe = svc.create(company_id, &pipe_input).await.expect("create pipe");
    let stage_id = insert_stage(&pool, pipe.id, "tmp", "Tmp", "working", 0).await;
    let _ = svc
        .delete_stage(company_id, stage_id)
        .await
        .expect("delete");

    let snapshot = in_mem.snapshot();
    let stage_events: Vec<_> = snapshot
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                ActivityKind::PipelineStageCreated
                    | ActivityKind::PipelineStageUpdated
                    | ActivityKind::PipelineStageRemoved
            )
        })
        .collect();
    assert!(
        stage_events.is_empty(),
        "no stage activity should be emitted without hook, got {snapshot:?}"
    );

    cleanup(&pool, company_id).await;
}

// ===========================================================================
// R603 v3: transition 子资源 contract 测试
// ===========================================================================

#[tokio::test(flavor = "current_thread")]
async fn r603v3_create_transition_emits_activity_and_live_event() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let (state, in_mem) = test_state_with_recording(db.clone());
    let realtime = state.realtime.clone();
    let state_arc = Arc::new(state);
    let hook = PipelineActivityHook::new(state_arc.clone());
    let hook: Arc<dyn PipelineHook> = Arc::new(hook);
    let svc = PipelineService::with_hooks(&db, vec![hook]);

    let company_id = insert_company(&pool).await;
    let pipe_input = CreatePipelineInput {
        key: "r603v3-create-tr".into(),
        name: "Pipeline".into(),
        description: None,
    };
    let pipe = svc.create(company_id, &pipe_input).await.expect("create pipe");
    let a = insert_stage(&pool, pipe.id, "a", "A", "working", 0).await;
    let b = insert_stage(&pool, pipe.id, "b", "B", "review", 1).await;

    let mut rx = realtime.subscribe();
    let baseline_count = in_mem.snapshot().len();

    let t = svc
        .create_transition(
            company_id,
            pipe.id,
            &CreateTransitionInput {
                from_stage_id: a,
                to_stage_id: b,
                label: Some("A->B".into()),
            },
        )
        .await
        .expect("create transition");

    // activity log: PipelineTransitionCreated
    let snapshot = in_mem.snapshot();
    let has_created = snapshot
        .iter()
        .any(|e| matches!(e.kind, ActivityKind::PipelineTransitionCreated));
    assert!(has_created, "expected PipelineTransitionCreated activity, got {snapshot:?}");
    assert!(snapshot.len() > baseline_count);

    // realtime: pipeline.transition.created
    let mut got_created = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
            Ok(Ok(ev)) => {
                if ev.event == "pipeline.transition.created" {
                    got_created = true;
                    break;
                }
            }
            _ => continue,
        }
    }
    assert!(got_created, "expected pipeline.transition.created live event");

    cleanup(&pool, company_id).await;
    let _ = t;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v3_delete_transition_emits_activity() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let (state, in_mem) = test_state_with_recording(db.clone());
    let state_arc = Arc::new(state);
    let hook = PipelineActivityHook::new(state_arc.clone());
    let hook: Arc<dyn PipelineHook> = Arc::new(hook);
    let svc = PipelineService::with_hooks(&db, vec![hook]);

    let company_id = insert_company(&pool).await;
    let pipe_input = CreatePipelineInput {
        key: "r603v3-delete-tr".into(),
        name: "Pipeline".into(),
        description: None,
    };
    let pipe = svc.create(company_id, &pipe_input).await.expect("create pipe");
    let a = insert_stage(&pool, pipe.id, "a", "A", "working", 0).await;
    let b = insert_stage(&pool, pipe.id, "b", "B", "review", 1).await;
    let t = svc
        .create_transition(
            company_id,
            pipe.id,
            &CreateTransitionInput {
                from_stage_id: a,
                to_stage_id: b,
                label: None,
            },
        )
        .await
        .expect("create transition");

    let baseline_count = in_mem.snapshot().len();
    let deleted = svc
        .delete_transition(company_id, t.id)
        .await
        .expect("delete transition");
    assert!(deleted);

    let snapshot = in_mem.snapshot();
    assert!(
        snapshot.len() > baseline_count,
        "PipelineTransitionRemoved activity expected"
    );
    let has_removed = snapshot
        .iter()
        .any(|e| matches!(e.kind, ActivityKind::PipelineTransitionRemoved));
    assert!(has_removed, "expected PipelineTransitionRemoved activity");

    cleanup(&pool, company_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r603v3_no_transition_activity_without_hook() {
    let _guard = TEST_LOCK.lock().await;
    let (db, pool) = setup_db().await;
    let (state, in_mem) = test_state_with_recording(db.clone());
    let _state_arc = Arc::new(state);

    let svc = PipelineService::new(&db); // no hooks
    let company_id = insert_company(&pool).await;
    let pipe_input = CreatePipelineInput {
        key: "r603v3-no-hook".into(),
        name: "Pipeline".into(),
        description: None,
    };
    let pipe = svc.create(company_id, &pipe_input).await.expect("create pipe");
    let a = insert_stage(&pool, pipe.id, "a", "A", "working", 0).await;
    let b = insert_stage(&pool, pipe.id, "b", "B", "review", 1).await;
    let t = svc
        .create_transition(
            company_id,
            pipe.id,
            &CreateTransitionInput {
                from_stage_id: a,
                to_stage_id: b,
                label: None,
            },
        )
        .await
        .expect("create");
    let _ = svc
        .delete_transition(company_id, t.id)
        .await
        .expect("delete");

    let snapshot = in_mem.snapshot();
    let transition_events: Vec<_> = snapshot
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                ActivityKind::PipelineTransitionCreated | ActivityKind::PipelineTransitionRemoved
            )
        })
        .collect();
    assert!(
        transition_events.is_empty(),
        "no transition activity without hook, got {snapshot:?}"
    );

    cleanup(&pool, company_id).await;
}
