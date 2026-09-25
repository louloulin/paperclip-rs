//! `DingTalk` 的解析器集合：安装路由 / 身份绑定 / 去重 / 会话 / 审计（上游 `resolvers.go` 533 行）。
//!
//! - **写者**：M7-7（`docs/60-M7-PLAN.md` §3.3；本片的写集勘误见 `docs/32` §19）。
//! - **共享状态一律走泛化渠道表**（上游注释逐字）：`channel_installation` /
//!   `channel_user_binding` / `channel_inbound_message_dedup` / `channel_chat_session_binding` /
//!   `channel_inbound_audit`。**没有**新查询、**没有** schema 变更。
//! - **端口形状**（与上游同款）：每个解析器只依赖一个**小接口**而不是仓储的具体类型 ——
//!   生产仓储满足它，用例注入替身也满足它。于是"未绑定发件人 ⇒ 回绑定卡"这条判决不需要真库。
//!
//! # 复用 engine 已有的通用实现（**不**重写）
//!
//! | 端口 | 实现 | 出处 |
//! | --- | --- | --- |
//! | `Deduper` | [`ChannelDeduper`]（泛化表） | M7-2 `engine/session.rs` |
//! | `Auditor` | [`ChannelAuditor`]（泛化表） | M7-2 `engine/session.rs` |
//! | `SessionBinder` | [`ChannelSessionBinder`]（`ChatId` 策略）+ 本文件的 [`DingTalkSessionBinder`] 包装 | M7-2 + 本片 |
//! | `InstallationResolver` | [`DingTalkInstallationResolver`] | 本文件 |
//! | `IdentityResolver` | [`DingTalkIdentityResolver`] | 本文件 |
//!
//! # 安装路由键（本平台唯一的形态差异）
//!
//! `DingTalk` 的回调**不带 robot code** ⇒ 路由键是接收它的那条连接盖进 `raw.app_id` 的 `AppKey`
//! （每条安装一条 Stream 连接，所以这个键唯一确定安装）。查询走
//! `channel_installation(config->>'app_id')` —— 与 telegram 同一条语句、不同的
//! `channel_type`。
//!
//! # 本片**登记不全**的三处（`docs/32` §19 的 D 项，不是静默略过）
//!
//! 1. **绑定行的 `config` 列**：上游把 `{conversation_type, conversation_id, staff_id}` 写进
//!    `channel_chat_session_binding.config`（出站寻址要用 `staff_id`）。本仓的通用 binder
//!    写 `null`（M7-2 的形态，动它要改 M7-5/M7-6 的已合用例）⇒ 出站退回**上游自己的**
//!    兜底路径（用 `channel_chat_id` 当会话 id 寻址）。[`outbound_target`] 把这条兜底
//!    **显式**实现成一个纯函数（含"config 是 null 时怎么退"），于是出站片拿到的是同一份判决；
//! 2. **群清单 / bot 身份的写入**：上游 `groupPresenceObserver` 写 `dingtalk_bot_identity` /
//!    `dingtalk_group_presence` / `dingtalk_group_activity`（并调 M7-9 的 `BotNameResolver`
//!    解析可读 bot 名）。本片的写集**不含** `crates/mc-repos/**`（那三张表的写语句要在那里落），
//!    所以本文件只落**接缝** [`GroupPresenceObserver`] + 诚实默认值 [`NoGroupPresence`]
//!    （什么都不写、不报错）。装配时换掉它即可 —— 与 M7-5 的"跨安装身份复用路径"同一类登记；
//! 3. **出站回复器 / 打字指示器**：上游的 `replier` 与 `ackNotifier` 都是 M7-8 的类型
//!    ⇒ 本片的 [`DingTalkResolverSet`] 让这两个端口默认 `None`（装配点留 `with_*`）。

use std::sync::Arc;

use async_trait::async_trait;
use mc_core::channel::message::{ChatType, InboundMessage};
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_repos::channel::binding::{ChannelBindingRepo, ChannelUserBindingRow};
use mc_repos::channel::dedup::ChannelInboundDedupRepo;
use mc_repos::channel::inbound_audit::ChannelInboundAuditRepo;
use mc_repos::channel::installation::{ChannelInstallationRepo, ChannelInstallationRow};
use mc_repos::channel::session::{ChannelChatSessionBindingRow, ChannelChatSessionRepo};
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

use super::inbound::{decode_dingtalk_raw, DingtalkRawEvent, ORIGIN_DINGTALK_CHAT, TYPE_DINGTALK};

