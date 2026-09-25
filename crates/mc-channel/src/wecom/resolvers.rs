//! `WeCom` 的 **`ResolverSet`**：引擎的 `Router` 按 `channel_type = "wecom"` 路由时走的那一组端口
//! （上游 `internal/integrations/wecom/wecom_resolvers.go`，**340 行**）。
//!
//! - **写者**：M7-19（`LUM-1784` / `docs/60-M7-PLAN.md` §3.3）。
//! - **上游定位**（文件头注释逐字）：每个接口方法都在引擎归一化的
//!   `channel.InboundMessage` 与 wecom 的 store / service 之间翻译。归一化信封**不带**的平台字段
//!   （`BotID`、发件人 userid）从塞在 [`InboundMessage::raw`] 里的 [`WeComInboundMessage`] 取出来。
//!
//! # 本文件"借"了什么、为什么（不是偷懒）
//!
//! 上游的五个解析器里有两个是**纯数据搬运**，而 M7-1/M7-2 已经落过一模一样的实现：
//!
//! | 上游 | 本仓 |
//! | --- | --- |
//! | `deduper`（`channel_inbound_message_dedup` 的三条查询） | [`crate::engine::ChannelDeduper::generalized`] |
//! | `auditor`（`RecordChannelInboundDrop`） | [`crate::engine::ChannelAuditor::generalized`] |
//!
//! 本片**不**再写第二份（写第二份 = 把"丢弃审计长什么样"这件事切成两半）。**登记为差异**
//! （`docs/32` §37 的 D7）：上游的 `auditor.RecordDrop` 把 `event_type` 填成**平台事件名**
//! （`wm.MsgType`，即 `"image"` / `"mixed"`…），而 [`ChannelAuditor`] 填的是**归一化后的**
//! `MessageKind`。M7-2 已经登记过这条口径（`engine/session/audit.rs` 的 `drop_from_message` 文档），
//! 本片照抄、不另起一份。
//!
//! 另外三个（安装路由 / 身份绑定 / 会话绑定）是**平台特有**的翻译，在本文件落地。
//!
//! # 会话绑定的两个键（上游逐字）
//!
//! - **单聊（p2p）**：wecom 的 `ChatID` **就是** userid ⇒ 一个用户一个会话；
//! - **群聊**：按 `chatid` 做键 ⇒ 一个群的全部流量进**同一个**会话（aibot API 没有一等的"话题"
//!   概念）。
//!
//! 两种情况下 [`SessionBinder::ensure_session`] 与 [`SessionBinder::append_message`] 用的都是
//! `message.source.chat_id`，而它正是读循环按 chat type 填好的那一个（见 `inbound.rs`）。
//!
//! # 身份解析为什么是显式绑定而不是启发式
//!
//! aibot 的 userid 与企业真实的 userid / 邮箱**没有**任何关系 —— 它是按 `(bot, user)` 匿名稳定的
//! id —— 所以内部客服那套"按邮箱前缀匹配"的做法在这里**不可能**成立。手册逐条见
//! [`crate::wecom::binding`]（M7-15）。
//!
//! 成员资格**重查**：绑定行的存在**不**证明当前的 workspace 成员资格 —— 一个被移除的成员，他的绑定
//! 行会留到管理员清理为止。`sender_not_member` 让 Router 静默丢弃而不是再提示一次（正确的产品结局，
//! 也避免泄露"这个人曾经是成员"）。

use std::sync::Arc;

use async_trait::async_trait;

use mc_core::channel::message::InboundMessage;
use mc_core::id::Id;
use mc_repos::channel::binding::ChannelBindingRepo;
use mc_repos::channel::installation::ChannelInstallationRepo;
use mc_repos::member::MemberRepo;
use mc_repos::RepoError;

