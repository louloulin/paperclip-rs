//! `telegram::resolvers` 的用例（写者 M7-5）。
//!
//! 上游 `resolvers_test.go` 的会话路由判决逐条移植，外加**端到端**的一条：真 `Update` →
//! adapter 自己的归一化 → 安装路由 → 身份（未绑定）⇒ 回绑定卡且 `Router::route` 返回
//! `Ok(())`（**nil error，不是失败**）。端口替身全部只实现被测分支需要的方法。

use std::collections::HashMap;
use std::sync::Mutex;

use super::*;
use crate::engine::resolvers::{
    ChatRunParams, DropReason, EngineError, IssueCreator, NoCommands, Outcome, RouteResult,
    RunTriggerer, SessionReader, WorkspaceIdentity,
};
use crate::engine::{
    AppendParams, AppendResult, BindMediaParams, BindMediaResult, EnsureSessionParams, Router,
    RouterConfig, StartSessionParams, StartSessionResult,
};
use crate::telegram::api::{ApiError, ApiResult, EditMessageText, SendMessage};
use crate::telegram::inbound::{inbound_from_update, Chat, Message, RawEvent, Update, User};

// ---------------------------------------------------------------------------
// 装置
// ---------------------------------------------------------------------------

/// 一条安装行（默认 `active`）。
fn row(config: serde_json::Value) -> InstallationRow {
    InstallationRow {
        id: Id::new(),
        workspace_id: Id::new(),
        agent_id: Id::new(),
        installer_user_id: Id::new(),
        status: "active".to_string(),
        config,
    }
}

/// 一条落库形态的 config（`app_id` = bot id；令牌用**身份解密器**可读的 base64）。
fn config_with_bot_id(bot_id: &str) -> serde_json::Value {
    use base64::Engine as _;

    serde_json::json!({
        "app_id": bot_id,
        "bot_username": "my_bot",
        "bot_token_encrypted": base64::engine::general_purpose::STANDARD.encode("123:token"),
    })
}

/// 一条**真** `Update` → 归一化消息（走 adapter 自己的翻译）。
fn translated(text: &str) -> InboundMessage {
    let update = Update {
        update_id: 1,
        message: Some(Box::new(Message {
            message_id: 2,
            from: Some(User {
                id: 111,
                first_name: "Ada".to_string(),
                last_name: "L".to_string(),
                ..User::default()
            }),
            chat: Chat {
                id: 555,
                chat_type: "private".to_string(),
            },
            text: text.to_string(),
            ..Message::default()
        })),
    };
    inbound_from_update(&update, 999, "my_bot").expect("accepted")
}

/// 一条带论坛话题的入站消息（会话隔离键的用例）。
fn topic_message(chat_type: ChatType, thread_id: &str) -> InboundMessage {
    let update = Update {
        update_id: 1,
        message: Some(Box::new(Message {
            message_id: 2,
            from: Some(User {
                id: 111,
                first_name: "Ada".to_string(),
                ..User::default()
            }),
            chat: Chat {
                id: -100,
                chat_type: if chat_type == ChatType::P2p {
                    "private".to_string()
                } else {
                    "supergroup".to_string()
                },
            },
            text: "@my_bot hi".to_string(),
            message_thread_id: thread_id.parse::<i64>().unwrap_or(0),
            is_topic_message: !thread_id.is_empty(),
            ..Message::default()
        })),
    };
    inbound_from_update(&update, 999, "my_bot").expect("accepted")
}

// ---------------------------------------------------------------------------
// 端口替身
// ---------------------------------------------------------------------------

/// 安装查询替身：按行自己的 `config->>'app_id'` 建索引（替身不许比真仓储宽松）。
#[derive(Default)]
struct FakeInstallations {
    rows: HashMap<String, InstallationRow>,
}

#[async_trait]
impl InstallationQueries for FakeInstallations {
    async fn find_active_by_app_id(
        &self,
        app_id: &str,
    ) -> Result<Option<InstallationRow>, RepoError> {
        Ok(self.rows.get(app_id).cloned())
    }
}

/// 身份查询替身：`bound` 里的 `(installation, 平台用户 id)` 视为已绑定。
#[derive(Default)]
struct FakeIdentities {
    bound: HashMap<(Id, String), Id>,
    members: Vec<(Id, Id)>,
}