// =====================================================================
// adapter 自己的安装值（不透明；Router 只搬运）
// =====================================================================

/// 安装行投影，**只在本 adapter 内流通**（上游把 `db.ChannelInstallation` 塞进
/// `ResolvedInstallation.Platform`）。
///
/// ⚠️ `config` 含**密文** `app_secret_encrypted` ⇒ 手写 `Debug`（`docs/60` §2.3 第 1 条）。
#[derive(Clone, PartialEq)]
pub struct InstallationRow {
    pub id: Id,
    pub workspace_id: Id,
    pub agent_id: Id,
    pub installer_user_id: Id,
    /// `active` / `revoked`（`channel_installation.status`）。
    pub status: String,
    /// 平台配置 blob（含密文 `AppSecret`）。**不**打印。
    pub config: serde_json::Value,
}

impl std::fmt::Debug for InstallationRow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InstallationRow")
            .field("id", &self.id)
            .field("workspace_id", &self.workspace_id)
            .field("agent_id", &self.agent_id)
            .field("installer_user_id", &self.installer_user_id)
            .field("status", &self.status)
            .field("config", &"<redacted>")
            .finish()
    }
}

impl InstallationRow {
    /// 是否还能承载消息（`active`）。
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.status == "active"
    }

    /// 安装的 `AppKey`（`config->>'app_id'`；路由键，**不是**密钥）。
    #[must_use]
    pub fn app_id(&self) -> &str {
        self.config
            .get("app_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
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
    /// 按 **`AppKey`** 查活跃安装（`config->>'app_id'`，`channel_type = 'dingtalk'`）。
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
        self.find_active_by_app_id(TYPE_DINGTALK, app_id)
            .await
            .map(|row| row.as_ref().map(InstallationRow::from))
    }
}

/// 安装路由（上游 `installationResolver`）：用连接盖章的 `AppKey` 找安装。
pub struct DingTalkInstallationResolver {
    queries: Arc<dyn InstallationQueries>,
}

impl std::fmt::Debug for DingTalkInstallationResolver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DingTalkInstallationResolver")
            .field("queries", &"<dyn InstallationQueries>")
            .finish()
    }
}

