//! Telegram 的解析器集合：安装路由 / 身份绑定 / 去重 / 会话 / 审计 + 打字指示器
//! （上游 `internal/integrations/telegram/resolvers.go`，342 行）。
//!
//! - **写者**：M7-5（`docs/60-M7-PLAN.md` §3.3；本片的写集勘误见 `docs/32` §17.1）。
//! - **共享状态一律走泛化渠道表**（上游注释逐字）：`channel_installation` /
//!   `channel_user_binding` / `channel_inbound_message_dedup` /
//!   `channel_chat_session_binding` / `channel_inbound_audit`。**没有**新查询、**没有** schema
//!   变更。
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
//! | `SessionBinder` | [`ChannelSessionBinder`]（`ChatIdPlusThreadRoot` 策略） | M7-2 `engine/session.rs` |
//! | `InstallationResolver` | [`TelegramInstallationResolver`] | 本文件 |
//! | `IdentityResolver` | [`TelegramIdentityResolver`] | 本文件 |
//!
//! # 与上游的两处**形态 / 语义**差异（登记 `docs/32` §17.2）
//!
//! 1. **会话隔离键的分隔符**：上游是 `chat_id:线程根`，本仓的通用隔离键策略
//!    （[`BindingKeyPolicy::ChatIdPlusThreadRoot`]）用 `#` 分隔。**隔离粒度完全一致**
//!    （论坛话题各自成一个会话 = 上游 `IsTopicMessage` 的语义），只有键的**字面形态**不同；
//!    这条是 M7-3-D1 已登记的跨片偏离（Slack 面同款）。
//! 2. **绑定行的 `config` 列**：上游写 `{"chat_id": …}`（复合键下出站要知道真实 chat id），
//!    本仓的通用实现写 `null`；真实 chat id 在隔离键的前缀里，读得回来（M7-3-D3 同款）。
//!
//! # 跨安装身份复用（**登记缺口**，不静默略过）
//!
//! 上游有一条 `FindReusableChannelUserBinding` 的等价路径在别的 adapter 里；本仓
//! `mc-repos/src/channel/binding.rs` 只提供"按 `(installation, 平台用户 id)` 绑定"的主路径，
//! 而本片写集**不含**仓储 ⇒ 复用路径的落地归拥有 binding 仓储的那一片。没有它只影响
//! "同一个 Telegram 用户在第二个 bot 上要重新走一次绑定卡"，不影响任何安全或数据正确性。

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
    Auditor, Deduper, EngineResult, IdentityResolver, InstallationResolver, MediaResolver,
    OutboundReplier, PipelineError, ResolvedIdentity, ResolvedInstallation, SessionBinder,
    TypingNotifier,
};
use crate::engine::session::{
    BindingKeyPolicy, ChannelAuditor, ChannelDeduper, ChannelSessionBinder, SessionBinderConfig,
};
use crate::telegram::api::{JsonBotApi, TelegramApi};
use crate::telegram::config::{decode_credentials, Decrypter};
use crate::telegram::inbound::{decode_raw, ORIGIN_TELEGRAM_CHAT, TYPE_TELEGRAM};

// =====================================================================
// adapter 自己的安装值（不透明；Router 只搬运）
// =====================================================================

