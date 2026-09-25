//! `dingtalk::outbound` 的用例（写者 M7-8）。
//!
//! 四段：
//!
//! 1. **纯函数**：目标解析（`channel_task_delivery` 的兜底）、两条端点的请求体、
//!    两个**发之前**的拒绝、分片与引用前缀；
//! 2. **端口替身**：401 ⇒ 作废令牌 + 重试**一次**（两条路径都钉：发送与媒体下载 URL）、
//!    两次都 401 ⇒ `Unauthorized`、表情两条路径；
//! 3. **引用源的图片占位**：内部附件链接被换成 `[Image]`，而围栏 / 行内代码 / 转义 `!` /
//!    普通链接里的同一串字面量**不动**；
//! 4. **端到端回路（`docs/60` §4.2 的 dingtalk 那一格）**：四份上游 golden 之一 → 真归一化
//!    → 真 `reqwest` → **本地 HTTP 替身**，断言替身收到的**原始帧**逐字段相符。
//!
//! 关于"本地替身"落在哪一侧（诚实交代，不当成全部）：入站那半条回路（Stream WS 帧 ⇒
//! 归一化）由 **M7-7** 的 `stream/tests.rs` + `inbound/tests.rs` 用**同一批** golden 与一个
//! 脚本化内存 socket 覆盖（`tokio-tungstenite` 在本仓**没有** `handshake` feature ⇒ 起不了
//! 真实 WS 服务端；见 `docs/32` §22 的 D6）。本文件承担**出站**那半条：真 socket、真 HTTP、
//! 真 `reqwest`，断言的是替身**实际收到**的字节。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mc_core::channel::message::{ChatType, InboundMessage, MessageKind, Source};
use mc_core::id::Id;
use serde_json::{json, Value};

mod http;

use super::{
    answer_text, api_base, decode_credentials, failure_notice, reply_markdown_chunks,
    reset_api_base, sealed_input_quote, set_api_base, Credentials, DingTalkApiError, HttpOpenApi,
    OpenApiTransport, OutboundDelivery, OutboundVerdict, ReplySource, ReplySourceCache, SendTarget,
    Sender, ACCESS_TOKEN_PATH, DEFAULT_API_BASE, MAX_REPLY_SOURCES, MSG_KEY_MARKDOWN,
    PATH_SEND_GROUP, PATH_SEND_P2P,
};
use crate::dingtalk::emotion::{Emotion, PATH_RECALL_EMOTION, PATH_REPLY_EMOTION};
use crate::dingtalk::stream::AppSecret;
use crate::dingtalk::Decrypter;

// =====================================================================
// 替身：端口
// =====================================================================

/// 一条被记下的 `postJSON`。
#[derive(Debug, Clone)]
struct Post {
    path: &'static str,
    token: String,
    body: Value,
}

#[derive(Default)]
struct RecordingTransport {
    posts: Mutex<Vec<Post>>,
    invalidated: Mutex<Vec<String>>,
    token_calls: Mutex<usize>,
    /// 第 n 次 `post_json` 是否回 401（0 起）。
    unauthorized_on: Mutex<Vec<usize>>,
    /// 令牌序列（每次铸造给下一个）。
    tokens: Mutex<Vec<String>>,
    responses: Mutex<Vec<Value>>,
}

impl RecordingTransport {
    fn with_tokens(tokens: &[&str]) -> Self {
        let this = Self::default();
        *this.tokens.lock().expect("lock") =
            tokens.iter().map(|token| (*token).to_string()).collect();
        this
    }

    fn failing_posts(self, indexes: &[usize]) -> Self {
        *self.unauthorized_on.lock().expect("lock") = indexes.to_vec();
        self
    }

    fn posting(self, responses: &[Value]) -> Self {
        *self.responses.lock().expect("lock") = responses.to_vec();
        self
    }

    fn posts(&self) -> Vec<Post> {
        self.posts.lock().expect("lock").clone()
    }
}

