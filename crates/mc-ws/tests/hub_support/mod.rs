//! daemon WS 传输层集成测试的共享设施：**真 socket**。
//!
//! 刻意不用 `tower::ServiceExt::oneshot` 或假 socket：本片的所有验收点（注册表、扇出、
//! 保活踢线、慢客户端驱逐、RPC 三条通道级失败、拆线取消）都在「字节真的过了一遍
//! WebSocket」之后才成立。于是这里起一个**真的** axum server（`127.0.0.1:0`），
//! 客户端用 `tokio-tungstenite` 真连。
//!
//! 身份在测试里由请求头注入（[`H_*`]），对应 M3-7 里 token 解析 + runtime 授权查询的
//! 产物 —— hub 本身不解析 token（见 `crates/mc-ws/src/hub.rs` 的 `handle_websocket`）。
#![allow(dead_code)]

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::time::Duration;

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use mc_ws::hub::Hub;
use mc_ws::identity::ClientIdentity;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

/// 客户端 WS 流。
pub type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// 所有等待的缺省上限：真 socket 上的正向断言不该等这么久，等到了就是失败。
pub const WAIT: Duration = Duration::from_secs(5);

/// 「确认没有帧」的观察窗口：比一次 RTT 大得多，又短到不拖慢测试。
pub const QUIET: Duration = Duration::from_millis(300);

pub const H_DAEMON_ID: &str = "x-test-daemon-id";
pub const H_RUNTIME_IDS: &str = "x-test-runtime-ids";
pub const H_WORKSPACE_ID: &str = "x-test-workspace-id";
pub const H_WORKSPACE_IDS: &str = "x-test-workspace-ids";
pub const H_USER_ID: &str = "x-test-user-id";
pub const H_CLIENT_VERSION: &str = "x-test-client-version";

/// 测试用身份；序列化成请求头，服务端侧再从请求头还原成 [`ClientIdentity`]。
#[derive(Debug, Clone, Default)]
pub struct TestIdentity {
    pub daemon_id: String,
    pub runtime_ids: Vec<String>,
    pub workspace_id: String,
    pub workspace_ids: Vec<String>,
    pub user_id: String,
    pub client_version: String,
}

impl TestIdentity {
    /// daemon 身份（授权若干 runtime）。
    #[must_use]
    pub fn daemon(daemon_id: &str, runtime_ids: &[&str]) -> Self {
        Self {
            daemon_id: daemon_id.to_owned(),
            runtime_ids: runtime_ids.iter().map(|id| (*id).to_owned()).collect(),
            client_version: "test-daemon/1".to_owned(),
            ..Self::default()
        }
    }

    /// 纯用户身份（无 runtime）：走 `by_user` 索引那条面。
    #[must_use]
    pub fn user(user_id: &str) -> Self {
        Self {
            user_id: user_id.to_owned(),
            client_version: "test-web/1".to_owned(),
            ..Self::default()
        }
    }

    /// 遗留的单工作区字段（`workspace_ids` 为空时生效）。
    #[must_use]
    pub fn with_workspace(mut self, workspace_id: &str) -> Self {
        workspace_id.clone_into(&mut self.workspace_id);
        self
    }

    /// 多工作区 scope（优先于单工作区字段）。
    #[must_use]
    pub fn with_workspaces(mut self, workspace_ids: &[&str]) -> Self {
        self.workspace_ids = workspace_ids.iter().map(|id| (*id).to_owned()).collect();
        self
    }

    /// 追加授权 runtime。
    #[must_use]
    pub fn with_runtimes(mut self, runtime_ids: &[&str]) -> Self {
        self.runtime_ids
            .extend(runtime_ids.iter().map(|id| (*id).to_owned()));
        self
    }

    fn headers(&self) -> Vec<(&'static str, String)> {
        vec![
            (H_DAEMON_ID, self.daemon_id.clone()),
            (H_RUNTIME_IDS, self.runtime_ids.join(",")),
            (H_WORKSPACE_ID, self.workspace_id.clone()),
            (H_WORKSPACE_IDS, self.workspace_ids.join(",")),
            (H_USER_ID, self.user_id.clone()),
            (H_CLIENT_VERSION, self.client_version.clone()),
        ]
    }
}

fn split_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

