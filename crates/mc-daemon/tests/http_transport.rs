//! `HttpTransport` 的真 socket 用例（M3-7 / LUM-1438）。
//!
//! `client_loop.rs` 用假传输证明**客户端逻辑**对；本文件用一条真 TCP 连接证明
//! **HTTP 腿**对：请求头（身份、能力、版本）、错误体解析、404 语义，以及
//! `DaemonClient` 在真传输上的端到端往返。
//!
//! 服务端是一个 axum 桩（`fallback` 吃掉所有路径）：它把每条请求的路径与头记下来，
//! 按路径回预先排好的响应。这样不需要起真 `mc-http` —— 那会让 `mc-daemon` 反向
//! 依赖服务端，依赖方向就错了。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use serde_json::{json, Value};

use mc_daemon::{
    ClaimOutcome, ClientConfig, ClientError, DaemonClient, DaemonTransport, HttpTransport,
    RegisterRuntime, TransportError, HEARTBEAT_PATH,
};

// ---------------------------------------------------------------------------
// 桩服务端
// ---------------------------------------------------------------------------

/// 桩看到的每条请求。
#[derive(Debug, Clone)]
struct Seen {
    path: String,
    headers: HeaderMap,
    body: Value,
}

/// 一种响应体：JSON，或原样文本（用来验证「错误体不是 JSON 时不猜」）。
#[derive(Debug, Clone)]
enum Reply {
    Json(Value),
    Text(String),
}

/// 桩的共享状态。
#[derive(Clone, Default)]
struct Stub {
    seen: Arc<Mutex<Vec<Seen>>>,
    replies: Arc<Mutex<BTreeMap<String, (u16, Reply)>>>,
}

impl Stub {
    fn reply(&self, path: &str, status: u16, body: Value) {
        self.store(path, status, Reply::Json(body));
    }

    fn reply_text(&self, path: &str, status: u16, text: &str) {
        self.store(path, status, Reply::Text(text.to_owned()));
    }

    fn store(&self, path: &str, status: u16, body: Reply) {
        self.replies
            .lock()
            .expect("replies lock")
            .insert(path.to_owned(), (status, body));
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("seen lock").clone()
    }

    fn paths(&self) -> Vec<String> {
        self.seen().into_iter().map(|seen| seen.path).collect()
    }

    fn last(&self) -> Seen {
        self.seen().pop().expect("桩至少该收到一条请求")
    }
}

async fn handle(State(stub): State<Stub>, uri: Uri, headers: HeaderMap, body: Body) -> Response {
    let bytes = match axum::body::to_bytes(body, 1 << 20).await {
        Ok(bytes) => bytes,
        Err(err) => return (StatusCode::BAD_REQUEST, err.to_string()).into_response(),
    };
    let parsed: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    stub.seen.lock().expect("seen lock").push(Seen {
        path: uri.path().to_owned(),
        headers,
        body: parsed,
    });
    let reply = stub
        .replies
        .lock()
        .expect("replies lock")
        .get(uri.path())
        .cloned();
    match reply {
        Some((status, Reply::Json(body))) => (
            StatusCode::from_u16(status).expect("status"),
            axum::Json(body),
        )
            .into_response(),
        Some((status, Reply::Text(text))) => (
            StatusCode::from_u16(status).expect("status"),
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            text,
        )
            .into_response(),
        None => axum::Json(json!({})).into_response(),
    }
}

/// 起桩，返回 `(base_url, stub)`。
async fn spawn_stub() -> (String, Stub) {
    let stub = Stub::default();
    let app = Router::new().fallback(handle).with_state(stub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), stub)
}

fn runtime_report(kind: &str) -> RegisterRuntime {
    RegisterRuntime {
        name: kind.to_owned(),
        kind: kind.to_owned(),
        version: "1.2.3".to_owned(),
        status: "online".to_owned(),
        profile_id: String::new(),
    }
}

fn config() -> ClientConfig {
    ClientConfig::new("ws-1", "machine-a", "devbox5", "0.4.21")
}

fn header<'a>(seen: &'a Seen, name: &str) -> Option<&'a str> {
    seen.headers.get(name).and_then(|value| value.to_str().ok())
}

