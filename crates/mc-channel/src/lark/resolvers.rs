//! lark 的解析器集合：安装路由 / 身份绑定 / 去重 / 会话 / 审计
//! （上游 `internal/integrations/lark/feishu_resolvers.go` 338 行）。
//!
//! - **写者**：M7-12（`docs/60-M7-PLAN.md` §3.3；本片的写集勘误见 `docs/32` §29）。
//! - **两套表并存，不得合并**（`docs/60` §6.4 / R-M7-5）：lark 的**安装行 / 用户绑定 /
//!   去重 / 审计**走**遗留** `lark_*` 表（上游 `ChannelStore` 就是那么写的），
//!   泛化 `channel_*` 表在别处并存 —— 所以本文件用的是
//!   [`ChannelInstallationRepo::find_lark_by_app_id`] / [`ChannelBindingRepo::find_lark_user_binding`] /
//!   [`ChannelDeduper::lark`] / [`ChannelAuditor::lark`]，**不是**它们的泛化同款。
//! - **端口形状**（与上游同款）：每个解析器只依赖一个**小接口**而不是仓储的具体类型 ——
//!   真仓储满足它，用例注入替身也满足它。于是"未绑定发件人 ⇒ 回绑定卡"这类判决
//!   不需要真库就能钉住。
//!
//! # 复用 engine 已有的通用实现（**不**重写）
//!
//! | 端口 | 实现 | 出处 |
//! | --- | --- | --- |
//! | `Deduper` | [`ChannelDeduper::lark`]（遗留去重表） | M7-2 `engine/session.rs` |
//! | `Auditor` | [`ChannelAuditor::lark`]（遗留审计表） | M7-2 `engine/session/audit.rs` |
//! | `SessionBinder` | [`LarkSessionBinder`]（包住 [`ChannelSessionBinder`]） | 本文件 |
//! | `InstallationResolver` | [`LarkInstallationResolver`] | 本文件 |
//! | `IdentityResolver` | [`LarkIdentityResolver`] | 本文件 |
//!
//! # 与上游的三处**形态 / 语义**差异（登记 `docs/32` §29 的 D 项）
//!
//! 1. **会话隔离键的分隔符**：上游是 `chat_id:话题 id`，本仓的通用隔离键策略
//!    （[`BindingKeyPolicy::ChatIdPlusThreadRoot`]）用 `#` 分隔。**隔离粒度完全一致**
//!    （一个群里两个 `@bot` 话题 = 两个会话），只是键的字面形态不同 —— 与 slack 的差异 1 同款。
//! 2. **绑定行的 `config` 列**：上游写 `{"chat_id": …}`（复合键下出站要知道真实 chat id），
//!    本仓的通用实现写 `null`；chat id 在隔离键的前缀里，读得回来（见差异 1）。
//! 3. **`lark_chat_session_binding`（遗留会话绑定面）本片只接线、不写行**：上游的
//!    `ChannelStore` 在**出站 / 存储面**（M7-13 的 `channel_store.rs`）维护它。本文件把
//!    [`LarkChatSessionBindingRepo`] 作为**可选**接线（[`LarkSessionBinder::with_lark_legacy`]）
//!    交出去，写行的责任因此明确地留在 M7-13 —— 不静默略过。
//!
//! # 本片**不**落的三件（都在 M7-13，逐条登记）
//!
//! - `feishuOutboundReplier` / `feishuTypingNotifier`（上游在本文件里，但它们分别调
//!   `OutcomeReplier` 与 `TypingIndicatorManager`，而那两个实现的写集是 M7-13 的
//!   `outcome_replier.go` / `typing_indicator.go`）⇒ 本片只给
//!   [`LarkResolverSet::with_replier`] / [`LarkResolverSet::with_typing`] 两个接线口；
//! - [`dispatch_result_from_engine`]（纯映射）照落，好让 M7-13 的回复器**只**写文案。

use std::sync::Arc;

use async_trait::async_trait;
use mc_core::channel::message::{ChatType, InboundMessage};
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_repos::channel::binding::{ChannelBindingRepo, LarkUserBindingRow};
use mc_repos::channel::dedup::LarkInboundDedupRepo;
use mc_repos::channel::inbound_audit::LarkInboundAuditRepo;
use mc_repos::channel::installation::{ChannelInstallationRepo, LarkInstallationRow};
use mc_repos::channel::session::{ChannelChatSessionRepo, LarkChatSessionBindingRepo};
use mc_repos::member::MemberRepo;
use mc_repos::RepoError;

