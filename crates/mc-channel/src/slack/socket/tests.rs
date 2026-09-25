//! Socket Mode 传输面与工厂的用例（信封帧、ACK 顺序、接收循环、工厂校验）。

use super::*;
use std::sync::{Arc, Mutex};

// ---- Socket Mode 信封 ----

/// 信封解析：四种形态 + 三类畸形帧。
#[test]
fn socket_frames_parse() {
    let hello = parse_socket_frame(r#"{"type":"hello","num_connections":1}"#).expect("hello");
    assert_eq!(hello, SocketFrame::Hello);
    assert!(hello.envelope_id().is_none());
    assert!(!hello.needs_ack());

    let events = parse_socket_frame(
        r#"{"type":"events_api","envelope_id":"e1","accepts_response_payload":false,
            "payload":{"team_id":"T1","api_app_id":"A1","event":{"type":"message"}}}"#,
    )
    .expect("events_api");
    assert_eq!(events.envelope_id(), Some("e1"));
    assert!(events.needs_ack());
    match &events {
        SocketFrame::EventsApi { payload, .. } => {
            let parsed = parse_events_api(payload).expect("payload 可解");
            assert_eq!(parsed.team_id, "T1");
            assert_eq!(parsed.api_app_id, "A1");
        }
        other => panic!("expected events_api, got {other:?}"),
    }

    let slash =
        parse_socket_frame(r#"{"type":"slash_commands","envelope_id":"e2"}"#).expect("slash");
    assert_eq!(
        slash,
        SocketFrame::SlashCommand {
            envelope_id: "e2".to_string()
        }
    );
    assert!(slash.needs_ack());

    let disconnect =
        parse_socket_frame(r#"{"type":"disconnect","reason":"warning"}"#).expect("disconnect");
    assert_eq!(
        disconnect,
        SocketFrame::Disconnect {
            reason: "warning".to_string()
        }
    );
    assert!(!disconnect.needs_ack(), "disconnect 不回 ACK");

    let unknown =
        parse_socket_frame(r#"{"type":"future_thing","envelope_id":"e3"}"#).expect("unknown");
    assert_eq!(
        unknown,
        SocketFrame::Other {
            kind: "future_thing".to_string(),
            envelope_id: Some("e3".to_string())
        }
    );
    assert!(unknown.needs_ack(), "认不出类型也要 ACK（否则对方重投）");

    // 畸形：非 JSON / 缺 type / events_api 缺 envelope_id / 缺 payload。
    assert_eq!(parse_socket_frame("not json"), Err(FrameError::NotJson));
    assert_eq!(parse_socket_frame("{}"), Err(FrameError::MissingType));
    assert_eq!(
        parse_socket_frame(r#"{"type":"events_api","payload":{}}"#),
        Err(FrameError::MissingEnvelopeId)
    );
    assert_eq!(
        parse_socket_frame(r#"{"type":"events_api","envelope_id":"e1"}"#),
        Err(FrameError::MissingPayload)
    );
    assert_eq!(
        ack_json("e1"),
        r#"{"envelope_id":"e1"}"#,
        "ACK 的线形态逐字"
    );
}

/// 判决表：`events_api` 投递、`disconnect` 重连、其余忽略。
#[test]
fn frame_actions() {
    let frame = parse_socket_frame(
        r#"{"type":"events_api","envelope_id":"e1","payload":{"team_id":"T1","api_app_id":"A1",
            "event":{"type":"message","user":"UALICE","text":"hi","channel":"D1",
                     "channel_type":"im","ts":"1.1"}}}"#,
    )
    .expect("frame");
    match dispatch_frame(&frame, "UBOT") {
        FrameAction::Dispatch(message) => {
            assert_eq!(message.message_id, "1.1");
            assert_eq!(message.source.chat_id, "D1");
        }
        other => panic!("expected dispatch, got {other:?}"),
    }
    assert_eq!(
        dispatch_frame(&SocketFrame::Hello, "UBOT"),
        FrameAction::Ignore
    );
    assert_eq!(
        dispatch_frame(
            &SocketFrame::Disconnect {
                reason: "warning".to_string()
            },
            "UBOT"
        ),
        FrameAction::Reconnect("warning".to_string())
    );
    // 认得出类型但核心不摄入的事件 ⇒ 忽略（不是错误）。
    let ignored = parse_socket_frame(
        r#"{"type":"events_api","envelope_id":"e2","payload":{"event":{"type":"reaction_added"}}}"#,
    )
    .expect("frame");
    assert_eq!(dispatch_frame(&ignored, "UBOT"), FrameAction::Ignore);
}

// ---- 接收循环（替身传输） ----

/// 脚本体：喂预设帧，记录发出的 ACK。
#[derive(Default)]
struct ScriptedTransport {
    frames: Mutex<Vec<String>>,
    sent: Arc<Mutex<Vec<String>>>,
}

struct ScriptedSession {
    frames: Vec<String>,
    sent: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl SocketTransport for ScriptedTransport {
    async fn connect(&self, _app_token: &str) -> ChannelResult<Box<dyn SocketSession>> {
        let mut frames = self.frames.lock().expect("lock");
        let frames = std::mem::take(&mut *frames);
        Ok(Box::new(ScriptedSession {
            frames,
            sent: Arc::clone(&self.sent),
        }))
    }
}

#[async_trait]
impl SocketSession for ScriptedSession {
    async fn next_text(&mut self) -> Option<ChannelResult<String>> {
        (!self.frames.is_empty()).then(|| Ok(self.frames.remove(0)))
    }
    async fn send_text(&mut self, text: &str) -> ChannelResult<()> {
        self.sent.lock().expect("lock").push(text.to_string());
        Ok(())
    }
}

/// 记录投递的 handler 替身。
#[derive(Default)]
struct RecordingHandler {
    delivered: Mutex<Vec<InboundMessage>>,
}

#[async_trait]
impl crate::message::InboundHandler for RecordingHandler {
    async fn handle(&self, message: InboundMessage) -> ChannelResult<()> {
        self.delivered.lock().expect("lock").push(message);
        Ok(())
    }
}

/// 接收循环：ACK 在**投递之前**发出、握手帧不 ACK、坏帧不致命、
/// 流结束 ⇒ 链路错误（supervisor 据此退避重连）。
#[tokio::test]
async fn connect_acks_before_dispatching_and_survives_bad_frames() {
    let transport = Arc::new(ScriptedTransport::default());
    let sent = Arc::clone(&transport.sent);
    transport.frames.lock().expect("lock").extend([
        r#"{"type":"hello"}"#.to_string(),
        "not a frame".to_string(),
        r#"{"type":"events_api","envelope_id":"e1","payload":{"team_id":"T1","api_app_id":"A1",
            "event":{"type":"message","user":"UALICE","text":"hello","channel":"D1",
                     "channel_type":"im","ts":"1.1"}}}"#
            .to_string(),
    ]);
    let handler = Arc::new(RecordingHandler::default());
    let channel = SlackChannel::new(
        "A1",
        "UBOT",
        "xapp-1",
        "xoxb-1",
        Some(Arc::clone(&handler) as SharedInboundHandler),
        transport,
    );
    let error = channel.connect().await.expect_err("流结束必须是链路错误");
    assert_eq!(error.code(), "channel_transport_error");

    let delivered = handler.delivered.lock().expect("lock");
    assert_eq!(delivered.len(), 1, "只投递那条可摄入的事件");
    assert_eq!(delivered[0].text, "hello");
    drop(delivered);
    let sent = sent.lock().expect("lock").clone();
    assert_eq!(
        sent,
        vec![ack_json("e1")],
        "只有 events_api 帧被 ACK，且内容是纯 ACK"
    );
}

/// 缺 handler / 缺 app token ⇒ 明说，而不是静默假连。
#[tokio::test]
async fn connect_requires_handler_and_app_token() {
    let channel = SlackChannel::new(
        "A1",
        "UBOT",
        "",
        "",
        None,
        Arc::new(ScriptedTransport::default()),
    );
    let error = channel.connect().await.expect_err("没有 handler");
    assert!(error.to_string().contains("inbound handler"));

    let handler = Arc::new(RecordingHandler::default());
    let channel = SlackChannel::new(
        "A1",
        "UBOT",
        "",
        "",
        Some(Arc::clone(&handler) as SharedInboundHandler),
        Arc::new(ScriptedTransport::default()),
    );
    let error = channel.connect().await.expect_err("没有 app token");
    assert!(error.to_string().contains("app-level token"));
}

/// `disconnect` 帧 ⇒ 退出让 supervisor 重连（不投递、不 ACK）。
#[tokio::test]
async fn disconnect_frame_asks_for_a_reconnect() {
    let transport = Arc::new(ScriptedTransport::default());
    let sent = Arc::clone(&transport.sent);
    transport
        .frames
        .lock()
        .expect("lock")
        .push(r#"{"type":"disconnect","reason":"warning"}"#.to_string());
    let handler = Arc::new(RecordingHandler::default());
    let channel = SlackChannel::new(
        "A1",
        "UBOT",
        "xapp-1",
        "xoxb-1",
        Some(Arc::clone(&handler) as SharedInboundHandler),
        transport,
    );
    let error = channel.connect().await.expect_err("disconnect ⇒ 错误");
    assert!(error.to_string().contains("warning"));
    assert!(sent.lock().expect("lock").is_empty());
    assert!(handler.delivered.lock().expect("lock").is_empty());
}

/// 工厂：配置合法 ⇒ 造出 `Slack` 的 Channel；缺 `xapp-` / 配置解不开 ⇒ `InvalidConfig`。
#[test]
fn factory_validates_the_config() {
    use base64::Engine as _;

    let deps = SlackDeps::plaintext();
    let raw = serde_json::json!({
        "app_id": "A1",
        "bot_user_id": "UBOT",
        "bot_token_encrypted": base64::engine::general_purpose::STANDARD.encode("xoxb-bot"),
        "app_token_encrypted": base64::engine::general_purpose::STANDARD.encode("xapp-app"),
    });
    let config = ChannelConfig {
        kind: TYPE_SLACK,
        raw,
        installation_id: None,
        handler: None,
    };
    let channel = factory(&deps)(config.clone()).expect("合法配置");
    assert_eq!(channel.kind(), TYPE_SLACK);
    assert!(channel.capabilities().has(Capability::TEXT));

    // 缺 `xapp-` ⇒ 拒（上游 `installation has no app-level token`）。
    let no_app = ChannelConfig {
        raw: serde_json::json!({ "app_id": "A1" }),
        ..config.clone()
    };
    let Err(error) = factory(&deps)(no_app) else {
        panic!("没有 app token 必须拒装配")
    };
    assert_eq!(error.code(), "channel_invalid_config");
    assert!(error.to_string().contains("app-level token"));

    // 不是对象 ⇒ 拒。
    let garbage = ChannelConfig {
        raw: serde_json::json!("nope"),
        ..config
    };
    let Err(error) = factory(&deps)(garbage) else {
        panic!("配置解不开必须拒装配")
    };
    assert_eq!(error.code(), "channel_invalid_config");
}

/// 出站：**未注入发送器**时失败关闭（M7-4 把接线点放在 `with_outbound`，
/// 而工厂**总是**注入 [`Sender::http`] ⇒ 这条只覆盖"有人手工装配却没接线"的形态）。
#[tokio::test]
async fn send_is_fail_closed_when_no_sender_is_wired() {
    let channel = SlackChannel::new(
        "A1",
        "UBOT",
        "xapp-1",
        "xoxb-1",
        None,
        Arc::new(ScriptedTransport::default()),
    );
    let error = channel
        .send(OutboundMessage {
            chat_id: "C1".to_string(),
            text: "hi".to_string(),
            thread_id: String::new(),
            reply_to: String::new(),
        })
        .await
        .expect_err("出站未接线");
    assert!(error.to_string().contains("not wired"));
    assert!(channel.disconnect().await.is_ok());
}

/// `DoD` 第 6 条：`SlackChannel` 的 `Debug` 不回显两个令牌。
#[test]
fn channel_debug_redacts_tokens() {
    let channel = SlackChannel::new(
        "A1",
        "UBOT",
        "xapp-DO-NOT-LOG",
        "xoxb-DO-NOT-LOG",
        None,
        Arc::new(ScriptedTransport::default()),
    );
    let rendered = format!("{channel:?}");
    assert!(!rendered.contains("xapp-DO-NOT-LOG"));
    assert!(!rendered.contains("xoxb-DO-NOT-LOG"));
    assert!(rendered.contains("Sensitive(<redacted>)"));
}