use crate::engine::resolvers::{
    AppendParams, AppendResult, BindMediaParams, BindMediaResult, EngineError, EngineResult,
    EnsureSessionParams, IdentityResolver, InstallationResolver, PipelineError, ResolvedIdentity,
    ResolvedInstallation, ResolverSet, SessionBinder, StartSessionParams, StartSessionResult,
};
use crate::engine::session::{ChannelAuditor, ChannelDeduper};
use crate::wecom::types::{Installation, KIND};
use crate::wecom::wecom_channel::inbound::WeComInboundMessage;

/// `/issue` 写给 `issue.origin_type` 的渠道标签（上游 `originWecomChat`）。
///
/// 与 lark 的 `lark_chat` 同形（平台 + `_chat`），这样看板的 origin 家族保持一致。
pub const ORIGIN_WECOM_CHAT: &str = "wecom_chat";

/// 从 [`InboundMessage::raw`] 解出 wecom 这一侧的信封。
///
/// 每个解析器最后都要做一次；JSON 标签的形状集中在这里，于是 `raw` 的形状一变只需改**一个文件**。
///
/// # Errors
///
/// `raw` 为空或不是 wecom 的形状 ⇒ [`PipelineError::InstallationNotFound`]（没有路由键就没法路由，
/// 这是产品性丢弃而不是基础设施失败）。
pub fn wecom_msg_from_raw(message: &InboundMessage) -> EngineResult<WeComInboundMessage> {
    if message.raw.is_null() {
        return Err(PipelineError::InstallationNotFound.into());
    }
    serde_json::from_value::<WeComInboundMessage>(message.raw.clone())
        .map_err(|_| PipelineError::InstallationNotFound.into())
}

// =====================================================================
// 安装路由
// =====================================================================

/// 安装查询接缝（上游 `store.GetInstallationByBotID` 的那**一个**问题）。
#[async_trait]
pub trait InstallationQueries: Send + Sync {
    /// 按 **`bot_id`** 查活跃安装（`config->>'app_id'`，`channel_type = 'wecom'`）。
    ///
    /// # Errors
    ///
    /// 仓储层故障（**只报结构信息**，不含 config blob）。
    async fn find_active_by_bot_id(&self, bot_id: &str) -> Result<Option<Installation>, RepoError>;
}

/// 生产形态：直接用泛化安装行仓储。
///
/// `bot_id` 同时是路由键（写侧保证 `app_id == bot_id`，见 [`crate::wecom::types::Installation::encode_config`]）
/// ⇒ 查的是同一个唯一索引 `idx_channel_installation_type_appid`。
#[async_trait]
impl InstallationQueries for ChannelInstallationRepo {
    async fn find_active_by_bot_id(&self, bot_id: &str) -> Result<Option<Installation>, RepoError> {
        let found = self.find_active_by_app_id(KIND, bot_id).await?;
        let Some(row) = found else {
            return Ok(None);
        };
        // config 坏 / 缺路由键 ⇒ 这条安装**读不出来**（上游 `installationFromRow` 的 error 那一支）。
        Installation::from_row(&row)
            .map(Some)
            .map_err(|_| RepoError::NotFound)
    }
}

/// 安装路由（上游 `installationResolver`）：用**连接盖章的 `bot_id`** 找安装。
///
/// 每一条 `aibot_msg_callback` 都经由它到达的那条 WebSocket 连接说明自己是哪个机器人（一个机器人
/// 一条连接）；连接器把 `bot_id` 盖进 [`WeComInboundMessage`]，于是本解析器是一次**纯 DB 查询**，
/// 不需要任何 socket 侧的管道。
pub struct WeComInstallationResolver {
    queries: Arc<dyn InstallationQueries>,
}

impl std::fmt::Debug for WeComInstallationResolver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WeComInstallationResolver")
            .field("queries", &"<dyn InstallationQueries>")
            .finish()
    }
}

