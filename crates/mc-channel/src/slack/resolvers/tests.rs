use super::*;
use crate::engine::resolvers::{
    CommandClassifier, DropReason, IssueCreator, NoCommands, Outcome, ResolverSet, RouteResult,
    RunTriggerer, SessionReader,
};
use crate::engine::router::{Router, RouterConfig};
use crate::slack::inbound::{parse_events_api, EventBody};
use crate::slack::socket::{dispatch_frame, parse_socket_frame, FrameAction};
use mc_core::channel::message::{MessageKind, Source};
use std::collections::HashMap;
use std::sync::Mutex;

fn inbound(
    chat_type: ChatType,
    chat_id: &str,
    thread_id: &str,
    message_id: &str,
) -> InboundMessage {
    InboundMessage {
        event_id: message_id.to_string(),
        message_id: message_id.to_string(),
        source: Source {
            channel_type: TYPE_SLACK,
            chat_id: chat_id.to_string(),
            chat_type,
            sender_id: "UALICE".to_string(),
            sender_stable_id: String::new(),
            thread_id: thread_id.to_string(),
        },
        kind: MessageKind::Text,
        text: "hello".to_string(),
        command_text: "hello".to_string(),
        has_selected_context: false,
        media_refs: Vec::new(),
        reply_to: None,
        addressed_to_bot: true,
        force_fresh: false,
        skip_agent_run: false,
        raw: serde_json::json!({ "team_id": "T1", "api_app_id": "A1", "event_type": "message" }),
    }
}

/// 上游 `TestSlackSessionRouting` 的逐条移植（含 `TestSlackThreadIsolation`）。
#[test]
fn session_routing_table() {
    let cases = [
        (
            "DM 顶层：一个频道一个会话",
            inbound(ChatType::P2p, "D1", "", "111.0"),
            "D1",
            "",
        ),
        (
            "DM 线程内：仍是一个会话，回复进线程",
            inbound(ChatType::P2p, "D1", "100.0", "111.0"),
            "D1",
            "100.0",
        ),
        (
            "频道顶层 @：新线程根 = 消息 ts",
            inbound(ChatType::Group, "C1", "", "111.0"),
            "C1:111.0",
            "111.0",
        ),
        (
            "频道线程回复：按线程根隔离",
            inbound(ChatType::Group, "C1", "100.0", "222.0"),
            "C1:100.0",
            "100.0",
        ),
    ];
    for (name, message, key, reply_thread) in cases {
        let routing = session_routing(&message);
        assert_eq!(routing.binding_key, key, "{name}");
        assert_eq!(routing.reply_thread, reply_thread, "{name}");
    }
    // 同一频道里两个顶层 `@bot` = 两个不同的隔离键（不塌成一个会话）。
    let first = session_routing(&inbound(ChatType::Group, "C1", "", "1.1"));
    let second = session_routing(&inbound(ChatType::Group, "C1", "", "2.2"));
    assert_ne!(first.binding_key, second.binding_key);
}

/// 归一化把"隔离键用的线程根"写进消息（差异 1 / 2 的实现点）。
#[test]
fn normalize_sets_the_thread_root_per_chat_type() {
    let mut group = inbound(ChatType::Group, "C1", "", "111.0");
    SlackSessionBinder::normalize(&mut group);
    assert_eq!(group.source.thread_id, "111.0", "顶层的根 = 自己的 ts");
    assert_eq!(
        BindingKeyPolicy::ChatIdPlusThreadRoot.compose(&group),
        "C1#111.0"
    );

    let mut thread = inbound(ChatType::Group, "C1", "100.0", "222.0");
    SlackSessionBinder::normalize(&mut thread);
    assert_eq!(
        BindingKeyPolicy::ChatIdPlusThreadRoot.compose(&thread),
        "C1#100.0"
    );

    let mut dm = inbound(ChatType::P2p, "D1", "100.0", "111.0");
    SlackSessionBinder::normalize(&mut dm);
    assert_eq!(dm.source.thread_id, "", "DM 的键必须只是 chat id");
    assert_eq!(BindingKeyPolicy::ChatIdPlusThreadRoot.compose(&dm), "D1");
}

