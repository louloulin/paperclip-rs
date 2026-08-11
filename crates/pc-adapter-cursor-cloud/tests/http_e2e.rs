//! Real HTTP server 集成测试 —— `ReqwestCursorCloudClient` 端到端验证。
//!
//! 启动本地 axum HTTP server 模拟 Cursor Cloud REST API：
//! - `POST /agents` → 创建 agent
//! - `GET /agents/{id}` → 获取 agent (resume)
//! - `POST /agents/{id}/runs` → 发送 prompt
//! - `GET /runs/{id}` → 获取 run 状态 (poll)
//! - `GET /runs/{id}/messages` → SSE stream
//!
//! 关键：`http_e2e` 用真实 TCP socket + reqwest client，而非 in-memory mock。
//! 这是 R617 的核心验证 —— Cursor Cloud adapter 从"mockable only"升级到"生产可用"。

#![allow(dead_code)]

use std::net::SocketAddr;
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

use pc_adapter_cursor_cloud::cloud_client::{
    AgentOptions, CloudAgent, CloudError, CloudRun, CloudRunStatus, CursorCloudClient,
    RunFetchOptions, SdkTransportMessage,
};
use pc_adapter_cursor_cloud::http_client::ReqwestCursorCloudClient;
use pc_adapter_cursor_cloud::session_codec::RuntimeEnvType;

/// Mock server state — pre-seed responses.
#[derive(Default)]
struct MockState {
    agents: Mutex<std::collections::HashMap<String, CloudAgent>>,
    runs: Mutex<std::collections::HashMap<String, CloudRun>>,
    run_states: Mutex<std::collections::HashMap<String, CloudRunStatus>>,
}

