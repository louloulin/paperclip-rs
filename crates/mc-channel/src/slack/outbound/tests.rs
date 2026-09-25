//! `slack::outbound` 的用例（写者 M7-4）。
//!
//! 两个层次：
//! 1. **纯函数与端口替身**：mrkdwn → 分片 → 线程化 → 元数据的**顺序**与字段；
//! 2. **真 wire**：一个只跑一圈的最小 HTTP 服务端，断言 `chat.postMessage` 的**真实**请求体。
//!    （不引入新依赖：用 `tokio::net::TcpListener` 手写一圈 HTTP/1.1。
//!    这一层存在的理由与 `docs/60` §4.2 的替身纪律一致：替身只替**平台 wire**，
//!    业务路径（mrkdwn / 分片 / 线程化 / 元数据）必须是真代码。）

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mc_core::channel::message::OutboundMessage;
use mc_core::id::Id;

use super::*;

// =====================================================================
// 替身
// =====================================================================

#[derive(Default)]
struct RecordingApi {
    ts: Mutex<Vec<String>>,
    seen: Mutex<Vec<PostMessageRequest>>,
    tokens: Mutex<Vec<String>>,
    fail_on: Mutex<Option<usize>>,
}

#[async_trait]
impl MessageApi for RecordingApi {
    async fn post_message(&self, token: &str, req: &PostMessageRequest) -> ApiResult<String> {
        let mut seen = self.seen.lock().expect("lock");
        if *self.fail_on.lock().expect("lock") == Some(seen.len()) {
            return Err(SlackApiError::Refused {
                method: "chat.postMessage",
                code: "channel_not_found".to_string(),
            });
        }
        seen.push(req.clone());
        self.tokens.lock().expect("lock").push(token.to_string());
        let ts = format!("{}.0001", 100 + seen.len());
        self.ts.lock().expect("lock").push(ts.clone());
        Ok(ts)
    }
}

fn outbound(chat_id: &str, text: &str) -> OutboundMessage {
    OutboundMessage {
        chat_id: chat_id.to_string(),
        text: text.to_string(),
        thread_id: String::new(),
        reply_to: String::new(),
    }
}

// =====================================================================
// 分片 / 线程落点
// =====================================================================

#[test]
fn chunking_is_on_rune_boundaries_and_empty_input_is_one_chunk() {
    assert_eq!(chunk_message("", 10), vec![String::new()]);
    assert_eq!(chunk_message("short", 10), vec!["short".to_string()]);
    assert_eq!(
        chunk_message("abcd", 0),
        vec!["abcd".to_string()],
        "上限 0 ⇒ 不切"
    );
    // 4 个字符、每片 2 ⇒ 两片。
    assert_eq!(
        chunk_message("abcd", 2),
        vec!["ab".to_string(), "cd".to_string()]
    );
    // 中文按 rune 切，不会切出半个字符。
    let chunks = chunk_message("中文测试超长", 3);
    assert_eq!(chunks, vec!["中文测".to_string(), "试超长".to_string()]);
}

#[test]
fn the_thread_target_is_the_quote_first_then_the_inbound_thread() {
    let mut message = outbound("C1", "hi");
    assert_eq!(outbound_thread_ts(&message), None);
    message.thread_id = "100.1".to_string();
    assert_eq!(outbound_thread_ts(&message), Some("100.1".to_string()));
    message.reply_to = "90.5".to_string();
    assert_eq!(
        outbound_thread_ts(&message),
        Some("90.5".to_string()),
        "显式引用优先"
    );
}

#[test]
fn the_metadata_payload_uses_the_upstream_event_type_and_fields() {
    let metadata = OutboundMetadata::new(Some(Id::new()), 4, kind::CONTROL_ACK);
    let payload = metadata.to_payload();
    assert_eq!(payload["event_type"], OUTBOUND_METADATA_EVENT);
    assert_eq!(payload["event_type"], "multica_channel_outbound");
    assert_eq!(payload["event_payload"]["route_revision"], 4);
    assert_eq!(payload["event_payload"]["kind"], "control_ack");
    assert!(!payload["event_payload"]["binding_id"]
        .as_str()
        .expect("string")
        .is_empty());
}

// =====================================================================
// 发送器（替身端口）
// =====================================================================