#[async_trait]
impl IdentityQueries for FakeIdentities {
    async fn find_user_binding(
        &self,
        installation_id: Id,
        channel_user_id: &str,
    ) -> Result<Option<ChannelUserBindingRow>, RepoError> {
        Ok(self
            .bound
            .get(&(installation_id, channel_user_id.to_string()))
            .map(|user_id| ChannelUserBindingRow {
                id: uuid::Uuid::new_v4(),
                workspace_id: uuid::Uuid::new_v4(),
                multica_user_id: user_id.0,
                installation_id: installation_id.0,
                channel_type: super::TYPE_TELEGRAM.storage_str().to_string(),
                channel_user_id: channel_user_id.to_string(),
                config: serde_json::json!({}),
                bound_at: chrono::Utc::now(),
            }))
    }

    async fn is_workspace_member(&self, workspace_id: Id, user_id: Id) -> Result<bool, RepoError> {
        Ok(self.members.contains(&(workspace_id, user_id)))
    }

    async fn upsert_user_binding(
        &self,
        _workspace_id: Id,
        _user_id: Id,
        _installation_id: Id,
        _channel_user_id: &str,
    ) -> Result<(), RepoError> {
        Ok(())
    }
}

/// 去重替身（本组用例绝不重复投递，claim 恒成功）。
struct StubDedup;

#[async_trait]
impl Deduper for StubDedup {
    async fn claim(&self, _installation_id: Id, _message_id: &str) -> EngineResult<Id> {
        Ok(Id::new())
    }
    async fn mark(&self, _i: Id, _m: &str, _t: Id) -> EngineResult<()> {
        Ok(())
    }
    async fn release(&self, _i: Id, _m: &str, _t: Id) -> EngineResult<()> {
        Ok(())
    }
}

/// 会话替身（未绑定分支永远不会调到它）。
struct StubSession;

#[async_trait]
impl SessionBinder for StubSession {
    async fn ensure_session(&self, _params: EnsureSessionParams) -> EngineResult<Id> {
        Err(EngineError::infra("unused"))
    }
    async fn start_session(&self, _params: StartSessionParams) -> EngineResult<StartSessionResult> {
        Err(EngineError::infra("unused"))
    }
    async fn mark_pending_fresh(&self, _s: Id, _m: &str) -> EngineResult<()> {
        Ok(())
    }
    async fn append_message(&self, _params: AppendParams) -> EngineResult<AppendResult> {
        Err(EngineError::infra("unused"))
    }
    async fn bind_media(&self, _params: BindMediaParams) -> EngineResult<BindMediaResult> {
        Err(EngineError::infra("unused"))
    }
}

/// 审计替身（记下丢弃原因）。
#[derive(Default)]
struct StubAudit {
    reasons: Mutex<Vec<DropReason>>,
}

#[async_trait]
impl Auditor for StubAudit {
    async fn record_drop(
        &self,
        _installation_id: Option<Id>,
        _message: &InboundMessage,
        reason: DropReason,
    ) -> EngineResult<()> {
        self.reasons.lock().expect("lock").push(reason);
        Ok(())
    }
}

/// 回复器替身（记下判决）。
#[derive(Default)]
struct StubReplier {
    outcomes: Mutex<Vec<Outcome>>,
}

impl OutboundReplier for StubReplier {
    fn reply(
        &self,
        _installation: &ResolvedInstallation,
        _message: &InboundMessage,
        result: &RouteResult,
    ) {
        self.outcomes.lock().expect("lock").push(result.outcome);
    }
}

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

/// 打字指示器的 API 替身（记下每次 chat action）。
#[derive(Default)]
struct StubApi {
    actions: Mutex<Vec<(i64, i64)>>,
}

