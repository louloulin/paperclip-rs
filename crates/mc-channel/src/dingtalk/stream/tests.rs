//! Stream 传输面的用例（`stream.rs` 的 `#[cfg(test)] mod tests;`）。
//!
//! 三段各自钉住：
//!
//! - **帧编解码**：纯 JSON 断言（`messageId` 回声是网关的相关键）；
//! - **连接引导**：纯函数表驱动 + 一个**真 `reqwest`** 的 loopback 服务端（含"错误不回显
//!   凭据"的反例）；
//! - **帧循环**：脚本化的内存 socket（不睡真觉、不开真 socket），覆盖 ping/pong、回调 ack、
//!   坏帧跳过、读截止（含 pong 刷新）、流结束、停机信号、以及拨号失败**不**回显带票据的 URL。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::{
    build_open_request, chatbot_subscriptions, dial_url_from_response, new_ack_response,
    new_pong_response, AppSecret, CallbackSink, ConnectionOpener, Connector, DataFrame,
    DataFrameResponse, OpenConnectionRequest, OpenConnectionResponse, ReqwestOpener,
    SessionOutcome, StopSignal, StreamKnobs, WsConnection, WsDialer, WsEvent, BOT_MESSAGE_TOPIC,
    CONNECTIONS_OPEN_PATH, FRAME_TYPE_CALLBACK, FRAME_TYPE_SYSTEM, STREAM_USER_AGENT,
    SYSTEM_TOPIC_DISCONNECT, SYSTEM_TOPIC_PING,
};
use crate::channel::{ChannelError, ChannelResult};
use crate::dingtalk::inbound::BotCallbackData;

// =====================================================================
// 帧编解码
// =====================================================================

/// ack / pong 的逐字形态（网关靠 `messageId` 配对）。
#[test]
fn ack_and_pong_match_the_upstream_shape() {
    let ack = new_ack_response("mid-1");
    assert_eq!(ack.code, 200);
    assert_eq!(ack.message, "");
    assert_eq!(ack.data, "");
    assert_eq!(
        serde_json::to_value(&ack).expect("序列化"),
        serde_json::json!({
            "code": 200,
            "headers": {"messageId": "mid-1", "contentType": "application/json"},
            "message": "",
            "data": "",
        })
    );

    let pong_frame = new_pong_response("mid-2", "{\"ping\":true}");
    assert_eq!(pong_frame.code, 200);
    assert_eq!(pong_frame.message, "ok");
    assert_eq!(pong_frame.data, "{\"ping\":true}");
    assert_eq!(pong_frame.headers["messageId"], "mid-2");

    // 回帧也能解回来（端点两侧同形）。
    let decoded: DataFrameResponse =
        serde_json::from_str(&serde_json::to_string(&ack).expect("序列化")).expect("反序列化");
    assert_eq!(decoded, ack);
}