// ---------------------------------------------------------------------------
// 请求头
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dev_identity_headers_ride_every_request() {
    let (base, stub) = spawn_stub().await;
    stub.reply(
        "/api/daemon/register",
        200,
        json!({ "runtimes": [{ "id": "rt-1", "provider": "claude" }], "repos_version": 3 }),
    );
    let transport = HttpTransport::new(&base, "0.4.21").with_dev_identity("user-1", "machine-a");
    let mut client = DaemonClient::new(transport, config());
    client
        .register(vec![runtime_report("claude")], Vec::new())
        .await
        .expect("register over real TCP");

    let seen = stub.last();
    assert_eq!(seen.path, "/api/daemon/register");
    assert_eq!(header(&seen, "x-multica-user-id"), Some("user-1"));
    assert_eq!(header(&seen, "x-daemon-id"), Some("machine-a"));
    assert_eq!(header(&seen, "x-client-version"), Some("0.4.21"));
    assert!(
        header(&seen, "content-type").is_some_and(|value| value.starts_with("application/json")),
        "必须是 JSON 请求：{:?}",
        header(&seen, "content-type")
    );
    assert!(
        header(&seen, "authorization").is_none(),
        "dev-mode 不带 Bearer"
    );
    assert_eq!(seen.body["workspace_id"], json!("ws-1"));
    assert_eq!(client.state().live_runtime_ids(), vec!["rt-1".to_owned()]);
    assert_eq!(
        client.state().registration().expect("台账").repos_version,
        3
    );
}

#[tokio::test]
async fn bearer_token_header_carries_the_http_capability_set() {
    let (base, stub) = spawn_stub().await;
    stub.reply(
        "/api/daemon/register",
        200,
        json!({ "runtimes": [], "repos_version": 0 }),
    );
    let transport = HttpTransport::new(&base, "0.4.21").with_token("mdt_secret");
    let mut client = DaemonClient::new(transport, config());
    client
        .register(Vec::new(), Vec::new())
        .await
        .expect("register");
    let seen = stub.last();
    assert_eq!(header(&seen, "authorization"), Some("Bearer mdt_secret"));
    let capabilities = header(&seen, "x-client-capabilities").expect("能力头");
    assert!(
        capabilities.contains("skill-bundles-v1"),
        "公共能力必须带上：{capabilities}"
    );
    assert!(
        !capabilities.contains("claim-poll-hints-v1"),
        "`claim-poll-hints-v1` 是 WS 专属：HTTP 腿声明它会让服务端白算一次 deferred 提示位"
    );
}

// ---------------------------------------------------------------------------
// 错误体 / 404
// ---------------------------------------------------------------------------

/// 裸传输发一条（验证错误路径时不必绕 `DaemonClient` 的业务前置检查）。
async fn post_raw(base: &str, path: &str, body: Value) -> TransportError {
    HttpTransport::new(base, "0.4.21")
        .post_json(path, &body)
        .await
        .expect_err("这条路径的桩回的是错误")
}

#[tokio::test]
async fn server_error_body_is_parsed_into_a_typed_status_error() {
    let (base, stub) = spawn_stub().await;
    stub.reply(
        "/api/daemon/register",
        400,
        json!({
            "error": {
                "code": "validation_error",
                "message": "validation error: workspace_id is required",
            }
        }),
    );
    match post_raw(&base, "/api/daemon/register", json!({})).await {
        TransportError::Status {
            status,
            code,
            message,
        } => {
            assert_eq!(status, 400);
            assert_eq!(code, "validation_error");
            assert!(message.contains("workspace_id is required"));
        }
        other => panic!("期望 Status，得到 {other:?}"),
    }
}

#[tokio::test]
async fn non_json_error_body_keeps_the_raw_text() {
    let (base, stub) = spawn_stub().await;
    stub.reply_text("/api/daemon/register", 502, "bad gateway");
    match post_raw(&base, "/api/daemon/register", json!({})).await {
        TransportError::Status {
            status,
            code,
            message,
        } => {
            assert_eq!(status, 502);
            assert_eq!(code, "", "认不出的错误形状不猜 code");
            assert_eq!(message, "bad gateway");
        }
        other => panic!("期望 Status，得到 {other:?}"),
    }
}

#[tokio::test]
async fn successful_non_json_body_is_malformed_not_ok() {
    let (base, stub) = spawn_stub().await;
    stub.reply_text("/api/daemon/register", 200, "<html>hello</html>");
    let err = post_raw(&base, "/api/daemon/register", json!({})).await;
    assert!(matches!(err, TransportError::Malformed { .. }), "{err:?}");
    assert!(!err.is_retriable(), "同样的字节重试一百次还是错的");
}