#[async_trait]
impl TelegramApi for StubApi {
    async fn get_me(&self, _bot_token: &str) -> ApiResult<User> {
        Err(ApiError::Malformed { method: "getMe" })
    }
    async fn get_webhook_info(
        &self,
        _bot_token: &str,
    ) -> ApiResult<crate::telegram::api::WebhookInfo> {
        Err(ApiError::Malformed {
            method: "getWebhookInfo",
        })
    }
    async fn get_updates(&self, _bot_token: &str, _offset: i64) -> ApiResult<Vec<Update>> {
        Err(ApiError::Malformed {
            method: "getUpdates",
        })
    }
    async fn send_message(&self, _bot_token: &str, _params: &SendMessage) -> ApiResult<Message> {
        Err(ApiError::Malformed {
            method: "sendMessage",
        })
    }
    /// M7-6 补的端口方法：打字指示器路径不编辑消息。
    async fn edit_message_text(
        &self,
        _bot_token: &str,
        _params: &EditMessageText,
    ) -> ApiResult<()> {
        Err(ApiError::Malformed {
            method: "editMessageText",
        })
    }

    async fn send_chat_action(
        &self,
        _bot_token: &str,
        chat_id: i64,
        message_thread_id: i64,
    ) -> ApiResult<()> {
        self.actions
            .lock()
            .expect("actions")
            .push((chat_id, message_thread_id));
        Ok(())
    }
}

