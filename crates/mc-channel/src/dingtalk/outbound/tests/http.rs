//! `dingtalk::outbound` 的**真 HTTP** 用例（写者 M7-8）。
//!
//! 与 `../tests.rs` 分工：那边是纯函数与端口替身；这边是**真 `reqwest` → 本地 HTTP 替身**，
//! 断言替身实际收到的原始帧（`docs/60` §4.2 的端到端回路那一格）。
//!
//! 关于"本地替身落在哪一侧"（诚实交代）：入站那半条回路（Stream WS 帧 ⇒ 归一化）由 M7-7
//! 的 `stream/tests.rs` + `inbound/tests.rs` 用**同一批** golden 与一个脚本化内存 socket
//! 覆盖（`tokio-tungstenite` 在本仓**没有** `handshake` feature ⇒ 起不了真实 WS 服务端；
//! 见 `docs/32` §22 的 D6）。本文件承担**出站**那半条：真 socket、真 HTTP、真 `reqwest`。

use std::sync::Arc;

use mc_core::channel::message::{ChatType, MessageKind};

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::{
    api_base, credentials, reset_api_base, set_api_base, DingTalkApiError, HttpOpenApi,
    OpenApiTransport, OutboundDelivery, RecordingTransport, SendTarget, Sender, ACCESS_TOKEN_PATH,
    DEFAULT_API_BASE, PATH_SEND_GROUP, PATH_SEND_P2P,
};
use crate::dingtalk::inbound::{inbound_from_callback, BotCallbackData};

const QUOTED_INTERACTIVE_CARD: &str = include_str!("../../testdata/quoted_interactive_card.json");
const QUOTED_CARD_LINK_GROUP: &str = include_str!("../../testdata/quoted_card_link_group.json");
const QUOTED_CARD_LINK_PRIVATE: &str = include_str!("../../testdata/quoted_card_link_private.json");
const QUOTED_BOT_CHANNELS: &str = include_str!("../../testdata/quoted_bot_channels.json");

// =====================================================================