use super::feishu_channel::LarkInboundMessage;
use super::types::{OpenId, Region};
use crate::engine::resolvers::{
    AppendParams, AppendResult, Auditor, BindMediaParams, BindMediaResult, Deduper, DropReason,
    EngineError, EngineResult, EnsureSessionParams, IdentityResolver, InstallationResolver,
    MediaResolver, OutboundReplier, PipelineError, ResolvedIdentity, ResolvedInstallation,
    RouteResult, SessionBinder, StartSessionParams, StartSessionResult, TypingNotifier,
};
use crate::engine::session::{
    BindingKeyPolicy, ChannelAuditor, ChannelDeduper, ChannelSessionBinder, SessionBinderConfig,
};

/// 本 adapter 的平台判别式（诊断用）。
pub const TYPE_LARK: ChannelKind = ChannelKind::Lark;

/// `/issue` 的 `origin_type`（**逐字** `lark_chat`，上游 `originFeishuChat`）。
///
/// 上游注释逐字：保持 `lark_chat` **不变**（不随 cutover 改名），否则分析口径会漂移。
pub const ORIGIN_LARK_CHAT: &str = "lark_chat";

// =====================================================================
// adapter 自己的安装值（不透明；Router 只搬运）
// =====================================================================

/// 遗留 `lark_installation` 行的投影（上游 `Installation`）。
///
/// ⚠️ 它与泛化 `channel_installation` 行**并存**：`app_secret_encrypted` 在这里是 `BYTEA`
/// （**裸密文**），而在泛化行里是 `config` JSON 里的 base64 字符串 —— 两套形态**不通用**。
///
/// [`Debug`] 是**手写**的：`app_secret_encrypted` 只报长度（`docs/60` §2.3 第 1 条）。
#[derive(Clone, PartialEq, Eq)]
pub struct LarkInstallation {
    pub id: Id,
    pub workspace_id: Id,
    pub agent_id: Id,
    pub app_id: String,
    /// `secretbox` 密文（`nonce(12) ‖ ct ‖ tag`）；**只有** [`super::feishu_channel`] 的解密器读它。
    pub app_secret_encrypted: Vec<u8>,
    pub tenant_key: Option<String>,
    /// Bot 的按安装 `open_id`（入站提及比对用）。
    pub bot_open_id: OpenId,
    /// Bot 的跨应用稳定 `union_id`；`None` / 空 = **未回填**（见模块文档与 `docs/32` §29 的 R 项）。
    pub bot_union_id: Option<String>,
    /// 存库的 region 字串（`lark_installation.region`，迁移 `116` 的 `CHECK` 只认两个值）。
    pub region: Region,
    pub installer_user_id: Id,
    /// `active` / `revoked`。
    pub status: String,
}

impl std::fmt::Debug for LarkInstallation {
    /// 手写脱敏：密文只报**长度**（诊断要能看出"配没配 / 多长"，不需要值）。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LarkInstallation")
            .field("id", &self.id)
            .field("workspace_id", &self.workspace_id)
            .field("agent_id", &self.agent_id)
            .field("app_id", &self.app_id)
            .field("app_secret_encrypted_len", &self.app_secret_encrypted.len())
            .field("tenant_key", &self.tenant_key)
            .field("bot_open_id", &self.bot_open_id)
            .field("has_bot_union_id", &self.bot_union_id.is_some())
            .field("region", &self.region)
            .field("installer_user_id", &self.installer_user_id)
            .field("status", &self.status)
            .finish()
    }
}

impl LarkInstallation {
    /// 是否还能承载消息（`active`）。
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.status == "active"
    }

    /// Bot 的 `union_id`（空串 = 未回填）—— [`super::content_flatten::contains_mention`] 的口。
    #[must_use]
    pub fn bot_union_id_or_empty(&self) -> &str {
        self.bot_union_id.as_deref().unwrap_or("")
    }
}

impl From<&LarkInstallationRow> for LarkInstallation {
    fn from(row: &LarkInstallationRow) -> Self {
        Self {
            id: row.id(),
            workspace_id: Id(row.workspace_id),
            agent_id: Id(row.agent_id),
            app_id: row.app_id.clone(),
            app_secret_encrypted: row.app_secret_encrypted.clone(),
            tenant_key: row.tenant_key.clone(),
            bot_open_id: OpenId::new(row.bot_open_id.clone()),
            bot_union_id: row.bot_union_id.clone(),
            region: Region::or_default(&row.region),
            installer_user_id: Id(row.installer_user_id),
            status: row.status.clone(),
        }
    }
}