/// 帧的访问器：`topic` / `messageId` 缺席时是空串（不 panic）；非字符串 header ⇒ 整帧失败。
#[test]
fn frames_expose_topic_and_message_id() {
    let frame: DataFrame = serde_json::from_str(
        r#"{"type":"SYSTEM","headers":{"topic":"ping","messageId":"m","extra":"x"},"data":"d"}"#,
    )
    .expect("合法帧");
    assert_eq!(frame.frame_type, FRAME_TYPE_SYSTEM);
    assert_eq!(frame.topic(), SYSTEM_TOPIC_PING);
    assert_eq!(frame.message_id(), "m");
    assert_eq!(frame.data, "d");

    let bare: DataFrame = serde_json::from_str("{}").expect("空帧也能解");
    assert_eq!(bare.topic(), "");
    assert_eq!(bare.message_id(), "");
    assert_eq!(bare.frame_type, "");

    // header 值不是字符串 ⇒ 整帧解码失败（与上游 `map[string]string` 同款）。
    assert!(serde_json::from_str::<DataFrame>(r#"{"headers":{"messageId":42}}"#).is_err());
}

/// 引导请求体的逐字形态：三个订阅、`ua`、以及 `AppKey` 在明处。
#[test]
fn open_request_matches_the_upstream_shape() {
    let request = build_open_request("app-key", "app-secret");
    assert_eq!(
        serde_json::to_value(&request).expect("序列化"),
        serde_json::json!({
            "clientId": "app-key",
            "clientSecret": "app-secret",
            "subscriptions": [
                {"type": "SYSTEM", "topic": "ping"},
                {"type": "SYSTEM", "topic": "disconnect"},
                {"type": "CALLBACK", "topic": "/v1.0/im/bot/messages/get"},
            ],
            "ua": STREAM_USER_AGENT,
        })
    );
    assert_eq!(chatbot_subscriptions().len(), 3);
    assert_eq!(STREAM_USER_AGENT, "multica-dingtalk/1.0");
    assert_eq!(CONNECTIONS_OPEN_PATH, "/v1.0/gateway/connections/open");
}

/// **凭据面**：承载 `AppSecret` 的两个类型手写脱敏（`docs/60` §2.3 第 1 条）。
#[test]
fn credentials_are_redacted_in_debug_output() {
    let secret = AppSecret::new("SUPER-SECRET-VALUE");
    assert_eq!(secret.expose(), "SUPER-SECRET-VALUE");
    assert!(!secret.is_empty());
    let rendered = format!("{secret:?}");
    assert!(!rendered.contains("SUPER-SECRET-VALUE"), "{rendered}");
    assert!(rendered.contains("<redacted>"));
    assert!(AppSecret::default().is_empty());

    let request = build_open_request("app-key", "SUPER-SECRET-VALUE");
    let rendered = format!("{request:?}");
    assert!(!rendered.contains("SUPER-SECRET-VALUE"), "{rendered}");
    assert!(
        rendered.contains("app-key"),
        "AppKey 不是密钥，诊断要看得见"
    );
    assert!(rendered.contains("<redacted>"));
    let _: &OpenConnectionRequest = &request;
}

/// dial URL 的组装：保留既有 query、追加编码后的 ticket。
#[test]
fn dial_url_keeps_existing_query_and_encodes_the_ticket() {
    let response = OpenConnectionResponse {
        endpoint: "wss://gateway.example.com/stream".to_string(),
        ticket: "a b/c+d?e=f".to_string(),
    };
    assert_eq!(
        dial_url_from_response(&response).expect("合法响应"),
        "wss://gateway.example.com/stream?ticket=a%20b%2Fc%2Bd%3Fe%3Df"
    );

    let with_query = OpenConnectionResponse {
        endpoint: "wss://gateway.example.com/stream?region=cn".to_string(),
        ticket: "ticket-1".to_string(),
    };
    assert_eq!(
        dial_url_from_response(&with_query).expect("合法响应"),
        "wss://gateway.example.com/stream?region=cn&ticket=ticket-1"
    );
}

/// 三条校验：端点 / 票据缺失、非 `wss`、没有 host。
#[test]
fn dial_url_rejects_bad_endpoints() {
    for (endpoint, ticket) in [
        ("", "t"),
        ("wss://gateway.example.com/stream", ""),
        ("ws://gateway.example.com/stream", "t"),
        ("https://gateway.example.com/stream", "t"),
        ("wss:///stream", "t"),
        ("gateway.example.com/stream", "t"),
    ] {
        let response = OpenConnectionResponse {
            endpoint: endpoint.to_string(),
            ticket: ticket.to_string(),
        };
        let error = dial_url_from_response(&response).expect_err("必须被拒");
        let message = error.to_string();
        // 端点为空串时 `contains("")` 恒真 ⇒ 只在确实有端点可回显时才查这一条。
        if !endpoint.is_empty() {
            assert!(!message.contains(endpoint), "错误回显了端点：{message}");
        }
        assert!(
            !message.contains("ticket="),
            "错误回显了票据参数：{message}"
        );
    }
}

// =====================================================================
// 真 reqwest 的 loopback 服务端
// =====================================================================

fn http_response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )
}