impl DingTalkInstallationResolver {
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
impl InstallationResolver for DingTalkInstallationResolver {
    async fn resolve_installation(
        &self,
        message: &InboundMessage,
    ) -> EngineResult<ResolvedInstallation> {
        let raw = decode_dingtalk_raw(message)?;
        let found = self
            .queries
            .find_active_by_app_id(&raw.app_id)
            .await
            .map_err(|error| crate::engine::EngineError::infra(error.to_string()))?;
        let Some(row) = found else {
            // 认不出的 AppKey：产品性丢弃（`installation_not_found`），不是错误。
            // 一条 Stream 连接只服务一个安装，所以这只可能是"装机行被撤销 / 换了 AppKey"。
            return Err(PipelineError::InstallationNotFound.into());
        };
        let active = row.is_active();
        Ok(ResolvedInstallation {
            id: row.id,
            workspace_id: row.workspace_id,
            agent_id: row.agent_id,
            installer_user_id: row.installer_user_id,
            active,
            kind: TYPE_DINGTALK,
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
}

/// 身份解析（上游 `identityResolver`）。
pub struct DingTalkIdentityResolver {
    queries: Arc<dyn IdentityQueries>,
}

impl std::fmt::Debug for DingTalkIdentityResolver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DingTalkIdentityResolver")
            .field("queries", &"<dyn IdentityQueries>")
            .finish()
    }
}

impl DingTalkIdentityResolver {
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
impl IdentityResolver for DingTalkIdentityResolver {
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
            // 没绑定 ⇒ 产品性判决（Router 会驱动绑定卡），不是错误。
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
// 会话路由与出站寻址
// =====================================================================

/// 存在 `channel_chat_session_binding.config` 里的出站寻址（上游 `dingtalkBindingConfig`）。
///
/// ⚠️ 本仓的通用 binder **不写**这一列（见模块文档第 1 条）⇒ 这个结构目前只在**读**侧
/// （[`outbound_target`]）与用例里出现；写侧留给 M7-8 / M7-21 收口。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DingTalkBindingConfig {
    /// `1` = 直聊，`2` = 群。
    ///
    /// ⚠️ 两个字段都 `#[serde(default)]`：上游是 Go 结构体 ⇒ 半截 config（只写了 `staff_id`）
    /// 解出来是**空串**而不是解码失败（[`outbound_target`] 靠这条退回绑定列）。
    #[serde(default)]
    pub conversation_type: String,
    #[serde(default)]
    pub conversation_id: String,
    /// 直聊才有：主动回复的**唯一**收件人。群里为空（群按 conversation id 寻址）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub staff_id: String,
}

/// 从一条入站消息推导会话隔离键与出站寻址（上游 `dingtalkSessionRouting`，纯函数）。
///
/// `DingTalk` **没有**线程 ⇒ 一个会话（直聊或群）就是一条连续会话，按 conversation id 隔离。
#[must_use]
pub fn dingtalk_session_routing(message: &InboundMessage) -> (String, DingTalkBindingConfig) {
    let chat_id = message.source.chat_id.clone();
    let mut config = DingTalkBindingConfig {
        conversation_type: super::inbound::CONV_TYPE_GROUP.to_string(),
        conversation_id: chat_id.clone(),
        staff_id: String::new(),
    };
    if message.source.chat_type == ChatType::P2p {
        config.conversation_type = super::inbound::CONV_TYPE_P2P.to_string();
        config.staff_id.clone_from(&message.source.sender_id);
    }
    (chat_id, config)
}

/// 从绑定行恢复出站寻址（上游 `outboundTarget`）：config 缺失 / 解不开时退回
/// `channel_chat_id`。
///
/// 兜底的那一支**不是**本仓发明的：上游自己就有它（`ConversationType: convTypeGroup,
/// ConversationID: b.ChannelChatID`）—— 本仓因为不写 config 列（模块文档第 1 条），
/// 走的就是这一支。
#[must_use]
pub fn outbound_target(binding: &ChannelChatSessionBindingRow) -> DingTalkBindingConfig {
    let mut target = DingTalkBindingConfig {
        conversation_type: super::inbound::CONV_TYPE_GROUP.to_string(),
        conversation_id: binding.channel_chat_id.clone(),
        staff_id: String::new(),
    };
    if !binding.config.is_null() {
        if let Ok(config) = serde_json::from_value::<DingTalkBindingConfig>(binding.config.clone())
        {
            if !config.conversation_type.is_empty() {
                target.conversation_type = config.conversation_type;
            }
            if !config.conversation_id.is_empty() {
                target.conversation_id = config.conversation_id;
            }
            target.staff_id = config.staff_id;
        }
    }
    target
}

/// 群回复的 Markdown 引用里要显示的那一段正文（上游 `dingtalkVisibleQuoteText`）。
///
/// 优先用 `raw.current_text`（adapter 冻结的**当前轮次**、含媒体占位符、不含引用历史）；
/// 退回 `command_text`；再退回（**只在**没有被引用富化、也不是媒体时）`text`。
#[must_use]
pub fn dingtalk_visible_quote_text(message: &InboundMessage) -> String {
    if let Ok(raw) = decode_dingtalk_raw(message) {
        let quote = raw.current_text.trim();
        if !quote.is_empty() {
            return quote.to_string();
        }
    }
    let quote = message.command_text.trim();
    if !quote.is_empty() {
        return quote.to_string();
    }
    if message.reply_to.is_some() || message.kind == mc_core::channel::MessageKind::Image {
        return String::new();
    }
    message.text.trim().to_string()
}

// =====================================================================
// 群清单 / bot 身份（接缝 + 诚实默认值）
// =====================================================================

/// 群清单与 bot 身份的观察接缝（上游 `groupPresenceObserver`）。
///
/// 两条调用时机**不同**（上游逐字，别合并）：
///
/// - [`GroupPresenceObserver::observe`] 在**寻址与成员资格检查都通过之后**、append 之前跑：
///   那里才第一次知道"这条群消息确实是要处理的"；
/// - [`GroupPresenceObserver::record_activity`] 在 **append 提交之后**跑：提前计数会把
///   "最终入库失败"的消息也算进去。
#[async_trait]
pub trait GroupPresenceObserver: Send + Sync {
    /// 记录群存在性与 bot 身份（尽力而为：失败**不得**让一条有效消息失败）。
    async fn observe(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
    ) -> EngineResult<()>;

    /// 推进群活动计数（同上，尽力而为）。
    async fn record_activity(
        &self,
        installation_id: Id,
        message: &InboundMessage,
    ) -> EngineResult<()>;
}

/// 什么都不写的诚实默认值。
///
/// 三张表的**写语句**在 `crates/mc-repos/src/channel/installation.rs`（本片写集不含它）⇒
/// 默认什么都不做、也不报错。装配时换掉它即可（见模块文档第 2 条）。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoGroupPresence;

#[async_trait]
impl GroupPresenceObserver for NoGroupPresence {
    async fn observe(
        &self,
        _installation: &ResolvedInstallation,
        _message: &InboundMessage,
    ) -> EngineResult<()> {
        Ok(())
    }