/// 从 `raw` 解出本 adapter 自己的入站载荷（解不开 ⇒ 基础设施失败：`raw` 是本片自己写的）。
///
/// 与 slack 的 `decode_raw` 同形：`raw` 空 / 不是本片的形状 ⇒ `Infra`（不是产品性丢弃）。
pub fn decode_raw(message: &InboundMessage) -> EngineResult<LarkInboundMessage> {
    if message.raw.is_null() {
        return Err(EngineError::infra("lark: inbound message raw is empty"));
    }
    serde_json::from_value::<LarkInboundMessage>(message.raw.clone())
        .map_err(|error| EngineError::infra(format!("lark: decode inbound raw: {error}")))
}

// =====================================================================
// 安装路由
// =====================================================================

/// 安装行查询接缝（上游 `*db.Queries` 的那一条语句）。
#[async_trait]
pub trait InstallationQueries: Send + Sync {
    /// 按真实 `app_id` 查**活跃的遗留**安装行（`lark_installation.app_id`）。
    async fn find_active_by_app_id(
        &self,
        app_id: &str,
    ) -> Result<Option<LarkInstallation>, RepoError>;
}

#[async_trait]
impl InstallationQueries for ChannelInstallationRepo {
    async fn find_active_by_app_id(
        &self,
        app_id: &str,
    ) -> Result<Option<LarkInstallation>, RepoError> {
        self.find_lark_by_app_id(app_id)
            .await
            .map(|row| row.as_ref().map(LarkInstallation::from))
    }
}

/// 安装路由（上游 `feishuInstallationResolver`）。
pub struct LarkInstallationResolver {
    queries: Arc<dyn InstallationQueries>,
}

impl std::fmt::Debug for LarkInstallationResolver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LarkInstallationResolver")
            .field("queries", &"<dyn InstallationQueries>")
            .finish()
    }
}

impl LarkInstallationResolver {
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
impl InstallationResolver for LarkInstallationResolver {
    async fn resolve_installation(
        &self,
        message: &InboundMessage,
    ) -> EngineResult<ResolvedInstallation> {
        let raw = decode_raw(message)?;
        let found = self
            .queries
            .find_active_by_app_id(&raw.app_id)
            .await
            .map_err(|error| EngineError::infra(error.to_string()))?;
        let Some(installation) = found else {
            // 认不出的 app id：产品性丢弃（`invalid_event`），**不是**错误。
            return Err(PipelineError::InstallationNotFound.into());
        };
        let active = installation.is_active();
        Ok(ResolvedInstallation {
            id: installation.id,
            workspace_id: installation.workspace_id,
            agent_id: installation.agent_id,
            installer_user_id: installation.installer_user_id,
            active,
            kind: TYPE_LARK,
            platform: Some(Arc::new(installation)),
        })
    }
}

/// 从 `ResolvedInstallation` 取回本 adapter 的安装值（媒体面 / 出站面都要它）。
///
/// `None` = 这条 `ResolvedInstallation` 不是本 adapter 造的（纯出站路径 / 用例）；
/// 调用方按"降级跳过"处理，**不要**猜。
#[must_use]
pub fn platform_installation(installation: &ResolvedInstallation) -> Option<&LarkInstallation> {
    installation
        .platform
        .as_ref()
        .and_then(|platform| platform.downcast_ref::<LarkInstallation>())
}

// =====================================================================
// 身份绑定
// =====================================================================

/// 身份查询接缝（上游 `identityQueries`）。
#[async_trait]
pub trait IdentityQueries: Send + Sync {
    /// `(installation, lark open_id)` 上的**遗留**绑定行。
    async fn find_user_binding(
        &self,
        installation_id: Id,
        lark_open_id: &str,
    ) -> Result<Option<LarkUserBindingRow>, RepoError>;

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
        lark_open_id: &str,
    ) -> Result<Option<LarkUserBindingRow>, RepoError> {
        self.bindings
            .find_lark_user_binding(installation_id, lark_open_id)
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

/// 身份解析（上游 `feishuIdentityResolver`）。
pub struct LarkIdentityResolver {
    queries: Arc<dyn IdentityQueries>,
}

impl std::fmt::Debug for LarkIdentityResolver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LarkIdentityResolver")
            .field("queries", &"<dyn IdentityQueries>")
            .finish()
    }
}