impl WeComInstallationResolver {
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
impl InstallationResolver for WeComInstallationResolver {
    async fn resolve_installation(
        &self,
        message: &InboundMessage,
    ) -> EngineResult<ResolvedInstallation> {
        let wm = wecom_msg_from_raw(message)?;
        if wm.bot_id.is_empty() {
            // 没有路由键 ⇒ 认不出这条事件（上游 `ErrInstallationNotFound`）。
            return Err(PipelineError::InstallationNotFound.into());
        }
        let found = self
            .queries
            .find_active_by_bot_id(&wm.bot_id)
            .await
            .map_err(|error| EngineError::infra(error.to_string()))?;
        let Some(installation) = found else {
            return Err(PipelineError::InstallationNotFound.into());
        };
        let active = installation.is_active();
        // 四个身份列先取出来，再把这个**值**放进 `platform` 供同组其它端口复用
        // （Router 只搬运它，从不读它 —— 它的 `Debug` 输出 `<opaque>`）。
        let (id, workspace_id, agent_id, installer_user_id) = (
            installation.id,
            installation.workspace_id,
            installation.agent_id,
            installation.installer_user_id,
        );
        Ok(ResolvedInstallation {
            id,
            workspace_id,
            agent_id,
            installer_user_id,
            active,
            kind: KIND,
            platform: Some(Arc::new(installation)),
        })
    }
}

// =====================================================================
// 身份绑定
// =====================================================================

/// 身份查询接缝（上游那两条查询）。
#[async_trait]
pub trait IdentityQueries: Send + Sync {
    /// `(installation, 平台用户 id)` 上的绑定行（**只**要 `multica_user_id`）。
    ///
    /// # Errors
    ///
    /// 仓储层故障（只报结构信息）。
    async fn find_user_binding(
        &self,
        installation_id: Id,
        channel_user_id: &str,
    ) -> Result<Option<Id>, RepoError>;

    /// 这个用户现在还是不是这个 workspace 的成员。
    ///
    /// # Errors
    ///
    /// 仓储层故障。
    async fn is_workspace_member(&self, workspace_id: Id, user_id: Id) -> Result<bool, RepoError>;
}

/// 生产形态：绑定表 + 成员表。
pub struct RepoIdentityQueries {
    bindings: ChannelBindingRepo,
    members: MemberRepo,
}

impl std::fmt::Debug for RepoIdentityQueries {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RepoIdentityQueries")
    }
}

impl RepoIdentityQueries {
    /// 装配。
    #[must_use]
    pub fn new(bindings: ChannelBindingRepo, members: MemberRepo) -> Self {
        Self { bindings, members }
    }
}

#[async_trait]
impl IdentityQueries for RepoIdentityQueries {
    async fn find_user_binding(
        &self,
        installation_id: Id,
        channel_user_id: &str,
    ) -> Result<Option<Id>, RepoError> {
        let found = self
            .bindings
            .find_user_binding(installation_id, channel_user_id)
            .await?;
        Ok(found.map(|row| row.multica_user_id()))
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
pub struct WeComIdentityResolver {
    queries: Arc<dyn IdentityQueries>,
}

impl std::fmt::Debug for WeComIdentityResolver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WeComIdentityResolver")
            .field("queries", &"<dyn IdentityQueries>")
            .finish()
    }
}

impl WeComIdentityResolver {
    /// 任意接缝（用例注入替身）。
    pub fn new(queries: Arc<dyn IdentityQueries>) -> Self {
        Self { queries }
    }

    /// 生产形态。
    #[must_use]
    pub fn from_repos(bindings: ChannelBindingRepo, members: MemberRepo) -> Self {
        Self::new(Arc::new(RepoIdentityQueries::new(bindings, members)))
    }
}

