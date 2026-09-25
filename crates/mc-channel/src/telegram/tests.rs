//! `telegram`（adapter 根：长轮询回路 / 工厂 / 注册面）的用例（写者 M7-5）。
//!
//! 上游 `telegram_test.go` 的 `TestConnectDispatchesAndAdvancesOffset` /
//! `TestGetUpdates409IsErrConflict` / `TestDispatchUnsupportedMediaInAddressedGroupPreservesTopicAndReply`
//! / `TestDispatchIssueErrorSendsFailureNotice` 逐条移植，外加**本片专属**的
//! 「offset 不落地 + 重启重投」三件套。
//!
//! 回路的时延全部靠注入（`retry_delay = 0` / 脚本化响应）⇒ 用例不睡真觉，只需要一次
//! 1 秒的 429 退避（那是 Telegram 强制的下界）。

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use super::*;
use crate::engine::resolvers::{
    ChatRunParams, EngineError, IssueCreator, NoCommands, RunTriggerer, SessionReader,
    WorkspaceIdentity,
};
use crate::engine::supervisor::{
    AcquireLeaseParams, Installation, InstallationStore, LeaseStore, ReleaseLeaseParams,
};
use crate::engine::EngineResult;
use crate::engine::{Router, RouterConfig};
use crate::message::InboundHandler;
use api::{ApiError, ApiResult};
use inbound::{Chat, Message, User};
use mc_core::id::Id;

use base64::Engine as _;
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// 脚本化替身
// ---------------------------------------------------------------------------

/// 一次 `getUpdates` 的脚本（用尽之后**永久挂起**，模拟 Telegram 的长轮询等待）。
#[derive(Debug, Clone)]
enum Script {
    Updates(Vec<Update>),
    Conflict,
    RateLimited,
    Unreachable,
    /// 永久挂起（长轮询的"没有新消息"形态）。
    Park,
}

/// 脚本化的 API 替身：记下每次轮询的 offset，也记下发出去的告知。
#[derive(Default)]
struct ScriptedApi {
    script: Mutex<VecDeque<Script>>,
    offsets: Mutex<Vec<i64>>,
    notices: Mutex<Vec<SendMessage>>,
}

impl ScriptedApi {
    fn new(script: Vec<Script>) -> Self {
        Self {
            script: Mutex::new(script.into()),
            offsets: Mutex::new(Vec::new()),
            notices: Mutex::new(Vec::new()),
        }
    }

    fn offsets(&self) -> Vec<i64> {
        self.offsets.lock().expect("offsets").clone()
    }

    fn notices(&self) -> Vec<SendMessage> {
        self.notices.lock().expect("notices").clone()
    }
}

#[async_trait]
impl TelegramApi for ScriptedApi {
    async fn get_me(&self, _bot_token: &str) -> ApiResult<User> {
        Err(ApiError::Malformed { method: "getMe" })
    }

    async fn get_webhook_info(&self, _bot_token: &str) -> ApiResult<api::WebhookInfo> {
        Err(ApiError::Malformed {
            method: "getWebhookInfo",
        })
    }

    async fn get_updates(&self, _bot_token: &str, offset: i64) -> ApiResult<Vec<Update>> {
        self.offsets.lock().expect("offsets").push(offset);
        let next = self.script.lock().expect("script").pop_front();
        match next {
            Some(Script::Updates(updates)) => Ok(updates),
            Some(Script::Conflict) => Err(ApiError::Conflict),
            Some(Script::RateLimited) => Err(ApiError::Api {
                method: "getUpdates",
                code: 429,
                description: "Too Many Requests".to_string(),
                retry_after: Some(1),
            }),
            Some(Script::Unreachable) => Err(ApiError::Transport {
                method: "getUpdates",
            }),
            // 脚本用尽 / 显式 Park：模拟服务端挂起 50 秒的"没有新消息"。
            Some(Script::Park) | None => {
                std::future::pending::<()>().await;
                unreachable!()
            }
        }
    }

    async fn send_message(&self, _bot_token: &str, params: &SendMessage) -> ApiResult<Message> {
        self.notices.lock().expect("notices").push(params.clone());
        Ok(Message {
            message_id: 1,
            ..Message::default()
        })
    }

