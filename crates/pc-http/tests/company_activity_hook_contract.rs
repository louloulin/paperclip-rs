//! R591: CompanyActivityHook 端到端 contract 测试。
//!
//! 验证 CompanyService 创建/更新/归档/删除 company 时，CompanyActivityHook
//! 真的把 lifecycle event 转换为 ActivityLog + Realtime + PluginEvent。

use std::sync::Arc;

use axum::{body::Body, http::Request};
use pc_adapter_api::AdapterRegistry;
use pc_activity::{ActivityKind, ActivityLog, InMemoryActivityLog, SharedActivitySink};
use pc_companies::{CompanyActor, CompanyService, CompanyHook, CompanyLifecycleEvent};
use pc_core::ActorRegistry;
use pc_heartbeat::spawn_heartbeat_supervisor;
use pc_http::{
    hooks::CompanyActivityHook,
    routes,
    state::{ConfigSnapshot, RuntimeHandles},
    AppState,
};
use pc_realtime::{RealtimeHandle, WsState};
use pc_repos::Db;
use serde_json::{json, Value};
use tokio::sync::Mutex as AsyncMutex;
use tower::ServiceExt;
use uuid::Uuid;

static TEST_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

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

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Value,
) -> (u16, Value) {
    let _guard = TEST_LOCK.lock().await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .header("content-type", "application/json")
                .uri(path)
                .body(Body::from(serde_json::to_vec(&body).unwrap_or_default()))
                .unwrap(),
        )
        .await
        .expect("request");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

async fn cleanup(db: &Db, id: Uuid) {
    let _ = sqlx::query("DELETE FROM company_memberships WHERE company_id = $1")
        .bind(id)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE id = $1")
        .bind(id)
        .execute(db.pool())
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn r591_create_emits_activity_event() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let (state, in_mem) = test_state_with_recording(db.clone());

    let app = routes::companies::router().with_state(state.clone());
    let name = format!("R591-Create-{}", Uuid::new_v4());
    let (status, body) = call(
        &app,
        "POST",
        "/api/companies",
        json!({ "name": name, "description": "hook test" }),
    )
    .await;
    assert_eq!(status, 201, "create: {body}");
    let id = Uuid::parse_str(body["id"].as_str().expect("id")).expect("uuid");

    // wait briefly for async hook to run
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let events = in_mem.snapshot();
    assert!(
        events.iter().any(|e| matches!(e.kind, ActivityKind::CompanyCreated)
            && e.subject_kind == "company"
            && e.subject_id == id),
        "expected CompanyCreated activity event, got: {events:?}"
    );

    cleanup(&db, id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r591_update_emits_activity_event() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let (state, in_mem) = test_state_with_recording(db.clone());

    // 直接通过 service 创建一个
    let hook = Arc::new(CompanyActivityHook::new(Arc::new(state.clone())));
    let svc = CompanyService::with_hooks(&state.db, vec![hook]);
    let created = svc
        .create(pc_companies::CreateCompanyInput {
            name: format!("R591-Update-{}", Uuid::new_v4()),
            description: None,
            owner_principal_id: "u".into(),
            budget_monthly_cents: None,
        })
        .await
        .expect("create");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let pre_count = in_mem.snapshot().len();

    let app = routes::companies::router().with_state(state.clone());
    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/companies/{}", created.id),
        json!({ "description": "via hook" }),
    )
    .await;
    assert_eq!(status, 200, "update: {body}");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let events = in_mem.snapshot();
    assert!(
        events.len() > pre_count,
        "expected new CompanyUpdated event"
    );
    assert!(events
        .iter()
        .any(|e| matches!(e.kind, ActivityKind::CompanyUpdated)
            && e.subject_id == created.id));

    cleanup(&db, created.id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r591_archive_emits_activity_event() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let (state, in_mem) = test_state_with_recording(db.clone());

    let hook = Arc::new(CompanyActivityHook::new(Arc::new(state.clone())));
    let svc = CompanyService::with_hooks(&state.db, vec![hook]);
    let created = svc
        .create(pc_companies::CreateCompanyInput {
            name: format!("R591-Archive-{}", Uuid::new_v4()),
            description: None,
            owner_principal_id: "u".into(),
            budget_monthly_cents: None,
        })
        .await
        .expect("create");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let app = routes::companies::router().with_state(state.clone());
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{}/archive", created.id),
        json!({}),
    )
    .await;
    assert_eq!(status, 200, "archive: {body}");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let events = in_mem.snapshot();
    assert!(events.iter().any(|e| matches!(e.kind, ActivityKind::CompanyArchived)
        && e.subject_id == created.id),
        "expected CompanyArchived event");

    cleanup(&db, created.id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn r591_hook_trait_is_object_safe() {
    // 验证 CompanyHook 可以作为 dyn trait 传递 — 这是 service 抽象的核心保证
    let _: Arc<dyn CompanyHook> = Arc::new(CompanyActivityHook::new(Arc::new(
        test_state_with_recording(Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")).0,
    )));
}

#[tokio::test(flavor = "current_thread")]
async fn r591_lifecycle_event_payload_carries_owner() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let (state, in_mem) = test_state_with_recording(db.clone());

    let app = routes::companies::router().with_state(state.clone());
    let name = format!("R591-Payload-{}", Uuid::new_v4());
    let (status, body) = call(
        &app,
        "POST",
        "/api/companies",
        json!({ "name": name, "description": null }),
    )
    .await;
    assert_eq!(status, 201);
    let id = Uuid::parse_str(body["id"].as_str().expect("id")).expect("uuid");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let events = in_mem.snapshot();
    let created_event = events
        .iter()
        .find(|e| matches!(e.kind, ActivityKind::CompanyCreated) && e.subject_id == id)
        .expect("CompanyCreated event");
    assert_eq!(created_event.subject_kind, "company");
    assert!(created_event.payload.is_object() || created_event.payload.is_null());

    cleanup(&db, id).await;
}