/// 安装行投影，**只在本 adapter 内流通**（上游把 `db.ChannelInstallation` 塞进
/// `ResolvedInstallation.Platform`）。
///
/// 出站面（判决回复器 / 打字指示器）复用它免得再查一次库：bot token 就在 `config` 里。
#[derive(Debug, Clone, PartialEq)]
pub struct InstallationRow {
    pub id: Id,
    pub workspace_id: Id,
    pub agent_id: Id,
    pub installer_user_id: Id,
    /// `active` / `revoked`（`channel_installation.status`）。
    pub status: String,
    /// 平台配置 blob（含 **密文** bot token）。
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

/// 从 [`ResolvedInstallation`] 取回本 adapter 的安装值（类型不符 ⇒ `None`）。
#[must_use]
pub fn installation_row(installation: &ResolvedInstallation) -> Option<Arc<InstallationRow>> {
    installation
        .platform
        .as_ref()
        .and_then(|value| Arc::clone(value).downcast::<InstallationRow>().ok())
}

// =====================================================================
// 安装路由
// =====================================================================

/// 安装行查询接缝（上游 `*db.Queries` 的那一条语句）。
#[async_trait]
pub trait InstallationQueries: Send + Sync {
    /// 按 **bot 数值 id** 查活跃安装（`config->>'app_id'`）。
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
        self.find_active_by_app_id(TYPE_TELEGRAM, app_id)
            .await
            .map(|row| row.as_ref().map(InstallationRow::from))
    }
}

/// 安装路由（上游 `installationResolver`）。
pub struct TelegramInstallationResolver {
    queries: Arc<dyn InstallationQueries>,
}

impl std::fmt::Debug for TelegramInstallationResolver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TelegramInstallationResolver")
            .field("queries", &"<dyn InstallationQueries>")
            .finish()
    }
}

impl TelegramInstallationResolver {
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
impl InstallationResolver for TelegramInstallationResolver {
    async fn resolve_installation(
        &self,
        message: &InboundMessage,
    ) -> EngineResult<ResolvedInstallation> {
        let raw = decode_raw(message)?;
        let found = self
            .queries
            .find_active_by_app_id(&raw.bot_id)
            .await
            .map_err(|error| crate::engine::EngineError::infra(error.to_string()))?;
        let Some(row) = found else {
            // 认不出的 bot id：产品性丢弃（`installation_not_found`），**不是**错误。
            // 一条轮询回路只服务一个 bot，所以这只可能是"装机行被撤销 / 换了 bot"。
            return Err(PipelineError::InstallationNotFound.into());
        };
        let active = row.is_active();
        Ok(ResolvedInstallation {
            id: row.id,
            workspace_id: row.workspace_id,
            agent_id: row.agent_id,
            installer_user_id: row.installer_user_id,
            active,
            kind: TYPE_TELEGRAM,
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
                TYPE_TELEGRAM,
                channel_user_id,
                serde_json::json!({}),
            )
            .await
            .map(|_| ())
    }
}

/// 身份解析（上游 `identityResolver`）。
pub struct TelegramIdentityResolver {
    queries: Arc<dyn IdentityQueries>,
}

impl std::fmt::Debug for TelegramIdentityResolver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TelegramIdentityResolver")
            .field("queries", &"<dyn IdentityQueries>")
            .finish()
    }
}

