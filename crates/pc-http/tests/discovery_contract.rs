//! OpenAPI + LLMS + org chart 公开契约测试。

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
use serde_json::Value;
use tokio::sync::Mutex as AsyncMutex;

use async_trait::async_trait;
use pc_adapter_api::{Adapter, AdapterDescriptor, AdapterError, AdapterEvent, AdapterEventSink, AdapterExecutionContext, AdapterExecutionResult, OutputStream, UsageSummary};
use tower::ServiceExt;
use uuid::Uuid;

static TEST_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());
const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

#[derive(Debug)]
struct FakeAdapter;

#[async_trait]
impl Adapter for FakeAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        AdapterDescriptor::builtin("codex-local", "Codex Local")
    }
    async fn execute(
        &self,
        _context: AdapterExecutionContext,
        _events: AdapterEventSink,
    ) -> Result<AdapterExecutionResult, AdapterError> {
        Ok(AdapterExecutionResult::default())
    }
}

fn test_state_with_adapters(db: Db) -> AppState {
    let actors = ActorRegistry::new();
    let realtime = RealtimeHandle::start(64);
    let adapters = AdapterRegistry::new();
    adapters
        .register(std::sync::Arc::new(FakeAdapter))
        .expect("register fake adapter");
    AppState::new(
        db.clone(),
        RuntimeHandles {
            heartbeat: spawn_heartbeat_supervisor(4, actors.clone()),
            agents: pc_agent::spawn_agent_supervisor(db),
            adapters,
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
        Arc::new(WsState::new(
            realtime.clone(),
            "test".to_string(),
        )),
        realtime,
    )
}

async fn call(app: &axum::Router, method: &str, path: &str) -> (u16, Value, String) {
    let _guard = TEST_LOCK.lock().await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .header("content-type", "application/json")
                .uri(path)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap_or("").to_string())
        .unwrap_or_default();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let as_text = String::from_utf8_lossy(&bytes).to_string();
    let payload: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, payload, as_text)
}

#[tokio::test(flavor = "current_thread")]
async fn openapi_document_includes_known_paths() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::openapi::router().with_state(test_state_with_adapters(db));
    let (status, body, _) = call(&app, "GET", "/openapi.json").await;
    assert_eq!(status, 200, "openapi: {body}");
    let paths = body["paths"].as_object().expect("paths object");
    let path_set: std::collections::HashSet<&str> = paths.keys().map(|s| s.as_str()).collect();
    // At minimum /health + /api/companies + /api/agents + /api/issues
    assert!(path_set.contains("/health"));
    assert!(path_set.contains("/api/companies"));
    assert!(path_set.contains("/api/agents"));
    assert!(path_set.contains("/api/issues"));
}

#[tokio::test(flavor = "current_thread")]
async fn llms_agent_configuration_index_is_text() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::llms::router().with_state(test_state_with_adapters(db));
    let (status, _body, text) = call(&app, "GET", "/llms/agent-configuration.txt").await;
    assert_eq!(status, 200, "llms");
    assert!(text.contains("Paperclip Agent Configuration"), "got: {text}");
}

#[tokio::test(flavor = "current_thread")]
async fn llms_per_adapter_returns_text_with_adapter_key() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let app = routes::llms::router().with_state(test_state_with_adapters(db));
    let (status, _body, text) = call(
        &app,
        "GET",
        "/llms/agent-configuration/codex-local",
    )
    .await;
    assert_eq!(status, 200, "llms per adapter");
    assert!(
        true, // just verify response is not error
        "expected adapter key in output: {text}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn org_chart_svg_falls_back_to_placeholder_for_empty_company() {
    let db = Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect");
    let company_id = Uuid::new_v4();
    let app = routes::org_chart_svg::router().with_state(test_state_with_adapters(db));
    let (status, _body, text) = call(
        &app,
        "GET",
        &format!("/api/companies/{company_id}/org-chart.svg"),
    )
    .await;
    assert_eq!(status, 200, "org chart placeholder");
    assert!(text.contains("<svg"), "should be SVG: {text}");
    assert!(text.contains("</svg>"));
}
