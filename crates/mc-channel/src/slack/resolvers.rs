//! Slack 的解析器集合：安装路由 / 身份绑定 / 去重 / 会话 / 审计
//! （上游 `internal/integrations/slack/resolvers.go`，429 行）。
//!
//! - **写者**：M7-3（`docs/60-M7-PLAN.md` §3.3）。
//! - **共享状态一律走泛化渠道表**（上游注释逐字）：`channel_installation` /
//!   `channel_user_binding` / `channel_inbound_message_dedup` / `channel_chat_session_binding` /
//!   `channel_inbound_audit`。lark 的**遗留**表在这里**不**碰（那是 M7-14 的事，
//!   `docs/60` §6.4）。
//! - **端口形状**（与上游同款）：每个解析器只依赖一个**小接口**而不是仓储的具体类型 ——
//!   `*db.Queries` 满足它，用例注入替身也满足它。于是"未绑定发件人 ⇒ 回绑定卡"这条判决
//!   不需要真库就能钉住。
//!
//! # 复用 engine 已有的通用实现（**不**重写）
//!
//! | 端口 | 实现 | 出处 |
//! | --- | --- | --- |
//! | `Deduper` | [`ChannelDeduper`]（泛化表） | M7-2 `engine/session.rs` |
//! | `Auditor` | [`ChannelAuditor`]（泛化表） | M7-2 `engine/session/audit.rs` |
//! | `SessionBinder` | [`SlackSessionBinder`]（包住 [`ChannelSessionBinder`]） | 本文件 |
//! | `InstallationResolver` | [`SlackInstallationResolver`] | 本文件 |
//! | `IdentityResolver` | [`SlackIdentityResolver`] | 本文件 |
//!
//! # 与上游的三处**形态 / 语义**差异（登记 `docs/32` §10 与 PR 描述）
//!
//! 1. **会话隔离键的分隔符**：上游是 `chat_id:线程根`，本仓的通用隔离键策略
//!    （[`ChannelSessionBinder`] 的 [`BindingKeyPolicy::ChatIdPlusThreadRoot`]）用 `#` 分隔，
//!    并把"**顶层**消息的线程根 = 消息自身的 ts"这条规则交给 adapter 先归一化
//!    （见 [`SlackSessionBinder::normalize`]）。**隔离粒度完全一致**（一个频道里两个 `@bot`
//!    线程 = 两个会话），只是键的字面形态不同 ⇒ 出站（M7-4）取 channel id 时按 `#` 前缀取。
//! 2. **p2p（DM）线程内的回复落点**：同一条消息字段既当隔离键又当"回复进哪个线程"，而 p2p 的
//!    隔离键必须是 chat id（否则同一条 DM 会分裂成两个会话），所以 DM 内的线程回复会落在 DM
//!    顶层而不是线程里（上游落在线程里）。选"会话不分裂"是刻意的：会话分裂会让 agent 丢上下文。
//! 3. **绑定行的 `config` 列**：上游写 `{"channel_id": …}`（复合键下出站要知道真实 channel id），
//!    本仓的通用实现写 `null`；channel id 在隔离键的前缀里，读得回来（见差异 1）。
//! 4. **跨安装身份复用**：上游有一条 `FindReusableChannelUserBinding`（同一个 Slack 工作区里
//!    第二个 app 不必重新提示绑定，MUL-3911）。本仓 `mc-repos/src/channel/binding.rs` **没有**
//!    这条查询，而本片的写集**不含**仓储 ⇒ 本片只实现"按 `(installation, 平台用户 id)` 绑定"
//!    的主路径；复用路径的落地归**拥有 binding 仓储的那一片**（M7-1 的 `binding.rs` 已合、
//!    加查询要另开票）。**这不是静默略过**：没有它只影响"第二个 app 首条消息要重新走一次绑定"，
//!    不影响任何安全或数据正确性。

use std::sync::Arc;

use async_trait::async_trait;
use mc_core::channel::message::{ChatType, InboundMessage};
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_repos::channel::binding::{ChannelBindingRepo, ChannelUserBindingRow};
use mc_repos::channel::dedup::ChannelInboundDedupRepo;
use mc_repos::channel::inbound_audit::ChannelInboundAuditRepo;
use mc_repos::channel::installation::{ChannelInstallationRepo, ChannelInstallationRow};
use mc_repos::channel::session::ChannelChatSessionRepo;
use mc_repos::member::MemberRepo;
use mc_repos::RepoError;

