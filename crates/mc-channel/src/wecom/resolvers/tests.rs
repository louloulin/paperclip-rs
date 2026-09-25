//! `resolvers.rs` 的用例（上游 `resolvers_test.go`，**约 300 行**）。
//!
//! 上游那五个解析器里，两个（去重 / 审计）本片直接用 M7-2 的泛化实现 ⇒ 它们的语义由那些片自己的
//! 用例钉；本文件钉的是**平台特有**的三处翻译（安装路由 / 身份绑定 / 会话绑定）与端口包的形状。

use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use mc_core::channel::message::{ChatType, InboundMessage, MessageKind, Source};
use mc_core::channel::{ChannelKind, InstallationStatus};
use mc_core::id::Id;
use mc_repos::RepoError;

use super::{
    wecom_msg_from_raw, IdentityQueries, InstallationQueries, WeComIdentityResolver,
    WeComInstallationResolver, WeComResolverSet, WeComSessionBinder, ORIGIN_WECOM_CHAT,
};
use crate::engine::resolvers::{
    AppendParams, AppendResult, BindMediaParams, BindMediaResult, Deduper, EngineResult,
    EnsureSessionParams, IdentityResolver, InstallationResolver, Outcome, PipelineError,
    SessionBinder, StartSessionParams, StartSessionResult,
};
use crate::wecom::types::Installation;
use crate::wecom::wecom_channel::inbound::WeComInboundMessage;

// =====================================================================
// 替身
// =====================================================================

/// 安装查询替身：一条固定安装（或"没有"）。
struct FakeInstallations {
    found: Option<Installation>,
    seen_bot_ids: Mutex<Vec<String>>,
}

impl FakeInstallations {
    fn with(installation: Installation) -> Self {
        Self {
            found: Some(installation),
            seen_bot_ids: Mutex::new(Vec::new()),
        }
    }