#[async_trait]
impl IdentityResolver for WeComIdentityResolver {
    async fn resolve_sender(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
    ) -> EngineResult<ResolvedIdentity> {
        let sender_id = message.source.sender_id.trim().to_string();
        if sender_id.is_empty() {
            return Err(PipelineError::SenderUnbound.into());
        }
        let binding = self
            .queries
            .find_user_binding(installation.id, &sender_id)
            .await
            .map_err(|error| EngineError::infra(error.to_string()))?;
        let Some(user_id) = binding else {
            // 第一次来的发件人没有行 ⇒ `needs_binding`，Router 据此配上出站绑定卡
            // （`replier.rs` 的 `send_binding_prompt`）。
            return Err(PipelineError::SenderUnbound.into());
        };
        let is_member = self
            .queries
            .is_workspace_member(installation.workspace_id, user_id)
            .await
            .map_err(|error| EngineError::infra(error.to_string()))?;
        if !is_member {
            return Err(PipelineError::SenderNotMember.into());
        }
        Ok(ResolvedIdentity { user_id })
    }
}

// =====================================================================
// 会话绑定
// =====================================================================

/// 会话绑定（上游 `sessionBinder`）：把引擎的 `SessionBinder` 调用按 wecom 的语义映射一次。
///
/// 五处映射，逐条对应上游：
///
/// 1. **会话隔离键 = `Source.ChatID`**（单聊里它就是 userid，群里它是 chatid）；
/// 2. **`StartSession` 的 `Initiator` = `p.Sender`**，而 `EnsureSession` 的 `Sender` 是 Router 决定的
///    会话创建者（p2p 是那个人，群聊是安装者）；
/// 3. **`command_text` 只从 adapter 自己的那一份退到 `text`** —— 上游这一条踩过一次坑（逐字抄在
///    下面 `append_message` 的注释里）；
/// 4. **媒体预算**（`MediaPendingSeconds`）原样带上：没有它，run 会在消息落地的**那一刻**触发，
///    而 agent 拿到的还是"[Image]"占位符、下载还在跑；
/// 5. **`BindMedia` 带上 issue 的四个字段**（`issue_id` / 描述基线 / 指令文本），使一条 `/issue`
///    轮次里的附件归 issue 而不是归引起它的那条聊天消息。
pub struct WeComSessionBinder {
    inner: Arc<dyn SessionBinder>,
}

impl std::fmt::Debug for WeComSessionBinder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WeComSessionBinder")
            .field("inner", &"<dyn SessionBinder>")
            .finish()
    }
}

impl WeComSessionBinder {
    /// 装配（上游 `NewResolverSet` 的 `session` 参数）。
    #[must_use]
    pub fn new(inner: Arc<dyn SessionBinder>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl SessionBinder for WeComSessionBinder {
    async fn ensure_session(&self, params: EnsureSessionParams) -> EngineResult<Id> {
        self.inner.ensure_session(params).await
    }

    async fn start_session(&self, params: StartSessionParams) -> EngineResult<StartSessionResult> {
        self.inner.start_session(params).await
    }

    async fn mark_pending_fresh(&self, session_id: Id, message_id: &str) -> EngineResult<()> {
        self.inner.mark_pending_fresh(session_id, message_id).await
    }

    async fn append_message(&self, params: AppendParams) -> EngineResult<AppendResult> {
        // adapter 自己的指令源**优先**，`text` 只是回落。用它覆盖（这里从前就是这么做的）会丢掉
        // adapter 已经算好的那行"剥掉提及之后的话"，于是群里 `/issue` 解析器拿到的是
        // "@Multica Bot /issue …"、看到的是一句散文。与 `lark/feishu_resolvers.rs` 和
        // `slack/resolvers.rs` 是同样两行。
        //
        // ⚠️ 本仓的形态差异（**不是**语义差异）：`AppendParams` 上只有 `message`，指令源由
        // `InboundMessage::command_source_text()`（`command_text` 为空时退到 `text`）表达 ——
        // 就是上游那三行做的事，只是判据落在类型上而不是落在这个方法里。
        self.inner.append_message(params).await
    }

    async fn bind_media(&self, params: BindMediaParams) -> EngineResult<BindMediaResult> {
        // 直到媒体解析器存在之前，这里"正确地"是个 no-op（没有东西可绑），而返回 `Ok` 在 Router
        // 眼里读成"绑好了" ⇒ 一旦媒体真的解析了，继续这样就等于每个附件都被下载、解密、存储，
        // 然后被**静默丢掉**。四个 issue 字段是让一条 `/issue` 轮次的附件归 issue 的那一半。
        self.inner.bind_media(params).await
    }
}

// =====================================================================
// 端口包
// =====================================================================

/// `WeCom` 的 [`ResolverSet`]（上游 `NewResolverSet` 的返回值）。
///
/// 上游在这里做两件"接口层面的"事，本仓用 `Option` 从类型上消灭了：
///
/// - `Typing` **留空**（`WeCom` 没有打字指示这一面）⇒ Router 把 `None` 当 no-op；
/// - `replier` 可选（传 `None` 关掉出站绑定提示），而"装在接口里的 `nil`"那个 Go 陷阱在 Rust 里
///   不存在。
///
/// `media` 可选：没有对象存储后端时传 `None`，入站附件降级成它们的占位文本。
pub struct WeComResolverSet {
    set: ResolverSet,
}

impl std::fmt::Debug for WeComResolverSet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WeComResolverSet")
            .field("inner", &self.set)
            .finish()
    }
}