#[tokio::test]
async fn heartbeat_404_over_a_real_socket_means_runtime_gone() {
    let (base, stub) = spawn_stub().await;
    stub.reply(
        "/api/daemon/register",
        200,
        json!({ "runtimes": [{ "id": "rt-1", "provider": "claude" }], "repos_version": 0 }),
    );
    stub.reply(
        HEARTBEAT_PATH,
        404,
        json!({ "error": { "code": "not_found", "message": "not found: runtime not found" } }),
    );
    let transport = HttpTransport::new(&base, "0.4.21").with_dev_identity("user-1", "machine-a");
    let mut client = DaemonClient::new(transport, config());
    client
        .register(vec![runtime_report("claude")], Vec::new())
        .await
        .expect("register");
    let outcome = client.heartbeat("rt-1").await.expect("404 不是错误");
    assert!(outcome.actions().is_empty());
    assert!(client.state().live_runtime_ids().is_empty());
    assert_eq!(client.state().gone_runtime_ids(), vec!["rt-1".to_owned()]);
}

#[tokio::test]
async fn unreachable_server_is_a_retriable_transport_error() {
    // 绑一个端口再立刻放开：得到一个几乎必然没人监听的地址。
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    drop(listener);
    let mut client = DaemonClient::new(
        HttpTransport::new(format!("http://{addr}"), "0.4.21"),
        config(),
    );
    let err = client
        .register(Vec::new(), Vec::new())
        .await
        .expect_err("连不上必须报错");
    assert!(
        matches!(
            err,
            ClientError::Transport(TransportError::Unreachable { .. })
        ),
        "{err:?}"
    );
    assert!(err.is_retriable(), "连不上该退避重试");
}

// ---------------------------------------------------------------------------
// 端到端：register → heartbeat → claim
// ---------------------------------------------------------------------------

#[tokio::test]
async fn register_heartbeat_claim_round_trip_over_one_client() {
    let (base, stub) = spawn_stub().await;
    stub.reply(
        "/api/daemon/register",
        200,
        json!({
            "runtimes": [
                { "id": "rt-1", "workspace_id": "ws-1", "provider": "claude", "status": "online" },
            ],
            "repos_version": 7,
        }),
    );
    stub.reply(
        HEARTBEAT_PATH,
        200,
        json!({ "status": "ok", "pending_model_list": { "id": "m-1" } }),
    );
    stub.reply(
        "/api/daemon/tasks/claim",
        200,
        json!({
            "tasks": [{
                "id": "t-1", "runtime_id": "rt-1", "issue_id": "issue-1",
                "workspace_id": "ws-1", "status": "dispatched", "auth_token": "mul_task",
            }],
            "claim_poll_hint_supported": true,
            "next_deferred_task_after_ms": 2500,
        }),
    );

    let transport = HttpTransport::new(&base, "0.4.21").with_dev_identity("user-1", "machine-a");
    let mut client = DaemonClient::new(transport, config());
    let registration = client
        .register(vec![runtime_report("claude")], Vec::new())
        .await
        .expect("register");
    assert_eq!(registration.repos_version, 7);

    let outcome = client.heartbeat("rt-1").await.expect("heartbeat");
    assert_eq!(outcome.actions().len(), 1, "心跳 ack 里有一条待办");

    let claimed = client.claim().await.expect("claim");
    assert_eq!(claimed.tasks.len(), 1);
    assert_eq!(claimed.tasks[0].auth_token, "mul_task");
    assert_eq!(claimed.next_deferred_task_after_ms, Some(2500));
    assert!(claimed.claim_poll_hint_supported);
    assert_eq!(client.state().in_flight_len(), 1);

    // 第二次 claim 服务端又回同一条：客户端必须丢掉（幂等护栏）。
    let again = client.claim().await.expect("claim 2");
    assert_eq!(
        again,
        ClaimOutcome {
            tasks: Vec::new(),
            claim_poll_hint_supported: true,
            next_deferred_task_after_ms: Some(2500),
        }
    );

    // 请求路径的**顺序**就是客户端一个完整周期的形状。
    assert_eq!(
        stub.paths(),
        vec![
            "/api/daemon/register".to_owned(),
            HEARTBEAT_PATH.to_owned(),
            "/api/daemon/tasks/claim".to_owned(),
            "/api/daemon/tasks/claim".to_owned(),
        ]
    );
}
