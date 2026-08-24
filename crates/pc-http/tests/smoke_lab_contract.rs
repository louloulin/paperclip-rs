//! Smoke Lab 路由契约测试。

use std::sync::Arc;

use axum::{body::Body, http::Request};
use pc_adapter_api::AdapterRegistry;
use pc_core::ActorRegistry;
use pc_heartbeat::spawn_heartbeat_supervisor;
use pc_http::{
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

fn test_state(db: Db) -> AppState {
    let actors = ActorRegistry::new();
    let realtime = RealtimeHandle::start(64);
    AppState::new(
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
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (u16, Value) {
    let _guard = TEST_LOCK.lock().await;
    let payload = body
        .as_ref()
        .map(|v| serde_json::to_vec(v).expect("serialize"))
        .unwrap_or_default();
    let mut builder = Request::builder()
        .method(method)
        .header("content-type", "application/json")
        .uri(path);
    if let Some(t) = token {
        builder = builder.header("authorization", format!("Bearer {t}"));
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(payload)).expect("request"))
        .await
        .expect("response");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let payload = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, payload)
}

async fn seed_company(db: &Db) -> Uuid {
    let prefix = Uuid::new_v4().simple().to_string();
    sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO companies (name, issue_prefix) VALUES ($1, $2) RETURNING id",
    )
    .bind(format!("smoke-lab-test-{}", Uuid::new_v4().simple()))
    .bind(&prefix)
    .fetch_one(db.pool())
    .await
    .expect("seed company")
}

#[tokio::test(flavor = "current_thread")]
async fn services_list_shape() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::smoke_lab::router().with_state(test_state(db.clone()));
    let company_id = seed_company(&db).await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/smoke-lab/services"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "services list: {body}");
    assert!(body["services"].is_array());
}

#[tokio::test(flavor = "current_thread")]
async fn install_fixtures_creates_full_set() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::smoke_lab::router().with_state(test_state(db.clone()));
    let company_id = seed_company(&db).await;
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/smoke-lab/install-fixtures"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 202, "install: {body}");
    assert_eq!(body["status"], "fixtures-installed");
    let installed = body["installed"].as_array().expect("installed array");
    // 安装了: project + agent + issue + service（company 已存在所以不计入）
    assert!(installed.iter().any(|v| v == "project"));
    assert!(installed.iter().any(|v| v == "agent"));
    assert!(installed.iter().any(|v| v == "issue"));
    assert!(installed.iter().any(|v| v == "service"));
    // 重复安装是幂等的，不重复创建
    let (status2, body2) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/smoke-lab/install-fixtures"),
        None,
        None,
    )
    .await;
    assert_eq!(status2, 202);
    let installed2 = body2["installed"].as_array().expect("installed2");
    assert_eq!(installed2.len(), 0, "second install is idempotent");
}

#[tokio::test(flavor = "current_thread")]
async fn runs_create_then_list() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::smoke_lab::router().with_state(test_state(db.clone()));
    let company_id = seed_company(&db).await;

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/smoke-lab/runs"),
        Some(json!({ "trigger": "manual", "suite": "ui" })),
        None,
    )
    .await;
    assert_eq!(status, 202, "create run: {body}");
    assert_eq!(body["status"], "running");
    assert_eq!(body["trigger"], "manual");
    let run_id = body["id"].as_str().expect("run id");

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/smoke-lab/runs/{run_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "get run: {body}");
    assert_eq!(body["run"]["id"], json!(run_id));
    assert!(body["steps"].is_array());

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/smoke-lab/runs/{run_id}/steps"),
        Some(json!({
            "path": "/",
            "scenarioStep": "load",
            "status": "passed",
            "durationMs": 100
        })),
        None,
    )
    .await;
    assert_eq!(status, 201, "add step: {body}");
    assert_eq!(body["status"], "passed");
}

#[tokio::test(flavor = "current_thread")]
async fn smoke_reset_clears_data() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::smoke_lab::router().with_state(test_state(db.clone()));
    let company_id = seed_company(&db).await;

    // 创建 run + step
    let (_, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/smoke-lab/runs"),
        Some(json!({ "trigger": "manual" })),
        None,
    )
    .await;
    let run_id = body["id"].as_str().expect("run id");
    let (_, _) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/smoke-lab/runs/{run_id}/steps"),
        Some(json!({ "path": "/", "status": "passed" })),
        None,
    )
    .await;

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/companies/{company_id}/smoke-lab/reset"),
        None,
        None,
    )
    .await;
    assert_eq!(status, 202, "reset: {body}");
    assert_eq!(body["status"], "reset-complete");
}