    async fn record_activity(
        &self,
        _installation_id: Id,
        _message: &InboundMessage,
    ) -> EngineResult<()> {
        Ok(())
    }
}

// =====================================================================
// 会话绑定（包装通用实现，补群清单）
// =====================================================================

/// `DingTalk` 的会话绑定：通用 [`ChannelSessionBinder`] + 群清单观察（上游 `sessionBinder`）。
///
/// 每个方法都是"委托 + 尽力而为的观察"：观察失败只记 warn，**绝不**改判决（上游逐字）。
pub struct DingTalkSessionBinder {
    inner: Arc<dyn SessionBinder>,
    presence: Arc<dyn GroupPresenceObserver>,
}

impl std::fmt::Debug for DingTalkSessionBinder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DingTalkSessionBinder")
            .field("inner", &"<dyn SessionBinder>")
            .field("presence", &"<dyn GroupPresenceObserver>")
            .finish()
    }
}

impl DingTalkSessionBinder {
    /// 装配。
    #[must_use]
    pub fn new(inner: Arc<dyn SessionBinder>, presence: Arc<dyn GroupPresenceObserver>) -> Self {
        Self { inner, presence }
    }

    /// 观察群清单（尽力而为：任何失败都只记 warn）。
    async fn observe(&self, installation: &ResolvedInstallation, message: &InboundMessage) {
        if let Err(error) = self.presence.observe(installation, message).await {
            tracing::warn!(
                installation_id = %installation.id,
                conversation_id = message.source.chat_id,
                code = error.code_hint(),
                "dingtalk: could not record group presence"
            );
        }
    }

    /// 推进活动计数（同上）。
    async fn record_activity(&self, installation_id: Id, message: &InboundMessage) {
        if let Err(error) = self
            .presence
            .record_activity(installation_id, message)
            .await
        {
            tracing::warn!(
                installation_id = %installation_id,
                conversation_id = message.source.chat_id,
                code = error.code_hint(),
                "dingtalk: could not record group activity"
            );
        }
    }
}

#[async_trait]
impl SessionBinder for DingTalkSessionBinder {
    async fn ensure_session(&self, params: EnsureSessionParams) -> EngineResult<Id> {
        let session_id = self.inner.ensure_session(params.clone()).await?;
        // 群清单是**尽力而为**的产品元数据：查不到 / 写不进都不该让一条有效的群消息失败。
        self.observe(&params.installation, &params.message).await;
        Ok(session_id)
    }

    async fn start_session(&self, params: StartSessionParams) -> EngineResult<StartSessionResult> {
        let installation = params.installation.clone();
        let message = params.message.clone();
        let persist = params.persist_message;
        let result = self.inner.start_session(params).await?;
        self.observe(&installation, &message).await;
        if persist {
            // 计数只在 append **提交之后**推进（上游逐字）。
            self.record_activity(installation.id, &message).await;
        }
        Ok(result)
    }

    async fn mark_pending_fresh(&self, session_id: Id, message_id: &str) -> EngineResult<()> {
        self.inner.mark_pending_fresh(session_id, message_id).await
    }

    async fn append_message(&self, params: AppendParams) -> EngineResult<AppendResult> {
        let installation_id = params.installation_id;
        let message = params.message.clone();
        let result = self.inner.append_message(params).await?;
        // 消息与去重标记都已经落库 ⇒ 计数写失败**不得**释放 claim、也不该让 DingTalk 重投
        // 造成重复的用户可见轮次（上游逐字）。
        self.record_activity(installation_id, &message).await;
        Ok(result)
    }

    async fn bind_media(&self, params: BindMediaParams) -> EngineResult<BindMediaResult> {
        self.inner.bind_media(params).await
    }
}

// =====================================================================
// 解析器集合
// =====================================================================

/// 五个必填端口 + 三个可选端口（媒体 / 出站 / 打字）。
pub struct DingTalkResolverSet {
    pub installation: Arc<dyn InstallationResolver>,
    pub identity: Arc<dyn IdentityResolver>,
    pub dedup: Arc<dyn Deduper>,
    pub session: Arc<dyn SessionBinder>,
    pub audit: Arc<dyn Auditor>,
    pub media: Option<Arc<dyn MediaResolver>>,
    pub replier: Option<Arc<dyn OutboundReplier>>,
    pub typing: Option<Arc<dyn TypingNotifier>>,
}

impl std::fmt::Debug for DingTalkResolverSet {
    /// 端口是 trait 对象 ⇒ 只列**存在性**。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DingTalkResolverSet")
            .field("media", &self.media.is_some())
            .field("replier", &self.replier.is_some())
            .field("typing", &self.typing.is_some())
            .finish_non_exhaustive()
    }
}