    async fn send_chat_action(
        &self,
        _bot_token: &str,
        _chat_id: i64,
        _message_thread_id: i64,
    ) -> ApiResult<()> {
        Ok(())
    }
}

/// 记下投递的消息。
#[derive(Default)]
struct RecordingHandler {
    messages: Mutex<Vec<InboundMessage>>,
    fail: bool,
}

// ---- 注册面用得着的三个 no-op 端口（`Router` 的位置参数） ----

struct StubTrigger;

#[async_trait]
impl RunTriggerer for StubTrigger {
    async fn schedule_chat_run(&self, _params: ChatRunParams) -> EngineResult<()> {
        Ok(())
    }
    async fn drain(&self) -> EngineResult<()> {
        Ok(())
    }
}

struct StubReader;

#[async_trait]
impl SessionReader for StubReader {
    async fn workspace_identity(&self, _workspace_id: Id) -> EngineResult<WorkspaceIdentity> {
        Ok(WorkspaceIdentity::default())
    }
}

struct StubIssues;

#[async_trait]
impl IssueCreator for StubIssues {
    async fn create_issue(
        &self,
        _params: crate::engine::resolvers::ChannelIssueParams,
    ) -> EngineResult<crate::engine::resolvers::ChannelIssueOutcome> {
        Err(EngineError::infra("unused"))
    }
}

#[async_trait]
impl InboundHandler for RecordingHandler {
    async fn handle(&self, message: InboundMessage) -> ChannelResult<()> {
        self.messages.lock().expect("messages").push(message);
        if self.fail {
            return Err(ChannelError::Storage {
                message: "db down".to_string(),
            });
        }
        Ok(())
    }
}

/// 一条文本更新（`private`）。
fn text_update(update_id: i64, message_id: i64, text: &str) -> Update {
    Update {
        update_id,
        message: Some(Box::new(Message {
            message_id,
            from: Some(User {
                id: 42,
                first_name: "A".to_string(),
                ..User::default()
            }),
            chat: Chat {
                id: 42,
                chat_type: "private".to_string(),
            },
            text: text.to_string(),
            ..Message::default()
        })),
    }
}

/// 一条**媒体**更新：`supergroup` + 话题 + **直接回复 bot 的一条消息**（群里的寻址方式之二）。
///
/// 正文为空（只有图片）⇒ 归一化成 `MessageKind::Image`；寻址靠 `replied_to_bot`。
fn media_update() -> Update {
    Update {
        update_id: 3,
        message: Some(Box::new(Message {
            message_id: 8,
            from: Some(User {
                id: 42,
                first_name: "A".to_string(),
                ..User::default()
            }),
            chat: Chat {
                id: -100,
                chat_type: "supergroup".to_string(),
            },
            photo: vec![serde_json::json!({ "file_id": "f1" })],
            reply_to_message: Some(Box::new(Message {
                message_id: 7,
                from: Some(User {
                    id: 999,
                    is_bot: true,
                    ..User::default()
                }),
                ..Message::default()
            })),
            message_thread_id: 8,
            is_topic_message: true,
            ..Message::default()
        })),
    }
}

/// 装一台回路（handler + 脚本化 API，瞬态分隔为 0）。
fn channel(
    api: &Arc<ScriptedApi>,
    handler: &Arc<RecordingHandler>,
    username: &str,
) -> TelegramChannel {
    TelegramChannel::new(
        999,
        username,
        Sensitive::new("123:token"),
        Arc::clone(api) as Arc<dyn TelegramApi>,
        Some(Arc::clone(handler) as SharedInboundHandler),
    )
    .with_retry_delay(Duration::ZERO)
}

// ---------------------------------------------------------------------------
// 用例
// ---------------------------------------------------------------------------