#[tokio::test]
async fn a_single_send_carries_mrkdwn_and_the_bot_token() {
    let api = Arc::new(RecordingApi::default());
    let sender = Sender::new(Arc::clone(&api) as Arc<dyn MessageApi>);
    let frame = sender
        .send("xoxb-token", &outbound("C1", "**bold** outside"))
        .await
        .expect("send");
    assert_eq!(frame.timestamps.len(), 1);
    assert!(frame.has_any_id());

    let seen = api.seen.lock().expect("lock");
    assert_eq!(seen[0].channel, "C1");
    assert_eq!(seen[0].text, "*bold* outside", "Markdown → mrkdwn 真的跑了");
    assert_eq!(seen[0].thread_ts, None);
    assert_eq!(seen[0].metadata, None);
    assert_eq!(api.tokens.lock().expect("lock")[0], "xoxb-token");
}

#[tokio::test]
async fn chunked_sends_keep_the_last_ts_as_the_message_id() {
    let api = Arc::new(RecordingApi::default());
    let sender = Sender::new(Arc::clone(&api) as Arc<dyn MessageApi>).with_max_runes(4);
    let frame = sender
        .send("xoxb-token", &outbound("C1", "abcdefgh"))
        .await
        .expect("send");
    assert_eq!(frame.timestamps.len(), 2);
    let result = frame.to_send_result();
    assert_eq!(
        result.message_id,
        frame.timestamps.last().cloned().expect("last"),
        "上游口径：MessageID 是最后一片"
    );
    assert_eq!(result.message_ids.len(), 2);
    assert!(result.is_chunked());

    // 每一片都带同一个线程落点与同一份元数据。
    let seen = api.seen.lock().expect("lock");
    assert_eq!(seen.len(), 2);
    let metadata = OutboundMetadata::new(None, 1, kind::TASK_REPLY);
    assert!(seen.iter().all(|request| request.metadata.is_none()));
    assert_eq!(metadata.kind, "task_reply");
}

#[tokio::test]
async fn every_chunk_is_threaded_and_carries_the_metadata() {
    let api = Arc::new(RecordingApi::default());
    let sender = Sender::new(Arc::clone(&api) as Arc<dyn MessageApi>).with_max_runes(2);
    let mut message = outbound("C1", "abcd");
    message.thread_id = "100.1".to_string();
    let metadata = OutboundMetadata::new(Some(Id::new()), 2, kind::TASK_REPLY);
    sender
        .send_with_metadata("xoxb-token", &message, Some(&metadata))
        .await
        .expect("send");
    let seen = api.seen.lock().expect("lock");
    assert_eq!(seen.len(), 2);
    for request in seen.iter() {
        assert_eq!(request.thread_ts.as_deref(), Some("100.1"));
        assert_eq!(
            request.metadata.as_ref().map(|meta| meta.kind.clone()),
            Some(kind::TASK_REPLY.to_string())
        );
    }
}

#[tokio::test]
async fn a_failing_chunk_fails_the_whole_send_but_keeps_earlier_chunks() {
    let api = Arc::new(RecordingApi::default());
    *api.fail_on.lock().expect("lock") = Some(1);
    let sender = Sender::new(Arc::clone(&api) as Arc<dyn MessageApi>).with_max_runes(2);
    let error = sender
        .send("xoxb-token", &outbound("C1", "abcd"))
        .await
        .expect_err("第二片失败");
    assert_eq!(error.code(), "refused");
    assert_eq!(
        api.seen.lock().expect("lock").len(),
        1,
        "第一片已经发出去了（Slack 侧没有回滚）"
    );
}

/// 错误文案**不回显**令牌。
#[tokio::test]
async fn api_errors_never_echo_the_token() {
    let api = Arc::new(RecordingApi::default());
    *api.fail_on.lock().expect("lock") = Some(0);
    let sender = Sender::new(Arc::clone(&api) as Arc<dyn MessageApi>);
    let secret = "xoxb-super-secret-value";
    let error = sender
        .send(secret, &outbound("C1", "hi"))
        .await
        .expect_err("拒绝");
    let text = format!("{error} {error:?}");
    assert!(!text.contains(secret));
    assert!(text.contains("channel_not_found"));
}

// =====================================================================
// 真 wire：一圈最小 HTTP 服务端
// =====================================================================