impl LarkIdentityResolver {
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
impl IdentityResolver for LarkIdentityResolver {
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
            .map_err(|error| EngineError::infra(error.to_string()))?;
        let Some(binding) = binding else {
            // 没绑定 ⇒ 产品性判决（Router 会驱动绑定卡），**不是**错误。
            return Err(PipelineError::SenderUnbound.into());
        };
        // 绑定行的存在**不再**证明成员资格（遗留绑定表也没有 member 外键）⇒ 重新校验。
        let member = self
            .queries
            .is_workspace_member(installation.workspace_id, Id(binding.multica_user_id))
            .await
            .map_err(|error| EngineError::infra(error.to_string()))?;
        if !member {
            return Err(PipelineError::SenderNotMember.into());
        }
        Ok(ResolvedIdentity {
            user_id: Id(binding.multica_user_id),
        })
    }
}

// =====================================================================
// 会话路由
// =====================================================================

/// 一条入站 lark 消息的会话路由（上游 `larkSessionRouting` 的**判决**部分）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRouting {
    /// 会话隔离键（存 `channel_chat_id`）。
    pub binding_key: String,
}

/// 从一条入站消息算出会话隔离键（上游 `larkSessionRouting`，纯函数）。
///
/// 上游注释逐字：p2p 或普通群聊是"每个会话一条连续会话"⇒ 键就是 chat id；
/// **话题（`thread_id`）里的消息按话题隔离** ⇒ 键是 `chat:话题`，于是同一个群里两个
/// `@bot` 话题是**两个**会话（与 slack 的 `channel:threadRoot` 同一个模型）。
///
/// ⚠️ 键的字面分隔符在本仓由 [`BindingKeyPolicy::ChatIdPlusThreadRoot`] 用 `#` 生成
/// （见模块文档差异 1）；本函数给出的是**上游形态**的键，供诊断与用例逐字比对。
#[must_use]
pub fn session_routing(message: &InboundMessage) -> SessionRouting {
    let chat_id = message.source.chat_id.as_str();
    if message.source.chat_type != ChatType::Group || message.source.thread_id.is_empty() {
        return SessionRouting {
            binding_key: chat_id.to_string(),
        };
    }
    SessionRouting {
        binding_key: format!("{}:{}", chat_id, message.source.thread_id),
    }
}

/// 会话绑定端口（上游 `feishuSessionBinder`）：把 lark 的路由判决喂给 engine 的共享会话组件。
///
/// 见模块文档差异 1 / 3：隔离键走 [`BindingKeyPolicy::ChatIdPlusThreadRoot`]（`#` 分隔），
/// 遗留 `lark_chat_session_binding` 只接线、不写行（写行归 M7-13 的 `channel_store.rs`）。
pub struct LarkSessionBinder {
    inner: Arc<ChannelSessionBinder>,
}

impl std::fmt::Debug for LarkSessionBinder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LarkSessionBinder")
            .field("inner", &self.inner)
            .finish()
    }
}

impl LarkSessionBinder {
    /// 装配（会话隔离键 = `chat_id#话题 id`；话题化平台的正确形态）。
    #[must_use]
    pub fn new(repo: Arc<ChannelChatSessionRepo>) -> Self {
        Self::build(repo, None)
    }

    /// 装配并接上 lark **遗留**会话绑定面（`lark_chat_session_binding`）。
    ///
    /// 通用 binder 目前**只持有**它（`lark_legacy()` 取出口），不写行 —— 上游在出站 /
    /// 存储面维护那些行（M7-13 的 `channel_store.rs`）。本片把它接线，责任因此显式落在 M7-13。
    #[must_use]
    pub fn new_with_legacy(
        repo: Arc<ChannelChatSessionRepo>,
        legacy: Arc<LarkChatSessionBindingRepo>,
    ) -> Self {
        Self::build(repo, Some(legacy))
    }