    fn empty() -> Self {
        Self {
            found: None,
            seen_bot_ids: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl InstallationQueries for FakeInstallations {
    async fn find_active_by_bot_id(&self, bot_id: &str) -> Result<Option<Installation>, RepoError> {
        self.seen_bot_ids
            .lock()
            .expect("lock")
            .push(bot_id.to_string());
        Ok(self.found.clone())
    }
}

/// 身份查询替身。
struct FakeIdentity {
    binding: Option<Id>,
    member: bool,
}

#[async_trait]
impl IdentityQueries for FakeIdentity {
    async fn find_user_binding(
        &self,
        _installation_id: Id,
        _channel_user_id: &str,
    ) -> Result<Option<Id>, RepoError> {
        Ok(self.binding)
    }

    async fn is_workspace_member(
        &self,
        _workspace_id: Id,
        _user_id: Id,
    ) -> Result<bool, RepoError> {
        Ok(self.member)
    }
}

/// 会话绑定替身：把**收到的**参数记下来（本片的映射就是它验证的对象）。
#[derive(Default)]
struct RecordingSession {
    ensured: Mutex<Vec<String>>,
    appended: Mutex<Vec<String>>,
    started: Mutex<usize>,
    bound_media: Mutex<usize>,
}

#[async_trait]
impl SessionBinder for RecordingSession {
    async fn ensure_session(&self, params: EnsureSessionParams) -> EngineResult<Id> {
        self.ensured
            .lock()
            .expect("lock")
            .push(params.message.source.chat_id.clone());
        Ok(Id::new())
    }

    async fn start_session(&self, _params: StartSessionParams) -> EngineResult<StartSessionResult> {
        *self.started.lock().expect("lock") += 1;
        Ok(StartSessionResult {
            session_id: Id::new(),
            binding_id: None,
            route_revision: 1,
            append: AppendResult::default(),
        })
    }

    async fn mark_pending_fresh(&self, _session_id: Id, _message_id: &str) -> EngineResult<()> {
        Ok(())
    }

    async fn append_message(&self, params: AppendParams) -> EngineResult<AppendResult> {
        self.appended
            .lock()
            .expect("lock")
            .push(params.message.command_source_text().to_string());
        Ok(AppendResult::default())
    }

    async fn bind_media(&self, _params: BindMediaParams) -> EngineResult<BindMediaResult> {
        *self.bound_media.lock().expect("lock") += 1;
        Ok(BindMediaResult::default())
    }
}

/// 去重替身（本片用 M7-2 的实现 ⇒ 这里只要一个能装配的）。
struct NoDedup;

#[async_trait]
impl Deduper for NoDedup {
    async fn claim(&self, _installation_id: Id, _message_id: &str) -> EngineResult<Id> {
        Ok(Id::new())
    }
    async fn mark(&self, _installation_id: Id, _message_id: &str, _t: Id) -> EngineResult<()> {
        Ok(())
    }
    async fn release(&self, _installation_id: Id, _message_id: &str, _t: Id) -> EngineResult<()> {
        Ok(())
    }
}

struct NoAudit;

#[async_trait]
impl crate::engine::Auditor for NoAudit {
    async fn record_drop(
        &self,
        _installation_id: Option<Id>,
        _message: &InboundMessage,
        _reason: crate::engine::DropReason,
    ) -> EngineResult<()> {
        Ok(())
    }
}

fn installation() -> Installation {
    let now = chrono::Utc::now();
    Installation {
        id: Id::new(),
        workspace_id: Id::new(),
        agent_id: Id::new(),
        installer_user_id: Id::new(),
        status: InstallationStatus::Active,
        bot_id: "bot_5f1c9a".to_string(),
        secret_encrypted: vec![1, 2, 3],
        bot_display_name: "Multica Bot".to_string(),
        config: serde_json::Value::Null,
        installed_at: now,
        created_at: now,
        updated_at: now,
    }
}

/// 造一条归一化消息（`raw` 是 wecom 那一侧的信封）。
fn message(bot_id: &str, chat_type: ChatType, sender_id: &str) -> InboundMessage {
    let raw = WeComInboundMessage {
        bot_id: bot_id.to_string(),
        msg_id: "m1".to_string(),
        msg_type: "text".to_string(),
        chat_type: chat_type.as_str().to_string(),
        chat_id: "chat_1".to_string(),
        sender_user_id: sender_id.to_string(),
        content: "你好".to_string(),
        req_id: "req-1".to_string(),
        media: Vec::new(),
    }
    .to_raw_value();
    InboundMessage {
        event_id: "m1".to_string(),
        message_id: "m1".to_string(),
        source: Source {
            channel_type: ChannelKind::WeCom,
            chat_id: "chat_1".to_string(),
            chat_type,
            sender_id: sender_id.to_string(),
            sender_stable_id: String::new(),
            thread_id: String::new(),
        },
        kind: MessageKind::Text,
        text: "你好".to_string(),
        command_text: String::new(),
        has_selected_context: false,
        media_refs: Vec::new(),
        reply_to: None,
        addressed_to_bot: true,
        force_fresh: false,
        skip_agent_run: false,
        raw,
    }
}

// =====================================================================
// 信封解码
// =====================================================================

/// 上游 `TestWecomMsgFromRaw`。
#[test]
fn raw_decodes_and_bad_raw_is_an_invalid_event() {
    let good = message("bot_1", ChatType::P2p, "u1");
    let decoded = wecom_msg_from_raw(&good).expect("raw");
    assert_eq!(decoded.bot_id, "bot_1");
    assert_eq!(decoded.req_id, "req-1");

    // `null` ⇒ 认不出这条事件（产品性丢弃，不是基础设施失败）。
    let mut null = good.clone();
    null.raw = serde_json::Value::Null;
    let error = wecom_msg_from_raw(&null).expect_err("null");
    assert_eq!(
        error,
        PipelineError::InstallationNotFound.into(),
        "raw 缺失是产品性判决"
    );

    // 不是 wecom 的形状（别的渠道的 raw）。
    let mut foreign = good.clone();
    foreign.raw = serde_json::json!({"app_id": "cli_a"});
    assert!(wecom_msg_from_raw(&foreign).is_err());
}

// =====================================================================
// 安装路由
// =====================================================================

/// 上游 `TestInstallationResolver*`：用连接盖章的 `bot_id` 查、撤销 ⇒ `active=false`、
/// 查不到 ⇒ `installation_not_found`。
#[tokio::test]
async fn installation_resolution_is_a_lookup_on_the_stamped_bot_id() {
    let expected = installation();
    let queries = Arc::new(FakeInstallations::with(expected.clone()));
    let resolver =
        WeComInstallationResolver::new(Arc::clone(&queries) as Arc<dyn InstallationQueries>);

    let routed = resolver
        .resolve_installation(&message("bot_5f1c9a", ChatType::P2p, "u1"))
        .await
        .expect("resolved");
    assert_eq!(routed.id, expected.id);
    assert_eq!(routed.workspace_id, expected.workspace_id);
    assert_eq!(routed.agent_id, expected.agent_id);
    assert_eq!(routed.installer_user_id, expected.installer_user_id);
    assert!(routed.active);
    assert_eq!(routed.kind, ChannelKind::WeCom);
    // 平台值放进 `platform` 供同组端口复用，而 `Debug` 只报"有一个"（不打印可能含凭据的值）。
    assert!(routed.platform.is_some());
    assert!(format!("{routed:?}").contains("<opaque>"));
    assert_eq!(
        queries.seen_bot_ids.lock().expect("lock").as_slice(),
        ["bot_5f1c9a"]
    );

    // 空 `bot_id` ⇒ 没有路由键，**不**查库。
    let empty = Arc::new(FakeInstallations::with(expected));
    let resolver =
        WeComInstallationResolver::new(Arc::clone(&empty) as Arc<dyn InstallationQueries>);
    let error = resolver
        .resolve_installation(&message("", ChatType::P2p, "u1"))
        .await
        .expect_err("empty bot id");
    assert_eq!(error, PipelineError::InstallationNotFound.into());
    assert!(empty.seen_bot_ids.lock().expect("lock").is_empty());

    // 查不到 ⇒ `installation_not_found`（产品性丢弃）。
    let missing = Arc::new(FakeInstallations::empty());
    let resolver =
        WeComInstallationResolver::new(Arc::clone(&missing) as Arc<dyn InstallationQueries>);
    let error = resolver
        .resolve_installation(&message("bot_unknown", ChatType::P2p, "u1"))
        .await
        .expect_err("missing");
    assert_eq!(error, PipelineError::InstallationNotFound.into());

    // 撤销的安装 ⇒ `active=false`（Router 据此记 `revoked_installation` 丢弃）。
    let mut revoked = installation();
    revoked.status = InstallationStatus::Revoked;
    let queries = Arc::new(FakeInstallations::with(revoked));
    let resolver =
        WeComInstallationResolver::new(Arc::clone(&queries) as Arc<dyn InstallationQueries>);
    let routed = resolver
        .resolve_installation(&message("bot_5f1c9a", ChatType::P2p, "u1"))
        .await
        .expect("resolved");
    assert!(!routed.active);
}

// =====================================================================
// 身份绑定
// =====================================================================

/// 上游 `TestIdentityResolver*`：没有绑定 ⇒ `sender_unbound`；不是成员 ⇒ `sender_not_member`。
#[tokio::test]
async fn identity_resolution_distinguishes_the_three_outcomes() {
    let expected = installation();
    let routed = crate::engine::ResolvedInstallation::new(
        expected.id,
        expected.workspace_id,
        expected.agent_id,
        expected.installer_user_id,
        ChannelKind::WeCom,
        true,
    );
    let message = message("bot_5f1c9a", ChatType::P2p, "u1");

    // 未绑定 ⇒ `needs_binding`（产品性：Router 据此发绑定卡，**不是**错误）。
    let unbound = Arc::new(FakeIdentity {
        binding: None,
        member: true,
    });
    let resolver = WeComIdentityResolver::new(Arc::clone(&unbound) as Arc<dyn IdentityQueries>);
    let error = resolver
        .resolve_sender(&routed, &message)
        .await
        .expect_err("unbound");
    assert_eq!(error, PipelineError::SenderUnbound.into());

    // 绑了但不是成员 ⇒ `non_workspace_member` 丢弃（**不**再提示一次）。
    let stranger = Arc::new(FakeIdentity {
        binding: Some(Id::new()),
        member: false,
    });
    let resolver = WeComIdentityResolver::new(Arc::clone(&stranger) as Arc<dyn IdentityQueries>);
    let error = resolver
        .resolve_sender(&routed, &message)
        .await
        .expect_err("not a member");
    assert_eq!(error, PipelineError::SenderNotMember.into());

    // 绑了且是成员 ⇒ 那个用户。
    let member = Id::new();
    let bound = Arc::new(FakeIdentity {
        binding: Some(member),
        member: true,
    });
    let resolver = WeComIdentityResolver::new(Arc::clone(&bound) as Arc<dyn IdentityQueries>);
    let identity = resolver
        .resolve_sender(&routed, &message)
        .await
        .expect("bound");
    assert_eq!(identity.user_id, member);

    // 空的发件人 id ⇒ 直接 unbound（不查库）。
    let mut blank = message.clone();
    blank.source.sender_id = "  ".to_string();
    let error = resolver
        .resolve_sender(&routed, &blank)
        .await
        .expect_err("blank");
    assert_eq!(error, PipelineError::SenderUnbound.into());
}

// =====================================================================
// 会话绑定
// =====================================================================

/// 上游 `TestSessionBinder_*`：五处映射里可**在这里**验证的那几条 —— 隔离键、指令源、媒体预算、
/// 绑媒体的四个 issue 字段。
#[tokio::test]
async fn the_session_binder_maps_wecom_semantics_through() {
    let expected = installation();
    let routed = crate::engine::ResolvedInstallation::new(
        expected.id,
        expected.workspace_id,
        expected.agent_id,
        expected.installer_user_id,
        ChannelKind::WeCom,
        true,
    );
    let inner = Arc::new(RecordingSession::default());
    let binder = WeComSessionBinder::new(Arc::clone(&inner) as Arc<dyn SessionBinder>);
    let message = message("bot_5f1c9a", ChatType::Group, "u1");

    binder
        .ensure_session(EnsureSessionParams {
            installation: routed.clone(),
            sender: expected.installer_user_id,
            message: message.clone(),
        })
        .await
        .expect("ensure");
    // 隔离键 = `Source.ChatID`（群里是 chatid；单聊里它就是 userid）。
    assert_eq!(
        inner.ensured.lock().expect("lock").as_slice(),
        ["chat_1"],
        "会话隔离键用的是 Source.ChatID"
    );

    // 指令源：adapter 写了 `command_text` 就用它（**不**用 text 覆盖 —— 上游那一次事故）。
    let mut with_command = message.clone();
    with_command.text = "@Multica Bot /issue 标题".to_string();
    with_command.command_text = "/issue 标题".to_string();
    binder
        .append_message(AppendParams {
            session_id: Id::new(),
            sender: expected.installer_user_id,
            installation_id: expected.id,
            message: with_command,
            claim_token: None,
            media_pending_seconds: 45.0,
        })
        .await
        .expect("append");
    assert_eq!(
        inner.appended.lock().expect("lock").as_slice(),
        ["/issue 标题"],
        "adapter 的指令源优先，text 只作回落"
    );

    // 没有 `command_text` ⇒ 退到 `text`（`command_source_text` 的空值语义）。
    let mut plain = message.clone();
    plain.text = "今天天气不错".to_string();
    plain.command_text = String::new();
    binder
        .append_message(AppendParams {
            session_id: Id::new(),
            sender: expected.installer_user_id,
            installation_id: expected.id,
            message: plain,
            claim_token: None,
            media_pending_seconds: 0.0,
        })
        .await
        .expect("append");
    assert_eq!(
        inner.appended.lock().expect("lock").as_slice(),
        ["/issue 标题", "今天天气不错"]
    );

    // 会话轮换与"绑媒体"都是**透传**（不是 no-op —— 返回 `Ok` 在 Router 眼里读成"绑好了"）。
    binder
        .start_session(StartSessionParams {
            installation: routed,
            creator: expected.installer_user_id,
            sender: expected.installer_user_id,
            message,
            claim_token: None,
            media_pending_seconds: 45.0,
            persist_message: true,
        })
        .await
        .expect("start");
    assert_eq!(*inner.started.lock().expect("lock"), 1);
    binder
        .bind_media(BindMediaParams {
            message_id: Some(Id::new()),
            session_id: Id::new(),
            workspace_id: expected.workspace_id,
            sender: Id::new(),
            issue_id: None,
            issue_description_base: None,
            issue_command_text: String::new(),
            body: String::new(),
            media_refs: Vec::new(),
        })
        .await
        .expect("bind");
    assert_eq!(*inner.bound_media.lock().expect("lock"), 1);
    binder
        .mark_pending_fresh(Id::new(), "m1")
        .await
        .expect("mark");
}

// =====================================================================
// 端口包
// =====================================================================

/// 上游 `TestNewResolverSet_WiresAllResolvers`：五个必填端口都在、`origin_type` 逐字、
/// 三个可选面**留空**（wecom 没有打字指示；媒体 / 回复器由宿主按部署能力挂）。
#[test]
fn the_resolver_set_carries_the_wecom_origin_and_no_typing() {
    let set = WeComResolverSet::new(
        Arc::new(WeComInstallationResolver::new(Arc::new(
            FakeInstallations::empty(),
        ))),
        Arc::new(WeComIdentityResolver::new(Arc::new(FakeIdentity {
            binding: None,
            member: true,
        }))),
        Arc::new(NoDedup),
        Arc::new(WeComSessionBinder::new(Arc::new(
            RecordingSession::default(),
        ))),
        Arc::new(NoAudit),
    );
    let engine = set.into_engine_set();
    assert_eq!(engine.origin_type, ORIGIN_WECOM_CHAT);
    assert_eq!(ORIGIN_WECOM_CHAT, "wecom_chat");
    // 三个可选面默认关闭 —— `None` 从类型上就是 no-op，不需要"统一返回 503"那类退化。
    assert!(engine.media.is_none());
    assert!(engine.replier.is_none());
    assert!(engine.typing.is_none());
    assert!(format!("{engine:?}").contains("wecom_chat"));
}

/// `wecom_origin` 与 lark 的 `lark_chat` 同形（平台 + `_chat`）。
#[test]
fn the_origin_label_follows_the_family() {
    assert!(ORIGIN_WECOM_CHAT.ends_with("_chat"));
    assert_eq!(
        crate::wecom::wecom_channel::origin_type(),
        ORIGIN_WECOM_CHAT
    );
    assert_eq!(Outcome::NeedsBinding.as_str(), "needs_binding");
}