// ---- 端口替身 ----

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

#[derive(Default)]
struct FakeIdentities {
    bindings: HashMap<(Id, String), Id>,
    members: bool,
}

#[async_trait]
impl IdentityQueries for FakeIdentities {
    async fn find_user_binding(
        &self,
        installation_id: Id,
        channel_user_id: &str,
    ) -> Result<Option<ChannelUserBindingRow>, RepoError> {
        Ok(self
            .bindings
            .get(&(installation_id, channel_user_id.to_string()))
            .map(|user_id| ChannelUserBindingRow {
                id: uuid::Uuid::nil(),
                workspace_id: uuid::Uuid::nil(),
                multica_user_id: user_id.0,
                installation_id: installation_id.0,
                channel_type: "slack".to_string(),
                channel_user_id: channel_user_id.to_string(),
                config: serde_json::json!({}),
                bound_at: chrono::Utc::now(),
            }))
    }

    async fn is_workspace_member(&self, _w: Id, _u: Id) -> Result<bool, RepoError> {
        Ok(self.members)
    }

    async fn upsert_user_binding(&self, _w: Id, _u: Id, _i: Id, _c: &str) -> Result<(), RepoError> {
        Ok(())
    }
}

fn row(app_id: &str, team_id: &str) -> InstallationRow {
    InstallationRow {
        id: Id::new(),
        workspace_id: Id::new(),
        agent_id: Id::new(),
        installer_user_id: Id::new(),
        status: "active".to_string(),
        config: serde_json::json!({ "app_id": app_id, "team_id": team_id }),
    }
}

/// 安装路由：命中 ⇒ `ResolvedInstallation`（含平台行）；未命中 / 工作区不符 ⇒ 产品性判决。
#[tokio::test]
async fn installation_resolution() {
    let mut installations = FakeInstallations::default();
    let installation = row("A1", "T1");
    installations
        .rows
        .insert("A1".to_string(), installation.clone());
    let installer = SlackInstallationResolver::new(Arc::new(installations));

    let out = installer
        .resolve_installation(&inbound(ChatType::P2p, "D1", "", "1.1"))
        .await
        .expect("命中");
    assert_eq!(out.id, installation.id);
    assert_eq!(out.workspace_id, installation.workspace_id);
    assert_eq!(out.kind, TYPE_SLACK);
    assert!(out.active);
    // 平台行可被出站 / 媒体面取回（bot token 就在它的 config 里）。
    assert!(out
        .platform
        .as_ref()
        .and_then(|platform| platform.downcast_ref::<InstallationRow>())
        .is_some());

    // 认不出的 app id ⇒ `invalid_event`（不是错误）。
    let mut unknown = inbound(ChatType::P2p, "D1", "", "1.1");
    unknown.raw = serde_json::json!({ "api_app_id": "A9", "team_id": "T1" });
    let error = installer.resolve_installation(&unknown).await.unwrap_err();
    assert_eq!(error, PipelineError::InstallationNotFound.into());

    // 同一个 app 被装进别的工作区（事件 team 不符）⇒ 也不路由。
    let mut other_team = inbound(ChatType::P2p, "D1", "", "1.1");
    other_team.raw = serde_json::json!({ "api_app_id": "A1", "team_id": "T2" });
    assert_eq!(
        installer
            .resolve_installation(&other_team)
            .await
            .unwrap_err(),
        PipelineError::InstallationNotFound.into()
    );

    // `raw` 空 ⇒ 基础设施失败（adapter 自己写的字段解不开）。
    let mut empty = inbound(ChatType::P2p, "D1", "", "1.1");
    empty.raw = serde_json::Value::Null;
    assert!(matches!(
        installer.resolve_installation(&empty).await.unwrap_err(),
        crate::engine::resolvers::EngineError::Infra { .. }
    ));
}