    fn build(
        repo: Arc<ChannelChatSessionRepo>,
        legacy: Option<Arc<LarkChatSessionBindingRepo>>,
    ) -> Self {
        let mut inner = ChannelSessionBinder::new(
            repo,
            TYPE_LARK,
            SessionBinderConfig {
                binding_key: BindingKeyPolicy::ChatIdPlusThreadRoot,
            },
        );
        if let Some(legacy) = legacy {
            inner = inner.with_lark_legacy(legacy);
        }
        Self {
            inner: Arc::new(inner),
        }
    }

    /// 共享会话组件（诊断 / 遗留面取出口）。
    #[must_use]
    pub fn inner(&self) -> &Arc<ChannelSessionBinder> {
        &self.inner
    }
}

#[async_trait]
impl SessionBinder for LarkSessionBinder {
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
        self.inner.append_message(params).await
    }

    async fn bind_media(&self, params: BindMediaParams) -> EngineResult<BindMediaResult> {
        // 媒体绑定不读路由（上游同形）：`BindMediaInput` 用的是会话与消息 id。
        self.inner.bind_media(params).await
    }
}

// =====================================================================
// 判决 → adapter 自己的出站值（上游 `dispatchResultFromEngine`）
// =====================================================================

/// 流水线判决的 lark 侧类别（上游 `Outcome`）。
///
/// 取值与 `engine::Outcome` **1:1 逐字**（`DispatchResult` 的消费方 M7-13 按它选文案）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Outcome {
    /// 没摄取（身份 / 去重 / 群过滤…）。
    #[default]
    Dropped,
    /// `open_id` 未绑定 ⇒ 发绑定卡。
    NeedsBinding,
    /// 消息落地，run 已（或将要）入队。
    Ingested,
    /// 一个裸 `/clear` 已为下一个 chat 轮次持久化。
    FreshPending,
    /// 新 chat 开始。
    ChatStarted,
    /// `/issue` 少了必需的标题。
    IssueUsage,
    /// 落地了，但 agent 没有绑定的运行时。
    AgentOffline,
    /// 落地了，但 agent 已归档。
    AgentArchived,
}

impl Outcome {
    /// `engine::Outcome` → 本枚举（未知取值**失败关闭**成 [`Outcome::Dropped`]：
    /// 一个认不出的判决不该产生任何出站回复）。
    #[must_use]
    pub fn from_engine(outcome: &crate::engine::resolvers::Outcome) -> Self {
        use crate::engine::resolvers::Outcome as EngineOutcome;
        match outcome {
            EngineOutcome::Dropped => Self::Dropped,
            EngineOutcome::NeedsBinding => Self::NeedsBinding,
            EngineOutcome::Ingested => Self::Ingested,
            EngineOutcome::FreshPending => Self::FreshPending,
            EngineOutcome::ChatStarted => Self::ChatStarted,
            EngineOutcome::IssueUsage => Self::IssueUsage,
            EngineOutcome::AgentOffline => Self::AgentOffline,
            EngineOutcome::AgentArchived => Self::AgentArchived,
        }
    }
}

/// 出站回复器消费的 lark 侧判决（上游 `DispatchResult`）。
///
/// [`Default`] 的 `outcome` 是 `Dropped` ⇒ 一个"什么都没发生"的判决**不会**触发回复。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct DispatchResult {
    pub outcome: Outcome,
    pub drop_reason: Option<DropReason>,
    pub installation_id: Option<Id>,
    pub chat_session_id: Option<Id>,
    pub channel_binding_id: Option<Id>,
    /// 平台原生发件人 id —— 绑定卡要发回给这个人。
    pub sender_open_id: String,
    /// `/issue` 建出来的 issue（没走命令路径时为 `None`）。
    pub issue_id: Option<Id>,
    pub issue_number: i64,
    /// workspace 限定的键（`MUL-42`），确认消息逐字用它。
    pub issue_identifier: String,
    /// 深链要用的 workspace 路由段。
    pub issue_workspace_slug: String,
    /// `/issue` 上给的标题，原样回显在确认消息里。
    pub issue_title: String,
    /// 区分"活跃 issue 冲突"与"创建成功"（上面那些 issue 字段照带）。
    pub issue_duplicate: bool,
    /// 用量回复要提示发件人"把这条消息的媒体带上、命令改对"。
    pub issue_usage_had_media: bool,
}