/// 起一个**只服务一次**的 HTTP 服务端，返回它的基址与"收到的请求"。
async fn serve_once(response: String) -> (String, tokio::sync::oneshot::Receiver<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let address = listener.local_addr().expect("local addr");
    let (sender, receiver) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buffer = vec![0_u8; 16 * 1024];
        let read = socket.read(&mut buffer).await.unwrap_or(0);
        let _ = sender.send(String::from_utf8_lossy(&buffer[..read]).to_string());
        let _ = socket.write_all(response.as_bytes()).await;
        let _ = socket.flush().await;
    });
    (format!("http://{address}"), receiver)
}

/// 真 `reqwest` 路径：POST 形态正确、响应被组装成可拨 URL。
#[tokio::test]
async fn reqwest_opener_posts_and_assembles_the_dial_url() {
    let body = r#"{"endpoint":"wss://gateway.example.com/stream","ticket":"ticket-1"}"#;
    let (base, request) = serve_once(http_response("200 OK", body)).await;
    let opener = ReqwestOpener::with_api_base(&base);
    assert_eq!(opener.api_base(), base);

    let url = opener
        .open("app-key", "app-secret")
        .await
        .expect("引导成功");
    assert_eq!(url, "wss://gateway.example.com/stream?ticket=ticket-1");

    let request = request.await.expect("服务端记下了请求");
    assert!(
        request.starts_with("POST /v1.0/gateway/connections/open "),
        "{request}"
    );
    assert!(request.contains("clientId"), "{request}");
    assert!(request.contains("app-key"), "{request}");
    let json: serde_json::Value =
        serde_json::from_str(request.split("\r\n\r\n").nth(1).expect("请求体"))
            .expect("请求体是 JSON");
    assert_eq!(json["clientId"], "app-key");
    assert_eq!(json["ua"], STREAM_USER_AGENT);
    assert_eq!(json["subscriptions"].as_array().map(Vec::len), Some(3));
}

/// **错误路径不回显凭据**：网关把请求回声进错误体也不该漏到我们的错误里。
#[tokio::test]
async fn a_failed_open_never_echoes_the_secret() {
    let body = r#"{"error":"bad clientSecret app-secret"}"#;
    let (base, _request) = serve_once(http_response("401 Unauthorized", body)).await;
    let opener = ReqwestOpener::with_api_base(&base);
    let error = opener
        .open("app-key", "app-secret")
        .await
        .expect_err("401 必须失败");
    let message = error.to_string();
    assert!(!message.contains("app-secret"), "错误回显了凭据：{message}");
    assert!(message.contains("401"), "状态码要留下：{message}");
}

/// 2xx 但不是我们认得的 JSON ⇒ 明确的失败（不是 panic、也不是半成品 URL）。
#[tokio::test]
async fn an_unexpected_body_is_a_clear_failure() {
    let (base, _request) = serve_once(http_response("200 OK", "not json")).await;
    let opener = ReqwestOpener::with_api_base(&base);
    assert!(opener.open("k", "s").await.is_err());

    // 2xx 但没有 endpoint / ticket。
    let (base, _request) = serve_once(http_response("200 OK", "{}")).await;
    let opener = ReqwestOpener::with_api_base(&base);
    assert!(opener.open("k", "s").await.is_err());
}

// =====================================================================
// 脚本化的内存 socket
// =====================================================================

#[derive(Debug, Default)]
struct FakeLog {
    written: Vec<String>,
    pings: usize,
    closed: bool,
}

/// 脚本里的一条事件。
///
/// `delay` 是**相对上一条事件**的延迟；`deadline` 在第一次被读到时就定下来（`None` = 还没定）。
/// 这个"定下来就不再变"的形态是**替身必须的保真度**：帧循环会在 ping 那一支赢下 `select!` 时
/// 丢弃正在等的读 future（真 socket 的 `poll_next` 是取消安全的），如果替身每次重新计时，
/// 用例就会测出一个真实实现不会有的行为。
#[derive(Debug)]
struct ScriptEntry {
    delay: Duration,
    deadline: Option<tokio::time::Instant>,
    event: Option<ChannelResult<WsEvent>>,
}

type Script = Arc<Mutex<VecDeque<ScriptEntry>>>;