/// 上游 `TestConnectDispatchesAndAdvancesOffset`：第一次轮询 `offset = 0`，处理完
/// `update_id = 10` 之后第二次轮询 `offset = 11`，并把消息交给 handler。
#[tokio::test]
async fn the_polling_loop_dispatches_and_advances_offset_after_each_batch() {
    let api = Arc::new(ScriptedApi::new(vec![
        Script::Updates(vec![text_update(10, 1, "hi")]),
        Script::Park,
    ]));
    let handler = Arc::new(RecordingHandler::default());
    let channel = channel(&api, &handler, "my_bot");

    let outcome = tokio::time::timeout(Duration::from_secs(2), channel.connect()).await;
    assert!(outcome.is_err(), "回路在 Park 上一直挂着（超时才结束）");

    let offsets = api.offsets();
    assert_eq!(offsets[0], 0, "第一次轮询必须从 0 开始");
    assert_eq!(offsets[1], 11, "处理完 update_id 10 之后推进到 11");
    let messages = handler.messages.lock().expect("messages").clone();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].text, "hi");
    assert_eq!(messages[0].source.chat_id, "42");
}

/// **本片专属验收**：offset **不落地** ⇒ 新一条回路（= 重启）从 0 重新开始，
/// Telegram 重投全部尚未确认的更新（**不丢**），而重投由 engine 的去重吸收（**不重复消费**）。
#[tokio::test]
async fn a_restart_replays_pending_updates_from_offset_zero() {
    let api = Arc::new(ScriptedApi::new(vec![
        // 第一条回路：拿到 10，处理完就 Park（= 长轮询等待），随后被"重启"（超时）打断。
        Script::Updates(vec![text_update(10, 1, "hi")]),
        Script::Park,
        // 第二条回路：Telegram 重投同一条更新（服务器端只在 offset 推进后出队）。
        Script::Updates(vec![text_update(10, 1, "hi")]),
        Script::Park,
    ]));
    let handler = Arc::new(RecordingHandler::default());

    let first = channel(&api, &handler, "my_bot");
    let _ = tokio::time::timeout(Duration::from_millis(500), first.connect()).await;
    let second = channel(&api, &handler, "my_bot");
    let _ = tokio::time::timeout(Duration::from_millis(500), second.connect()).await;

    let offsets = api.offsets();
    assert_eq!(offsets, vec![0, 11, 0, 11], "两条回路各自从 0 开始");
    let messages = handler.messages.lock().expect("messages").clone();
    assert_eq!(messages.len(), 2, "重投确实到达了 handler（不丢）");
    assert_eq!(
        messages[0].message_id, messages[1].message_id,
        "两条的 message_id 相同 ⇒ 去重键相同 ⇒ engine 能吸收重投（不重复消费）"
    );
}

/// 409 Conflict 是对**这一次尝试**致命的：回一个准确文案（退避修不好它）。
#[tokio::test]
async fn a_409_conflict_is_fatal_for_the_attempt() {
    let api = Arc::new(ScriptedApi::new(vec![Script::Conflict]));
    let handler = Arc::new(RecordingHandler::default());
    let channel = channel(&api, &handler, "my_bot");
    let error = channel.connect().await.expect_err("conflict");
    assert!(matches!(error, ChannelError::Transport { .. }));
    assert!(
        error.to_string().contains("another instance"),
        "文案要能让运维定位：{error}"
    );
    assert_eq!(api.offsets(), vec![0], "409 之后不再轮询");
}

/// 429 被回路**吸收**：按 Telegram 强制的退避睡一次再继续（不算这次尝试失败）。
#[tokio::test]
async fn a_429_is_absorbed_with_the_mandated_backoff() {
    let api = Arc::new(ScriptedApi::new(vec![
        Script::RateLimited,
        Script::Updates(vec![text_update(7, 1, "after backoff")]),
        Script::Park,
    ]));
    let handler = Arc::new(RecordingHandler::default());
    let channel = channel(&api, &handler, "my_bot");
    let _ = tokio::time::timeout(Duration::from_secs(3), channel.connect()).await;
    assert_eq!(api.offsets(), vec![0, 0, 8], "退避后仍从同一个 offset 继续");
    let messages = handler.messages.lock().expect("messages").clone();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].text, "after backoff");
}

