//! `telegram::api` 的用例（写者 M7-5）。
//!
//! 上游 `telegram_test.go` 的 `TestGetUpdates409IsErrConflict` / `TestRetryAfterOn429` /
//! `TestGetWebhookInfo` / `TestTransportErrorDoesNotExposeBotToken` 逐条移植，外加 wire 形状
//! （`getUpdates` 的 `offset` / `timeout` / `allowed_updates`、`sendMessage` 的话题与引用参数）
//! 的**逐字段**断言。
//!
//! 替身是一个**手写的 `tokio` TCP 服务端**（`crates/mc-channel` 的依赖面里没有 axum
//! —— `docs/60` §3.1 把依赖面冻结在 anchor，本片不得新增）：它逐条吐出脚本化的 HTTP 响应，
//! 并把收到的方法路径与请求体记下来。`docs/60` §4.2 的替身纪律第 1 条照旧：**只替平台
//! wire，不替业务路径**。
//!
//! ⚠️ 基址接缝是**进程全局**的（[`set_api_base`]）⇒ 用到替身的用例必须串行（[`STUB_LOCK`]）。

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::*;

/// 注入基址是进程全局的 ⇒ 用替身的用例串行。
static STUB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 一次被捕获的请求。
#[derive(Debug, Clone)]
struct Captured {
    /// 请求行里的目标（`/bot<token>/<method>`）。
    target: String,
    /// 请求体（JSON 文本）。
    body: String,
}

impl Captured {
    /// 请求体解析成 JSON（非 JSON ⇒ `Null`）。
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }

    /// 方法名（`target` 的最后一段）。
    fn method(&self) -> &str {
        self.target.rsplit('/').next().unwrap_or_default()
    }
}

/// 起一个脚本化的最小 HTTP/1.1 服务端：**每条连接一次请求**，按序吐出响应。
///
/// 返回 `(base, 捕获到的请求)`。脚本用尽后监听循环退出（后续调用会拿到连接错误）。
async fn spawn_stub(responses: Vec<(u16, String)>) -> (String, Arc<Mutex<Vec<Captured>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub");
    let address = listener.local_addr().expect("stub addr");
    let captured = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&captured);
    tokio::spawn(async move {
        for (status, body) in responses {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let request = read_request(&mut socket).await;
            sink.lock().expect("stub lock").push(request);
            let response = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.flush().await;
        }
    });
    (format!("http://{address}"), captured)
}