#[async_trait]
impl OpenApiTransport for RecordingTransport {
    async fn access_token(
        &self,
        _app_key: &str,
        _app_secret: &AppSecret,
    ) -> Result<String, DingTalkApiError> {
        let mut calls = self.token_calls.lock().expect("lock");
        *calls += 1;
        let tokens = self.tokens.lock().expect("lock");
        Ok(tokens
            .get(*calls - 1)
            .or_else(|| tokens.last())
            .cloned()
            .unwrap_or_else(|| "token".to_string()))
    }

    fn invalidate(&self, app_key: &str) {
        self.invalidated
            .lock()
            .expect("lock")
            .push(app_key.to_string());
    }

    async fn post_json(
        &self,
        path: &'static str,
        access_token: &str,
        body: Value,
    ) -> Result<Value, DingTalkApiError> {
        let index = {
            let posts = self.posts.lock().expect("lock");
            posts.len()
        };
        self.posts.lock().expect("lock").push(Post {
            path,
            token: access_token.to_string(),
            body,
        });
        if self.unauthorized_on.lock().expect("lock").contains(&index) {
            return Err(DingTalkApiError::Unauthorized);
        }
        let responses = self.responses.lock().expect("lock");
        Ok(responses
            .get(index)
            .or_else(|| responses.last())
            .cloned()
            .unwrap_or(Value::Null))
    }
}

fn credentials() -> Credentials {
    Credentials {
        app_key: "app-key".to_string(),
        app_secret: AppSecret::new("SUPER-SECRET-VALUE"),
        robot_code: "robot-1".to_string(),
    }
}

fn stub_sender(transport: Arc<dyn OpenApiTransport>) -> Sender {
    let creds = credentials();
    Sender::new(transport, creds.robot_code, creds.app_key, creds.app_secret)
}

fn inbound(chat_type: ChatType) -> InboundMessage {
    InboundMessage {
        event_id: "ev".to_string(),
        message_id: "msg".to_string(),
        source: Source {
            channel_type: crate::dingtalk::inbound::TYPE_DINGTALK,
            chat_id: "chat".to_string(),
            chat_type,
            sender_id: "staff".to_string(),
            sender_stable_id: String::new(),
            thread_id: String::new(),
        },
        kind: MessageKind::Text,
        text: "hello".to_string(),
        command_text: String::new(),
        has_selected_context: false,
        media_refs: Vec::new(),
        reply_to: None,
        addressed_to_bot: true,
        force_fresh: false,
        skip_agent_run: false,
        raw: json!({ "app_id": "app-key" }),
    }
}

// =====================================================================
// 纯函数：目标解析
// =====================================================================

/// 造一条 `channel_task_delivery` 行（只改本用例关心的三个字段）。
fn delivery_row(
    config: Value,
    chat_id: &str,
    message_id: Option<&str>,
) -> mc_repos::channel::delivery::ChannelTaskDeliveryRow {
    mc_repos::channel::delivery::ChannelTaskDeliveryRow {
        task_id: uuid::Uuid::new_v4(),
        binding_id: uuid::Uuid::new_v4(),
        installation_id: uuid::Uuid::new_v4(),
        channel_type: "dingtalk".to_string(),
        channel_chat_id: chat_id.to_string(),
        chat_type: "group".to_string(),
        channel_message_id: message_id.map(str::to_string),
        channel_thread_id: None,
        route_revision: 3,
        config,
        created_at: chrono::Utc::now(),
    }
}