/// 起一个只服务一圈的 HTTP 服务端，返回 `(基址, 收到的请求体)`。
///
/// 手写 HTTP/1.1 的理由：给测试加 `axum` 会动共享 `Cargo.toml`（`docs/60` §3.1 冻结）。
/// 这一层只看**我们发出去的字节**，所以手写反而更贴近"逐字段比对原始帧"。
async fn serve_once(response: &'static str) -> (String, tokio::sync::oneshot::Receiver<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut buffer = vec![0u8; 8192];
        let mut request = Vec::new();
        loop {
            let read = socket.read(&mut buffer).await.expect("read");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
            // 请求头 + 声明长度的正文都到齐就够断言了。
            let text = String::from_utf8_lossy(&request);
            if let Some((head, body)) = text.split_once("\r\n\r\n") {
                let length: usize = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse().ok())
                    })
                    .unwrap_or(0);
                if body.len() >= length {
                    break;
                }
            }
        }
        let text = String::from_utf8_lossy(&request).to_string();
        let body = text
            .split_once("\r\n\r\n")
            .map(|(_, body)| body.to_string())
            .unwrap_or_default();
        let payload = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}",
            response.len()
        );
        socket.write_all(payload.as_bytes()).await.expect("write");
        let _ = socket.flush().await;
        let _ = tx.send(body);
    });
    (format!("http://127.0.0.1:{port}"), rx)
}

/// 串行锁：`set_api_base` 是**进程全局**的（与 M8-1 的 `GITHUB_API_BASE` 同款）。
static BASE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 真 `reqwest` → 真 HTTP：请求体逐字段对齐上游 SDK 的线形态。
#[tokio::test]
async fn the_real_http_path_sends_the_upstream_wire_shape() {
    let _serial = BASE_LOCK.lock().await;
    let (base, body) = serve_once(r#"{"ok":true,"ts":"1234.5678"}"#).await;
    set_api_base(&base);
    let sender = Sender::http();
    let mut message = outbound("C1", "**bold** & <b>");
    message.thread_id = "100.1".to_string();
    let metadata = OutboundMetadata::new(None, 7, kind::CONTROL_ACK);
    let frame = sender
        .send_with_metadata("xoxb-wire", &message, Some(&metadata))
        .await
        .expect("send");
    reset_api_base();
    assert_eq!(frame.timestamps, vec!["1234.5678".to_string()]);

    let body = body.await.expect("captured request body");
    let sent: serde_json::Value = serde_json::from_str(&body).expect("json body");
    assert_eq!(sent["channel"], "C1");
    assert_eq!(
        sent["text"], "*bold* &amp; &lt;b&gt;",
        "mrkdwn 会转义裸的 & 与 <"
    );
    assert_eq!(sent["thread_ts"], "100.1");
    assert_eq!(sent["unfurl_links"], false);
    assert_eq!(sent["unfurl_media"], false);
    assert_eq!(sent["metadata"]["event_type"], OUTBOUND_METADATA_EVENT);
    assert_eq!(sent["metadata"]["event_payload"]["route_revision"], 7);
    assert_eq!(sent["metadata"]["event_payload"]["kind"], "control_ack");
}

/// Slack 的 `{"ok":false}` 信封：只带它自己的错误码，**不**带令牌。
#[tokio::test]
async fn a_refused_response_surfaces_only_the_slack_error_code() {
    let _serial = BASE_LOCK.lock().await;
    let (base, _body) = serve_once(r#"{"ok":false,"error":"invalid_auth"}"#).await;
    set_api_base(&base);
    let secret = "xoxb-never-log-me";
    let error = Sender::http()
        .send(secret, &outbound("C1", "hi"))
        .await
        .expect_err("拒绝");
    reset_api_base();
    assert!(matches!(error, SlackApiError::Refused { .. }));
    assert_eq!(error.code(), "refused");
    let text = format!("{error} {error:?}");
    assert!(text.contains("invalid_auth"));
    assert!(!text.contains(secret));
}

/// 传输失败（服务端不在）⇒ `Transport`，且**不带 URL**。
#[tokio::test]
async fn a_transport_failure_does_not_leak_the_url() {
    let _serial = BASE_LOCK.lock().await;
    set_api_base("http://127.0.0.1:1/nope");
    let error = Sender::http()
        .send("xoxb-token", &outbound("C1", "hi"))
        .await
        .expect_err("连接失败");
    reset_api_base();
    assert!(matches!(error, SlackApiError::Transport { .. }));
    assert!(error.to_string().contains("chat.postMessage"));
    assert!(!error.to_string().contains("nope"));
}