fn scripted(
    events: Vec<(Duration, Option<ChannelResult<WsEvent>>)>,
) -> (Script, Arc<Mutex<FakeLog>>) {
    let entries: VecDeque<ScriptEntry> = events
        .into_iter()
        .map(|(delay, event)| ScriptEntry {
            delay,
            deadline: None,
            event,
        })
        .collect();
    (
        Arc::new(Mutex::new(entries)),
        Arc::new(Mutex::new(FakeLog::default())),
    )
}

fn text(value: &str) -> (Duration, Option<ChannelResult<WsEvent>>) {
    (Duration::ZERO, Some(Ok(WsEvent::Text(value.to_string()))))
}

fn ending() -> (Duration, Option<ChannelResult<WsEvent>>) {
    (Duration::ZERO, None)
}

struct ScriptConnection {
    script: Script,
    log: Arc<Mutex<FakeLog>>,
}

#[async_trait]
impl WsConnection for ScriptConnection {
    async fn next_event(&mut self) -> Option<ChannelResult<WsEvent>> {
        // 1. 先给队首定下截止时间（只定一次 ⇒ 被取消的读不会把它往后推）。
        let deadline = {
            let mut script = self.script.lock().expect("lock");
            let front = script.front_mut()?;
            *front
                .deadline
                .get_or_insert_with(|| tokio::time::Instant::now() + front.delay)
        };
        // 2. 等到那个时刻（取消 ⇒ 下次仍等同一个时刻）。
        tokio::time::sleep_until(deadline).await;
        // 3. 取走它。
        self.script
            .lock()
            .expect("lock")
            .pop_front()
            .and_then(|entry| entry.event)
    }

    async fn send_text(&mut self, value: &str) -> ChannelResult<()> {
        self.log
            .lock()
            .expect("lock")
            .written
            .push(value.to_string());
        Ok(())
    }

    async fn send_ping(&mut self) -> ChannelResult<()> {
        self.log.lock().expect("lock").pings += 1;
        Ok(())
    }

    async fn close(&mut self) {
        self.log.lock().expect("lock").closed = true;
    }
}