/// `channel_task_delivery` 的兜底：config 缺失 / 半截都退回 `channel_chat_id` + 群。
#[test]
fn a_task_delivery_target_falls_back_to_the_chat_column() {
    let target = SendTarget::from_task_delivery(&delivery_row(
        Value::Null,
        "chat-from-column",
        Some("src-1"),
    ));
    assert_eq!(target.conversation_type, "2");
    assert_eq!(target.conversation_id, "chat-from-column");
    assert_eq!(target.staff_id, "");
    assert_eq!(target.source_message_id, "src-1");
    assert!(target.is_group());

    // 只写了 `staff_id` 的半截 config：会话类型退回群、会话 id 退回列（上游零值语义）。
    let target = SendTarget::from_task_delivery(&delivery_row(
        json!({ "staff_id": "staff-9" }),
        "chat-from-column",
        None,
    ));
    assert_eq!(target.conversation_type, "2");
    assert_eq!(target.conversation_id, "chat-from-column");
    assert_eq!(target.staff_id, "staff-9");
    assert_eq!(target.source_message_id, "");

    // 完整 config：三条都按 config 走（列只作兜底）。
    let target = SendTarget::from_task_delivery(&delivery_row(
        json!({
            "conversation_type": "1",
            "conversation_id": "dm-1",
            "staff_id": "staff-9",
        }),
        "chat-from-column",
        None,
    ));
    assert!(!target.is_group());
    assert_eq!(target.conversation_id, "dm-1");
}

/// 两条端点的请求体逐字（上游 `request`）。
#[test]
fn the_two_endpoints_carry_the_upstream_wire_fields() {
    let sender = stub_sender(Arc::new(RecordingTransport::default()));
    let (path, body) = sender
        .request(&SendTarget::group("group-1"), "{\"title\":\"t\"}")
        .expect("群发目标合法");
    assert_eq!(path, PATH_SEND_GROUP);
    assert_eq!(body["robotCode"], "robot-1");
    assert_eq!(body["openConversationId"], "group-1");
    assert_eq!(body["msgKey"], MSG_KEY_MARKDOWN);
    assert_eq!(body["msgKey"], "sampleMarkdown");
    assert_eq!(body["msgParam"], "{\"title\":\"t\"}");

    let (path, body) = sender
        .request(&SendTarget::direct("staff-1"), "{}")
        .expect("直聊目标合法");
    assert_eq!(path, PATH_SEND_P2P);
    assert_eq!(body["robotCode"], "robot-1");
    assert_eq!(body["userIds"], json!(["staff-1"]));
    assert_eq!(body["msgKey"], "sampleMarkdown");
    assert!(body.get("openConversationId").is_none());
}

/// 两个**发之前**的拒绝（不许把半截目标发出去）。
#[test]
fn incomplete_targets_are_refused_before_any_call() {
    let transport = Arc::new(RecordingTransport::default());
    let sender = stub_sender(Arc::clone(&transport) as Arc<dyn OpenApiTransport>);
    let mut direct = SendTarget::direct("");
    direct.conversation_type = "1".to_string();
    let error = sender.request(&direct, "{}").expect_err("缺收件人");
    assert!(matches!(error, DingTalkApiError::InvalidTarget { .. }));
    assert!(error.to_string().contains("staff id"), "{error}");

    let error = sender
        .request(&SendTarget::group(""), "{}")
        .expect_err("缺会话 id");
    assert!(error.to_string().contains("conversation id"), "{error}");
    assert!(transport.posts().is_empty(), "拒绝必须发生在网络之前");
}

/// 空正文不发（上游 `send` 的第一条短路），且**不**碰端口。
#[tokio::test]
async fn an_empty_body_sends_nothing() {
    let transport = Arc::new(RecordingTransport::default());
    let sender = stub_sender(Arc::clone(&transport) as Arc<dyn OpenApiTransport>);
    assert_eq!(
        sender
            .send(&SendTarget::group("g"), "")
            .await
            .expect("短路"),
        ""
    );
    assert!(transport.posts().is_empty());
}

/// 引用只进**首片**；直聊不带引用（上游 `send` 里那条 `if target.ConversationType != p2p`）。
#[test]
fn the_quote_prefix_only_lands_in_the_first_chunk() {
    let answer = "b".repeat(20_000);
    let chunks = reply_markdown_chunks(&answer, "Question").expect("分片");
    assert!(chunks.len() > 1);
    assert!(
        chunks[0].text.starts_with("> Question\n\n---\n\n"),
        "首片带引用"
    );
    for chunk in &chunks[1..] {
        assert!(!chunk.text.contains("---"), "续片不带引用");
    }
    // 标题就是那片正文（不截断）。
    assert_eq!(chunks[1].title, chunks[1].text);
}