impl TelegramIdentityResolver {
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
impl IdentityResolver for TelegramIdentityResolver {
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
            .map_err(|error| crate::engine::EngineError::infra(error.to_string()))?;
        let Some(binding) = binding else {
            // 没绑定 ⇒ 产品性判决（Router 会驱动绑定卡），**不是**错误。
            return Err(PipelineError::SenderUnbound.into());
        };
        // 绑定行的存在**不再**证明成员资格（泛化层没有 member 外键）⇒ 重新校验。
        let member = self
            .queries
            .is_workspace_member(installation.workspace_id, binding.multica_user_id())
            .await
            .map_err(|error| crate::engine::EngineError::infra(error.to_string()))?;
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

/// 一条入站 Telegram 消息的会话路由（上游 `telegramSessionRouting` 的**判决**部分）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRouting {
    /// 会话隔离键（存 `channel_chat_id`）。
    pub binding_key: String,
    /// 回复要进哪个话题（空 = 会话层）。
    pub reply_thread: String,
}

/// 从一条入站消息算出隔离键与回复话题（上游 `telegramSessionRouting`，纯函数，**无库可测**）。
///
/// 三件事：
///
/// - **私聊**是一条连续会话（键 = chat id）；上游对私聊**忽略**线程字段（论坛话题只存在于
///   超级群，所以这条在本 adapter 的入站归一化下本来也不会矛盾）；
/// - **论坛话题**按话题隔离（键 = `chat:thread`）—— 与 Feishu 的线程隔离最接近的类比；
/// - **回复话题**恒为消息自己的 `thread_id`（空 = 会话层）。
#[must_use]
pub fn session_routing(message: &InboundMessage) -> SessionRouting {
    let chat_id = message.source.chat_id.as_str();
    let thread = message.source.thread_id.as_str();
    if message.source.chat_type == ChatType::Group && !thread.is_empty() {
        return SessionRouting {
            binding_key: format!("{chat_id}:{thread}"),
            reply_thread: thread.to_string(),
        };
    }
    SessionRouting {
        binding_key: chat_id.to_string(),
        reply_thread: thread.to_string(),
    }
}

// =====================================================================
// 打字指示器
// =====================================================================

/// 打字指示器（上游 `typingNotifier`）：入库后点亮 Telegram 原生的"typing…"。
///
/// 该 chat action 约 **5 秒**后自动消失，所以与 Slack 的"反应"不同，**没有**要清的东西 ——
/// [`TypingNotifier::on_settled`] 是空操作（上游逐字）。
pub struct TelegramTypingNotifier {
    api: Arc<dyn TelegramApi>,
    decrypt: Decrypter,
}

impl std::fmt::Debug for TelegramTypingNotifier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TelegramTypingNotifier")
            .field("api", &"<dyn TelegramApi>")
            .field("decrypt", &self.decrypt)
            .finish()
    }
}

impl TelegramTypingNotifier {
    /// 装配（任意 API 实现）。
    #[must_use]
    pub fn new(api: Arc<dyn TelegramApi>, decrypt: Decrypter) -> Self {
        Self { api, decrypt }
    }

    /// 生产形态：`reqwest` 实现（基址见 [`crate::telegram::api::set_api_base`]）。
    #[must_use]
    pub fn http(decrypt: Decrypter) -> Self {
        Self::new(Arc::new(JsonBotApi::new()), decrypt)
    }

    /// 点亮指示器（**async** 实体；同步接缝只推一个脱离任务，见模块文档）。
    ///
    /// 错误**只告警不返回**：指示器跑在入站流水线**之外** —— 一次 `sendChatAction` 失败
    /// 不该让整条消息变成"投递失败"（上游逐字）。
    pub async fn show_now(&self, installation: &ResolvedInstallation, message: &InboundMessage) {
        let Some(row) = installation_row(installation) else {
            tracing::warn!(
                installation_id = %installation.id,
                "telegram typing: installation platform row unavailable"
            );
            return;
        };
        let credentials = match decode_credentials(&row.config, &self.decrypt) {
            Ok(credentials) => credentials,
            Err(error) => {
                tracing::warn!(
                    installation_id = %installation.id,
                    "telegram typing: decode credentials failed: {error}"
                );
                return;
            }
        };
        let Ok(chat_id) = message.source.chat_id.parse::<i64>() else {
            return;
        };
        let thread_id = message.source.thread_id.parse::<i64>().unwrap_or(0);
        if let Err(error) = self
            .api
            .send_chat_action(&credentials.bot_token, chat_id, thread_id)
            .await
        {
            tracing::warn!(
                installation_id = %installation.id,
                "telegram typing: sendChatAction failed ({})",
                error.method()
            );
        }
    }
}

impl TypingNotifier for TelegramTypingNotifier {
    /// 同步接缝：推一个脱离任务后立刻返回（engine 的调用点绝不阻塞在 Telegram HTTP 上）。
    fn on_ingested(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
        _session_id: Id,
    ) {
        let (installation, message) = (installation.clone(), message.clone());
        let handle = self.handle();
        crate::telegram::spawn_detached(async move {
            handle.show_now(&installation, &message).await;
        });
    }