struct FakeDialer {
    script: Script,
    log: Arc<Mutex<FakeLog>>,
    error: Option<ChannelError>,
    dialed: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl WsDialer for FakeDialer {
    async fn dial(&self, dial_url: &str) -> ChannelResult<Box<dyn WsConnection>> {
        self.dialed.lock().expect("lock").push(dial_url.to_string());
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        Ok(Box::new(ScriptConnection {
            script: Arc::clone(&self.script),
            log: Arc::clone(&self.log),
        }))
    }
}

struct FakeOpener {
    url: ChannelResult<String>,
    seen: Arc<Mutex<Vec<(String, String)>>>,
}

#[async_trait]
impl ConnectionOpener for FakeOpener {
    async fn open(&self, app_key: &str, app_secret: &str) -> ChannelResult<String> {
        self.seen
            .lock()
            .expect("lock")
            .push((app_key.to_string(), app_secret.to_string()));
        self.url.clone()
    }
}

#[derive(Default)]
struct RecordingSink {
    seen: Mutex<Vec<BotCallbackData>>,
}

#[async_trait]
impl CallbackSink for RecordingSink {
    async fn on_callback(&self, callback: BotCallbackData) -> ChannelResult<()> {
        self.seen.lock().expect("lock").push(callback);
        Ok(())
    }
}

/// 一个连接 + 它的可观测面。
struct Harness {
    connector: Connector,
    sink: Arc<RecordingSink>,
    log: Arc<Mutex<FakeLog>>,
    dialed: Arc<Mutex<Vec<String>>>,
    opener_seen: Arc<Mutex<Vec<(String, String)>>>,
}

fn harness(
    events: Vec<(Duration, Option<ChannelResult<WsEvent>>)>,
    dial_error: Option<ChannelError>,
    read_deadline: Duration,
) -> Harness {
    let (script, log) = scripted(events);
    let dialed = Arc::new(Mutex::new(Vec::new()));
    let opener_seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::new(RecordingSink::default());
    let connector = Connector::new(
        Arc::new(FakeOpener {
            url: Ok("wss://gateway.example.com/stream?ticket=ticket-1".to_string()),
            seen: Arc::clone(&opener_seen),
        }),
        Arc::new(FakeDialer {
            script,
            log: Arc::clone(&log),
            error: dial_error,
            dialed: Arc::clone(&dialed),
        }),
        "app-key",
        AppSecret::new("app-secret"),
        Arc::clone(&sink) as Arc<dyn CallbackSink>,
    )
    .with_knobs(StreamKnobs {
        // 心跳在用例里几乎不发生（每次都靠读那一支推进）。
        ping_interval: Duration::from_secs(3600),
        read_deadline,
        write_timeout: Duration::from_secs(1),
    });
    Harness {
        connector,
        sink,
        log,
        dialed,
        opener_seen,
    }
}

fn callback_payload(message_id: &str) -> serde_json::Value {
    serde_json::json!({
        "senderStaffId": "staff",
        "conversationId": "chat",
        "msgId": message_id,
        "msgtype": "text",
        "text": {"content": "hello"},
    })
}

fn callback_frame(frame_message_id: &str, payload: &serde_json::Value) -> String {
    serde_json::json!({
        "type": FRAME_TYPE_CALLBACK,
        "headers": {"topic": BOT_MESSAGE_TOPIC, "messageId": frame_message_id},
        "data": payload.to_string(),
    })
    .to_string()
}

fn system_frame(topic: &str, message_id: &str, data: &str) -> String {
    serde_json::json!({
        "type": FRAME_TYPE_SYSTEM,
        "headers": {"topic": topic, "messageId": message_id},
        "data": data,
    })
    .to_string()
}

/// ping ⇒ pong、回调 ⇒ ack、网关 disconnect ⇒ 干净返回（三条分支一次跑完）。
#[tokio::test]
async fn a_session_serves_ping_callback_and_disconnect() {
    let heartbeat = system_frame(SYSTEM_TOPIC_PING, "ping-1", "{\"t\":1}");
    let callback = callback_frame("frame-1", &callback_payload("msg-1"));
    let disconnect = system_frame(SYSTEM_TOPIC_DISCONNECT, "bye", "");
    let harness = harness(
        vec![
            text(&heartbeat),
            text(&callback),
            text(&disconnect),
            ending(),
        ],
        None,
        Duration::from_secs(5),
    );

    let (signal, handle) = StopSignal::pair();
    let outcome = harness
        .connector
        .run_session(handle)
        .await
        .expect("干净返回");
    assert_eq!(outcome, SessionOutcome::DisconnectRequested);
    assert!(!signal.is_stopped());

    let written = harness.log.lock().expect("lock").written.clone();
    assert_eq!(written.len(), 2, "pong + ack：{written:?}");
    let heartbeat_reply: DataFrameResponse =
        serde_json::from_str(&written[0]).expect("pong 是 JSON");
    assert_eq!(heartbeat_reply.headers["messageId"], "ping-1");
    assert_eq!(heartbeat_reply.message, "ok");
    assert_eq!(heartbeat_reply.data, "{\"t\":1}");
    let ack: DataFrameResponse = serde_json::from_str(&written[1]).expect("ack 是 JSON");
    assert_eq!(ack.code, 200);
    assert_eq!(ack.headers["messageId"], "frame-1");
    assert_eq!(ack.data, "");

    let seen = harness.sink.seen.lock().expect("lock").clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].msg_id, "msg-1");
    assert_eq!(seen[0].text.content, "hello");
    assert!(harness.log.lock().expect("lock").closed, "收尾要关连接");

    // 引导与拨号各发生一次（用的是引导返回的那条 URL）。
    assert_eq!(
        harness.opener_seen.lock().expect("lock").as_slice(),
        &[("app-key".to_string(), "app-secret".to_string())]
    );
    assert_eq!(
        harness.dialed.lock().expect("lock").as_slice(),
        &["wss://gateway.example.com/stream?ticket=ticket-1".to_string()]
    );
}