/// 装好一台 router（安装行 + 三个端口）。
fn telegram_router(
    installation: InstallationRow,
    identities: FakeIdentities,
    replier: Arc<StubReplier>,
    audit: Arc<StubAudit>,
) -> Router {
    let mut installations = FakeInstallations::default();
    let app_id = installation
        .config
        .get("app_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    installations.rows.insert(app_id, installation);
    let set = TelegramResolverSet::new(
        Arc::new(TelegramInstallationResolver::new(Arc::new(installations))),
        Arc::new(TelegramIdentityResolver::new(Arc::new(identities))),
        Arc::new(StubDedup),
        Arc::new(StubSession),
        audit,
    )
    .with_replier(replier);
    let router = Router::new(
        Arc::new(NoCommands) as Arc<dyn crate::engine::resolvers::CommandClassifier>,
        Arc::new(StubTrigger),
        Arc::new(StubReader),
        Arc::new(StubIssues),
        RouterConfig::default(),
    );
    router.register(TYPE_TELEGRAM, set.into_engine_set());
    router
}

// ---------------------------------------------------------------------------
// 用例
// ---------------------------------------------------------------------------

/// **本片的专属验收之一**：真 update → 归一化 → 安装路由 → 身份（未绑定）⇒ 回绑定卡，
/// 且 `route` 返回 `Ok(())`（nil error，**不是失败**）+ 一条 `unbound_user` 审计。
#[tokio::test]
async fn unbound_sender_gets_a_binding_card_and_no_error() {
    let replier = Arc::new(StubReplier::default());
    let audit = Arc::new(StubAudit::default());
    let router = telegram_router(
        row(config_with_bot_id("999")),
        FakeIdentities::default(),
        Arc::clone(&replier),
        Arc::clone(&audit),
    );
    router
        .route(translated("hello bot"))
        .await
        .expect("未绑定必须是判决，不是 Err");
    // 出站回复是 `tokio::spawn` 的（同步接缝）：给它一点时间跑完再断言。
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    assert_eq!(
        replier.outcomes.lock().expect("lock").clone(),
        vec![Outcome::NeedsBinding],
        "未绑定 ⇒ 驱动绑定卡"
    );
    assert_eq!(
        audit.reasons.lock().expect("lock").clone(),
        vec![DropReason::UnboundUser]
    );
}

/// 绑定存在但**成员资格已失效** ⇒ `SenderNotMember` 判决（上游逐字：绑定行的存在不再证明
/// 成员资格，泛化层没有 member 外键）。
#[tokio::test]
async fn a_stale_binding_is_refused_when_membership_is_gone() {
    let installation = row(config_with_bot_id("999"));
    let user = Id::new();
    let mut identities = FakeIdentities::default();
    identities
        .bound
        .insert((installation.id, "111".to_string()), user);
    // 刻意**不**把 (workspace, user) 放进 members。
    let replier = Arc::new(StubReplier::default());
    let audit = Arc::new(StubAudit::default());
    let router = telegram_router(
        installation,
        identities,
        Arc::clone(&replier),
        Arc::clone(&audit),
    );
    router.route(translated("hello")).await.expect("不是 Err");
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    assert_eq!(
        audit.reasons.lock().expect("lock").clone(),
        vec![DropReason::NonWorkspaceMember]
    );
    assert_eq!(
        replier.outcomes.lock().expect("lock").clone(),
        vec![Outcome::Dropped],
        "非成员 ⇒ 丢弃判决（回复器只在**被寻址的 /issue** 上才回文案）"
    );
}

/// 认不出的 bot id ⇒ 产品性丢弃（`invalid_event`），同样不是错误。
#[tokio::test]
async fn unknown_bot_id_is_dropped_without_an_error() {
    let replier = Arc::new(StubReplier::default());
    let audit = Arc::new(StubAudit::default());
    // 行里的 app_id 是 `123`，消息里的 bot id 是 `999` ⇒ 认不出。
    let router = telegram_router(
        row(config_with_bot_id("123")),
        FakeIdentities::default(),
        Arc::clone(&replier),
        Arc::clone(&audit),
    );
    router.route(translated("hello")).await.expect("不是 Err");
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    assert_eq!(
        audit.reasons.lock().expect("lock").clone(),
        vec![DropReason::InvalidEvent]
    );
    assert!(replier.outcomes.lock().expect("lock").is_empty());
}

/// 安装行的 `status` 决定 `active`；撤销过的行**仍**会被本解析器解出来（真仓储的
/// `find_active_by_app_id` 才会过滤掉它）——这里钉的是 adapter 侧的投影语义。
#[tokio::test]
async fn the_resolved_installation_carries_its_status_and_opaque_platform_row() {
    let mut installation = row(config_with_bot_id("999"));
    installation.status = "revoked".to_string();
    let mut rows = FakeInstallations::default();
    rows.rows.insert("999".to_string(), installation.clone());
    let resolver = TelegramInstallationResolver::new(Arc::new(rows));
    let message = translated("hello");
    let projection = InstallationResolver::resolve_installation(&resolver, &message)
        .await
        .expect("resolved");
    assert_eq!(projection.kind, TYPE_TELEGRAM);
    assert!(!projection.active, "revoked ⇒ inactive（Router 据此丢弃）");
    assert_eq!(projection.id, installation.id);
    let platform = installation_row(&projection).expect("不透明平台值");
    assert_eq!(platform.config, installation.config);
    assert!(!platform.is_active());
}

/// 会话路由（上游 `TestTelegramSessionRouting`）：私聊 = chat id；论坛话题 = `chat:thread`。
#[test]
fn session_routing_isolates_forum_topics_only() {
    let p2p = topic_message(ChatType::P2p, "");
    let routing = session_routing(&p2p);
    assert_eq!(routing.binding_key, "-100");
    assert_eq!(routing.reply_thread, "");

    let plain_group = topic_message(ChatType::Group, "");
    let routing = session_routing(&plain_group);
    assert_eq!(routing.binding_key, "-100");
    assert_eq!(routing.reply_thread, "");

    let topic = topic_message(ChatType::Group, "77");
    let routing = session_routing(&topic);
    assert_eq!(routing.binding_key, "-100:77");
    assert_eq!(routing.reply_thread, "77");
}

/// 运行时的隔离键策略与上面的判决**等价**（只差分隔符 `#` vs `:`，M7-3-D1 已登记的偏离）。
#[test]
fn the_runtime_policy_matches_the_upstream_routing_modulo_the_separator() {
    for (chat_type, thread) in [
        (ChatType::P2p, ""),
        (ChatType::Group, ""),
        (ChatType::Group, "77"),
    ] {
        let message = topic_message(chat_type, thread);
        let upstream = session_routing(&message);
        let composed = BindingKeyPolicy::ChatIdPlusThreadRoot.compose(&message);
        assert_eq!(
            composed,
            upstream.binding_key.replace(':', "#"),
            "{chat_type:?}/{thread} 的隔离键形态不一致"
        );
    }
}

/// 打字指示器：送 `chat_id` + 话题；**解密失败时一次都不打**（凭据纪律的失败关闭）。
#[tokio::test]
async fn typing_shows_ingested_topics_and_fails_closed_without_a_decrypter() {
    let api = Arc::new(StubApi::default());
    let installation = row(config_with_bot_id("999"));
    let mut resolved = ResolvedInstallation::new(
        installation.id,
        installation.workspace_id,
        installation.agent_id,
        installation.installer_user_id,
        TYPE_TELEGRAM,
        true,
    );
    resolved.platform = Some(Arc::new(installation.clone()));

    let notifier = TelegramTypingNotifier::new(
        Arc::clone(&api) as Arc<dyn TelegramApi>,
        Decrypter::plaintext(),
    );
    notifier
        .show_now(&resolved, &topic_message(ChatType::Group, "77"))
        .await;
    assert_eq!(
        api.actions.lock().expect("actions").clone(),
        vec![(-100, 77)],
        "话题消息带上 message_thread_id"
    );

    // fail-closed：密文解不开 ⇒ 不调用 API（也就不会把密文当令牌发出去）。
    let fail_closed = TelegramTypingNotifier::new(
        Arc::clone(&api) as Arc<dyn TelegramApi>,
        Decrypter::fail_closed(),
    );
    fail_closed
        .show_now(&resolved, &topic_message(ChatType::Group, "77"))
        .await;
    assert_eq!(
        api.actions.lock().expect("actions").len(),
        1,
        "解密失败必须失败关闭"
    );

    // 没有平台行的 resolved（工厂未注入）也不 panic、不调用。
    let bare = ResolvedInstallation::new(
        installation.id,
        installation.workspace_id,
        installation.agent_id,
        installation.installer_user_id,
        TYPE_TELEGRAM,
        true,
    );
    fail_closed
        .show_now(&bare, &topic_message(ChatType::Group, "77"))
        .await;
    assert_eq!(api.actions.lock().expect("actions").len(), 1);
    // `on_settled` 是空操作（Telegram 的 chat action 自己会过期）。
    notifier.on_settled(Id::new());
    assert!(format!("{notifier:?}").contains("TelegramTypingNotifier"));
}

/// `ResolverSet` 的装配：`origin_type` 逐字 = `telegram_chat`，可选端口按注入与否出现。
#[test]
fn resolver_set_assembly() {
    let set = TelegramResolverSet::new(
        Arc::new(TelegramInstallationResolver::new(Arc::new(
            FakeInstallations::default(),
        ))),
        Arc::new(TelegramIdentityResolver::new(Arc::new(
            FakeIdentities::default(),
        ))),
        Arc::new(StubDedup),
        Arc::new(StubSession),
        Arc::new(StubAudit::default()),
    );
    let engine_set = set.into_engine_set();
    assert_eq!(engine_set.origin_type, "telegram_chat");
    assert!(engine_set.media.is_none());
    assert!(engine_set.replier.is_none());
    assert!(engine_set.typing.is_none());
    assert_eq!(origin_type(), "telegram_chat");
    assert_eq!(kind(), TYPE_TELEGRAM);
    let rendered = format!("{engine_set:?}");
    assert!(rendered.contains("telegram_chat"));
}

/// 可选端口：挂上之后 engine 侧真的看得到（`with_*` 的回归防线）。
#[test]
fn optional_ports_show_up_on_the_engine_set() {
    let set = TelegramResolverSet::new(
        Arc::new(TelegramInstallationResolver::new(Arc::new(
            FakeInstallations::default(),
        ))),
        Arc::new(TelegramIdentityResolver::new(Arc::new(
            FakeIdentities::default(),
        ))),
        Arc::new(StubDedup),
        Arc::new(StubSession),
        Arc::new(StubAudit::default()),
    )
    .with_replier(Arc::new(StubReplier::default()))
    .with_typing(Arc::new(TelegramTypingNotifier::new(
        Arc::new(StubApi::default()),
        Decrypter::plaintext(),
    )));
    let engine_set = set.into_engine_set();
    assert!(engine_set.replier.is_some());
    assert!(engine_set.typing.is_some());
    assert!(engine_set.media.is_none());
}

/// `raw` 里承载 bot id（安装路由只读它）。
#[test]
fn the_raw_envelope_carries_the_routing_key() {
    let message = translated("hello");
    let raw: RawEvent = serde_json::from_value(message.raw.clone()).expect("raw");
    assert_eq!(raw.bot_id, "999");
    assert_eq!(raw.sender_name, "Ada L");
    assert_eq!(raw.event_type, "message");
}