async fn spawn_mock_server() -> (SocketAddr, std::sync::Arc<MockState>) {
    let state = std::sync::Arc::new(MockState::default());

    async fn create_agent(
        State(state): State<std::sync::Arc<MockState>>,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<CloudAgent>) {
        let id = format!("cu-{}", uuid::Uuid::new_v4());
        let agent = CloudAgent {
            agent_id: id.clone(),
            env_type: RuntimeEnvType::Cloud,
            env_name: body
                .get("envName")
                .and_then(|v| v.as_str())
                .map(String::from),
            repos: vec![],
        };
        state.agents.lock().await.insert(id.clone(), agent.clone());
        (StatusCode::OK, Json(agent))
    }

    async fn get_agent(
        State(state): State<std::sync::Arc<MockState>>,
        Path(agent_id): Path<String>,
    ) -> Result<Json<CloudAgent>, StatusCode> {
        let agents = state.agents.lock().await;
        agents
            .get(&agent_id)
            .cloned()
            .map(Json)
            .ok_or(StatusCode::NOT_FOUND)
    }

    async fn send_prompt(
        State(state): State<std::sync::Arc<MockState>>,
        Path(agent_id): Path<String>,
        Json(_body): Json<Value>,
    ) -> Result<(StatusCode, Json<CloudRun>), StatusCode> {
        let run_id = format!("r-{}", uuid::Uuid::new_v4());
        let run = CloudRun {
            id: run_id.clone(),
            agent_id: agent_id.clone(),
            status: CloudRunStatus::Running,
            model: None,
            result: None,
            duration_ms: None,
            git: None,
        };
        state.runs.lock().await.insert(run_id.clone(), run.clone());
        state
            .run_states
            .lock()
            .await
            .insert(run_id.clone(), CloudRunStatus::Running);
        Ok((StatusCode::OK, Json(run)))
    }

    async fn get_run(
        State(state): State<std::sync::Arc<MockState>>,
        Path(run_id): Path<String>,
    ) -> Result<Json<CloudRun>, StatusCode> {
        let runs = state.runs.lock().await;
        runs.get(&run_id)
            .cloned()
            .map(Json)
            .ok_or(StatusCode::NOT_FOUND)
    }

    async fn stream_messages(
        State(state): State<std::sync::Arc<MockState>>,
        Path(run_id): Path<String>,
    ) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
        // Mark run as finished after streaming
        {
            let mut runs = state.runs.lock().await;
            if let Some(run) = runs.get_mut(&run_id) {
                run.status = CloudRunStatus::Finished;
                run.result = Some("Hello world".to_owned());
            }
            let mut states = state.run_states.lock().await;
            states.insert(run_id.clone(), CloudRunStatus::Finished);
        }
        let events = vec![
            Event::default().data(r#"{"type":"assistant","text":"hello "}"#),
            Event::default().data(r#"{"type":"assistant","text":"world"}"#),
            Event::default().data(r#"{"type":"status","status":"finished"}"#),
        ];
        Sse::new(futures_util::stream::iter(
            events.into_iter().map(Ok::<_, Infallible>),
        ))
    }

    let app = Router::new()
        .route("/agents", post(create_agent))
        .route("/agents/:id", get(get_agent))
        .route("/agents/:id/runs", post(send_prompt))
        .route("/runs/:id", get(get_run))
        .route("/runs/:id/messages", get(stream_messages))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    // Small wait for server to be ready
    tokio::time::sleep(Duration::from_millis(50)).await;
    (addr, state)
}

fn test_agent_opts() -> AgentOptions {
    AgentOptions {
        api_key: "test-key".to_owned(),
        name: "TestAgent".to_owned(),
        model: Some("gpt-4".to_owned()),
        env_type: RuntimeEnvType::Cloud,
        env_name: None,
        repos: vec![],
        work_on_current_branch: false,
        auto_create_pr: false,
        skip_reviewer_request: false,
        env_vars: Default::default(),
    }
}

#[tokio::test]
async fn http_client_create_agent_returns_id() {
    let (addr, _state) = spawn_mock_server().await;
    let url = format!("http://{addr}");
    let client = ReqwestCursorCloudClient::new(url, "test-key");
    let agent = client
        .create_agent(&test_agent_opts())
        .await
        .expect("create");
    assert!(agent.agent_id.starts_with("cu-"));
    assert_eq!(agent.env_type, RuntimeEnvType::Cloud);
}

#[tokio::test]
async fn http_client_resume_agent_finds_existing() {
    let (addr, state) = spawn_mock_server().await;
    let url = format!("http://{addr}");
    let client = ReqwestCursorCloudClient::new(url, "test-key");

    let created = client.create_agent(&test_agent_opts()).await.unwrap();
    let resumed = client
        .resume_agent(&created.agent_id, &test_agent_opts())
        .await
        .unwrap();
    assert_eq!(created.agent_id, resumed.agent_id);

    let _ = state;
}

#[tokio::test]
async fn http_client_send_prompt_returns_running_run() {
    let (addr, _state) = spawn_mock_server().await;
    let url = format!("http://{addr}");
    let client = ReqwestCursorCloudClient::new(url, "test-key");

    let agent = client.create_agent(&test_agent_opts()).await.unwrap();
    let run = client
        .send_prompt(&agent, "hello", &Default::default())
        .await
        .expect("send");
    assert!(run.id.starts_with("r-"));
    assert_eq!(run.status, CloudRunStatus::Running);
}

#[tokio::test]
async fn http_client_get_run_returns_404_as_none() {
    let (addr, _state) = spawn_mock_server().await;
    let url = format!("http://{addr}");
    let client = ReqwestCursorCloudClient::new(url, "test-key");

    let opts = RunFetchOptions {
        runtime: "cloud".to_owned(),
        agent_id: "cu-x".to_owned(),
        api_key: "test-key".to_owned(),
    };
    let result = client
        .get_run("r-nonexistent", &opts)
        .await
        .expect("get_run");
    assert!(result.is_none());
}

#[tokio::test]
async fn http_client_stream_messages_collects_sse_events() {
    let (addr, _state) = spawn_mock_server().await;
    let url = format!("http://{addr}");
    let client = ReqwestCursorCloudClient::new(url, "test-key");

    let agent = client.create_agent(&test_agent_opts()).await.unwrap();
    let run = client
        .send_prompt(&agent, "x", &Default::default())
        .await
        .unwrap();

    let mut collected: Vec<SdkTransportMessage> = Vec::new();
    client
        .stream_messages(&run, &mut |m| collected.push(m))
        .await
        .expect("stream");

    // Should have 2 assistant + 1 status
    assert_eq!(collected.len(), 3);
    let text: String = collected
        .iter()
        .filter_map(|m| match m {
            SdkTransportMessage::Assistant { text } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "hello world");
}

#[tokio::test]
async fn http_client_404_returns_cloud_error_with_code() {
    let (addr, _state) = spawn_mock_server().await;
    let url = format!("http://{addr}");
    let client = ReqwestCursorCloudClient::new(url, "test-key");

    // Trigger an error via resume_agent on non-existent id
    let err = client
        .resume_agent("cu-nonexistent", &test_agent_opts())
        .await;
    match err {
        Err(CloudError {
            message,
            gateway_code,
            ..
        }) => {
            // The 404 status code should propagate
            assert!(
                gateway_code.is_some(),
                "expected gateway_code for 404 error, message={message}"
            );
            let code = gateway_code.unwrap();
            assert!(code.contains("404") || code == "404 Not Found");
        }
        Ok(_) => panic!("expected error for non-existent agent"),
    }
}

#[tokio::test]
async fn http_client_implements_cursor_cloud_client_trait() {
    let (addr, _state) = spawn_mock_server().await;
    let url = format!("http://{addr}");
    let client = ReqwestCursorCloudClient::new(url, "test-key");

    let dyn_client: std::sync::Arc<dyn CursorCloudClient> = std::sync::Arc::new(client);
    let agent = dyn_client.create_agent(&test_agent_opts()).await.unwrap();
    assert!(agent.agent_id.starts_with("cu-"));
}