impl DingTalkResolverSet {
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
    /// 会话隔离键用 [`BindingKeyPolicy::ChatId`]：`DingTalk` **没有**线程 ⇒ 一个会话一条连续会话
    /// （与上游 `dingtalkSessionRouting` 的判决一致；`config` 列的差异见 [`outbound_target`]）。
    #[must_use]
    pub fn from_repos(
        installations: ChannelInstallationRepo,
        bindings: ChannelBindingRepo,
        members: MemberRepo,
        dedup: ChannelInboundDedupRepo,
        sessions: Arc<ChannelChatSessionRepo>,
        audits: ChannelInboundAuditRepo,
    ) -> Self {
        Self::from_repos_with_presence(
            installations,
            bindings,
            members,
            dedup,
            sessions,
            audits,
            Arc::new(NoGroupPresence),
        )
    }

    /// 同 [`Self::from_repos`]，但换掉群清单观察器（M7-9 的 `bot_identity.go` 面接进来时用）。
    #[must_use]
    pub fn from_repos_with_presence(
        installations: ChannelInstallationRepo,
        bindings: ChannelBindingRepo,
        members: MemberRepo,
        dedup: ChannelInboundDedupRepo,
        sessions: Arc<ChannelChatSessionRepo>,
        audits: ChannelInboundAuditRepo,
        presence: Arc<dyn GroupPresenceObserver>,
    ) -> Self {
        let generic = Arc::new(ChannelSessionBinder::new(
            sessions,
            TYPE_DINGTALK,
            SessionBinderConfig {
                binding_key: BindingKeyPolicy::ChatId,
            },
        ));
        Self::new(
            Arc::new(DingTalkInstallationResolver::from_repo(installations)),
            Arc::new(DingTalkIdentityResolver::from_repos(bindings, members)),
            Arc::new(ChannelDeduper::generalized(dedup, TYPE_DINGTALK)),
            Arc::new(DingTalkSessionBinder::new(generic, presence)),
            Arc::new(ChannelAuditor::generalized(audits, TYPE_DINGTALK)),
        )
    }

    /// 挂上媒体面（M7-8 的媒体归它；本片默认 `None`）。
    #[must_use]
    pub fn with_media(mut self, media: Arc<dyn MediaResolver>) -> Self {
        self.media = Some(media);
        self
    }

    /// 挂上出站回复器（绑定卡 / 离线提示 / `/issue` 确认；M7-8）。
    #[must_use]
    pub fn with_replier(mut self, replier: Arc<dyn OutboundReplier>) -> Self {
        self.replier = Some(replier);
        self
    }

    /// 挂上打字指示器（DingTalk 没有原生 typing ⇒ 上游用 ack 通知器代替；M7-8）。
    #[must_use]
    pub fn with_typing(mut self, typing: Arc<dyn TypingNotifier>) -> Self {
        self.typing = Some(typing);
        self
    }

    /// 交给 engine 的 [`crate::engine::resolvers::ResolverSet`]
    /// （`origin_type = dingtalk_chat`）。
    #[must_use]
    pub fn into_engine_set(self) -> crate::engine::resolvers::ResolverSet {
        let mut set = crate::engine::resolvers::ResolverSet::new(
            self.installation,
            self.identity,
            self.dedup,
            self.session,
            self.audit,
            ORIGIN_DINGTALK_CHAT,
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

/// `/issue` 的 `origin_type`（**逐字** `dingtalk_chat`）。
#[must_use]
pub fn origin_type() -> &'static str {
    ORIGIN_DINGTALK_CHAT
}

/// 本 adapter 的平台判别式（诊断用）。
#[must_use]
pub fn kind() -> ChannelKind {
    TYPE_DINGTALK
}

/// 入站信封的平台载荷（诊断 / 用例：需要 `raw` 时不必再解一次）。
#[must_use]
pub fn raw_event(message: &InboundMessage) -> Option<DingtalkRawEvent> {
    decode_dingtalk_raw(message).ok()
}

#[cfg(test)]
mod tests;