impl WeComResolverSet {
    /// 用五个**必填**端口装配。
    #[must_use]
    pub fn new(
        installation: Arc<dyn InstallationResolver>,
        identity: Arc<dyn IdentityResolver>,
        dedup: Arc<dyn crate::engine::Deduper>,
        session: Arc<dyn SessionBinder>,
        audit: Arc<dyn crate::engine::Auditor>,
    ) -> Self {
        Self {
            set: ResolverSet::new(
                installation,
                identity,
                dedup,
                session,
                audit,
                ORIGIN_WECOM_CHAT,
            ),
        }
    }

    /// 生产形态：泛化渠道仓储 + 一个已经装配好的会话绑定端口。
    ///
    /// 去重与审计直接用 M7-2 的泛化实现（见模块文档的表）；`ChannelDeduper::generalized` 用的
    /// `channel_inbound_message_dedup` **正是**飞书 / slack 用的那张表，于是两阶段幂等的不变式在
    /// 各渠道上是**同一个**。
    #[must_use]
    pub fn from_repos(
        installations: ChannelInstallationRepo,
        bindings: ChannelBindingRepo,
        members: MemberRepo,
        dedup: mc_repos::channel::dedup::ChannelInboundDedupRepo,
        session: Arc<dyn SessionBinder>,
        audits: mc_repos::channel::inbound_audit::ChannelInboundAuditRepo,
    ) -> Self {
        Self::new(
            Arc::new(WeComInstallationResolver::from_repo(installations)),
            Arc::new(WeComIdentityResolver::from_repos(bindings, members)),
            Arc::new(ChannelDeduper::generalized(dedup, KIND)),
            session,
            Arc::new(ChannelAuditor::generalized(audits, KIND)),
        )
    }

    /// 挂上媒体解析面（M7-18 的 `WecomMediaResolver`）。
    #[must_use]
    pub fn with_media(mut self, media: Arc<dyn crate::engine::MediaResolver>) -> Self {
        self.set = self.set.with_media(media);
        self
    }

    /// 挂上出站回复器（M7-17 的 `WeComOutboundReplier`）。
    #[must_use]
    pub fn with_replier(mut self, replier: Arc<dyn crate::engine::OutboundReplier>) -> Self {
        self.set = self.set.with_replier(replier);
        self
    }

    /// 交给 `Router::register` 的那一份。
    #[must_use]
    pub fn into_engine_set(self) -> ResolverSet {
        self.set
    }

    /// 只读借用（诊断 / 用例）。
    #[must_use]
    pub fn engine_set(&self) -> &ResolverSet {
        &self.set
    }
}

#[cfg(test)]
mod tests;