/// 坏帧 / 不认识的帧 / 二进制帧：告警继续，**不**拆链路、也**不**回帧。
#[tokio::test]
async fn unusable_frames_are_skipped() {
    let harness = harness(
        vec![
            text("not json at all"),
            text(r#"{"type":"SYSTEM","headers":{"topic":"something-else"}}"#),
            text(r#"{"type":"CALLBACK","headers":{"topic":"/v1.0/im/other"}}"#),
            text(&serde_json::json!({"type": FRAME_TYPE_CALLBACK, "headers": {"topic": BOT_MESSAGE_TOPIC, "messageId": "f"}, "data": "not json"}).to_string()),
            (Duration::ZERO, Some(Ok(WsEvent::Binary(vec![1, 2, 3])))),
            (
                Duration::ZERO,
                Some(Ok(WsEvent::Ping)),
            ),
            (
                Duration::ZERO,
                Some(Ok(WsEvent::Pong)),
            ),
            text(&system_frame(SYSTEM_TOPIC_DISCONNECT, "bye", "")),
            ending(),
        ],
        None,
        Duration::from_secs(5),
    );
    let (_signal, handle) = StopSignal::pair();
    let outcome = harness
        .connector
        .run_session(handle)
        .await
        .expect("干净返回");
    assert_eq!(outcome, SessionOutcome::DisconnectRequested);
    let log = harness.log.lock().expect("lock");
    // 唯一一条回帧是那条**无法解码**的回调帧的 ack（上游：无论解码成败都 ACK）。
    assert_eq!(log.written.len(), 1, "{:?}", log.written);
    let ack: DataFrameResponse = serde_json::from_str(&log.written[0]).expect("ack 是 JSON");
    assert_eq!(ack.headers["messageId"], "f");
    assert!(harness.sink.seen.lock().expect("lock").is_empty());
}

/// 读失败 / 流结束 ⇒ `Err`（supervisor 按"这次尝试失败"退避重连）。
#[tokio::test]
async fn a_broken_socket_is_an_error() {
    let (script, log) = scripted(vec![(
        Duration::ZERO,
        Some(Err(ChannelError::Transport {
            message: "dingtalk stream: socket read failed".to_string(),
        })),
    )]);
    let sink = Arc::new(RecordingSink::default());
    let connector = Connector::new(
        Arc::new(FakeOpener {
            url: Ok("wss://gateway/stream".to_string()),
            seen: Arc::new(Mutex::new(Vec::new())),
        }),
        Arc::new(FakeDialer {
            script,
            log: Arc::clone(&log),
            error: None,
            dialed: Arc::new(Mutex::new(Vec::new())),
        }),
        "app-key",
        AppSecret::new("app-secret"),
        Arc::clone(&sink) as Arc<dyn CallbackSink>,
    );
    let (_signal, handle) = StopSignal::pair();
    let error = connector
        .run_session(handle)
        .await
        .expect_err("读失败是错误");
    assert!(matches!(error, ChannelError::Transport { .. }));

    // 流结束（`None`）也是错误。
    let harness = harness(vec![ending()], None, Duration::from_secs(5));
    let (_signal, handle) = StopSignal::pair();
    assert!(harness.connector.run_session(handle).await.is_err());
}

/// 读截止：静默的 socket 到点就判链路已死。
#[tokio::test]
async fn a_silent_socket_hits_the_read_deadline() {
    let harness = harness(
        vec![(Duration::from_millis(300), Some(Ok(WsEvent::Pong)))],
        None,
        Duration::from_millis(60),
    );
    let (_signal, handle) = StopSignal::pair();
    let error = harness
        .connector
        .run_session(handle)
        .await
        .expect_err("读截止必须生效");
    assert_eq!(
        error.to_string(),
        "channel: transport failure: dingtalk stream: read deadline exceeded"
    );
    assert!(harness.log.lock().expect("lock").closed);
}

/// **每个事件都刷新读截止**：两个间隔 40ms 的事件在 60ms 的截止下都能读到。
#[tokio::test]
async fn a_pong_refreshes_the_read_deadline() {
    let harness = harness(
        vec![
            (Duration::from_millis(40), Some(Ok(WsEvent::Pong))),
            (
                Duration::from_millis(40),
                Some(Ok(WsEvent::Text(system_frame(
                    SYSTEM_TOPIC_DISCONNECT,
                    "bye",
                    "",
                )))),
            ),
            ending(),
        ],
        None,
        Duration::from_millis(60),
    );
    let (_signal, handle) = StopSignal::pair();
    let outcome = harness
        .connector
        .run_session(handle)
        .await
        .expect("每个事件都刷新 ⇒ 不超时");
    assert_eq!(outcome, SessionOutcome::DisconnectRequested);
}

/// 停机信号置位 ⇒ `Cancelled`（**不是**错误，`connect` 会回 `Ok`）。
#[tokio::test]
async fn the_stop_signal_ends_the_session_cleanly() {
    let harness = harness(
        vec![(Duration::from_millis(200), Some(Ok(WsEvent::Pong)))],
        None,
        Duration::from_secs(5),
    );
    let (signal, handle) = StopSignal::pair();
    let runner = harness.connector.run_session(handle);
    let stopper = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        signal.stop();
    });
    let outcome = runner.await.expect("停机不是错误");
    stopper.await.expect("stopper");
    assert_eq!(outcome, SessionOutcome::Cancelled);
    assert!(harness.log.lock().expect("lock").closed);
}