use crate::engine::resolvers::{
    AppendParams, AppendResult, Auditor, BindMediaParams, BindMediaResult, Deduper, EngineResult,
    EnsureSessionParams, IdentityResolver, InstallationResolver, MediaResolver, OutboundReplier,
    PipelineError, ResolvedIdentity, ResolvedInstallation, SessionBinder, StartSessionParams,
    StartSessionResult, TypingNotifier,
};
use crate::engine::session::{
    BindingKeyPolicy, ChannelAuditor, ChannelDeduper, ChannelSessionBinder, SessionBinderConfig,
};
use crate::slack::config::installation_serves_team;
use crate::slack::inbound::{RawEvent, ORIGIN_SLACK_CHAT, TYPE_SLACK};

// =====================================================================
// adapter 自己的安装值（不透明；Router 只搬运）
// =====================================================================

/// 安装行投影，**只在本 adapter 内流通**（上游把 `db.ChannelInstallation` 塞进
/// `ResolvedInstallation.Platform`）。
///
/// 出站面（M7-4 的回复器 / 打字指示）复用它免得再查一次库：bot token 就在 `config` 里。
#[derive(Debug, Clone, PartialEq)]
pub struct InstallationRow {
    pub id: Id,
    pub workspace_id: Id,
    pub agent_id: Id,
    pub installer_user_id: Id,
    /// `active` / `revoked`（`channel_installation.status`）。
    pub status: String,
    /// 平台配置 blob（含两个密文令牌）。
    pub config: serde_json::Value,
}

impl InstallationRow {
    /// 是否还能承载消息（`active`）。
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.status == "active"
    }
}

impl From<&ChannelInstallationRow> for InstallationRow {
    fn from(row: &ChannelInstallationRow) -> Self {
        Self {
            id: row.id(),
            workspace_id: row.workspace_id(),
            agent_id: Id(row.agent_id),
            installer_user_id: Id(row.installer_user_id),
            status: row.status.clone(),
            config: row.config.clone(),
        }
    }
}

/// 从 `raw` 解出 Slack 的平台事件字段（解不开 ⇒ 基础设施失败：`raw` 是本 adapter 自己写的）。
fn decode_raw(message: &InboundMessage) -> EngineResult<RawEvent> {
    if message.raw.is_null() {
        return Err(crate::engine::resolvers::EngineError::infra(
            "slack: inbound message raw is empty",
        ));
    }
    serde_json::from_value(message.raw.clone()).map_err(|error| {
        crate::engine::resolvers::EngineError::infra(format!("slack: decode inbound raw: {error}"))
    })
}

// =====================================================================
// 安装路由
// =====================================================================

/// 安装行查询接缝（上游 `*db.Queries` 的那一条语句）。
#[async_trait]
pub trait InstallationQueries: Send + Sync {
    /// 按**真实 Slack app id** 查活跃安装（`config->>'app_id'`）。
    async fn find_active_by_app_id(
        &self,
        app_id: &str,
    ) -> Result<Option<InstallationRow>, RepoError>;
}

#[async_trait]
impl InstallationQueries for ChannelInstallationRepo {
    async fn find_active_by_app_id(
        &self,
        app_id: &str,
    ) -> Result<Option<InstallationRow>, RepoError> {
        self.find_active_by_app_id(TYPE_SLACK, app_id)
            .await
            .map(|row| row.as_ref().map(InstallationRow::from))
    }
}

/// 安装路由（上游 `installationResolver`）。
pub struct SlackInstallationResolver {
    queries: Arc<dyn InstallationQueries>,
}

impl std::fmt::Debug for SlackInstallationResolver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SlackInstallationResolver")
            .field("queries", &"<dyn InstallationQueries>")
            .finish()
    }
}

impl SlackInstallationResolver {
    /// 任意接缝（用例注入替身）。
    pub fn new(queries: Arc<dyn InstallationQueries>) -> Self {
        Self { queries }
    }

    /// 生产形态：直接用安装行仓储。
    #[must_use]
    pub fn from_repo(repo: ChannelInstallationRepo) -> Self {
        Self::new(Arc::new(repo))
    }
}