/// 串行锁：`set_api_base` 是**进程全局**的。
static BASE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 起一个服务 `expected` 个请求的小 HTTP 服务端，返回基址与"收到的请求体 + 路径"。
async fn serve(
    expected: usize,
    responses: Vec<(&'static str, String)>,
) -> (String, tokio::sync::mpsc::Receiver<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let address = listener.local_addr().expect("local addr");
    let (sender, receiver) = tokio::sync::mpsc::channel(expected.max(1));
    tokio::spawn(async move {
        let mut responses = responses.into_iter();
        for _ in 0..expected {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buffer = vec![0_u8; 16 * 1024];
            let mut request = Vec::new();
            loop {
                let read = socket.read(&mut buffer).await.unwrap_or(0);
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
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
            let _ = sender.send(text).await;
            let (status, response) = responses
                .next()
                .unwrap_or_else(|| ("200 OK", r#"{"processQueryKey":"k"}"#.to_string()));
            let payload = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
                 connection: close\r\n\r\n{response}",
                response.len()
            );
            let _ = socket.write_all(payload.as_bytes()).await;
            let _ = socket.flush().await;
        }
    });
    (format!("http://{address}"), receiver)
}

fn request_body(raw: &str) -> Value {
    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .expect("有正文");
    serde_json::from_str(body).expect("请求体是 JSON")
}

/// **端到端回路**：golden 回调 → 真归一化 → 真 `reqwest` → 本地替身；断言替身收到的**原始帧**。
///
/// 入站帧用的就是 M7-7 落的那四份上游 golden。四帧的顺序是确定的（发送是串行的）：
/// 令牌 → 文本（群）→ 引用卡片（群）→ 媒体占位（群）→ 互动卡片（**私聊**，不带引用）。
#[tokio::test]
#[allow(clippy::too_many_lines)] // 一条回路要从入站 golden 一路断言到替身收到的帧，拆开就没有"回路"了
async fn the_end_to_end_loop_sends_the_upstream_frame_for_the_golden_quotes() {
    let _serial = BASE_LOCK.lock().await;
    let (base, mut received) = serve(
        5,
        vec![
            (
                "200 OK",
                r#"{"accessToken":"tok-1","expireIn":7200}"#.to_string(),
            ),
            ("200 OK", r#"{"processQueryKey":"k-text"}"#.to_string()),
            ("200 OK", r#"{"processQueryKey":"k-card"}"#.to_string()),
            ("200 OK", r#"{"processQueryKey":"k-media"}"#.to_string()),
            ("200 OK", r#"{"processQueryKey":"k-dm"}"#.to_string()),
        ],
    )
    .await;
    set_api_base(&base);
    let sender = Sender::http(&credentials());

    // ① 文本：无引用。
    let key = sender
        .send(&SendTarget::group("group-e2e"), "plain answer")
        .await
        .expect("文本");
    assert_eq!(key, "k-text");

    // ② 引用卡片（群）：golden 解码 → 真归一化 → 引用正文进 `msgParam`。
    let callback: BotCallbackData =
        serde_json::from_str(QUOTED_CARD_LINK_GROUP).expect("golden 是回调");
    let message = inbound_from_callback(Some(&callback), "app-key").expect("有发送者");
    assert_eq!(
        message.source.chat_type,
        ChatType::Group,
        "golden 的 conversationType = 2"
    );
    assert_eq!(message.command_text, "QUOTE-CAPTURE-7429");
    assert!(message.has_selected_context, "这张卡片是被选中的引用");
    OutboundDelivery::new(
        SendTarget::group("group-e2e"),
        &message.command_text,
        "answer for the quoted card",
    )
    .deliver(&sender)
    .await
    .expect("引用卡片");

    // ③ 媒体占位：密封输入里的内部附件链接在 wire 上变成 `[Image]`。
    let sealed = "look at this ![](/api/attachments/abc/download) please";
    OutboundDelivery::new(
        SendTarget::group("group-e2e"),
        sealed,
        "answer for the media",
    )
    .deliver(&sender)
    .await
    .expect("媒体占位");

    // ④ 互动卡片（**私聊**容器）：上线是 `oToMessages/batchSend`，且**不带**引用。
    let private: BotCallbackData =
        serde_json::from_str(QUOTED_INTERACTIVE_CARD).expect("golden 是回调");
    let dm = inbound_from_callback(Some(&private), "app-key").expect("有发送者");
    assert_eq!(dm.source.chat_type, ChatType::P2p);
    assert_eq!(dm.kind, MessageKind::Text);
    let target = SendTarget::from_message(&dm);
    assert!(!target.is_group(), "conversationType = 1 ⇒ 直聊");
    OutboundDelivery::new(target, &dm.command_text, "answer for the interactive card")
        .deliver(&sender)
        .await
        .expect("互动卡片");

    // 另外两份 golden 也要能解码（它们各有两种容器，M7-7 已逐条钉住归一化）。
    let private_card: BotCallbackData =
        serde_json::from_str(QUOTED_CARD_LINK_PRIVATE).expect("golden 是回调");
    assert!(inbound_from_callback(Some(&private_card), "app-key").is_some());
    let bot_channels: Vec<Value> = serde_json::from_str(QUOTED_BOT_CHANNELS).expect("golden 数组");
    assert!(bot_channels.len() >= 2, "两种容器的 fixture 都在");

    reset_api_base();

    // ---- 替身收到的原始帧（逐字段） ----
    let token_request = received.recv().await.expect("令牌请求");
    assert!(
        token_request.starts_with(&format!("POST {ACCESS_TOKEN_PATH} ")),
        "{token_request}"
    );
    let token_body = request_body(&token_request);
    assert_eq!(token_body["appKey"], "app-key");
    assert_eq!(
        token_body["appSecret"], "SUPER-SECRET-VALUE",
        "铸造请求体本就带密钥"
    );

    let group_frames: Vec<Value> = {
        let mut frames = Vec::new();
        for _ in 0..3 {
            let raw = received.recv().await.expect("群帧");
            assert!(
                raw.starts_with(&format!("POST {PATH_SEND_GROUP} ")),
                "{raw}"
            );
            assert!(raw.contains("x-acs-dingtalk-access-token"), "{raw}");
            frames.push(request_body(&raw));
        }
        frames
    };
    for frame in &group_frames {
        assert_eq!(frame["robotCode"], "robot-1");
        assert_eq!(frame["openConversationId"], "group-e2e");
        assert_eq!(frame["msgKey"], "sampleMarkdown");
        assert!(
            frame["msgParam"].is_string(),
            "msgParam 是**字符串**不是对象"
        );
    }

    let param = |frame: &Value| -> Value {
        serde_json::from_str(frame["msgParam"].as_str().expect("字符串")).expect("JSON")
    };

    // ① 纯文本：没有引用块。
    let text_frame = param(&group_frames[0]);
    assert_eq!(text_frame["text"], "plain answer");
    assert_eq!(text_frame["title"], "plain answer");

    // ② 引用卡片：`> ` 开头 + 水平线 + 答案（上游的 Markdown 引用形态）。
    let card_frame = param(&group_frames[1]);
    let card_text = card_frame["text"].as_str().expect("text");
    assert!(card_text.starts_with("> QUOTE-CAPTURE-7429"), "{card_text}");
    assert!(
        card_text.contains("\n\n---\n\nanswer for the quoted card"),
        "{card_text}"
    );
    assert_eq!(
        card_frame["title"], "answer for the quoted card",
        "标题是那片**正文**（引用前缀不算在里面，上游 `markdownTitle(body)`）"
    );

    // ③ 媒体：内部附件链接变成占位符，其余正文逐字保留。
    let media_frame = param(&group_frames[2]);
    let media_text = media_frame["text"].as_str().expect("text");
    // 占位符到了引用块里会被逐字转义（`escapeMarkdownQuoteText` 把方括号转义，渲染端再消费掉）。
    assert!(
        media_text.contains(r"\[Image\]"),
        "占位符没进引用块：{media_text}"
    );
    assert!(!media_text.contains("/api/attachments/"), "{media_text}");
    assert!(media_text.contains("answer for the media"), "{media_text}");

    // ④ 私聊：另一个端点，且没有引用块。
    let dm_raw = received.recv().await.expect("直聊帧");
    assert!(
        dm_raw.starts_with(&format!("POST {PATH_SEND_P2P} ")),
        "{dm_raw}"
    );
    let dm_body = request_body(&dm_raw);
    assert_eq!(dm_body["userIds"], json!(["test-sender"]));
    assert!(dm_body.get("openConversationId").is_none());
    let dm_param = param(&dm_body);
    assert_eq!(dm_param["text"], "answer for the interactive card");
    assert!(!dm_param["text"].as_str().expect("text").contains("---"));
}

/// 直聊端点：真 HTTP 上的 `oToMessages/batchSend`（上一条只覆盖了群）。
#[tokio::test]
async fn the_private_endpoint_is_used_for_a_direct_target() {
    let _serial = BASE_LOCK.lock().await;
    let (base, mut received) = serve(
        2,
        vec![
            (
                "200 OK",
                r#"{"accessToken":"tok-dm","expireIn":7200}"#.to_string(),
            ),
            ("200 OK", r#"{"processQueryKey":"k-dm"}"#.to_string()),
        ],
    )
    .await;
    set_api_base(&base);
    let sender = Sender::http(&credentials());
    let mut target = SendTarget::direct("staff-1");
    target.quote_text = "should not be quoted in a direct reply".to_string();
    let key = sender.send(&target, "dm answer").await.expect("直聊");
    reset_api_base();
    assert_eq!(key, "k-dm");

    let _token = received.recv().await.expect("令牌请求");
    let raw = received.recv().await.expect("发送请求");
    assert!(raw.starts_with(&format!("POST {PATH_SEND_P2P} ")), "{raw}");
    let body = request_body(&raw);
    assert_eq!(body["userIds"], json!(["staff-1"]));
    assert!(body.get("openConversationId").is_none());
    let param: Value =
        serde_json::from_str(body["msgParam"].as_str().expect("字符串")).expect("JSON");
    assert_eq!(param["text"], "dm answer", "直聊不带引用");
}

/// 令牌缓存：第二次发送**不**再铸（真 HTTP 上只看到一次 `/oauth2/accessToken`）。
#[tokio::test]
async fn the_token_is_minted_once_per_install() {
    let _serial = BASE_LOCK.lock().await;
    let (base, mut received) = serve(
        3,
        vec![
            (
                "200 OK",
                r#"{"accessToken":"tok-cache","expireIn":7200}"#.to_string(),
            ),
            ("200 OK", r#"{"processQueryKey":"a"}"#.to_string()),
            ("200 OK", r#"{"processQueryKey":"b"}"#.to_string()),
        ],
    )
    .await;
    set_api_base(&base);
    let transport = Arc::new(HttpOpenApi::new());
    let creds = credentials();
    let sender =
        Sender::from_credentials(Arc::clone(&transport) as Arc<dyn OpenApiTransport>, &creds);
    sender
        .send(&SendTarget::group("g"), "one")
        .await
        .expect("1");
    sender
        .send(&SendTarget::group("g"), "two")
        .await
        .expect("2");
    reset_api_base();

    let first = received.recv().await.expect("第一条");
    assert!(
        first.starts_with(&format!("POST {ACCESS_TOKEN_PATH} ")),
        "{first}"
    );
    let second = received.recv().await.expect("第二条");
    assert!(
        second.starts_with(&format!("POST {PATH_SEND_GROUP} ")),
        "第二条应当是发送（令牌已缓存）：{second}"
    );
    let third = received.recv().await.expect("第三条");
    assert!(
        third.starts_with(&format!("POST {PATH_SEND_GROUP} ")),
        "{third}"
    );
    assert!(request_body(&second)["msgParam"].as_str().is_some());
}

/// 传输失败 ⇒ `Transport`，且**不带** URL（基址里可能有内网信息）。
#[tokio::test]
async fn a_transport_failure_does_not_leak_the_base_url() {
    let _serial = BASE_LOCK.lock().await;
    assert_eq!(api_base(), DEFAULT_API_BASE, "未注入时是生产基址");
    set_api_base("http://127.0.0.1:1/definitely-not-a-dingtalk-host");
    let sender = Sender::http(&credentials());
    let error = sender
        .send(&SendTarget::group("g"), "hello")
        .await
        .expect_err("连接失败");
    reset_api_base();
    assert!(matches!(error, DingTalkApiError::Transport { .. }));
    let text = format!("{error} {error:?}");
    assert!(!text.contains("definitely-not-a-dingtalk-host"), "{text}");
    assert!(!text.contains("SUPER-SECRET-VALUE"), "{text}");
}

/// 平台拒绝 ⇒ 只带平台自己的 `code`（不带 `message` 回声）。
#[tokio::test]
async fn a_refusal_surfaces_only_the_platform_code() {
    let _serial = BASE_LOCK.lock().await;
    let (base, mut received) = serve(
        1,
        vec![(
            "403 Forbidden",
            r#"{"code":"Forbidden.AccessDenied","message":"appSecret SUPER-SECRET-VALUE"}"#
                .to_string(),
        )],
    )
    .await;
    set_api_base(&base);
    let sender = Sender::http(&credentials());
    let error = sender
        .send(&SendTarget::group("g"), "hello")
        .await
        .expect_err("平台拒绝");
    reset_api_base();
    assert!(matches!(error, DingTalkApiError::Refused { .. }));
    let text = format!("{error} {error:?}");
    assert!(text.contains("Forbidden.AccessDenied"), "{text}");
    assert!(
        !text.contains("SUPER-SECRET-VALUE"),
        "错误回显了凭据：{text}"
    );
    assert!(!text.contains("message"), "平台 message 被丢掉：{text}");
    let _ = received.recv().await;
}

// =====================================================================
// 接线：工厂注入端口之后 `Channel::send` 真的发得出去
// =====================================================================

/// 工厂注入出站端口 ⇒ `Channel::send` 发的是**群**帧（上游 `dingtalkChannel.Send` 只给
/// `out.ChatID`）；凭据解不开 / 平台拒绝各自映射到正确的 `ChannelError`。
///
/// 不注入的那条路（直接 `DingTalkChannel::new`）由 M7-7 的
/// `send_is_fail_closed_and_capabilities_match_upstream` 钉住 ⇒ 两条路都在。
#[tokio::test]
async fn the_factory_wires_outbound_so_channel_send_posts_a_group_frame() {
    use crate::channel::{ChannelConfig, ChannelError};
    use crate::dingtalk::{factory, DingTalkDeps};
    use mc_core::channel::message::OutboundMessage;

    let transport = Arc::new(
        RecordingTransport::with_tokens(&["token"])
            .posting(&[json!({ "processQueryKey": "frame-1" })]),
    );
    let deps =
        DingTalkDeps::default().with_outbound(Arc::clone(&transport) as Arc<dyn OpenApiTransport>);
    let channel = factory(&deps)(ChannelConfig {
        kind: crate::dingtalk::inbound::TYPE_DINGTALK,
        raw: json!({
            "app_id": "app-key",
            "robot_code": "robot-9",
            "app_secret": "SUPER-SECRET-VALUE",
        }),
        installation_id: None,
        handler: None,
    })
    .expect("工厂注入端口后能装配");

    let result = channel
        .send(OutboundMessage {
            chat_id: "group-1".to_string(),
            text: "hello".to_string(),
            thread_id: "ignored-by-dingtalk".to_string(),
            reply_to: "ignored".to_string(),
        })
        .await
        .expect("群发送");
    assert_eq!(result.message_id, "frame-1");
    let posts = transport.posts();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].path, PATH_SEND_GROUP);
    assert_eq!(
        posts[0].body["robotCode"], "robot-9",
        "工厂把 robot_code 也接上了"
    );
    assert_eq!(posts[0].body["openConversationId"], "group-1");
    assert_eq!(posts[0].body["msgKey"], "sampleMarkdown");
    assert!(
        !posts[0].body.to_string().contains("ignored"),
        "线程 / 引用不是 DingTalk 的字段"
    );

    // 平台拒绝（`Refused`）只带平台自己的 `code`，且凭据面无泄露 —— 真 HTTP 上的那一条在
    // `a_refusal_surfaces_only_the_platform_code` 里；这里直接构造错误，钉住**映射**那一跳。
    let error = DingTalkApiError::Refused {
        path: PATH_SEND_GROUP,
        code: "Forbidden.AccessDenied".to_string(),
    }
    .into_channel_error();
    let text = format!("{error} {error:?}");
    assert!(text.contains("Forbidden.AccessDenied"), "{text}");
    assert!(!text.contains("SUPER-SECRET-VALUE"), "{text}");
    // 平台拒绝落 `Transport`（`send` 本来就不在 supervisor 的退避路径上，见 `channel.rs`）。
    assert_eq!(error.code(), "channel_transport_error");

    // 两次 401 ⇒ `Auth`（可重试的传输失败会误导 supervisor）。
    let transport = Arc::new(RecordingTransport::with_tokens(&["stale"]).failing_posts(&[0, 1]));
    let deps =
        DingTalkDeps::default().with_outbound(Arc::clone(&transport) as Arc<dyn OpenApiTransport>);
    let channel = factory(&deps)(ChannelConfig {
        kind: crate::dingtalk::inbound::TYPE_DINGTALK,
        raw: json!({ "app_id": "app-key", "app_secret": "SUPER-SECRET-VALUE" }),
        installation_id: None,
        handler: None,
    })
    .expect("装配");
    let error = channel
        .send(OutboundMessage {
            chat_id: "group-1".to_string(),
            text: "hello".to_string(),
            thread_id: String::new(),
            reply_to: String::new(),
        })
        .await
        .expect_err("401");
    assert!(matches!(error, ChannelError::Auth { .. }), "{error:?}");
    assert_eq!(error.code(), "channel_auth_error");
    assert!(!error.is_retryable());
}