/// 瞬态失败：尝试内先隔一次，再把错误交给 Supervisor 的退避。
#[tokio::test]
async fn a_transient_failure_is_handed_to_the_supervisor_backoff() {
    let api = Arc::new(ScriptedApi::new(vec![Script::Unreachable]));
    let handler = Arc::new(RecordingHandler::default());
    let channel = channel(&api, &handler, "my_bot");
    let error = channel.connect().await.expect_err("transient");
    assert!(matches!(error, ChannelError::Transport { .. }));
    assert!(
        error.to_string().contains("getUpdates"),
        "文案只带方法名（不带 URL / 令牌）：{error}"
    );
    assert!(error.is_retryable(), "瞬态失败必须值得退避重连");
}

/// 私聊里的非文本消息 ⇒ 回一条"暂不支持"（**保留**话题与引用参数）。
#[tokio::test]
async fn a_non_text_message_gets_a_courteous_notice() {
    let api = Arc::new(ScriptedApi::new(vec![
        Script::Updates(vec![media_update()]),
        Script::Park,
    ]));
    let handler = Arc::new(RecordingHandler::default());
    let channel = channel(&api, &handler, "my_bot");
    let _ = tokio::time::timeout(Duration::from_millis(500), channel.connect()).await;
    assert!(
        handler.messages.lock().expect("messages").is_empty(),
        "非文本消息不进 handler"
    );
    let notices = api.notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].text, UNSUPPORTED_TYPE_TEXT);
    assert_eq!(notices[0].chat_id, -100, "群里的告知回到原群");
    assert_eq!(notices[0].message_thread_id, 8, "保留话题路由");
    assert_eq!(notices[0].reply_to_message_id, 8, "引用触发它的那条消息");
}

/// 未被寻址的群内媒体消息 ⇒ **不**回告知（避免对每张图都插一句话）。
#[tokio::test]
async fn unaddressed_group_media_stays_silent() {
    let mut update = media_update();
    if let Some(message) = update.message.as_mut() {
        // 去掉"回复 bot"这条寻址依据 ⇒ 群里未寻址的媒体消息。
        message.reply_to_message = None;
    }
    let api = Arc::new(ScriptedApi::new(vec![
        Script::Updates(vec![update]),
        Script::Park,
    ]));
    let handler = Arc::new(RecordingHandler::default());
    let channel = channel(&api, &handler, "my_bot");
    let _ = tokio::time::timeout(Duration::from_millis(500), channel.connect()).await;
    assert!(api.notices().is_empty(), "未寻址的群内媒体不插话");
}

/// 上游 `TestDispatchIssueErrorSendsFailureNotice`：被寻址的 `/issue` 派发失败 ⇒
/// 回一条告知，且 `connect` **仍然**把错误冒泡给 Supervisor。
#[tokio::test]
async fn an_addressed_issue_dispatch_failure_notifies_the_user_and_still_errors() {
    let api = Arc::new(ScriptedApi::new(vec![Script::Updates(vec![text_update(
        1,
        4,
        "/issue fix login",
    )])]));
    let handler = Arc::new(RecordingHandler {
        messages: Mutex::new(Vec::new()),
        fail: true,
    });
    let channel = channel(&api, &handler, "my_bot");
    let error = channel.connect().await.expect_err("infra failure");
    assert!(matches!(error, ChannelError::Storage { .. }));
    // 告知是脱离任务 ⇒ 给它一点时间。
    tokio::time::sleep(Duration::from_millis(60)).await;
    let notices = api.notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].text, ISSUE_DISPATCH_FAILED_TEXT);
    assert_eq!(notices[0].reply_to_message_id, 4);
}

/// 派发失败但**不是**被寻址的 `/issue` ⇒ 不打扰用户（只有那条路径值得一条告知）。
#[tokio::test]
async fn a_plain_dispatch_failure_does_not_notify() {
    let api = Arc::new(ScriptedApi::new(vec![Script::Updates(vec![text_update(
        1, 4, "hello",
    )])]));
    let handler = Arc::new(RecordingHandler {
        messages: Mutex::new(Vec::new()),
        fail: true,
    });
    let channel = channel(&api, &handler, "my_bot");
    let _ = channel.connect().await.expect_err("infra failure");
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(api.notices().is_empty());
}