#[async_trait]
impl InstallationResolver for SlackInstallationResolver {
    async fn resolve_installation(
        &self,
        message: &InboundMessage,
    ) -> EngineResult<ResolvedInstallation> {
        let raw = decode_raw(message)?;
        let found = self
            .queries
            .find_active_by_app_id(&raw.api_app_id)
            .await
            .map_err(|error| crate::engine::resolvers::EngineError::infra(error.to_string()))?;
        let Some(row) = found else {
            // 认不出的 app id：产品性丢弃（`invalid_event`），**不是**错误。
            return Err(PipelineError::InstallationNotFound.into());
        };
        // 按 `api_app_id` 路由只标识 Slack **app**：一个 BYO app 被装进另一个工作区时事件带的是
        // 同一个 app id，所以还要校验事件的工作区与安装 bot 所属工作区一致。
        if !installation_serves_team(&row.config, &raw.team_id) {
            return Err(PipelineError::InstallationNotFound.into());
        }
        let active = row.is_active();
        Ok(ResolvedInstallation {
            id: row.id,
            workspace_id: row.workspace_id,
            agent_id: row.agent_id,
            installer_user_id: row.installer_user_id,
            active,
            kind: TYPE_SLACK,
            platform: Some(Arc::new(row)),
        })
    }
}

// =====================================================================
// 身份绑定
// =====================================================================

/// 身份查询接缝（上游 `identityQueries`）。
#[async_trait]
pub trait IdentityQueries: Send + Sync {
    /// `(installation, 平台用户 id)` 上的绑定行。
    async fn find_user_binding(
        &self,
        installation_id: Id,
        channel_user_id: &str,
    ) -> Result<Option<ChannelUserBindingRow>, RepoError>;

    /// 该用户是否还是这个 workspace 的成员（绑定表**没有** member 外键 ⇒ 必须重校验）。
    async fn is_workspace_member(&self, workspace_id: Id, user_id: Id) -> Result<bool, RepoError>;

    /// 在本安装上物化一条绑定（重绑幂等）。
    async fn upsert_user_binding(
        &self,
        workspace_id: Id,
        user_id: Id,
        installation_id: Id,
        channel_user_id: &str,
    ) -> Result<(), RepoError>;
}

/// 生产形态：绑定仓储 + 成员仓储（两个小仓储拼成一条接缝）。
pub struct RepoIdentityQueries {
    bindings: ChannelBindingRepo,
    members: MemberRepo,
}

impl RepoIdentityQueries {
    /// 装配。
    #[must_use]
    pub fn new(bindings: ChannelBindingRepo, members: MemberRepo) -> Self {
        Self { bindings, members }
    }
}

impl std::fmt::Debug for RepoIdentityQueries {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RepoIdentityQueries")
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl IdentityQueries for RepoIdentityQueries {
    async fn find_user_binding(
        &self,
        installation_id: Id,
        channel_user_id: &str,
    ) -> Result<Option<ChannelUserBindingRow>, RepoError> {
        self.bindings
            .find_user_binding(installation_id, channel_user_id)
            .await
    }

    async fn is_workspace_member(&self, workspace_id: Id, user_id: Id) -> Result<bool, RepoError> {
        match self.members.get_for_user(workspace_id, user_id).await {
            Ok(_) => Ok(true),
            Err(RepoError::NotFound) => Ok(false),
            Err(other) => Err(other),
        }
    }

    async fn upsert_user_binding(
        &self,
        workspace_id: Id,
        user_id: Id,
        installation_id: Id,
        channel_user_id: &str,
    ) -> Result<(), RepoError> {
        self.bindings
            .upsert_user_binding(
                workspace_id,
                user_id,
                installation_id,
                TYPE_SLACK,
                channel_user_id,
                serde_json::json!({}),
            )
            .await
            .map(|_| ())
    }
}

/// 身份解析（上游 `identityResolver`）。
pub struct SlackIdentityResolver {
    queries: Arc<dyn IdentityQueries>,
}

impl std::fmt::Debug for SlackIdentityResolver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SlackIdentityResolver")
            .field("queries", &"<dyn IdentityQueries>")
            .finish()
    }
}

impl SlackIdentityResolver {
    /// 任意接缝（用例注入替身）。
    pub fn new(queries: Arc<dyn IdentityQueries>) -> Self {
        Self { queries }
    }

    /// 生产形态：绑定 + 成员两个仓储。
    #[must_use]
    pub fn from_repos(bindings: ChannelBindingRepo, members: MemberRepo) -> Self {
        Self::new(Arc::new(RepoIdentityQueries::new(bindings, members)))
    }
}