/// engine 判决 → lark 侧判决（上游 `dispatchResultFromEngine`）。
///
/// 纯函数：**没有** DB、没有网络 ⇒ M7-13 的回复器只写文案，判决映射在本片被钉住。
#[must_use]
pub fn dispatch_result_from_engine(result: &RouteResult) -> DispatchResult {
    DispatchResult {
        outcome: Outcome::from_engine(&result.outcome),
        drop_reason: result.drop_reason,
        installation_id: result.installation_id,
        chat_session_id: result.chat_session_id,
        channel_binding_id: result.channel_binding_id,
        sender_open_id: result.sender.clone(),
        issue_id: result.issue.as_ref().map(|issue| issue.id),
        issue_number: result.issue.as_ref().map_or(0, |issue| issue.number),
        issue_identifier: result.issue_identifier.clone(),
        issue_workspace_slug: result.issue_workspace_slug.clone(),
        issue_title: result
            .issue
            .as_ref()
            .map_or_else(String::new, |issue| issue.title.clone()),
        issue_duplicate: result.issue_duplicate,
        issue_usage_had_media: result.issue_usage_had_media,
    }
}

// =====================================================================
// 解析器集合
// =====================================================================

/// 五个必填端口 + 三个可选端口（媒体 / 出站回复器 / 打字指示）。
///
/// 可选端口的落地分片：媒体 = 本片（[`super::media`]）；回复器与打字指示 = **M7-13**。
pub struct LarkResolverSet {
    pub installation: Arc<dyn InstallationResolver>,
    pub identity: Arc<dyn IdentityResolver>,
    pub dedup: Arc<dyn Deduper>,
    pub session: Arc<dyn SessionBinder>,
    pub audit: Arc<dyn Auditor>,
    pub media: Option<Arc<dyn MediaResolver>>,
    pub replier: Option<Arc<dyn OutboundReplier>>,
    pub typing: Option<Arc<dyn TypingNotifier>>,
}

impl std::fmt::Debug for LarkResolverSet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LarkResolverSet")
            .field("media", &self.media.is_some())
            .field("replier", &self.replier.is_some())
            .field("typing", &self.typing.is_some())
            .finish_non_exhaustive()
    }
}

impl LarkResolverSet {
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

    /// 从**遗留 lark 仓储 + 两个泛化仓储**装配（不含媒体 / 出站 / 打字：那三面各自注入）。
    #[must_use]
    pub fn from_repos(
        installations: ChannelInstallationRepo,
        bindings: ChannelBindingRepo,
        members: MemberRepo,
        dedup: LarkInboundDedupRepo,
        sessions: Arc<ChannelChatSessionRepo>,
        audits: LarkInboundAuditRepo,
    ) -> Self {
        Self::new(
            Arc::new(LarkInstallationResolver::from_repo(installations)),
            Arc::new(LarkIdentityResolver::from_repos(bindings, members)),
            Arc::new(ChannelDeduper::lark(dedup)),
            Arc::new(LarkSessionBinder::new(sessions)),
            Arc::new(ChannelAuditor::lark(audits)),
        )
    }

    /// 挂上媒体面（本片的 [`super::media`]；`None` = 该部署不做媒体）。
    #[must_use]
    pub fn with_media(mut self, media: Arc<dyn MediaResolver>) -> Self {
        self.media = Some(media);
        self
    }

    /// 挂上出站回复器（**M7-13**）。
    #[must_use]
    pub fn with_replier(mut self, replier: Arc<dyn OutboundReplier>) -> Self {
        self.replier = Some(replier);
        self
    }

    /// 挂上打字指示器（**M7-13**）。
    #[must_use]
    pub fn with_typing(mut self, typing: Arc<dyn TypingNotifier>) -> Self {
        self.typing = Some(typing);
        self
    }

    /// 交给 engine 的 [`crate::engine::resolvers::ResolverSet`]（`origin_type = lark_chat`）。
    #[must_use]
    pub fn into_engine_set(self) -> crate::engine::resolvers::ResolverSet {
        let mut set = crate::engine::resolvers::ResolverSet::new(
            self.installation,
            self.identity,
            self.dedup,
            self.session,
            self.audit,
            ORIGIN_LARK_CHAT,
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

/// `/issue` 的 `origin_type`（**逐字** `lark_chat`）。
#[must_use]
pub fn origin_type() -> &'static str {
    ORIGIN_LARK_CHAT
}

/// 本 adapter 的平台判别式（诊断用）。
#[must_use]
pub fn kind() -> ChannelKind {
    TYPE_LARK
}

#[cfg(test)]
mod tests;