/// 身份解析的三条判决：未绑定 / 非成员 / 已绑定。
#[tokio::test]
async fn identity_resolution() {
    let installation = row("A1", "T1");
    let resolved = ResolvedInstallation::new(
        installation.id,
        installation.workspace_id,
        installation.agent_id,
        installation.installer_user_id,
        TYPE_SLACK,
        true,
    );
    let message = inbound(ChatType::P2p, "D1", "", "1.1");
    let user_id = Id::new();

    // 未绑定 ⇒ `needs_binding`（产品性判决，**不是**错误）。
    let identity = SlackIdentityResolver::new(Arc::new(FakeIdentities::default()));
    assert_eq!(
        identity
            .resolve_sender(&resolved, &message)
            .await
            .unwrap_err(),
        PipelineError::SenderUnbound.into()
    );

    // 已绑定但不是成员 ⇒ `non_workspace_member`。
    let mut identities = FakeIdentities::default();
    identities
        .bindings
        .insert((installation.id, "UALICE".to_string()), user_id);
    let identity = SlackIdentityResolver::new(Arc::new(identities));
    assert_eq!(
        identity
            .resolve_sender(&resolved, &message)
            .await
            .unwrap_err(),
        PipelineError::SenderNotMember.into()
    );

    // 已绑定且是成员 ⇒ 落到那个用户。
    let mut identities = FakeIdentities {
        members: true,
        ..FakeIdentities::default()
    };
    identities
        .bindings
        .insert((installation.id, "UALICE".to_string()), user_id);
    let identity = SlackIdentityResolver::new(Arc::new(identities));
    assert_eq!(
        identity
            .resolve_sender(&resolved, &message)
            .await
            .expect("已绑定")
            .user_id,
        user_id
    );
}

// ---- 端到端（本 adapter 的解析器集合 + engine 的 Router） ----

struct StubTrigger;

#[async_trait]
impl RunTriggerer for StubTrigger {
    async fn schedule_chat_run(
        &self,
        _params: crate::engine::resolvers::ChatRunParams,
    ) -> EngineResult<()> {
        Ok(())
    }
    async fn drain(&self) -> EngineResult<()> {
        Ok(())
    }
}

struct StubReader;

#[async_trait]
impl SessionReader for StubReader {
    async fn workspace_identity(
        &self,
        _workspace_id: Id,
    ) -> EngineResult<crate::engine::resolvers::WorkspaceIdentity> {
        Ok(crate::engine::resolvers::WorkspaceIdentity::default())
    }
}

struct StubIssues;

#[async_trait]
impl IssueCreator for StubIssues {
    async fn create_issue(
        &self,
        _params: crate::engine::resolvers::ChannelIssueParams,
    ) -> EngineResult<crate::engine::resolvers::ChannelIssueOutcome> {
        Err(crate::engine::resolvers::EngineError::infra("unused"))
    }
}

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

/// 一条**真**的 `events_api` 帧 → 归一化消息（走 adapter 自己的翻译）。
fn translated(text: &str) -> InboundMessage {
    let frames = [
        r#"{"type":"events_api","envelope_id":"e1","payload":{"team_id":"T1","api_app_id":"A1",
            "event":{"type":"message","user":"UALICE","text":"TEXT","channel":"D1",
                     "channel_type":"im","ts":"1.1"}}}"#,
    ]
    .map(|frame| frame.replace("TEXT", text));
    let frame = parse_socket_frame(&frames[0]).expect("frame");
    match dispatch_frame(&frame, "UBOT") {
        FrameAction::Dispatch(message) => *message,
        other => panic!("expected dispatch, got {other:?}"),
    }
}

