//! 真实 SSE + Dashboard HTTP server 集成测试 —— Hermes gateway adapter。
//!
//! 启动本地 axum HTTP server 模拟 Hermes gateway：
//! - `POST /v1/runs` —— 创建 run
//! - `GET /v1/runs/{id}` —— 查询状态
//! - `GET /v1/events` —— SSE 事件流（text/event-stream）
//!
//! 关键：`sse_e2e` 用真实 TCP socket + reqwest client，而非 in-memory mock。
//! 这是 R622 的核心验证 —— hermes-gateway adapter 真正具备 SSE/dashboard 集成。

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::sse::{Event, Sse},
    routing::{get, post},
    Json, Router,
};
use futures_util::stream::Stream;
use serde_json::{json, Value};
use std::convert::Infallible;
use tokio::sync::Mutex;

use pc_adapter_hermes_gateway::dashboard::{
    CreateRunRequest, DashboardClient, HermesRun, RunStatus,
};
use pc_adapter_hermes_gateway::sse_client::{HermesSseClient, InMemorySseSink, SseEvent};

#[derive(Default)]
struct MockState {
    runs: Mutex<std::collections::HashMap<String, HermesRun>>,
}

async fn spawn_mock_server() -> (SocketAddr, Arc<MockState>) {
    let state = Arc::new(MockState::default());

    async fn create_run(
        State(state): State<Arc<MockState>>,
        Json(req): Json<CreateRunRequest>,
    ) -> (StatusCode, Json<Value>) {
        let run_id = format!("r-{}", uuid::Uuid::new_v4());
        let run = HermesRun {
            run_id: run_id.clone(),
            status: RunStatus::Running,
            summary: None,
            error: None,
            duration_ms: None,
            model: None,
            raw: json!({"run_id": run_id, "status": "running"}),
        };
        state.runs.lock().await.insert(run_id.clone(), run);
        let _ = req; // suppress unused
        (
            StatusCode::OK,
            Json(json!({"run_id": run_id, "status": "running"})),
        )
    }

    async fn get_run(
        State(state): State<Arc<MockState>>,
        Path(run_id): Path<String>,
    ) -> Result<Json<Value>, StatusCode> {
        let runs = state.runs.lock().await;
        runs.get(&run_id)
            .map(|r| Json(r.raw.clone()))
            .ok_or(StatusCode::NOT_FOUND)
    }

    async fn stream_events() -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
        let events = vec![
            Event::default().data(r#"{"type":"agent_message","text":"hello ","delta":true}"#),
            Event::default().data(r#"{"type":"agent_message","text":"world","delta":true}"#),
            Event::default().data(r#"{"type":"status","status":"running","message":"computing"}"#),
            Event::default().data(r#"{"type":"task_complete","summary":"done"}"#),
        ];
        Sse::new(futures_util::stream::iter(
            events.into_iter().map(Ok::<_, Infallible>),
        ))
    }

    let app = Router::new()
        .route("/v1/runs", post(create_run))
        .route("/v1/runs/:id", get(get_run))
        .route("/v1/events", get(stream_events))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    (addr, state)
}

#[tokio::test]
async fn dashboard_create_run_returns_run_id() {
    let (addr, _state) = spawn_mock_server().await;
    let url = format!("http://{addr}");
    let client = DashboardClient::new(url, "test-key", None);

    let req = CreateRunRequest {
        prompt: "say hi".to_owned(),
        model: None,
        session_key: None,
        workspace: None,
        metadata: None,
    };
    let run = client.create_run(&req).await.expect("create");
    assert!(run.run_id.starts_with("r-"));
    assert_eq!(run.status, RunStatus::Running);
}

#[tokio::test]
async fn dashboard_get_run_returns_existing() {
    let (addr, _state) = spawn_mock_server().await;
    let url = format!("http://{addr}");
    let client = DashboardClient::new(url, "test-key", None);

    let created = client
        .create_run(&CreateRunRequest {
            prompt: "hi".into(),
            model: None,
            session_key: None,
            workspace: None,
            metadata: None,
        })
        .await
        .unwrap();
    let fetched = client.get_run(&created.run_id).await.expect("get_run");
    assert_eq!(fetched.run_id, created.run_id);
}

#[tokio::test]
async fn dashboard_get_run_returns_error_for_missing() {
    let (addr, _state) = spawn_mock_server().await;
    let url = format!("http://{addr}");
    let client = DashboardClient::new(url, "test-key", None);

    let result = client.get_run("r-nonexistent").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn sse_consume_collects_events_until_terminal() {
    let (addr, _state) = spawn_mock_server().await;
    let url = format!("http://{addr}");
    let client = HermesSseClient::new(url, "test-key", None);
    let sink = InMemorySseSink::new();

    let result = client
        .consume_until_terminal("/v1/events", &sink, 3)
        .await
        .expect("consume");

    // Expect: 2 agent_message + 1 status + 1 task_complete = 4 events
    assert_eq!(result.events.len(), 4);
    assert!(result.terminal.is_some());
    assert!(matches!(
        result.terminal.unwrap(),
        SseEvent::TaskComplete { .. }
    ));

    // Verify sink collected all events
    let snapshot = sink.snapshot();
    assert_eq!(snapshot.len(), 4);

    // First 2 are agent_message with delta=true
    assert!(matches!(
        &snapshot[0],
        SseEvent::AgentMessage { delta: true, .. }
    ));
    assert!(matches!(
        &snapshot[1],
        SseEvent::AgentMessage { delta: true, .. }
    ));
}

#[tokio::test]
async fn sse_extract_text_from_agent_messages() {
    let (addr, _state) = spawn_mock_server().await;
    let url = format!("http://{addr}");
    let client = HermesSseClient::new(url, "test-key", None);
    let sink = InMemorySseSink::new();

    let result = client
        .consume_until_terminal("/v1/events", &sink, 3)
        .await
        .unwrap();

    let text: String = result
        .events
        .iter()
        .filter_map(|e| e.extract_text())
        .collect::<Vec<_>>()
        .join("");
    assert!(text.contains("hello"));
    assert!(text.contains("world"));
}

#[tokio::test]
async fn sse_terminal_event_marked_correctly() {
    let (addr, _state) = spawn_mock_server().await;
    let url = format!("http://{addr}");
    let client = HermesSseClient::new(url, "test-key", None);

    let result = client
        .consume_until_terminal("/v1/events", &InMemorySseSink::new(), 3)
        .await
        .unwrap();

    // task_complete is terminal
    let terminal = result.terminal.unwrap();
    assert!(terminal.is_terminal());
}

#[tokio::test]
async fn sse_consume_to_closed_port_returns_error() {
    let client = HermesSseClient::new("http://127.0.0.1:1", "test-key", None);
    let result = client
        .consume_until_terminal("/v1/events", &InMemorySseSink::new(), 3)
        .await;
    assert!(result.is_err());
}
