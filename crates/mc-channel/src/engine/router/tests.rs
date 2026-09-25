//! `Router` 入站流水线的用例（M7-1 的专属验收：`Channel` 五方法 / `Registry` / `Capability`
//! 之外，engine 那半边的"路由/监管/解析"）。
//!
//! 拆到本文件是门 ⑩ 的要求（`router.rs` 已到 800 行上限）；模块路径仍是
//! `engine/router.rs` 的 `mod tests`。
//!
//! 全部端口用**进程内替身**：流水线的顺序、判决、以及"谁在什么时候被调"都是断言对象
//! （DB 实现归后续片，语义由本文件钉住）。

use super::*;

use crate::engine::resolvers::*;
use async_trait::async_trait;
use mc_core::channel::message::{ChatType, MessageKind, Source};
use std::sync::Mutex as StdMutex;

/// 事件日志：断言"先 dedup 再身份再会话"这类顺序。
type Log = Arc<StdMutex<Vec<String>>>;

/// 五个独立语义开关（不是可以打包的状态位）：与 `InboundMessage` 的豁免同理由。
#[allow(clippy::struct_excessive_bools)]
#[derive(Default)]
struct State {
    active: bool,
    found: bool,
    bound: bool,
    member: bool,
    duplicate: bool,
    route_changes: u32,
    issue_duplicate: bool,
    has_media: bool,
    media_resolved: usize,
    media_bound: usize,
    marks: usize,
    releases: usize,
    runs: Vec<i64>,
    replies: Vec<String>,
}

struct Ports {
    state: StdMutex<State>,
    log: Log,
}