/// 读一条 HTTP/1.1 请求（先头部、再按 `content-length` 读体）。
async fn read_request(socket: &mut tokio::net::TcpStream) -> Captured {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    let header_end = loop {
        let Ok(read) = socket.read(&mut chunk).await else {
            return Captured {
                target: String::new(),
                body: String::new(),
            };
        };
        if read == 0 {
            break buffer.len();
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let target = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default()
        .to_string();
    let content_length: usize = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    let mut body = buffer[header_end..].to_vec();
    while body.len() < content_length {
        let Ok(read) = socket.read(&mut chunk).await else {
            break;
        };
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    Captured {
        target,
        body: String::from_utf8_lossy(&body[..content_length.min(body.len())]).to_string(),
    }
}

/// 一段 `getUpdates` 的单条消息结果（与上游 `TestConnectDispatchesAndAdvancesOffset` 同一形状）。
fn one_update_result() -> String {
    serde_json::json!({
        "ok": true,
        "result": [{
            "update_id": 10,
            "message": {
                "message_id": 1,
                "from": { "id": 42, "first_name": "A" },
                "chat": { "id": 42, "type": "private" },
                "text": "hi"
            }
        }]
    })
    .to_string()
}

/// 上游 `TestGetWebhookInfo`：`result` 解出 `url` 与 `pending_update_count`。
#[tokio::test]
async fn get_webhook_info_decodes_the_result() {
    let _guard = STUB_LOCK.lock().await;
    let (base, captured) = spawn_stub(vec![(
        200,
        serde_json::json!({
            "ok": true,
            "result": { "url": "https://example.test/telegram", "pending_update_count": 3 }
        })
        .to_string(),
    )])
    .await;
    set_api_base(&base);
    let info = JsonBotApi::new()
        .get_webhook_info("123:secret")
        .await
        .expect("webhook info");
    reset_api_base();
    assert_eq!(info.url, "https://example.test/telegram");
    assert_eq!(info.pending_update_count, 3);
    let requests = captured.lock().expect("captured");
    assert_eq!(requests[0].method(), "getWebhookInfo");
    assert_eq!(
        requests[0].target, "/bot123:secret/getWebhookInfo",
        "token 在 URL 路径里（这也正是错误文案不得带 URL 的原因）"
    );
}

/// `getMe` 解出 bot 身份，且**只订阅 message** 的 `getUpdates` 信封逐字段对齐上游。
#[tokio::test]
async fn get_me_and_get_updates_send_the_upstream_wire_shapes() {
    let _guard = STUB_LOCK.lock().await;
    let (base, captured) = spawn_stub(vec![
        (
            200,
            serde_json::json!({
                "ok": true,
                "result": { "id": 999, "is_bot": true, "first_name": "Acme", "username": "acme_bot" }
            })
            .to_string(),
        ),
        (200, one_update_result()),
    ])
    .await;
    set_api_base(format!("{base}/"));
    let api = JsonBotApi::new();
    let me = api.get_me("1:abc").await.expect("getMe");
    assert_eq!(me.id, 999);
    assert!(me.is_bot);
    assert_eq!(me.username, "acme_bot");

    let updates = api.get_updates("1:abc", 0).await.expect("getUpdates");
    reset_api_base();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].update_id, 10);
    let text = updates[0].message.as_deref().expect("message").text.clone();
    assert_eq!(text, "hi");

    let requests = captured.lock().expect("captured");
    assert_eq!(requests[0].method(), "getMe");
    assert_eq!(requests[0].body, "", "`getMe` 不带参数体");
    let poll = requests[1].json();
    assert_eq!(poll["offset"], 0);
    assert_eq!(poll["timeout"], LONG_POLL_TIMEOUT_SECS);
    assert_eq!(poll["allowed_updates"], serde_json::json!(["message"]));
}

/// 上游 `TestConnectDispatchesAndAdvancesOffset` 的**客户端**半边：`offset` 是调用方给的形参，
/// 客户端逐字送出去（回路的推进语义在 `mod.rs` 的用例里钉）。
#[tokio::test]
async fn get_updates_forwards_the_offset_verbatim() {
    let _guard = STUB_LOCK.lock().await;
    let (base, captured) = spawn_stub(vec![
        (200, r#"{"ok":true,"result":[]}"#.to_string()),
        (200, r#"{"ok":true,"result":[]}"#.to_string()),
    ])
    .await;
    set_api_base(&base);
    let api = JsonBotApi::new();
    api.get_updates("1:abc", 0).await.expect("first poll");
    api.get_updates("1:abc", 11).await.expect("second poll");
    reset_api_base();
    let requests = captured.lock().expect("captured");
    assert_eq!(requests[0].json()["offset"], 0);
    assert_eq!(requests[1].json()["offset"], 11);
}

/// 上游 `TestGetUpdates409IsErrConflict`：409 **不**走通用 API 错误，而是一个独立变体。
#[tokio::test]
async fn a_409_on_get_updates_is_a_conflict() {
    let _guard = STUB_LOCK.lock().await;
    let (base, _captured) = spawn_stub(vec![(
        409,
        serde_json::json!({
            "ok": false,
            "error_code": 409,
            "description": "Conflict: terminated by other getUpdates request"
        })
        .to_string(),
    )])
    .await;
    set_api_base(&base);
    let error = JsonBotApi::new()
        .get_updates("123:abc", 0)
        .await
        .expect_err("conflict");
    reset_api_base();
    assert_eq!(error, ApiError::Conflict);
    assert!(error.is_conflict());
    assert_eq!(error.method(), "getUpdates");
    assert!(error.to_string().contains("another instance"));
}

/// 上游 `TestRetryAfterOn429`：429 的强制退避；没带 `parameters.retry_after` 时按 1 秒。
#[tokio::test]
async fn a_429_carries_the_mandated_backoff() {
    let _guard = STUB_LOCK.lock().await;
    let (base, _captured) = spawn_stub(vec![
        (
            429,
            serde_json::json!({
                "ok": false,
                "error_code": 429,
                "description": "Too Many Requests",
                "parameters": { "retry_after": 7 }
            })
            .to_string(),
        ),
        (
            429,
            serde_json::json!({ "ok": false, "error_code": 429, "description": "Too Many Requests" })
                .to_string(),
        ),
    ])
    .await;
    set_api_base(&base);
    let api = JsonBotApi::new();
    let error = api
        .send_message("1:abc", &SendMessage::text(1, "x"))
        .await
        .expect_err("rate limited");
    assert_eq!(error.retry_after(), Some(Duration::from_secs(7)));
    assert_eq!(error.http_code(), Some(429));
    let second = api
        .send_message("1:abc", &SendMessage::text(1, "x"))
        .await
        .expect_err("rate limited");
    reset_api_base();
    assert_eq!(second.retry_after(), Some(Duration::from_secs(1)));
}

/// 上游 `TestTransportErrorDoesNotExposeBotToken`：连不上时**只回方法名**，
/// 绝不回显 URL（URL 里带 token）。
#[tokio::test]
async fn transport_failures_never_expose_the_bot_token() {
    let _guard = STUB_LOCK.lock().await;
    // 一个刚被关掉的端口：连接必然失败。
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("addr");
    drop(listener);
    set_api_base(format!("http://{address}"));
    let error = JsonBotApi::new()
        .get_me("123:DO-NOT-LOG-secret")
        .await
        .expect_err("transport failure");
    reset_api_base();
    assert_eq!(
        error,
        ApiError::Transport { method: "getMe" },
        "上游把 cause 留着（errors.Is）；本仓**故意丢弃**它 —— 见 docs/32 §17.2"
    );
    let rendered = format!("{error} {error:?}");
    assert!(!rendered.contains("123:DO-NOT-LOG-secret"), "{rendered}");
    assert!(!rendered.contains("http://"), "{rendered}");
}

/// 响应不是 JSON envelope / `result` 形状不对：都是 `Malformed`（**不是**传输失败）。
#[tokio::test]
async fn malformed_bodies_and_results_are_classified() {
    let _guard = STUB_LOCK.lock().await;
    let (base, _captured) = spawn_stub(vec![
        (200, "<html>proxy error</html>".to_string()),
        (
            200,
            r#"{"ok":true,"result":{"id":"not-a-number"}}"#.to_string(),
        ),
    ])
    .await;
    set_api_base(&base);
    let api = JsonBotApi::new();
    let not_json = api.get_me("1:abc").await.expect_err("malformed");
    assert_eq!(not_json, ApiError::Malformed { method: "getMe" });
    let wrong_shape = api.get_me("1:abc").await.expect_err("malformed");
    reset_api_base();
    assert_eq!(wrong_shape, ApiError::Malformed { method: "getMe" });
    assert!(wrong_shape.retry_after().is_none());
}

/// `sendMessage` 的话题 / 引用参数逐字段对齐上游 `sendMessageParams`（`omitempty` 的字段
/// 在零值时**不出现**）。
#[tokio::test]
async fn send_message_omits_zeroed_thread_and_reply_parameters() {
    let _guard = STUB_LOCK.lock().await;
    let (base, captured) = spawn_stub(vec![
        (200, r#"{"ok":true,"result":{"message_id":5}}"#.to_string()),
        (200, r#"{"ok":true,"result":{"message_id":6}}"#.to_string()),
    ])
    .await;
    set_api_base(&base);
    let api = JsonBotApi::new();
    let plain = api
        .send_message("1:abc", &SendMessage::text(42, "hello"))
        .await
        .expect("plain send");
    assert_eq!(plain.message_id, 5);
    let threaded = api
        .send_message(
            "1:abc",
            &SendMessage::text(42, "hello")
                .in_thread(77)
                .with_reply_to(9),
        )
        .await
        .expect("threaded send");
    reset_api_base();
    assert_eq!(threaded.message_id, 6);

    let requests = captured.lock().expect("captured");
    let plain_body = requests[0].json();
    assert_eq!(plain_body["chat_id"], 42);
    assert_eq!(plain_body["text"], "hello");
    assert_eq!(plain_body["message_thread_id"], Value::Null, "零值不出现");
    assert_eq!(plain_body["reply_parameters"], Value::Null, "零值不出现");
    assert_eq!(plain_body["parse_mode"], Value::Null, "判决回复走纯文本");

    let threaded_body = requests[1].json();
    assert_eq!(threaded_body["message_thread_id"], 77);
    assert_eq!(threaded_body["reply_parameters"]["message_id"], 9);
    assert_eq!(
        threaded_body["reply_parameters"]["allow_sending_without_reply"],
        true
    );
}

/// `sendChatAction` 的动作恒为 `typing`，并带上话题（上游 `SendChatAction`）。
#[tokio::test]
async fn send_chat_action_uses_typing_and_preserves_the_topic() {
    let _guard = STUB_LOCK.lock().await;
    let (base, captured) = spawn_stub(vec![
        (200, r#"{"ok":true,"result":true}"#.to_string()),
        (200, r#"{"ok":true,"result":true}"#.to_string()),
    ])
    .await;
    set_api_base(&base);
    let api = JsonBotApi::new();
    api.send_chat_action("1:abc", 42, 8).await.expect("topic");
    api.send_chat_action("1:abc", 42, 0)
        .await
        .expect("no topic");
    reset_api_base();
    let requests = captured.lock().expect("captured");
    assert_eq!(requests[0].method(), "sendChatAction");
    assert_eq!(requests[0].json()["action"], "typing");
    assert_eq!(requests[0].json()["message_thread_id"], 8);
    assert_eq!(requests[1].json()["message_thread_id"], Value::Null);
}

/// 基址接缝：默认是生产主机；注入后生效；清掉后回到默认（末尾无 `/`）。
#[tokio::test]
async fn the_api_base_seam_round_trips() {
    let _guard = STUB_LOCK.lock().await;
    reset_api_base();
    assert_eq!(api_base(), DEFAULT_API_BASE);
    set_api_base("http://127.0.0.1:1/");
    assert_eq!(api_base(), "http://127.0.0.1:1", "注入值去掉尾部斜杠");
    reset_api_base();
    assert_eq!(api_base(), DEFAULT_API_BASE);
}

/// 两个客户端的超时：长轮询的必须**大于**服务端挂起时间，否则我们会先掐掉自己。
#[test]
fn the_poll_timeout_exceeds_the_server_side_hold() {
    assert!(POLL_TIMEOUT > Duration::from_secs(u64::from(LONG_POLL_TIMEOUT_SECS)));
    assert!(CREDENTIAL_TIMEOUT < POLL_TIMEOUT);
}

/// 错误码缺省时回落 HTTP 状态（本仓的小加固，见 `call_raw` 的注释）。
#[tokio::test]
async fn a_missing_error_code_falls_back_to_the_http_status() {
    let _guard = STUB_LOCK.lock().await;
    let (base, _captured) = spawn_stub(vec![(
        401,
        r#"{"ok":false,"description":"Unauthorized"}"#.to_string(),
    )])
    .await;
    set_api_base(&base);
    let error = JsonBotApi::new()
        .get_me("1:abc")
        .await
        .expect_err("unauthorized");
    reset_api_base();
    assert_eq!(error.http_code(), Some(401));
    // 安装面的分类判据正是 401（`install::classify_credential_verification_error`）。
    assert_eq!(error.method(), "getMe");
    assert!(!error.is_conflict());
}