/// 没有 handler / 没有令牌 ⇒ 失败关闭（不给一个跑不起来的回路）。
#[tokio::test]
async fn a_channel_without_a_handler_or_token_fails_closed() {
    let api = Arc::new(ScriptedApi::default());
    let no_handler = TelegramChannel::new(
        999,
        "my_bot",
        Sensitive::new("123:token"),
        Arc::clone(&api) as Arc<dyn TelegramApi>,
        None,
    );
    let error = no_handler.connect().await.expect_err("no handler");
    assert!(error.to_string().contains("inbound handler"));

    let empty_token = TelegramChannel::new(
        999,
        "my_bot",
        Sensitive::default(),
        Arc::clone(&api) as Arc<dyn TelegramApi>,
        Some(Arc::new(RecordingHandler::default()) as SharedInboundHandler),
    );
    let error = empty_token.connect().await.expect_err("no token");
    assert!(error.to_string().contains("bot token"));
    assert!(api.offsets().is_empty(), "失败关闭时一次都不轮询");
}

/// 出站：纯文本 + 话题 + 引用；`SendResult` 带平台消息 id。
#[tokio::test]
async fn send_uses_plain_text_with_thread_and_quote() {
    let api = Arc::new(ScriptedApi::default());
    let handler = Arc::new(RecordingHandler::default());
    let channel = channel(&api, &handler, "my_bot");
    let result = channel
        .send(OutboundMessage {
            chat_id: "-100".to_string(),
            text: "agent reply".to_string(),
            thread_id: "77".to_string(),
            reply_to: "-100:8".to_string(),
        })
        .await
        .expect("send");
    assert_eq!(result.message_id, "1");
    let sent = api.notices();
    assert_eq!(sent[0].text, "agent reply");
    assert_eq!(sent[0].chat_id, -100);
    assert_eq!(sent[0].message_thread_id, 77);
    assert_eq!(
        sent[0].reply_to_message_id, 8,
        "复合引用里取出裸 message id"
    );

    // 非数值 chat id ⇒ 明确的传输错误（不是 panic）。
    let error = channel
        .send(OutboundMessage {
            chat_id: "not-a-number".to_string(),
            text: "x".to_string(),
            thread_id: String::new(),
            reply_to: String::new(),
        })
        .await
        .expect_err("bad chat id");
    assert!(error.to_string().contains("not a number"));

    // `disconnect` 是空操作（生命周期关在 `connect` 里）。
    channel.disconnect().await.expect("disconnect");
}

/// 上游的五个能力位逐条对齐；`Debug` 不吐令牌。
#[test]
fn capabilities_and_debug_match_the_upstream_declaration() {
    let channel = TelegramChannel::new(
        999,
        "my_bot",
        Sensitive::new("123:DO-NOT-LOG"),
        Arc::new(ScriptedApi::default()) as Arc<dyn TelegramApi>,
        None,
    );
    let bits = channel.capabilities();
    for want in [
        Capability::TEXT,
        Capability::THREAD_REPLY,
        Capability::QUOTE_REPLY,
        Capability::TYPING_INDICATOR,
        Capability::MESSAGE_EDIT,
    ] {
        assert!(bits.has(want), "缺能力位 {want:?}");
    }
    assert!(!bits.has(Capability::RICH_CARD));
    assert!(!bits.has(Capability::ATTACHMENT));
    assert_eq!(channel.kind(), ChannelKind::Telegram);

    let rendered = format!("{channel:?}");
    assert!(!rendered.contains("123:DO-NOT-LOG"), "{rendered}");
    assert!(rendered.contains("<redacted>"));
    assert!(rendered.contains("TelegramChannel"));
}

