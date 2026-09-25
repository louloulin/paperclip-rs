//! 解析器集合的用例（`resolvers.rs` 的 `#[cfg(test)] mod tests;`）。
//!
//! 五个端口的判决都用替身钉住（**不需要真库**）：安装路由、身份与成员资格、会话隔离键、
//! 出站寻址的兜底、以及"群清单观察是尽力而为的"这一条。装配面（泛化仓储）只在真库用例里
//! 才成立，所以这里断言的是**形状**（`origin_type` / 可选端口的存在性）。

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mc_core::channel::message::{ChatType, InboundMessage};
use mc_core::id::Id;
use mc_repos::channel::binding::ChannelUserBindingRow;
use mc_repos::channel::session::ChannelChatSessionBindingRow;
use mc_repos::RepoError;

use super::{
    dingtalk_session_routing, dingtalk_visible_quote_text, installation_row, kind, origin_type,
    outbound_target, DingTalkBindingConfig, DingTalkIdentityResolver, DingTalkInstallationResolver,
    DingTalkResolverSet, DingTalkSessionBinder, GroupPresenceObserver, IdentityQueries,
    InstallationQueries, InstallationRow, NoGroupPresence,
};
use crate::dingtalk::inbound::{
    decode_dingtalk_raw, inbound_from_callback, BotCallbackData, TYPE_DINGTALK,
};
use crate::engine::resolvers::{
    AppendParams, AppendResult, BindMediaParams, BindMediaResult, Deduper, EngineError,
    EngineResult, EnsureSessionParams, IdentityResolver, InstallationResolver, PipelineError,
    ResolvedInstallation, SessionBinder, StartSessionParams, StartSessionResult,
};

// =====================================================================
// 替身
// =====================================================================

struct FakeInstallations {
    rows: Mutex<Vec<InstallationRow>>,
    seen: Mutex<Vec<String>>,
}

impl FakeInstallations {
    fn with(row: InstallationRow) -> Arc<Self> {
        Arc::new(Self {
            rows: Mutex::new(vec![row]),
            seen: Mutex::new(Vec::new()),
        })
    }