/// 平台零值：空引用 ⇒ 不加引用块。
#[test]
fn an_empty_quote_renders_plain_markdown() {
    let chunks = reply_markdown_chunks("answer", "").expect("分片");
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].text, "answer");
    assert_eq!(chunks[0].title, "answer");
}

// =====================================================================
// 端口替身：401 重试
// =====================================================================

/// 发送：第一次 401 ⇒ 作废令牌 + 重试一次成功；返回末片的 `processQueryKey`。
#[tokio::test]
async fn a_send_refreshes_the_token_once_after_401() {
    let transport = Arc::new(
        RecordingTransport::with_tokens(&["stale", "fresh"])
            .failing_posts(&[0])
            .posting(&[json!({ "processQueryKey": "key-1" })]),
    );
    let sender = stub_sender(Arc::clone(&transport) as Arc<dyn OpenApiTransport>);
    let key = sender
        .send(&SendTarget::group("g"), "hello")
        .await
        .expect("重试后成功");
    assert_eq!(key, "key-1");
    assert_eq!(
        *transport.token_calls.lock().expect("lock"),
        2,
        "两次取令牌"
    );
    assert_eq!(
        transport.invalidated.lock().expect("lock").as_slice(),
        ["app-key"]
    );
    let posts = transport.posts();
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[0].token, "stale");
    assert_eq!(posts[1].token, "fresh", "第二次用的是刷新后的令牌");
    assert_eq!(posts[1].path, PATH_SEND_GROUP);
    let param: Value = serde_json::from_str(
        posts[1].body["msgParam"]
            .as_str()
            .expect("msgParam 是字符串"),
    )
    .expect("msgParam 是 JSON 字符串");
    assert_eq!(param["text"], "hello");
    assert_eq!(param["title"], "hello");
}

/// 两次都 401 ⇒ 原样把 `Unauthorized` 交出去（上游循环末尾的 `return errUnauthorized`）。
#[tokio::test]
async fn two_401_surface_unauthorized() {
    let transport = Arc::new(RecordingTransport::with_tokens(&["a"]).failing_posts(&[0, 1]));
    let sender = stub_sender(Arc::clone(&transport) as Arc<dyn OpenApiTransport>);
    let error = sender
        .send(&SendTarget::group("g"), "hello")
        .await
        .expect_err("两次都 401");
    assert!(matches!(error, DingTalkApiError::Unauthorized));
    assert_eq!(error.code(), "unauthorized");
    assert_eq!(transport.posts().len(), 2, "只重试一次");

    // `Auth` 而不是可重试的 `Transport`（supervisor 不该把它当链路抖动）。
    let channel_error = error.into_channel_error();
    assert_eq!(
        channel_error.code(),
        "channel_auth_error",
        "{channel_error:?}"
    );
    assert!(!channel_error.is_retryable());
}

/// 媒体下载 URL：401 ⇒ 刷新一次；平台不给 `downloadUrl` ⇒ `Malformed`。
#[tokio::test]
async fn the_media_download_url_refreshes_once_and_rejects_empty_answers() {
    let transport = Arc::new(
        RecordingTransport::with_tokens(&["stale", "fresh"])
            .failing_posts(&[0])
            .posting(&[json!({ "downloadUrl": "https://cdn.example.test/x" })]),
    );
    let sender = stub_sender(Arc::clone(&transport) as Arc<dyn OpenApiTransport>);
    let url = sender
        .message_file_download_url("code-1")
        .await
        .expect("重试后拿到 URL");
    assert_eq!(url, "https://cdn.example.test/x");
    assert_eq!(transport.posts().len(), 2);
    assert_eq!(
        transport.posts()[1].body["downloadCode"],
        "code-1",
        "重试带同一份 body"
    );

    let transport = Arc::new(RecordingTransport::default().posting(&[json!({})]));
    let sender = stub_sender(Arc::clone(&transport) as Arc<dyn OpenApiTransport>);
    let error = sender
        .message_file_download_url("code-2")
        .await
        .expect_err("空 downloadUrl 必须失败");
    assert!(matches!(error, DingTalkApiError::Malformed { .. }));
}