    /// Telegram 的 chat action 自己会过期 ⇒ 没有要清的东西（上游逐字）。
    fn on_settled(&self, _session_id: Id) {}
}

impl TelegramTypingNotifier {
    /// 可 `'static` 的句柄（脱离任务要它）。克隆的是 `Arc`，**不复制任何凭据**
    /// （本结构只有 API 端口与解密器）。
    #[must_use]
    fn handle(&self) -> Arc<Self> {
        Arc::new(Self {
            api: Arc::clone(&self.api),
            decrypt: self.decrypt.clone(),
        })
    }
}

// =====================================================================
// 解析器集合
// =====================================================================

/// 五个必填端口 + 三个可选端口（出站 / 打字 / 媒体）。
pub struct TelegramResolverSet {
    pub installation: Arc<dyn InstallationResolver>,
    pub identity: Arc<dyn IdentityResolver>,
    pub dedup: Arc<dyn Deduper>,
    pub session: Arc<dyn SessionBinder>,
    pub audit: Arc<dyn Auditor>,
    pub media: Option<Arc<dyn MediaResolver>>,
    pub replier: Option<Arc<dyn OutboundReplier>>,
    pub typing: Option<Arc<dyn TypingNotifier>>,
}

impl std::fmt::Debug for TelegramResolverSet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TelegramResolverSet")
            .field("media", &self.media.is_some())
            .field("replier", &self.replier.is_some())
            .field("typing", &self.typing.is_some())
            .finish_non_exhaustive()
    }
}

impl TelegramResolverSet {
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
    ///
    /// 会话隔离键用 [`BindingKeyPolicy::ChatIdPlusThreadRoot`] —— 见模块文档差异 1。
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
            Arc::new(TelegramInstallationResolver::from_repo(installations)),
            Arc::new(TelegramIdentityResolver::from_repos(bindings, members)),
            Arc::new(ChannelDeduper::generalized(dedup, TYPE_TELEGRAM)),
            Arc::new(ChannelSessionBinder::new(
                sessions,
                TYPE_TELEGRAM,
                SessionBinderConfig {
                    binding_key: BindingKeyPolicy::ChatIdPlusThreadRoot,
                },
            )),
            Arc::new(ChannelAuditor::generalized(audits, TYPE_TELEGRAM)),
        )
    }

    /// 挂上媒体面（本片**没有**媒体解析器：Telegram 的媒体取回归 M7-6 之后的片，
    /// 所以这里保留接口而默认 `None`）。
    #[must_use]
    pub fn with_media(mut self, media: Arc<dyn MediaResolver>) -> Self {
        self.media = Some(media);
        self
    }

    /// 挂上出站回复器（**本片**）。
    #[must_use]
    pub fn with_replier(mut self, replier: Arc<dyn OutboundReplier>) -> Self {
        self.replier = Some(replier);
        self
    }

    /// 挂上打字指示器（**本片**）。
    #[must_use]
    pub fn with_typing(mut self, typing: Arc<dyn TypingNotifier>) -> Self {
        self.typing = Some(typing);
        self
    }

    /// 交给 engine 的 [`crate::engine::resolvers::ResolverSet`]
    /// （`origin_type = telegram_chat`）。
    #[must_use]
    pub fn into_engine_set(self) -> crate::engine::resolvers::ResolverSet {
        let mut set = crate::engine::resolvers::ResolverSet::new(
            self.installation,
            self.identity,
            self.dedup,
            self.session,
            self.audit,
            ORIGIN_TELEGRAM_CHAT,
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

/// `/issue` 的 `origin_type`（**逐字** `telegram_chat`）。
#[must_use]
pub fn origin_type() -> &'static str {
    ORIGIN_TELEGRAM_CHAT
}

/// 本 adapter 的平台判别式（诊断用）。
#[must_use]
pub fn kind() -> ChannelKind {
    TYPE_TELEGRAM
}

#[cfg(test)]
mod tests;