/// 引导失败 / 拨号失败：错误里**不**出现带票据的 URL，也不出现 `AppSecret`。
#[tokio::test]
async fn a_failed_dial_never_echoes_the_url_or_the_secret() {
    let harness = harness(
        vec![ending()],
        Some(ChannelError::Transport {
            message: "dingtalk stream: websocket handshake failed".to_string(),
        }),
        Duration::from_secs(5),
    );
    let (_signal, handle) = StopSignal::pair();
    let error = harness
        .connector
        .run_session(handle)
        .await
        .expect_err("拨号失败");
    let message = error.to_string();
    assert!(!message.contains("ticket-1"), "{message}");
    assert!(!message.contains("app-secret"), "{message}");
    assert!(
        !harness.log.lock().expect("lock").closed,
        "没连上就 nothing to close"
    );

    // 引导失败同理。
    let sink = Arc::new(RecordingSink::default());
    let connector = Connector::new(
        Arc::new(FakeOpener {
            url: Err(ChannelError::Transport {
                message: "dingtalk stream: open connection failed with status 401".to_string(),
            }),
            seen: Arc::new(Mutex::new(Vec::new())),
        }),
        Arc::new(FakeDialer {
            script: Arc::new(Mutex::new(VecDeque::new())),
            log: Arc::new(Mutex::new(FakeLog::default())),
            error: None,
            dialed: Arc::new(Mutex::new(Vec::new())),
        }),
        "app-key",
        AppSecret::new("app-secret"),
        Arc::clone(&sink) as Arc<dyn CallbackSink>,
    );
    let (_signal, handle) = StopSignal::pair();
    let error = connector.run_session(handle).await.expect_err("引导失败");
    assert!(!error.to_string().contains("app-secret"));
}

/// 心跳：ping 间隔到了就写一个 ping，且写到点还没成功 ⇒ 错误（上游：pong 写失败回错误）。
#[tokio::test]
async fn the_heartbeat_writes_a_ping_between_reads() {
    let harness = harness(
        vec![
            (Duration::from_millis(60), Some(Ok(WsEvent::Pong))),
            (
                Duration::from_millis(60),
                Some(Ok(WsEvent::Text(system_frame(
                    SYSTEM_TOPIC_DISCONNECT,
                    "bye",
                    "",
                )))),
            ),
            ending(),
        ],
        None,
        Duration::from_secs(5),
    );
    let connector = harness.connector.with_knobs(StreamKnobs {
        ping_interval: Duration::from_millis(40),
        read_deadline: Duration::from_secs(5),
        write_timeout: Duration::from_secs(1),
    });
    let (_signal, handle) = StopSignal::pair();
    let outcome = connector.run_session(handle).await.expect("干净返回");
    assert_eq!(outcome, SessionOutcome::DisconnectRequested);
    assert!(
        harness.log.lock().expect("lock").pings >= 1,
        "心跳至少写了一次 ping"
    );
}