/// 表情：贴 / 撤走两条不同路径，且 401 由 `emotion` 那条规则作废缓存后重试一次。
#[tokio::test]
async fn emoji_reactions_use_both_paths_and_retry_once() {
    let transport = Arc::new(RecordingTransport::with_tokens(&["tok"]));
    let sender = stub_sender(Arc::clone(&transport) as Arc<dyn OpenApiTransport>);
    let target = SendTarget::reaction_from_message(&inbound(ChatType::Group));
    sender
        .set_emoji_reaction(&target, Emotion::Acknowledged, false)
        .await
        .expect("贴表情");
    sender
        .set_emoji_reaction(&target, Emotion::Done, true)
        .await
        .expect("撤表情");
    let posts = transport.posts();
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[0].path, PATH_REPLY_EMOTION);
    assert_eq!(posts[1].path, PATH_RECALL_EMOTION);
    assert_eq!(posts[0].body["emotionName"], "收到");
    assert_eq!(posts[1].body["emotionName"], "Done");
    assert_eq!(posts[1].body["emotionType"], 1);
    assert_eq!(posts[1].body["openMsgId"], "msg");
    assert_eq!(posts[1].body["robotCode"], "robot-1");

    // 第一次 401 ⇒ 作废 + 重试一次（然后成功）。
    let transport = Arc::new(RecordingTransport::with_tokens(&["a", "b"]).failing_posts(&[0]));
    let sender = stub_sender(Arc::clone(&transport) as Arc<dyn OpenApiTransport>);
    sender
        .set_emoji_reaction(&target, Emotion::Acknowledged, false)
        .await
        .expect("401 后重试成功");
    assert_eq!(transport.posts().len(), 2);
    assert_eq!(transport.invalidated.lock().expect("lock").len(), 1);

    // 两次都 401 ⇒ `Unauthorized`。
    let transport = Arc::new(RecordingTransport::with_tokens(&["a"]).failing_posts(&[0, 1]));
    let sender = stub_sender(Arc::clone(&transport) as Arc<dyn OpenApiTransport>);
    let error = sender
        .set_emoji_reaction(&target, Emotion::Acknowledged, false)
        .await
        .expect_err("两次都 401");
    assert!(matches!(
        error,
        crate::dingtalk::emotion::EmotionError::Unauthorized
    ));

    // 前置校验：缺消息 id ⇒ 一次网络调用都不发。
    let transport = Arc::new(RecordingTransport::default());
    let sender = stub_sender(Arc::clone(&transport) as Arc<dyn OpenApiTransport>);
    let mut bare = SendTarget::group("chat");
    bare.source_message_id = String::new();
    assert!(sender
        .set_emoji_reaction(&bare, Emotion::Acknowledged, false)
        .await
        .is_err());
    assert!(transport.posts().is_empty());
}

// =====================================================================
// 引用里的图片占位
// =====================================================================

/// 内部附件形态被换成 `[Image]`；围栏 / 行内代码 / 转义 `!` / 普通链接里的同一串**不动**。
#[test]
fn internal_attachment_images_become_the_placeholder_but_literals_do_not() {
    let image = "![](/api/attachments/0f8f-1/download)";
    assert_eq!(
        sealed_input_quote(&format!("before {image} after")),
        "before [Image] after"
    );
    // 没有内部附件前缀 ⇒ 逐字返回（连扫描都不做）。
    let plain = "![](https://example.test/a.png)";
    assert_eq!(sealed_input_quote(plain), plain);

    let body = format!(
        "{image}\n\n```\n{image}\n```\n\n`{image}`\n\ntext \\![]/api/attachments/x/download)"
    );
    let quoted = sealed_input_quote(&body);
    assert!(quoted.starts_with("[Image]\n"), "{quoted}");
    assert!(quoted.contains(&format!("```\n{image}\n```")), "围栏里不动");
    assert!(quoted.contains(&format!("`{image}`")), "行内代码里不动");

    // 普通链接（没有 `!`）不动。
    let link = "[x](/api/attachments/abc/download)";
    assert_eq!(sealed_input_quote(link), link);
    // 带 title 的写法上游也不会替换（`literal` 比对失败）⇒ 逐字保留。
    let titled = "![](/api/attachments/abc/download \"t\")";
    assert_eq!(sealed_input_quote(titled), titled);
    // 形状不对（少了 `/download`）⇒ 不动。
    let wrong = "![](/api/attachments/abc)";
    assert_eq!(sealed_input_quote(wrong), wrong);
}