impl Ports {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: StdMutex::new(State {
                active: true,
                found: true,
                bound: true,
                member: true,
                ..State::default()
            }),
            log: Arc::new(StdMutex::new(Vec::new())),
        })
    }

    fn note(&self, what: &str) {
        self.log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(what.to_string());
    }

    fn events(&self) -> Vec<String> {
        self.log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn set<F: FnOnce(&mut State)>(&self, edit: F) {
        edit(
            &mut self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
    }

    fn get<F: FnOnce(&State) -> R, R>(&self, read: F) -> R {
        read(
            &self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
}

fn inbound(chat_type: ChatType) -> InboundMessage {
    InboundMessage {
        event_id: "Ev1".into(),
        message_id: "Msg1".into(),
        source: Source {
            channel_type: ChannelKind::Lark,
            chat_id: "C1".into(),
            chat_type,
            sender_id: "U1".into(),
            sender_stable_id: String::new(),
            thread_id: String::new(),
        },
        kind: MessageKind::Text,
        text: "hello".into(),
        command_text: String::new(),
        has_selected_context: false,
        media_refs: Vec::new(),
        reply_to: None,
        addressed_to_bot: true,
        force_fresh: false,
        skip_agent_run: false,
        raw: serde_json::json!({}),
    }
}

fn installation(ports: &Ports) -> ResolvedInstallation {
    let mut site = ResolvedInstallation::new(
        Id::new(),
        Id::new(),
        Id::new(),
        Id::new(),
        ChannelKind::Lark,
        ports.get(|state| state.active),
    );
    site.platform = Some(Arc::new("platform-installation"));
    site
}

// ---- 端口替身：每条都要一个 struct，但都只干一件事 ----

struct Install(Arc<Ports>);

#[async_trait]
impl InstallationResolver for Install {
    async fn resolve_installation(
        &self,
        _message: &InboundMessage,
    ) -> EngineResult<ResolvedInstallation> {
        self.0.note("installation");
        if !self.0.get(|state| state.found) {
            return Err(PipelineError::InstallationNotFound.into());
        }
        Ok(installation(&self.0))
    }
}

struct Identity(Arc<Ports>);

#[async_trait]
impl IdentityResolver for Identity {
    async fn resolve_sender(
        &self,
        _installation: &ResolvedInstallation,
        _message: &InboundMessage,
    ) -> EngineResult<ResolvedIdentity> {
        self.0.note("identity");
        if !self.0.get(|state| state.bound) {
            return Err(PipelineError::SenderUnbound.into());
        }
        if !self.0.get(|state| state.member) {
            return Err(PipelineError::SenderNotMember.into());
        }
        Ok(ResolvedIdentity { user_id: Id::new() })
    }
}

struct Dedup(Arc<Ports>);

#[async_trait]
impl Deduper for Dedup {
    async fn claim(&self, _installation_id: Id, _message_id: &str) -> EngineResult<Id> {
        self.0.note("claim");
        if self.0.get(|state| state.duplicate) {
            return Err(PipelineError::Duplicate.into());
        }
        Ok(Id::new())
    }
    async fn mark(&self, _i: Id, _m: &str, _t: Id) -> EngineResult<()> {
        self.0.note("mark");
        self.0.set(|state| state.marks += 1);
        Ok(())
    }
    async fn release(&self, _i: Id, _m: &str, _t: Id) -> EngineResult<()> {
        self.0.note("release");
        self.0.set(|state| state.releases += 1);
        Ok(())
    }
}

struct Session(Arc<Ports>);

#[async_trait]
impl SessionBinder for Session {
    async fn ensure_session(&self, _params: EnsureSessionParams) -> EngineResult<Id> {
        self.0.note("ensure_session");
        Ok(Id::new())
    }
    async fn start_session(&self, _params: StartSessionParams) -> EngineResult<StartSessionResult> {
        self.0.note("start_session");
        Ok(StartSessionResult {
            session_id: Id::new(),
            binding_id: Some(Id::new()),
            route_revision: 3,
            append: AppendResult {
                message_id: Some(Id::new()),
                context_revision: 1,
                binding_id: Some(Id::new()),
                route_revision: 3,
                ..AppendResult::default()
            },
        })
    }
    async fn mark_pending_fresh(&self, _session_id: Id, _message_id: &str) -> EngineResult<()> {
        self.0.note("mark_pending_fresh");
        Ok(())
    }
    async fn append_message(&self, _params: AppendParams) -> EngineResult<AppendResult> {
        self.0.note("append_message");
        let changes = self.0.get(|state| state.route_changes);
        if changes > 0 {
            self.0.set(|state| state.route_changes -= 1);
            return Err(PipelineError::RouteChanged.into());
        }
        Ok(AppendResult {
            message_id: Some(Id::new()),
            issue_command: None,
            dedup_marked: false,
            context_revision: 1,
            pending_contexts: Vec::new(),
            initial_title: "hello".into(),
            became_visible: true,
            binding_id: Some(Id::new()),
            route_revision: 3,
        })
    }
    async fn bind_media(&self, params: BindMediaParams) -> EngineResult<BindMediaResult> {
        self.0.note("bind_media");
        self.0
            .set(|state| state.media_bound += params.media_refs.len());
        Ok(BindMediaResult::default())
    }
}

struct Audit(Arc<Ports>);

#[async_trait]
impl Auditor for Audit {
    async fn record_drop(
        &self,
        _installation_id: Option<Id>,
        _message: &InboundMessage,
        reason: DropReason,
    ) -> EngineResult<()> {
        self.0.note(&format!("audit:{}", reason.as_str()));
        Ok(())
    }
}

struct Media(Arc<Ports>);

impl MediaResolver for Media {
    fn has_media(&self, _message: &InboundMessage) -> bool {
        self.0.get(|state| state.has_media)
    }
    fn resolve_media(
        &self,
        _installation: &ResolvedInstallation,
        _sender: &ResolvedIdentity,
        _session_id: Id,
        _chat_message_id: Option<Id>,
        message: &InboundMessage,
    ) -> InboundMessage {
        self.0.note("resolve_media");
        self.0.set(|state| state.media_resolved += 1);
        let mut resolved = message.clone();
        resolved
            .media_refs
            .push(mc_core::channel::message::MediaRef {
                message_kind: MessageKind::Image,
                storage_key: "k".into(),
                storage_url: "u".into(),
                filename: "a.png".into(),
                mime_type: "image/png".into(),
                size_bytes: 1,
                inline_placeholder: String::new(),
                inline_index: 0,
            });
        resolved
    }
}

struct Replier(Arc<Ports>);

impl OutboundReplier for Replier {
    fn reply(
        &self,
        _installation: &ResolvedInstallation,
        _message: &InboundMessage,
        result: &RouteResult,
    ) {
        self.0
            .set(|state| state.replies.push(result.outcome.as_str().into()));
    }
}

struct Typing(Arc<Ports>);

impl TypingNotifier for Typing {
    fn on_ingested(&self, _i: &ResolvedInstallation, _m: &InboundMessage, _s: Id) {
        self.0.note("typing");
    }
    fn on_settled(&self, _s: Id) {
        self.0.note("typing_settled");
    }
}

struct Trigger(Arc<Ports>);

#[async_trait]
impl RunTriggerer for Trigger {
    async fn schedule_chat_run(&self, params: ChatRunParams) -> EngineResult<()> {
        self.0.note("run");
        self.0.set(|state| state.runs.push(params.context_revision));
        Ok(())
    }
    async fn drain(&self) -> EngineResult<()> {
        Ok(())
    }
}

struct Reader;

#[async_trait]
impl SessionReader for Reader {
    async fn workspace_identity(&self, _workspace_id: Id) -> EngineResult<WorkspaceIdentity> {
        Ok(WorkspaceIdentity {
            issue_prefix: "ABC".into(),
            slug: "acme".into(),
        })
    }
}

struct Issues(Arc<Ports>);

#[async_trait]
impl IssueCreator for Issues {
    async fn create_issue(&self, params: ChannelIssueParams) -> EngineResult<ChannelIssueOutcome> {
        self.0.note("create_issue");
        assert_eq!(
            params.origin_type, "lark_chat",
            "渠道标签由 ResolverSet 给出"
        );
        Ok(ChannelIssueOutcome {
            issue: ChannelIssue {
                id: Id::new(),
                number: 42,
                title: params.title,
            },
            duplicate: self.0.get(|state| state.issue_duplicate),
            assigned_task_id: None,
        })
    }
}

/// 只认三个命令的最小分类器（`/issue` / `/new` / `/clear`）。
struct Classifier;

impl CommandClassifier for Classifier {
    fn classify(&self, body: &str) -> CommandIntent {
        let body = body.trim();
        if let Some(rest) = body.strip_prefix("/issue") {
            let rest = rest.trim();
            return match rest.split_once('\n') {
                Some((title, description)) => CommandIntent::Issue {
                    title: title.trim().to_string(),
                    description: description.trim().to_string(),
                },
                None => CommandIntent::Issue {
                    title: rest.to_string(),
                    description: String::new(),
                },
            };
        }
        if let Some(rest) = body.strip_prefix("/new") {
            return CommandIntent::NewChat {
                body: rest.trim().to_string(),
            };
        }
        if let Some(rest) = body.strip_prefix("/clear") {
            return CommandIntent::FreshSession {
                body: rest.trim().to_string(),
            };
        }
        CommandIntent::None
    }
}

fn build_router(ports: &Arc<Ports>) -> Router {
    Router::new(
        Arc::new(Classifier),
        Arc::new(Trigger(Arc::clone(ports))),
        Arc::new(Reader),
        Arc::new(Issues(Arc::clone(ports))),
        RouterConfig {
            media_timeout: Duration::from_millis(200),
            reply_timeout: Duration::from_millis(200),
            ..RouterConfig::default()
        },
    )
}

fn set(ports: &Arc<Ports>) -> ResolverSet {
    ResolverSet::new(
        Arc::new(Install(Arc::clone(ports))),
        Arc::new(Identity(Arc::clone(ports))),
        Arc::new(Dedup(Arc::clone(ports))),
        Arc::new(Session(Arc::clone(ports))),
        Arc::new(Audit(Arc::clone(ports))),
        "lark_chat",
    )
    .with_media(Arc::new(Media(Arc::clone(ports))))
    .with_replier(Arc::new(Replier(Arc::clone(ports))))
    .with_typing(Arc::new(Typing(Arc::clone(ports))))
}

async fn settle() {
    // 脱离式回复 / 打字 / 媒体是 `tokio::spawn` 的：给它们一点时间跑完再断言。
    tokio::time::sleep(Duration::from_millis(60)).await;
}

/// 平凡路径：dedup → 身份 → 会话 → 追加 → 触发 run → 出站面。
#[tokio::test]
async fn p2p_happy_path_ingests_and_schedules_a_run() {
    let ports = Ports::new();
    let router = build_router(&ports);
    router.register(ChannelKind::Lark, set(&ports));

    router.route(inbound(ChatType::P2p)).await.expect("route");
    settle().await;

    // 顺序有语义（docs/60 §4.1 的六步）：去掉脱离式任务留下的痕迹再比。
    let events: Vec<String> = ports
        .events()
        .into_iter()
        .filter(|event| event != "bind_media" && event != "typing")
        .collect();
    assert_eq!(
        events,
        vec![
            "installation",
            "claim",
            "identity",
            "ensure_session",
            "append_message",
            "run",
            "mark"
        ],
        "流水线顺序（写在 docs/60 §4.1 的六步）"
    );
    assert_eq!(ports.get(|state| state.runs.clone()), vec![1]);
    assert_eq!(ports.get(|state| state.marks), 1);
    assert_eq!(ports.get(|state| state.replies.clone()), vec!["ingested"]);
}

/// 群聊未 @bot ⇒ 丢弃，且**在身份之前**（不能给未绑定用户的群闲聊刷绑定卡）。
#[tokio::test]
async fn group_message_not_addressed_to_bot_is_dropped_before_identity() {
    let ports = Ports::new();
    let router = build_router(&ports);
    router.register(ChannelKind::Lark, set(&ports));

    let mut message = inbound(ChatType::Group);
    message.addressed_to_bot = false;
    router.route(message).await.expect("route");
    settle().await;

    let events = ports.events();
    assert!(events.contains(&"audit:not_addressed_in_group".to_string()));
    assert!(
        !events.contains(&"identity".to_string()),
        "群过滤在身份之前"
    );
    assert!(!events.contains(&"run".to_string()));
    assert_eq!(ports.get(|state| state.marks), 1, "丢弃也要落 claim 终态");
}

/// 群聊 @bot ⇒ 正常入库。
#[tokio::test]
async fn group_message_addressed_to_bot_is_ingested() {
    let ports = Ports::new();
    let router = build_router(&ports);
    router.register(ChannelKind::Lark, set(&ports));
    router.route(inbound(ChatType::Group)).await.expect("route");
    settle().await;
    assert!(ports.events().contains(&"run".to_string()));
}

/// 发件人未绑定 ⇒ `needs_binding`（**不是**错误），并落一条 `unbound_user` 审计。
#[tokio::test]
async fn unbound_sender_needs_binding_without_an_error() {
    let ports = Ports::new();
    ports.set(|state| state.bound = false);
    let router = build_router(&ports);
    router.register(ChannelKind::Lark, set(&ports));

    router
        .route(inbound(ChatType::P2p))
        .await
        .expect("不是 Err");
    settle().await;

    let events = ports.events();
    assert!(events.contains(&"audit:unbound_user".to_string()));
    assert!(!events.contains(&"ensure_session".to_string()));
    assert_eq!(
        ports.get(|state| state.replies.clone()),
        vec!["needs_binding"]
    );
}

/// 已绑定但不是 workspace 成员 ⇒ `non_workspace_member` 丢弃。
#[tokio::test]
async fn non_member_is_dropped() {
    let ports = Ports::new();
    ports.set(|state| state.member = false);
    let router = build_router(&ports);
    router.register(ChannelKind::Lark, set(&ports));
    router.route(inbound(ChatType::P2p)).await.expect("route");
    settle().await;
    assert!(ports
        .events()
        .contains(&"audit:non_workspace_member".to_string()));
}

/// 安装已撤销 ⇒ 在 dedup claim **之前**丢弃。
#[tokio::test]
async fn revoked_installation_is_dropped_before_the_claim() {
    let ports = Ports::new();
    ports.set(|state| state.active = false);
    let router = build_router(&ports);
    router.register(ChannelKind::Lark, set(&ports));
    router.route(inbound(ChatType::P2p)).await.expect("route");
    settle().await;

    let events = ports.events();
    assert!(events.contains(&"audit:revoked_installation".to_string()));
    assert!(!events.contains(&"claim".to_string()), "撤销在去重之前");
}

/// 没有安装匹配 ⇒ `invalid_event` 丢弃（连 claim 都没有可挂的安装）。
#[tokio::test]
async fn missing_installation_is_an_invalid_event_drop() {
    let ports = Ports::new();
    ports.set(|state| state.found = false);
    let router = build_router(&ports);
    router.register(ChannelKind::Lark, set(&ports));
    router.route(inbound(ChatType::P2p)).await.expect("route");
    settle().await;
    assert!(ports.events().contains(&"audit:invalid_event".to_string()));
}

/// 去重命中 ⇒ `duplicate` 丢弃，且**不**摸会话。
#[tokio::test]
async fn duplicate_is_dropped_before_the_session() {
    let ports = Ports::new();
    ports.set(|state| state.duplicate = true);
    let router = build_router(&ports);
    router.register(ChannelKind::Lark, set(&ports));
    router.route(inbound(ChatType::P2p)).await.expect("route");
    settle().await;

    let events = ports.events();
    assert!(events.contains(&"audit:duplicate".to_string()));
    assert!(!events.contains(&"append_message".to_string()));
    assert!(!events.contains(&"run".to_string()));
}

/// 基础设施失败（这里用"路由一直变"打到重试上限）⇒ `Err`。
#[tokio::test]
async fn unstable_route_surfaces_an_infrastructure_error() {
    let ports = Ports::new();
    ports.set(|state| state.route_changes = 99);
    let router = build_router(&ports);
    router.register(ChannelKind::Lark, set(&ports));
    let error = router
        .route(inbound(ChatType::P2p))
        .await
        .expect_err("必须 Err");
    assert_eq!(error.code(), "channel_storage_error");
    assert!(!ports.events().contains(&"run".to_string()));
}

/// 路由换一次之后稳定 ⇒ 重试成功（产品性丢弃与错误都不产生）。
#[tokio::test]
async fn route_change_is_retried_and_then_succeeds() {
    let ports = Ports::new();
    ports.set(|state| state.route_changes = 1);
    let router = build_router(&ports);
    router.register(ChannelKind::Lark, set(&ports));
    router.route(inbound(ChatType::P2p)).await.expect("route");
    settle().await;
    let appends = ports
        .events()
        .iter()
        .filter(|event| *event == "append_message")
        .count();
    assert_eq!(appends, 2, "第一次 RouteChanged，第二次成功");
    assert!(ports.events().contains(&"run".to_string()));
}

/// `/issue <title>` ⇒ 建 issue（终态：**不**排普通 chat run），标识符带 workspace 前缀。
#[tokio::test]
async fn issue_command_creates_an_issue_and_never_schedules_a_run() {
    let ports = Ports::new();
    let router = build_router(&ports);
    router.register(ChannelKind::Lark, set(&ports));

    let mut message = inbound(ChatType::P2p);
    message.text = "/issue fix the thing\ndetails".into();
    message.command_text = message.text.clone();
    router.route(message).await.expect("route");
    settle().await;

    let events = ports.events();
    assert!(events.contains(&"create_issue".to_string()));
    assert!(!events.contains(&"run".to_string()), "issue 命令是终态");
    assert_eq!(ports.get(|state| state.replies.clone()), vec!["ingested"]);
}

/// `/issue` 缺标题 ⇒ `issue_usage`，不建 issue。
#[tokio::test]
async fn bare_issue_command_reports_usage() {
    let ports = Ports::new();
    let router = build_router(&ports);
    router.register(ChannelKind::Lark, set(&ports));

    let mut message = inbound(ChatType::P2p);
    message.text = "/issue".into();
    message.command_text = message.text.clone();
    router.route(message).await.expect("route");
    settle().await;

    assert!(!ports.events().contains(&"create_issue".to_string()));
    assert_eq!(
        ports.get(|state| state.replies.clone()),
        vec!["issue_usage"]
    );
}

/// 裸 `/clear` ⇒ `fresh_pending`（只记待开新会话）；`/new` 带正文 ⇒ `chat_started`。
#[tokio::test]
async fn control_commands_take_their_own_paths() {
    let ports = Ports::new();
    let router = build_router(&ports);
    router.register(ChannelKind::Lark, set(&ports));

    let mut clear = inbound(ChatType::P2p);
    clear.text = "/clear".into();
    clear.command_text = clear.text.clone();
    router.route(clear).await.expect("route");
    settle().await;
    assert!(ports.events().contains(&"mark_pending_fresh".to_string()));
    assert_eq!(
        ports.get(|state| state.replies.clone()),
        vec!["fresh_pending"]
    );
    assert!(!ports.events().contains(&"run".to_string()));

    // 裸 /new（没有正文）⇒ 只轮换路由；带正文的 /new ⇒ 轮换 + 正文入库（Outcome=ingested）。
    let ports2 = Ports::new();
    let router2 = build_router(&ports2);
    router2.register(ChannelKind::Lark, set(&ports2));
    let mut new = inbound(ChatType::P2p);
    new.text = "/new".into();
    new.command_text = new.text.clone();
    router2.route(new).await.expect("route");
    settle().await;
    assert!(ports2.events().contains(&"start_session".to_string()));
    assert_eq!(
        ports2.get(|state| state.replies.clone()),
        vec!["chat_started"]
    );
    assert!(!ports2.events().contains(&"run".to_string()));

    let ports3 = Ports::new();
    let router3 = build_router(&ports3);
    router3.register(ChannelKind::Lark, set(&ports3));
    let mut new_with_body = inbound(ChatType::P2p);
    new_with_body.text = "/new fresh start".into();
    new_with_body.command_text = new_with_body.text.clone();
    router3.route(new_with_body).await.expect("route");
    settle().await;
    assert!(ports3.events().contains(&"start_session".to_string()));
    assert_eq!(ports3.get(|state| state.replies.clone()), vec!["ingested"]);
    assert!(
        ports3.events().contains(&"run".to_string()),
        "带正文 ⇒ 触发 run"
    );
}

/// `skip_agent_run` ⇒ 持久化但不排 run（wecom 的独立 `/issue` 形态）。
#[tokio::test]
async fn skip_agent_run_persists_without_scheduling() {
    let ports = Ports::new();
    let router = build_router(&ports);
    router.register(ChannelKind::Lark, set(&ports));
    let mut message = inbound(ChatType::P2p);
    message.skip_agent_run = true;
    router.route(message).await.expect("route");
    settle().await;
    assert!(ports.events().contains(&"append_message".to_string()));
    assert!(!ports.events().contains(&"run".to_string()));
}

/// 媒体：`has_media` ⇒ 脱离式解析 + 绑定（`drain` 之后断言）。
#[tokio::test]
async fn media_is_resolved_and_bound_off_the_ack_path() {
    let ports = Ports::new();
    ports.set(|state| state.has_media = true);
    let router = build_router(&ports);
    router.register(ChannelKind::Lark, set(&ports));
    router.route(inbound(ChatType::P2p)).await.expect("route");

    assert!(router.drain().await, "drain 必须等媒体任务收尾");
    assert_eq!(ports.get(|state| state.media_resolved), 1);
    assert_eq!(ports.get(|state| state.media_bound), 1, "refs 交给 binder");
}

/// 没有媒体面 / 没有媒体 ⇒ 不排媒体任务。
#[tokio::test]
async fn no_media_means_no_media_job() {
    let ports = Ports::new();
    let router = build_router(&ports);
    router.register(ChannelKind::Lark, set(&ports));
    router.route(inbound(ChatType::P2p)).await.expect("route");
    assert!(router.drain().await);
    assert_eq!(ports.get(|state| state.media_bound), 0);
}

/// 未注册的 kind ⇒ **基础设施**错误（上游 `ErrNoResolverSet`），不是静默丢弃。
#[tokio::test]
async fn unregistered_kind_is_an_infrastructure_error() {
    let ports = Ports::new();
    let router = build_router(&ports);
    let error = router
        .route(inbound(ChatType::P2p))
        .await
        .expect_err("必须 Err");
    assert_eq!(error.code(), "channel_invalid_config");
    assert!(error.to_string().contains(NO_RESOLVER_SET));
}

/// 注册表：last-writer-wins + `kinds()` 字典序 + `has_kind`。
#[tokio::test]
async fn register_is_last_writer_wins_and_kinds_are_sorted() {
    let ports = Ports::new();
    let router = build_router(&ports);
    router.register(ChannelKind::WeCom, set(&ports));
    router.register(ChannelKind::Lark, set(&ports));
    router.register(ChannelKind::Lark, set(&ports));
    let names: Vec<&str> = router.kinds().iter().map(|kind| kind.as_str()).collect();
    assert_eq!(names, vec!["lark", "wecom"]);
    assert!(router.has_kind(ChannelKind::Lark));
    assert!(!router.has_kind(ChannelKind::Slack));
    assert!(router.resolver_set(ChannelKind::Lark).is_some());
    assert!(router.resolver_set(ChannelKind::Slack).is_none());
    assert_eq!(router.config().max_route_change_retries, 8);
}