    fn empty() -> Arc<Self> {
        Arc::new(Self {
            rows: Mutex::new(Vec::new()),
            seen: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl InstallationQueries for FakeInstallations {
    async fn find_active_by_app_id(
        &self,
        app_id: &str,
    ) -> Result<Option<InstallationRow>, RepoError> {
        self.seen.lock().expect("lock").push(app_id.to_string());
        Ok(self
            .rows
            .lock()
            .expect("lock")
            .iter()
            .find(|row| row.app_id() == app_id)
            .cloned())
    }
}

struct FakeIdentity {
    binding: Option<ChannelUserBindingRow>,
    is_member: bool,
}

fn binding_row(user: Id) -> ChannelUserBindingRow {
    ChannelUserBindingRow {
        id: uuid::Uuid::new_v4(),
        workspace_id: uuid::Uuid::new_v4(),
        multica_user_id: user.0,
        installation_id: uuid::Uuid::new_v4(),
        channel_type: TYPE_DINGTALK.storage_str().to_string(),
        channel_user_id: "staff".to_string(),
        config: serde_json::Value::Null,
        bound_at: chrono::Utc::now(),
    }
}

#[async_trait]
impl IdentityQueries for FakeIdentity {
    async fn find_user_binding(
        &self,
        _installation_id: Id,
        _channel_user_id: &str,
    ) -> Result<Option<ChannelUserBindingRow>, RepoError> {
        Ok(self.binding.clone())
    }

    async fn is_workspace_member(
        &self,
        _workspace_id: Id,
        _user_id: Id,
    ) -> Result<bool, RepoError> {
        Ok(self.is_member)
    }
}

#[derive(Default)]
struct RecordingPresence {
    observed: Mutex<Vec<String>>,
    activities: Mutex<Vec<String>>,
    fail: bool,
}

#[async_trait]
impl GroupPresenceObserver for RecordingPresence {
    async fn observe(
        &self,
        _installation: &ResolvedInstallation,
        message: &InboundMessage,
    ) -> EngineResult<()> {
        self.observed
            .lock()
            .expect("lock")
            .push(message.source.chat_id.clone());
        if self.fail {
            return Err(EngineError::infra("presence write failed"));
        }
        Ok(())
    }

    async fn record_activity(
        &self,
        _installation_id: Id,
        message: &InboundMessage,
    ) -> EngineResult<()> {
        self.activities
            .lock()
            .expect("lock")
            .push(message.source.chat_id.clone());
        if self.fail {
            return Err(EngineError::infra("activity write failed"));
        }
        Ok(())
    }
}

#[derive(Default)]
struct StubBinder {
    calls: Mutex<Vec<&'static str>>,
}

#[async_trait]
impl SessionBinder for StubBinder {
    async fn ensure_session(&self, _params: EnsureSessionParams) -> EngineResult<Id> {
        self.calls.lock().expect("lock").push("ensure");
        Ok(Id::new())
    }

    async fn start_session(&self, _params: StartSessionParams) -> EngineResult<StartSessionResult> {
        self.calls.lock().expect("lock").push("start");
        Ok(StartSessionResult {
            session_id: Id::new(),
            binding_id: None,
            route_revision: 0,
            append: AppendResult::default(),
        })
    }

    async fn mark_pending_fresh(&self, _session_id: Id, _message_id: &str) -> EngineResult<()> {
        self.calls.lock().expect("lock").push("mark_pending_fresh");
        Ok(())
    }

    async fn append_message(&self, _params: AppendParams) -> EngineResult<AppendResult> {
        self.calls.lock().expect("lock").push("append");
        Ok(AppendResult::default())
    }

    async fn bind_media(&self, _params: BindMediaParams) -> EngineResult<BindMediaResult> {
        self.calls.lock().expect("lock").push("bind_media");
        Ok(BindMediaResult::default())
    }
}

// =====================================================================
// 夹具
// =====================================================================

fn row(app_id: &str, status: &str) -> InstallationRow {
    InstallationRow {
        id: Id::new(),
        workspace_id: Id::new(),
        agent_id: Id::new(),
        installer_user_id: Id::new(),
        status: status.to_string(),
        config: serde_json::json!({
            "app_id": app_id,
            "app_secret_encrypted": "CIPHERTEXT",
        }),
    }
}

fn callback(conversation_type: &str, msgtype: &str) -> BotCallbackData {
    serde_json::from_value(serde_json::json!({
        "senderStaffId": "staff",
        "conversationId": "chat",
        "conversationType": conversation_type,
        "isInAtList": true,
        "msgId": "msg-1",
        "msgtype": msgtype,
        "text": {"content": "hi"},
    }))
    .expect("最小回调")
}

fn message(conversation_type: &str, app_id: &str) -> InboundMessage {
    inbound_from_callback(Some(&callback(conversation_type, "text")), app_id).expect("有发送者")
}

// =====================================================================
// 安装路由
// =====================================================================

#[tokio::test]
async fn installation_routing_uses_the_stamped_app_key() {
    let target = row("app-key", "active");
    let expected_id = target.id;
    let queries = FakeInstallations::with(target);
    let resolver =
        DingTalkInstallationResolver::new(Arc::clone(&queries) as Arc<dyn InstallationQueries>);

    let routed = resolver
        .resolve_installation(&message("2", "app-key"))
        .await
        .expect("找得到");
    assert_eq!(routed.id, expected_id);
    assert_eq!(routed.kind, TYPE_DINGTALK);
    assert!(routed.active);
    // 平台值可以取回来（出站面复用同一行，免得再查一次库）。
    let platform = installation_row(&routed).expect("Platform 是本 adapter 的投影");
    assert_eq!(platform.app_id(), "app-key");
    assert_eq!(
        queries.seen.lock().expect("lock").as_slice(),
        &["app-key".to_string()]
    );
    // 凭据面：安装行含密文 ⇒ `Debug` 不回显它。
    let rendered = format!("{platform:?}");
    assert!(!rendered.contains("CIPHERTEXT"), "{rendered}");
    assert!(rendered.contains("<redacted>"));
}

#[tokio::test]
async fn an_unknown_app_key_is_a_product_level_drop() {
    let resolver = DingTalkInstallationResolver::new(
        Arc::clone(&FakeInstallations::empty()) as Arc<dyn InstallationQueries>
    );
    let error = resolver
        .resolve_installation(&message("2", "app-key"))
        .await
        .expect_err("必须失败");
    assert_eq!(
        error,
        EngineError::Pipeline(PipelineError::InstallationNotFound)
    );
    assert_eq!(error.code_hint(), "installation_not_found");
}

#[tokio::test]
async fn a_revoked_installation_is_resolved_but_inactive() {
    let resolver = DingTalkInstallationResolver::new(Arc::clone(&FakeInstallations::with(row(
        "app-key", "revoked",
    ))) as Arc<dyn InstallationQueries>);
    let routed = resolver
        .resolve_installation(&message("2", "app-key"))
        .await
        .expect("找得到");
    assert!(!routed.active, "撤销由 active=false 表达，不是找不到");
}

#[tokio::test]
async fn a_message_without_our_raw_payload_is_an_infrastructure_failure() {
    let resolver = DingTalkInstallationResolver::new(Arc::clone(&FakeInstallations::with(row(
        "app-key", "active",
    ))) as Arc<dyn InstallationQueries>);
    let mut stripped = message("2", "app-key");
    stripped.raw = serde_json::Value::Null;
    let error = resolver
        .resolve_installation(&stripped)
        .await
        .expect_err("raw 是本 adapter 自己写的");
    assert_eq!(error.code_hint(), "engine_infra_error");
}

// =====================================================================
// 身份
// =====================================================================

#[tokio::test]
async fn identity_re_checks_membership() {
    let installation = ResolvedInstallation::new(
        Id::new(),
        Id::new(),
        Id::new(),
        Id::new(),
        TYPE_DINGTALK,
        true,
    );
    let message = message("2", "app-key");

    // 未绑定 ⇒ 产品性判决（Router 会驱动绑定卡）。
    let unbound = DingTalkIdentityResolver::new(Arc::new(FakeIdentity {
        binding: None,
        is_member: false,
    }));
    let error = unbound
        .resolve_sender(&installation, &message)
        .await
        .expect_err("未绑定");
    assert_eq!(error, EngineError::Pipeline(PipelineError::SenderUnbound));

    // 已绑定但不再是成员 ⇒ 丢弃。
    let not_member = DingTalkIdentityResolver::new(Arc::new(FakeIdentity {
        binding: Some(binding_row(Id::new())),
        is_member: false,
    }));
    let error = not_member
        .resolve_sender(&installation, &message)
        .await
        .expect_err("不是成员");
    assert_eq!(error, EngineError::Pipeline(PipelineError::SenderNotMember));

    // 绑定 + 成员 ⇒ 映射到那个用户。
    let user = Id::new();
    let ok = DingTalkIdentityResolver::new(Arc::new(FakeIdentity {
        binding: Some(binding_row(user)),
        is_member: true,
    }));
    let resolved = ok
        .resolve_sender(&installation, &message)
        .await
        .expect("ok");
    assert_eq!(resolved.user_id, user);
}

// =====================================================================
// 会话路由与出站寻址
// =====================================================================

/// `DingTalk` **没有**线程 ⇒ 隔离键就是会话 id（直聊与群各自一条连续会话）。
#[test]
fn session_routing_is_the_conversation_id() {
    let p2p = message("1", "app-key");
    let (key, config) = dingtalk_session_routing(&p2p);
    assert_eq!(key, "chat");
    assert_eq!(config.conversation_type, "1");
    assert_eq!(config.conversation_id, "chat");
    assert_eq!(config.staff_id, "staff");

    let group = message("2", "app-key");
    let (key, config) = dingtalk_session_routing(&group);
    assert_eq!(key, "chat");
    assert_eq!(config.conversation_type, "2");
    assert!(config.staff_id.is_empty(), "群里按 conversation id 寻址");
    assert_eq!(group.source.chat_type, ChatType::Group);
}

/// 会话绑定行（`channel_chat_session_binding` 的 17 列）。
fn session_binding_row(config: serde_json::Value) -> ChannelChatSessionBindingRow {
    ChannelChatSessionBindingRow {
        id: uuid::Uuid::new_v4(),
        chat_session_id: uuid::Uuid::new_v4(),
        installation_id: uuid::Uuid::new_v4(),
        channel_type: "dingtalk".to_string(),
        channel_chat_id: "fallback-chat".to_string(),
        chat_type: "group".to_string(),
        last_message_id: None,
        last_thread_id: None,
        config,
        created_at: chrono::Utc::now(),
        pending_fresh: false,
        context_revision: 0,
        route_revision: 1,
        retired_at: None,
        history_start_message_id: None,
        history_end_message_id: None,
        history_boundary_pending: false,
    }
}

/// 出站寻址：config 缺席 / 是 null / 解不开时**退回** `channel_chat_id`（上游自己的兜底）。
#[test]
fn outbound_target_falls_back_to_the_channel_chat_id() {
    let mut binding = session_binding_row(serde_json::Value::Null);
    assert_eq!(
        outbound_target(&binding),
        DingTalkBindingConfig {
            conversation_type: "2".to_string(),
            conversation_id: "fallback-chat".to_string(),
            staff_id: String::new(),
        }
    );

    binding.config =
        serde_json::json!({"conversation_type": "1", "conversation_id": "c", "staff_id": "s"});
    assert_eq!(
        outbound_target(&binding),
        DingTalkBindingConfig {
            conversation_type: "1".to_string(),
            conversation_id: "c".to_string(),
            staff_id: "s".to_string(),
        }
    );

    // 半截 config：缺的字段各退回各自的值（config 里的 conversation_id 空 ⇒ 用绑定列）。
    binding.config = serde_json::json!({"staff_id": "s"});
    let target = outbound_target(&binding);
    assert_eq!(target.conversation_id, "fallback-chat");
    assert_eq!(target.conversation_type, "2");
    assert_eq!(target.staff_id, "s");

    // 解不开 ⇒ 整体退回。
    binding.config = serde_json::json!("not-an-object");
    assert_eq!(outbound_target(&binding).conversation_id, "fallback-chat");
}

/// 群回复的引用文案：`current_text` 优先 —— 它冻结在 Router 消费控制指令之前。
#[test]
fn visible_quote_text_prefers_the_frozen_current_text() {
    let message = message("2", "app-key");
    assert_eq!(dingtalk_visible_quote_text(&message), "hi");

    // `current_text` 空 ⇒ 退回 `command_text`。
    let mut stripped = message.clone();
    stripped.raw = serde_json::json!({"app_id": "app-key"});
    stripped.command_text = "/clear go".to_string();
    assert_eq!(dingtalk_visible_quote_text(&stripped), "/clear go");

    // 两者都空、且正文已被引用富化 ⇒ **不**退回 `text`（那会把引用历史当成用户输入）。
    let mut quoted = stripped.clone();
    quoted.command_text = String::new();
    quoted.text = "> parent\n\ncurrent".to_string();
    quoted.reply_to = Some(mc_core::channel::message::ReplyCtx {
        message_id: "parent".to_string(),
        root_id: String::new(),
    });
    assert_eq!(dingtalk_visible_quote_text(&quoted), "");

    // 媒体消息同理不退回。
    let mut media = stripped;
    media.command_text = String::new();
    media.kind = mc_core::channel::MessageKind::Image;
    assert_eq!(dingtalk_visible_quote_text(&media), "");

    // 普通消息（没被富化、不是媒体）才退回 `text`。
    let mut plain = quoted;
    plain.reply_to = None;
    plain.text = "hello".to_string();
    assert_eq!(dingtalk_visible_quote_text(&plain), "hello");
}

// =====================================================================
// 群清单观察（尽力而为）
// =====================================================================

fn installation() -> ResolvedInstallation {
    ResolvedInstallation::new(
        Id::new(),
        Id::new(),
        Id::new(),
        Id::new(),
        TYPE_DINGTALK,
        true,
    )
}

fn ensure_params(installation: &ResolvedInstallation) -> EnsureSessionParams {
    EnsureSessionParams {
        installation: installation.clone(),
        sender: Id::new(),
        message: message("2", "app-key"),
    }
}

#[tokio::test]
async fn the_session_binder_reports_group_presence_without_gating_the_session() {
    let presence = Arc::new(RecordingPresence::default());
    let inner = Arc::new(StubBinder::default());
    let binder = DingTalkSessionBinder::new(
        Arc::clone(&inner) as Arc<dyn SessionBinder>,
        Arc::clone(&presence) as Arc<dyn GroupPresenceObserver>,
    );
    let params = ensure_params(&installation());
    binder
        .ensure_session(params.clone())
        .await
        .expect("会话照常");
    assert_eq!(
        inner.calls.lock().expect("lock").as_slice(),
        &["ensure".to_string()]
    );
    assert_eq!(
        presence.observed.lock().expect("lock").as_slice(),
        &["chat".to_string()]
    );
    assert!(presence.activities.lock().expect("lock").is_empty());

    // `persist_message = false` ⇒ **不**记活动（计数只在 append 提交之后）。
    binder
        .start_session(StartSessionParams {
            installation: params.installation.clone(),
            creator: Id::new(),
            sender: Id::new(),
            message: params.message.clone(),
            claim_token: None,
            media_pending_seconds: 0.0,
            persist_message: false,
        })
        .await
        .expect("轮换照常");
    assert!(presence.activities.lock().expect("lock").is_empty());
    assert_eq!(presence.observed.lock().expect("lock").len(), 2);

    // append 之后记活动。
    binder
        .append_message(AppendParams {
            session_id: Id::new(),
            sender: Id::new(),
            installation_id: params.installation.id,
            message: params.message.clone(),
            claim_token: None,
            media_pending_seconds: 0.0,
        })
        .await
        .expect("追加照常");
    assert_eq!(
        presence.activities.lock().expect("lock").as_slice(),
        &["chat".to_string()]
    );
}

#[tokio::test]
async fn a_presence_failure_never_fails_the_pipeline() {
    let presence = Arc::new(RecordingPresence {
        fail: true,
        ..RecordingPresence::default()
    });
    let binder = DingTalkSessionBinder::new(
        Arc::new(StubBinder::default()) as Arc<dyn SessionBinder>,
        Arc::clone(&presence) as Arc<dyn GroupPresenceObserver>,
    );
    let params = ensure_params(&installation());
    binder
        .ensure_session(params.clone())
        .await
        .expect("群清单写失败不该让一条有效的群消息失败");
    binder
        .append_message(AppendParams {
            session_id: Id::new(),
            sender: Id::new(),
            installation_id: params.installation.id,
            message: params.message,
            claim_token: None,
            media_pending_seconds: 0.0,
        })
        .await
        .expect("同上");
}

/// 默认观察器什么都不写、也不报错（登记缺口的**诚实形态**）。
#[tokio::test]
async fn the_default_presence_observer_is_a_no_op() {
    let observer = NoGroupPresence;
    observer
        .observe(&installation(), &message("2", "app-key"))
        .await
        .expect("no-op");
    observer
        .record_activity(Id::new(), &message("2", "app-key"))
        .await
        .expect("no-op");
}

// =====================================================================
// 集合形状
// =====================================================================

#[test]
fn the_resolver_set_carries_the_dingtalk_origin_and_optional_ports() {
    let set = DingTalkResolverSet::new(
        Arc::new(DingTalkInstallationResolver::new(
            Arc::clone(&FakeInstallations::empty()) as Arc<dyn InstallationQueries>,
        )),
        Arc::new(DingTalkIdentityResolver::new(Arc::new(FakeIdentity {
            binding: None,
            is_member: false,
        }))),
        Arc::new(StubDeduper) as Arc<dyn Deduper>,
        Arc::new(StubBinder::default()) as Arc<dyn SessionBinder>,
        Arc::new(StubAuditor) as Arc<dyn crate::engine::resolvers::Auditor>,
    );
    assert!(set.media.is_none() && set.replier.is_none() && set.typing.is_none());
    let rendered = format!("{set:?}");
    assert!(rendered.contains("media: false"), "{rendered}");

    let engine_set = set.into_engine_set();
    assert_eq!(engine_set.origin_type, "dingtalk_chat");
    assert!(engine_set.media.is_none());
    assert_eq!(origin_type(), "dingtalk_chat");
    assert_eq!(kind(), TYPE_DINGTALK);
    assert_eq!(engine_set.origin_type, origin_type());
}

struct StubDeduper;

#[async_trait]
impl Deduper for StubDeduper {
    async fn claim(&self, _installation_id: Id, _message_id: &str) -> EngineResult<Id> {
        Ok(Id::new())
    }

    async fn mark(
        &self,
        _installation_id: Id,
        _message_id: &str,
        _claim_token: Id,
    ) -> EngineResult<()> {
        Ok(())
    }

    async fn release(
        &self,
        _installation_id: Id,
        _message_id: &str,
        _claim_token: Id,
    ) -> EngineResult<()> {
        Ok(())
    }
}

struct StubAuditor;

#[async_trait]
impl crate::engine::resolvers::Auditor for StubAuditor {
    async fn record_drop(
        &self,
        _installation_id: Option<Id>,
        _message: &InboundMessage,
        _reason: crate::engine::DropReason,
    ) -> EngineResult<()> {
        Ok(())
    }
}

/// 用例自己也要能解 `raw`（与安装路由读的是同一份载荷）。
#[test]
fn the_raw_event_is_readable_from_outside() {
    let raw = decode_dingtalk_raw(&message("2", "app-key")).expect("raw is ours");
    assert_eq!(raw.app_id, "app-key");
    let seen: HashSet<&str> = raw.conversation_title.as_str().split_whitespace().collect();
    assert!(seen.is_empty());
}