// =====================================================================
// 引用源缓存
// =====================================================================

/// 记住 / 取回 / 反查 / 会话兴趣；安装不符 ⇒ 取不到。
#[test]
fn the_reply_source_cache_is_keyed_by_input_and_installation() {
    let cache = ReplySourceCache::new();
    let installation = Id::new();
    let input = Id::new();
    let session = Id::new();
    let message = inbound(ChatType::Group);
    assert!(cache.is_empty());
    assert!(!cache.has_session(session));

    let release = cache.begin_input(session);
    assert!(cache.has_session(session), "提交前就登记了兴趣");
    release();
    assert!(!cache.has_session(session), "释放后兴趣归零");

    cache.remember(ReplySource::new(installation, input, session, &message));
    assert_eq!(cache.len(), 1);
    assert!(cache.has_session(session), "保留的引用算在会话上");
    let source = cache.source_for(installation, input).expect("取得到");
    assert_eq!(source.message_id, "msg");
    assert_eq!(source.reaction_target().source_message_id, "msg");
    assert_eq!(source.reaction_target().conversation_id, "chat");
    assert_eq!(cache.input_for(installation, &message), Some(input));

    assert!(
        cache.source_for(Id::new(), input).is_none(),
        "别的安装取不到"
    );
    assert!(
        cache.source_for(installation, Id::new()).is_none(),
        "别的输入取不到"
    );
}

/// 同一个输入 id 重复登记只算一条；缺消息 id / 会话 id 的输入不入缓存。
#[test]
fn duplicate_and_incomplete_sources_are_not_cached() {
    let cache = ReplySourceCache::new();
    let (installation, input, session) = (Id::new(), Id::new(), Id::new());
    let message = inbound(ChatType::Group);
    cache.remember(ReplySource::new(installation, input, session, &message));
    cache.remember(ReplySource::new(installation, input, session, &message));
    assert_eq!(cache.len(), 1);

    let mut blank = message.clone();
    blank.message_id = String::new();
    cache.remember(ReplySource::new(installation, Id::new(), session, &blank));
    assert_eq!(cache.len(), 1, "缺消息 id 不记");
    let mut no_chat = message;
    no_chat.source.chat_id = String::new();
    cache.remember(ReplySource::new(installation, Id::new(), session, &no_chat));
    assert_eq!(cache.len(), 1, "缺会话 id 不记");
}

/// 缓存有界（上游 `maxReplySources = 1024`）：环形淘汰最老的，且会话计数跟着回落。
#[test]
fn the_cache_evicts_the_oldest_entry_at_its_bound() {
    let cache = ReplySourceCache::new();
    let installation = Id::new();
    let message = inbound(ChatType::Group);
    let mut ids = Vec::new();
    for index in 0..=MAX_REPLY_SOURCES {
        let input = Id::new();
        if index == 0 {
            ids.push(input);
        }
        cache.remember(ReplySource::new(installation, input, Id::new(), &message));
    }
    assert_eq!(cache.len(), MAX_REPLY_SOURCES, "上限就是不变量");
    assert!(
        cache.source_for(installation, ids[0]).is_none(),
        "最老的一条被淘汰"
    );
}

// =====================================================================
// 判决与凭据面
// =====================================================================