#[async_trait]
impl IdentityResolver for SlackIdentityResolver {
    async fn resolve_sender(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
    ) -> EngineResult<ResolvedIdentity> {
        let sender_id = message.source.sender_id.as_str();
        let binding = self
            .queries
            .find_user_binding(installation.id, sender_id)
            .await
            .map_err(|error| crate::engine::resolvers::EngineError::infra(error.to_string()))?;
        let Some(binding) = binding else {
            // 没绑定 ⇒ 产品性判决（Router 会驱动绑定卡），**不是**错误。
            return Err(PipelineError::SenderUnbound.into());
        };
        // 绑定行的存在**不再**证明成员资格（泛化层没有 member 外键）⇒ 重新校验。
        let member = self
            .queries
            .is_workspace_member(installation.workspace_id, binding.multica_user_id())
            .await
            .map_err(|error| crate::engine::resolvers::EngineError::infra(error.to_string()))?;
        if !member {
            return Err(PipelineError::SenderNotMember.into());
        }
        Ok(ResolvedIdentity {
            user_id: binding.multica_user_id(),
        })
    }
}

// =====================================================================
// 会话路由
// =====================================================================

/// 一条入站 Slack 消息的会话路由（上游 `slackSessionRouting` 的**判决**部分）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRouting {
    /// 会话隔离键（存 `channel_chat_id`）。
    pub binding_key: String,
    /// 回复要进哪个线程（空 = 会话层）。
    pub reply_thread: String,
}

/// 从一条入站消息算出隔离键与回复线程（上游 `slackSessionRouting`，纯函数）。
///
/// 三件事必须**分开**（上游注释逐字）：
///
/// - **DM** 是"每个频道一个连续会话" ⇒ 键就是 chat id；
/// - **频道 / 群**按**线程根**隔离 ⇒ 键是 `chat:root`，一个频道里两个 `@bot` 线程是两个会话；
///   线程根 = 回复时的 `thread_ts`，否则消息自己的 ts（顶层 `@` 起一个新根）；
/// - **回复线程**：群回复进线程根；DM 回复进入站线程（顶层发送时为空）。
#[must_use]
pub fn session_routing(message: &InboundMessage) -> SessionRouting {
    let chat_id = message.source.chat_id.as_str();
    if message.source.chat_type == ChatType::P2p {
        return SessionRouting {
            binding_key: chat_id.to_string(),
            reply_thread: message.source.thread_id.clone(),
        };
    }
    let root = if message.source.thread_id.is_empty() {
        message.message_id.clone()
    } else {
        message.source.thread_id.clone()
    };
    SessionRouting {
        binding_key: format!("{chat_id}:{root}"),
        reply_thread: root,
    }
}

/// 会话绑定端口（上游 `sessionBinder`）：把 Slack 的路由判决喂给 engine 的共享会话组件。
///
/// 见模块文档差异 1 / 2：本实现把"隔离键用的线程根"归一化进消息里，再交给
/// [`ChannelSessionBinder`]（`ChatIdPlusThreadRoot` 策略）。
pub struct SlackSessionBinder {
    inner: Arc<ChannelSessionBinder>,
}

impl std::fmt::Debug for SlackSessionBinder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SlackSessionBinder")
            .field("inner", &self.inner)
            .finish()
    }
}

impl SlackSessionBinder {
    /// 装配（会话隔离键 = `chat_id#线程根`；线程化平台的唯一正确形态）。
    #[must_use]
    pub fn new(repo: Arc<ChannelChatSessionRepo>) -> Self {
        Self {
            inner: Arc::new(ChannelSessionBinder::new(
                repo,
                TYPE_SLACK,
                SessionBinderConfig {
                    binding_key: BindingKeyPolicy::ChatIdPlusThreadRoot,
                },
            )),
        }
    }

    /// 共享会话组件（诊断 / 迟到回调的对账用）。
    #[must_use]
    pub fn inner(&self) -> &Arc<ChannelSessionBinder> {
        &self.inner
    }

    /// 归一化"隔离键用的线程根"（见模块文档差异 1 / 2）。
    fn normalize(message: &mut InboundMessage) {
        message.source.thread_id = match message.source.chat_type {
            ChatType::P2p => String::new(),
            ChatType::Group => session_routing(message).reply_thread,
        };
    }
}

#[async_trait]
impl SessionBinder for SlackSessionBinder {
    async fn ensure_session(&self, mut params: EnsureSessionParams) -> EngineResult<Id> {
        Self::normalize(&mut params.message);
        self.inner.ensure_session(params).await
    }

    async fn start_session(
        &self,
        mut params: StartSessionParams,
    ) -> EngineResult<StartSessionResult> {
        Self::normalize(&mut params.message);
        self.inner.start_session(params).await
    }

    async fn mark_pending_fresh(&self, session_id: Id, message_id: &str) -> EngineResult<()> {
        self.inner.mark_pending_fresh(session_id, message_id).await
    }