fn header_value(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

/// 请求头 → 连接身份（M3-7 里这一步是 token 解析 + 授权查询）。
#[must_use]
pub fn identity_from_headers(headers: &HeaderMap) -> ClientIdentity {
    ClientIdentity {
        daemon_id: header_value(headers, H_DAEMON_ID),
        user_id: header_value(headers, H_USER_ID),
        workspace_id: header_value(headers, H_WORKSPACE_ID),
        workspace_ids: split_list(&header_value(headers, H_WORKSPACE_IDS)),
        runtime_ids: split_list(&header_value(headers, H_RUNTIME_IDS)),
        client_version: header_value(headers, H_CLIENT_VERSION),
        ..ClientIdentity::default()
    }
}

async fn daemon_ws_route(
    State(hub): State<Hub>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    hub.handle_websocket(ws, identity_from_headers(&headers))
}

/// 测试 server：真 listener + 真 `axum::serve`（drop 时 abort）。
pub struct TestServer {
    pub addr: SocketAddr,
    pub hub: Hub,
    task: tokio::task::JoinHandle<()>,
}

impl TestServer {
    pub async fn start(hub: Hub) -> Self {
        let app = Router::new()
            .route("/daemon/ws", get(daemon_ws_route))
            .with_state(hub.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("local addr");
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { addr, hub, task }
    }

    pub fn url(&self) -> String {
        format!("ws://{}/daemon/ws", self.addr)
    }

    pub async fn connect(&self, identity: &TestIdentity) -> Ws {
        connect(self.addr, identity).await
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// 用真 socket 建一条 WS 连接（身份走请求头）。
pub async fn connect(addr: SocketAddr, identity: &TestIdentity) -> Ws {
    let mut request = format!("ws://{addr}/daemon/ws")
        .into_client_request()
        .expect("build ws request");
    for (name, value) in identity.headers() {
        if value.is_empty() {
            continue;
        }
        request
            .headers_mut()
            .insert(name, HeaderValue::from_str(&value).expect("header value"));
    }
    let (ws, response) = tokio::time::timeout(WAIT, connect_async(request))
        .await
        .expect("ws handshake timed out")
        .expect("ws handshake failed");
    assert_eq!(response.status().as_u16(), 101, "期望 101 升级");
    ws
}

// ------------------------------------------------------------------ 客户端动作

pub async fn send_frame(ws: &mut Ws, frame: Message) {
    ws.send(frame).await.expect("send frame");
}

pub async fn send_text(ws: &mut Ws, text: &str) {
    send_frame(ws, Message::Text(text.to_owned())).await;
}

pub async fn send_json(ws: &mut Ws, value: &serde_json::Value) {
    send_text(ws, &value.to_string()).await;
}

/// 等下一帧文本（跳过 ping/pong/二进制）；超时、EOF、Close 都 panic。
pub async fn expect_text(ws: &mut Ws, wait: Duration) -> String {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "等文本帧超时（{wait:?}）");
        match tokio::time::timeout(remaining, ws.next()).await {
            Err(err) => panic!("等文本帧超时（{wait:?}）: {err}"),
            Ok(None) => panic!("等文本帧时对端已关闭连接"),
            Ok(Some(Err(err))) => panic!("读 WS 失败: {err}"),
            Ok(Some(Ok(Message::Text(text)))) => return text,
            Ok(Some(Ok(Message::Close(frame)))) => panic!("意外收到 Close: {frame:?}"),
            Ok(Some(Ok(_))) => {}
        }
    }
}

/// 「这段时间内没有任何帧」；返回 `true` 表示窗口内确实静默。
pub async fn quiet_for(ws: &mut Ws, wait: Duration) -> bool {
    tokio::time::timeout(wait, ws.next()).await.is_err()
}

/// 等连接数（或任意谓词）成立；超时 panic。
pub async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        if cond() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "等待条件超时（{WAIT:?}）: {what}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

// ---------------------------------------------------------------- 原始 HTTP 探针

/// 手写 HTTP/1.1 响应（只解析 400 这类小响应，`Content-Length` 兜住边界）。
pub struct RawResponse {
    pub status: u16,
    pub headers: String,
    pub body: String,
}

impl RawResponse {
    #[must_use]
    pub fn header(&self, name: &str) -> Option<String> {
        let needle = format!("{}:", name.to_ascii_lowercase());
        self.headers
            .lines()
            .find(|line| line.to_ascii_lowercase().starts_with(&needle))
            .map(|line| line[needle.len()..].trim().to_owned())
    }
}

/// 发一个**合法的** WS 升级请求（没有身份头也行），读回响应。
///
/// 用它的原因：400 那条分支必须由 [`Hub::handle_websocket`] 产生，而不是 axum 的
/// `WebSocketUpgrade` 提取器（缺升级头时提取器自己就回 400/426）——所以探针必须带齐
/// `Upgrade` / `Sec-WebSocket-Key` / `Sec-WebSocket-Version`。
pub async fn raw_upgrade(addr: SocketAddr, extra_headers: &[(&str, &str)]) -> RawResponse {
    let mut stream = TcpStream::connect(addr).await.expect("connect raw");
    let mut request = format!(
        "GET /daemon/ws HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n"
    );
    for (name, value) in extra_headers {
        write!(request, "{name}: {value}\r\n").expect("String 写入不会失败");
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write raw request");
    stream.flush().await.expect("flush raw request");

    let deadline = tokio::time::Instant::now() + WAIT;
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0_u8; 1024];
    let mut head_end: Option<usize> = None;
    loop {
        if head_end.is_none() {
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                head_end = Some(pos + 4);
            }
        }
        if let Some(end) = head_end {
            let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
            let content_length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if buf.len() >= end + content_length {
                let body = String::from_utf8_lossy(&buf[end..end + content_length]).into_owned();
                let head_text = String::from_utf8_lossy(&buf[..end]).into_owned();
                return RawResponse {
                    status: parse_status(&head_text),
                    headers: head_text,
                    body,
                };
            }
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "读原始响应超时");
        match tokio::time::timeout(remaining, stream.read(&mut chunk)).await {
            Err(err) => panic!("读原始响应超时: {err}"),
            Ok(Ok(0)) => panic!("对端在读满响应前关闭（已收 {} 字节）", buf.len()),
            Ok(Ok(n)) => buf.extend_from_slice(&chunk[..n]),
            Ok(Err(err)) => panic!("读原始响应失败: {err}"),
        }
        assert!(buf.len() < 64 * 1024, "响应头异常膨胀");
    }
}

fn parse_status(head: &str) -> u16 {
    head.lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .expect("状态行")
}