/// `eventContent` 的三条分支（上游 `outbound.go`）。
#[test]
fn the_terminal_verdicts_match_upstream() {
    assert_eq!(answer_text(""), OutboundVerdict::Silent);
    assert_eq!(
        answer_text("done"),
        OutboundVerdict::Deliver("done".to_string())
    );
    assert_eq!(failure_notice("boom", false), {
        OutboundVerdict::Deliver("⚠️ boom".to_string())
    });
    assert_eq!(
        failure_notice("boom", true),
        OutboundVerdict::Silent,
        "重试在飞则静默"
    );
    assert_eq!(failure_notice("", false), OutboundVerdict::Silent);
}

/// 凭据面：`Debug` 脱敏 + 错误路径不回显 `AppSecret`（`docs/60` §2.3 第 1/3 条）。
#[tokio::test]
async fn credentials_are_redacted_and_never_echoed_by_errors() {
    let creds = credentials();
    let rendered = format!("{creds:?}");
    assert!(!rendered.contains("SUPER-SECRET-VALUE"), "{rendered}");
    assert!(rendered.contains("<redacted>"));
    assert!(rendered.contains("app-key"), "AppKey 不是密钥");

    let sender = stub_sender(Arc::new(RecordingTransport::default()));
    let rendered = format!("{sender:?}");
    assert!(!rendered.contains("SUPER-SECRET-VALUE"), "{rendered}");

    // 平台把请求体回声进响应体，也不许出现在我们的错误里（`message` 字段被丢掉）。
    let error = DingTalkApiError::Refused {
        path: ACCESS_TOKEN_PATH,
        code: "Forbidden.AccessDenied".to_string(),
    };
    let text = format!("{error} {error:?}");
    assert!(text.contains("Forbidden.AccessDenied"), "{text}");
    assert!(!text.contains("SUPER-SECRET-VALUE"));
    assert!(!text.contains("appSecret"));
}

/// 解凭据：明文 / 密文 / 失败关闭三条形态（三条分支与连接面**共用**同一份判据）。
#[test]
fn credential_decoding_covers_the_three_input_shapes() {
    let closed = Decrypter::fail_closed();
    let creds = decode_credentials(
        &json!({ "app_id": "app-key", "app_secret": "SUPER-SECRET-VALUE" }),
        &closed,
    )
    .expect("最小可用形态");
    assert_eq!(creds.app_key, "app-key");
    assert_eq!(creds.robot_code, "app-key", "robot code 退到 app_id");

    let creds = decode_credentials(
        &json!({
            "app_id": "app-key",
            "robot_code": "robot-9",
            "app_secret": "SUPER-SECRET-VALUE",
        }),
        &closed,
    )
    .expect("显式 robot code 优先");
    assert_eq!(creds.robot_code, "robot-9");

    // 密文列非空 ⇒ **必须**有解密器：失败关闭时**拒**（不把密文当明文用）。
    let encrypted = json!({ "app_id": "app-key", "app_secret_encrypted": "CIPHERTEXT-B64" });
    let error = decode_credentials(&encrypted, &closed).expect_err("失败关闭");
    let text = format!("{error} {error:?}");
    assert!(!text.contains("CIPHERTEXT-B64"), "错误回显了密文：{text}");
    assert!(!text.contains("SUPER-SECRET-VALUE"));

    // 接上解密器 ⇒ 同一条密文形态解得出明文。
    let decrypt = Decrypter::new(
        "test",
        Arc::new(|ciphertext: &str| Ok(format!("plain-of-{ciphertext}"))),
    );
    let creds = decode_credentials(&encrypted, &decrypt).expect("密文优先");
    assert_eq!(creds.app_secret.expose(), "plain-of-CIPHERTEXT-B64");

    assert!(decode_credentials(&json!({ "app_secret": "x" }), &closed).is_err());
    assert!(decode_credentials(&json!({ "app_id": "x" }), &closed).is_err());
    assert!(decode_credentials(&json!("not an object"), &closed).is_err());
}

// =====================================================================
// 端到端回路：golden 入站 → 真 HTTP 出站