/// 工厂：四种拒装配 + 一种接受（`app_id` 必须是数值 id）。
#[test]
fn the_factory_rejects_configs_it_cannot_use() {
    let deps = TelegramDeps::plaintext();
    let build = factory(&deps);
    let config = |raw: serde_json::Value| ChannelConfig {
        kind: TYPE_TELEGRAM,
        raw,
        installation_id: None,
        handler: None,
    };
    let encoded = base64::engine::general_purpose::STANDARD.encode("123:token");

    // 配置不是 JSON 对象 / 字段类型不对。
    assert!(build(config(serde_json::json!("garbage"))).is_err());
    // 没有令牌。
    assert!(build(config(serde_json::json!({ "app_id": "123" }))).is_err());
    // `app_id` 不是数值 id。
    assert!(build(config(serde_json::json!({
        "app_id": "not-a-number",
        "bot_token_encrypted": encoded,
    })))
    .is_err());
    // 密文不是合法 base64。
    let error = build(config(serde_json::json!({
        "app_id": "123",
        "bot_token_encrypted": "not base64 !!",
    })))
    .err()
    .expect("bad base64");
    assert!(matches!(error, ChannelError::InvalidConfig { .. }));

    // 接受：合法配置交出一条可用的 channel。
    let good = build(config(serde_json::json!({
        "app_id": "123",
        "bot_username": "my_bot",
        "bot_token_encrypted": encoded,
    })))
    .expect("built");
    assert_eq!(good.kind(), ChannelKind::Telegram);
    assert!(good.capabilities().has(Capability::TEXT));

    // 失败关闭的解密器：有密文就拒装配（不把密文当令牌用）。
    let fail_closed = factory(&TelegramDeps::default());
    let error = fail_closed(config(serde_json::json!({
        "app_id": "123",
        "bot_token_encrypted": encoded,
    })))
    .err()
    .expect("fail closed");
    assert!(matches!(error, ChannelError::InvalidConfig { .. }));
    let rendered = format!("{error} {error:?}");
    assert!(
        !rendered.contains(&encoded),
        "错误路径回显了密文：{rendered}"
    );
    assert!(!rendered.contains("123:token"));
}

/// 注册面：`register`（失败关闭）与 `register_with`（接线好）都真的把工厂放进表里。
#[test]
fn registering_puts_a_factory_in_the_registry() {
    let registry = Registry::new();
    register(&registry, &no_op_deps());
    let refused = registry
        .build(channel_config_for_registry())
        .err()
        .expect("失败关闭的注册面拒装配带密文的配置");
    assert!(matches!(refused, ChannelError::InvalidConfig { .. }));
    assert_eq!(registry.kinds(), vec![ChannelKind::Telegram]);

    register_with(&registry, &TelegramDeps::plaintext());
    assert!(
        registry.build(channel_config_for_registry()).is_ok(),
        "接线好的注册面能装配"
    );
    assert!(format!("{:?}", fail_closed_deps()).contains("fail-closed"));
    assert_eq!(kind(), ChannelKind::Telegram);
}

// ---- 注册面用的最小 `ChannelDeps`（`register` 的签名要它；实现全部为 no-op） ----

struct NoInstallations;

#[async_trait]
impl InstallationStore for NoInstallations {
    async fn list_active(&self) -> EngineResult<Vec<Installation>> {
        Ok(Vec::new())
    }
}

struct NoLeases;

#[async_trait]
impl LeaseStore for NoLeases {
    async fn list_held(&self, _ids: &[Id]) -> EngineResult<HashSet<Id>> {
        Ok(HashSet::new())
    }
    async fn try_acquire(&self, _params: AcquireLeaseParams) -> EngineResult<()> {
        Ok(())
    }
    async fn renew(&self, _params: AcquireLeaseParams) -> EngineResult<()> {
        Ok(())
    }
    async fn release(&self, _params: ReleaseLeaseParams) -> EngineResult<()> {
        Ok(())
    }
}

/// 空 `ChannelDeps`（注册面只做类型装配，不启动任何连接）。
fn no_op_deps() -> ChannelDeps {
    let router = Arc::new(Router::new(
        Arc::new(NoCommands),
        Arc::new(StubTrigger),
        Arc::new(StubReader),
        Arc::new(StubIssues),
        RouterConfig::default(),
    ));
    ChannelDeps::new(router, Arc::new(NoInstallations), Arc::new(NoLeases))
}

/// 注册面用例的连接配置（带一个**合法**密文列）。
fn channel_config_for_registry() -> ChannelConfig {
    ChannelConfig {
        kind: TYPE_TELEGRAM,
        raw: serde_json::json!({
            "app_id": "123",
            "bot_username": "my_bot",
            "bot_token_encrypted": base64::engine::general_purpose::STANDARD.encode("123:token"),
        }),
        installation_id: None,
        handler: None,
    }
}