fn slack_router(
    installation: InstallationRow,
    identities: FakeIdentities,
    replier: Arc<StubReplier>,
    audit: Arc<StubAudit>,
) -> Router {
    let mut installations = FakeInstallations::default();
    // 按行自己的 `config->>'app_id'` 建索引（替身不许比真仓储宽松）。
    let app_id = installation
        .config
        .get("app_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    installations.rows.insert(app_id, installation);
    let set = SlackResolverSet::new(
        Arc::new(SlackInstallationResolver::new(Arc::new(installations))),
        Arc::new(SlackIdentityResolver::new(Arc::new(identities))),
        Arc::new(StubDedup),
        Arc::new(StubSession),
        audit,
    )
    .with_replier(replier);
    let router = Router::new(
        Arc::new(NoCommands) as Arc<dyn CommandClassifier>,
        Arc::new(StubTrigger),
        Arc::new(StubReader),
        Arc::new(StubIssues),
        RouterConfig::default(),
    );
    router.register(TYPE_SLACK, set.into_engine_set());
    router
}

/// 会话端口替身（本用例只走 `needs_binding` 分支，永远不会被调到）。
struct StubSession;

#[async_trait]
impl SessionBinder for StubSession {
    async fn ensure_session(&self, _params: EnsureSessionParams) -> EngineResult<Id> {
        Err(crate::engine::resolvers::EngineError::infra("unused"))
    }
    async fn start_session(&self, _params: StartSessionParams) -> EngineResult<StartSessionResult> {
        Err(crate::engine::resolvers::EngineError::infra("unused"))
    }
    async fn mark_pending_fresh(&self, _s: Id, _m: &str) -> EngineResult<()> {
        Ok(())
    }
    async fn append_message(&self, _params: AppendParams) -> EngineResult<AppendResult> {
        Err(crate::engine::resolvers::EngineError::infra("unused"))
    }
    async fn bind_media(&self, _params: BindMediaParams) -> EngineResult<BindMediaResult> {
        Err(crate::engine::resolvers::EngineError::infra("unused"))
    }
}

/// **M7-3 的专属验收**：Socket Mode 信封帧 → 归一化 → 安装路由 → 身份（未绑定）
/// ⇒ 回绑定卡，且 `route` 返回 `Ok(())`（**nil error，不是失败**）+ 一条 `unbound_user` 审计。
#[tokio::test]
async fn unbound_sender_gets_a_binding_card_and_no_error() {
    let replier = Arc::new(StubReplier::default());
    let audit = Arc::new(StubAudit::default());
    let router = slack_router(
        row("A1", "T1"),
        FakeIdentities::default(),
        Arc::clone(&replier),
        Arc::clone(&audit),
    );
    router
        .route(translated("hello bot"))
        .await
        .expect("未绑定必须是判决，不是 Err");
    // 出站回复是 `tokio::spawn` 的：给它一点时间跑完再断言。
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

/// 同一个信封但安装认不出 ⇒ 产品性丢弃（`invalid_event`），同样不是错误。
#[tokio::test]
async fn unknown_installation_is_dropped_without_an_error() {
    let replier = Arc::new(StubReplier::default());
    let audit = Arc::new(StubAudit::default());
    let router = slack_router(
        row("A2", "T1"),
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

/// `ResolverSet` 的装配：`origin_type` 逐字 = `slack_chat`，可选端口按注入与否出现。
#[test]
fn resolver_set_assembly() {
    let set = SlackResolverSet::new(
        Arc::new(SlackInstallationResolver::new(Arc::new(
            FakeInstallations::default(),
        ))),
        Arc::new(SlackIdentityResolver::new(Arc::new(
            FakeIdentities::default(),
        ))),
        Arc::new(StubDedup),
        Arc::new(StubSession),
        Arc::new(StubAudit::default()),
    );
    let engine_set: ResolverSet = set.into_engine_set();
    assert_eq!(engine_set.origin_type, "slack_chat");
    assert!(engine_set.media.is_none());
    assert!(engine_set.replier.is_none());
    assert!(engine_set.typing.is_none());
    assert_eq!(origin_type(), "slack_chat");
    assert_eq!(kind(), TYPE_SLACK);
    let rendered = format!("{engine_set:?}");
    assert!(rendered.contains("slack_chat"));
}

/// `events_api` 帧里的 `event` 走的是 adapter 自己的类型（防止 `EventBody` 漂移）。
#[test]
fn events_api_payload_decodes_into_the_adapter_types() {
    let payload = serde_json::json!({
        "team_id": "T1",
        "api_app_id": "A1",
        "event": { "type": "app_mention", "user": "UALICE", "text": "<@UBOT> hi",
                   "channel": "C1", "ts": "9.9" }
    });
    let event = parse_events_api(&payload).expect("可解");
    assert_eq!(event.event.kind, "app_mention");
    assert_eq!(event.event.channel, "C1");
    assert_eq!(event.api_app_id, "A1");
    assert_eq!(EventBody::default().kind, "");
}