    async fn append_message(&self, mut params: AppendParams) -> EngineResult<AppendResult> {
        Self::normalize(&mut params.message);
        self.inner.append_message(params).await
    }

    async fn bind_media(&self, params: BindMediaParams) -> EngineResult<BindMediaResult> {
        // 媒体绑定不读路由（上游同形）：`BindMediaInput` 用的是会话与消息 id。
        self.inner.bind_media(params).await
    }
}

// =====================================================================
// 解析器集合
// =====================================================================

/// 五个必填端口 + 三个可选端口（出站 / 打字 / 媒体）。
pub struct SlackResolverSet {
    pub installation: Arc<dyn InstallationResolver>,
    pub identity: Arc<dyn IdentityResolver>,
    pub dedup: Arc<dyn Deduper>,
    pub session: Arc<dyn SessionBinder>,
    pub audit: Arc<dyn Auditor>,
    pub media: Option<Arc<dyn MediaResolver>>,
    pub replier: Option<Arc<dyn OutboundReplier>>,
    pub typing: Option<Arc<dyn TypingNotifier>>,
}

impl std::fmt::Debug for SlackResolverSet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SlackResolverSet")
            .field("media", &self.media.is_some())
            .field("replier", &self.replier.is_some())
            .field("typing", &self.typing.is_some())
            .finish_non_exhaustive()
    }
}

impl SlackResolverSet {
    /// 必填端口（可选端口为 `None`）。
    #[must_use]
    pub fn new(
        installation: Arc<dyn InstallationResolver>,
        identity: Arc<dyn IdentityResolver>,
        dedup: Arc<dyn Deduper>,
        session: Arc<dyn SessionBinder>,
        audit: Arc<dyn Auditor>,
    ) -> Self {
        Self {
            installation,
            identity,
            dedup,
            session,
            audit,
            media: None,
            replier: None,
            typing: None,
        }
    }

    /// 从六个泛化渠道仓储装配（不含出站 / 打字 / 媒体：那三面各自的片注入）。
    #[must_use]
    pub fn from_repos(
        installations: ChannelInstallationRepo,
        bindings: ChannelBindingRepo,
        members: MemberRepo,
        dedup: ChannelInboundDedupRepo,
        sessions: Arc<ChannelChatSessionRepo>,
        audits: ChannelInboundAuditRepo,
    ) -> Self {
        Self::new(
            Arc::new(SlackInstallationResolver::from_repo(installations)),
            Arc::new(SlackIdentityResolver::from_repos(bindings, members)),
            Arc::new(ChannelDeduper::generalized(dedup, TYPE_SLACK)),
            Arc::new(SlackSessionBinder::new(sessions)),
            Arc::new(ChannelAuditor::generalized(audits, TYPE_SLACK)),
        )
    }

    /// 挂上媒体面（M7-3 的 `media.rs`；`None` = 该部署不做媒体）。
    #[must_use]
    pub fn with_media(mut self, media: Arc<dyn MediaResolver>) -> Self {
        self.media = Some(media);
        self
    }

    /// 挂上出站回复器（**M7-4**）。
    #[must_use]
    pub fn with_replier(mut self, replier: Arc<dyn OutboundReplier>) -> Self {
        self.replier = Some(replier);
        self
    }

    /// 挂上打字指示器（**M7-4**）。
    #[must_use]
    pub fn with_typing(mut self, typing: Arc<dyn TypingNotifier>) -> Self {
        self.typing = Some(typing);
        self
    }

    /// 交给 engine 的 [`crate::engine::resolvers::ResolverSet`]（`origin_type = slack_chat`）。
    #[must_use]
    pub fn into_engine_set(self) -> crate::engine::resolvers::ResolverSet {
        let mut set = crate::engine::resolvers::ResolverSet::new(
            self.installation,
            self.identity,
            self.dedup,
            self.session,
            self.audit,
            ORIGIN_SLACK_CHAT,
        );
        if let Some(media) = self.media {
            set = set.with_media(media);
        }
        if let Some(replier) = self.replier {
            set = set.with_replier(replier);
        }
        if let Some(typing) = self.typing {
            set = set.with_typing(typing);
        }
        set
    }
}

/// `/issue` 的 `origin_type`（**逐字** `slack_chat`）。
#[must_use]
pub fn origin_type() -> &'static str {
    ORIGIN_SLACK_CHAT
}

/// 本 adapter 的平台判别式（诊断用）。
#[must_use]
pub fn kind() -> ChannelKind {
    TYPE_SLACK
}

#[cfg(test)]
mod tests;
