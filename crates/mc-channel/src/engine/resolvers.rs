//! 端口与流水线词表：engine 跑入站流水线要用的**全部**接缝 + 判决词表。
//!
//! - **写者**：M7-0 建（anchor 只落 `ResolverSet` 的雏形）；**M7-1 重写**（`docs/60`
//!   §3.3 的写集表：`engine/resolvers.rs` 归 M7-1）。
//! - **上游**：`server/internal/integrations/channel/engine/resolvers.go`（433 行，
//!   `M7-1` 的上游文件之一）。逐条对应见下表。
//!
//! **平台专有的一切都在这些 trait 后面**：Router 只认 `ResolverSet`（一台平台一组端口），
//! 核心永远不长出平台分支。这份词表是 M7-2…M7-20 全部切片的**共同语言** ⇒ 必须一次落准，
//! 别让各片各写一份"什么算命中 / 什么算丢弃"。
//!
//! # 上游 → 本文件
//!
//! | 上游 | 本文件 | 备注 |
//! | --- | --- | --- |
//! | `Outcome` / `DropReason` | [`Outcome`] / [`DropReason`] | 取值**逐字**对齐（值班看板按它聚合） |
//! | `Result` | [`RouteResult`] | 改名的原因：`Result` 在 Rust 里是 `std` 的别名，同文件内会互相遮蔽 |
//! | `ResolvedInstallation` / `ResolvedIdentity` | 同名 | `Platform any` → [`ResolvedInstallation::platform`]（`Arc<dyn Any>`） |
//! | `ResolverSet` | [`ResolverSet`] | 结构体（不是 trait）：一台平台一组端口 |
//! | `ErrInstallationNotFound` … `ErrClaimLost` | [`PipelineError`] | 上游用 `errors.Is` 的哨兵；Rust 侧用**枚举变体**（可穷举、可 `match`） |
//! | `InstallationResolver` … `SessionReader` | 同名的 `trait` | 全部 `async_trait`；实现分散在 M7-2…M7-20 的各自写集 |
//! | `NewDBMediaIntentLedger` | **不落** | 它适配上游的 `db.Queries`；本仓的实现在 `mc_repos::channel::media`，由 adapter 片接上（M7-18） |
//!
//! # 三条纪律（逐条可测）
//!
//! 1. **engine 不知道平台**：本文件**不得** `use` 任何 `slack` / `lark` / `dingtalk` /
//!    `wecom` / `telegram` 类型；反向同理，adapter 只拿 [`ResolverSet`] 里注入的 `Arc<dyn …>`。
//!    `engine/mod.rs` 的测试用"源码扫描"钉住这一条。
//! 2. **凭据不进 `Debug`/日志**：本文件的结构只有 id / 词表，**没有**密钥字段；
//!    [`ResolvedInstallation::platform`] 的 `Debug` 手写成 `<opaque>`。
//! 3. **判决不是错误**：丢弃是 `RouteResult` 里的 `Outcome`，**不是** `Err`。

use std::any::Any;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use mc_core::channel::message::{InboundMessage, MediaRef};
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_core::timestamp::Timestamp;

use crate::channel::ChannelError;

// =====================================================================
// 错误：哨兵（上游 `errors.Is` 的那批）→ 枚举变体
// =====================================================================

/// 流水线的**产品性**判决（上游那批哨兵错误）。
///
/// 上游用 `errors.Is(err, ErrSenderUnbound)` 把产品结果从基础设施失败里分出来；
/// Rust 没有哨兵，用**可穷举的枚举**表达同一件事：Router `match` 它决定 Outcome / `DropReason`，
/// **从不**把它当 `Err` 抛给 adapter。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PipelineError {
    /// 没有任何安装匹配这条消息的路由键 ⇒ `invalid_event` 丢弃（上游 `ErrInstallationNotFound`）。
    #[error("engine: installation not found")]
    InstallationNotFound,
    /// 发件人没有身份绑定 ⇒ `needs_binding`（**不是**错误，上游 `ErrSenderUnbound`）。
    #[error("engine: sender unbound")]
    SenderUnbound,
    /// 已绑定但不是 workspace 成员 ⇒ `non_workspace_member` 丢弃（上游 `ErrSenderNotMember`）。
    #[error("engine: sender not a workspace member")]
    SenderNotMember,
    /// 平台路由在两次读取之间变了 ⇒ Router 重新解析并重试（上游 `ErrRouteChanged`）。
    #[error("engine: route changed")]
    RouteChanged,
    /// 这条消息已经处理过 / 正在处理 ⇒ `duplicate` 丢弃（上游 `ErrDuplicate`）。
    #[error("engine: duplicate message")]
    Duplicate,
    /// 去重所有权在飞行中被别的 worker 抢走 ⇒ 等价于 `duplicate`（上游 `ErrClaimLost`）。
    #[error("engine: dedup claim lost")]
    ClaimLost,
    /// 长连接租约在别处（另一副本 / 同进程前一个任务持着）⇒ supervisor **不连**，
    /// 等下一轮 sweep（上游 `ErrLeaseNotAcquired`；R-M7-1 的进程内替身同语义）。
    #[error("engine: ws lease held elsewhere")]
    LeaseNotAcquired,
}

impl PipelineError {
    /// 稳定码（诊断 / 看板聚合用；与 `docs/60` 的判决词表一致）。
    pub fn code(&self) -> &'static str {
        match self {
            Self::InstallationNotFound => "installation_not_found",
            Self::SenderUnbound => "sender_unbound",
            Self::SenderNotMember => "sender_not_member",
            Self::RouteChanged => "route_changed",
            Self::Duplicate => "duplicate",
            Self::ClaimLost => "claim_lost",
            Self::LeaseNotAcquired => "lease_not_acquired",
        }
    }

    /// 这个判决对应的丢弃原因；`None` = 不是丢弃（`needs_binding` / 路由重试）。
    pub fn drop_reason(&self) -> Option<DropReason> {
        match self {
            Self::InstallationNotFound => Some(DropReason::InvalidEvent),
            Self::SenderNotMember => Some(DropReason::NonWorkspaceMember),
            Self::Duplicate | Self::ClaimLost => Some(DropReason::Duplicate),
            Self::SenderUnbound | Self::RouteChanged | Self::LeaseNotAcquired => None,
        }
    }
}

/// engine 的错误面：产品判决 + 基础设施失败 + 链路错误。
///
/// `From<PipelineError>` / `From<ChannelError>` 让各端口实现可以用 `?` 直接抛哨兵。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    /// 产品性判决（见 [`PipelineError`]）。
    #[error(transparent)]
    Pipeline(#[from] PipelineError),
    /// 基础设施失败（DB 挂了、dispatcher 配错…）⇒ adapter 按"投递失败"处理。
    #[error("engine: infrastructure failure: {message}")]
    Infra { message: String },
    /// 链路 / 注册表层的错误（`channel.rs` 的词表）。
    #[error(transparent)]
    Channel(#[from] ChannelError),
}

impl EngineError {
    /// 便捷构造（各端口的 `map_err` 落点）。
    pub fn infra(message: impl Into<String>) -> Self {
        Self::Infra {
            message: message.into(),
        }
    }

    /// 稳定码（诊断用）：判决走 [`PipelineError::code`]，其余两类各有前缀。
    pub fn code_hint(&self) -> &'static str {
        match self {
            Self::Pipeline(error) => error.code(),
            Self::Infra { .. } => "engine_infra_error",
            Self::Channel(error) => error.code(),
        }
    }

    /// 映射回 [`ChannelError`]（`InboundHandler::handle` 的返回类型只有那一套词表）。
    ///
    /// 产品判决在 Router 里**已经**被消费成 `RouteResult`，走到这里说明是基础设施问题。
    pub fn into_channel_error(self) -> ChannelError {
        match self {
            Self::Channel(error) => error,
            Self::Pipeline(error) => ChannelError::Storage {
                message: error.to_string(),
            },
            Self::Infra { message } => ChannelError::Storage { message },
        }
    }
}

/// `Result` 别名（engine 内部）。
pub type EngineResult<T> = std::result::Result<T, EngineError>;

// =====================================================================
// 判决词表（上游 `Outcome` / `DropReason`）
// =====================================================================

/// Router 对一条入站消息做了什么（上游 `Outcome`，取值**逐字**对齐）。
#[allow(clippy::doc_markdown)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Outcome {
    /// 被丢弃（原因见 [`RouteResult::drop_reason`]）。
    #[default]
    Dropped,
    /// 发件人未绑定 ⇒ 驱动绑定卡（**不是**错误）。
    NeedsBinding,
    /// 已入库（`chat_message` 落了行），run 触发可能还在去抖窗口里。
    Ingested,
    /// 裸 `/clear`：只记下"待开新会话"，没有正文入库。
    FreshPending,
    /// `/new`：轮换了会话路由并开了新 Chat。
    ChatStarted,
    /// `/issue` 缺标题：回一条用法提示。
    IssueUsage,
    /// agent 没有可用 runtime（离线）。
    AgentOffline,
    /// agent 已归档。
    AgentArchived,
}

impl Outcome {
    /// 稳定字符串（日志 / 看板；与上游 `Outcome` 的 wire 取值逐字一致）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dropped => "dropped",
            Self::NeedsBinding => "needs_binding",
            Self::Ingested => "ingested",
            Self::FreshPending => "fresh_pending",
            Self::ChatStarted => "chat_started",
            Self::IssueUsage => "issue_usage",
            Self::AgentOffline => "agent_offline",
            Self::AgentArchived => "agent_archived",
        }
    }
}

/// 丢弃审计的分类（上游 `DropReason`，取值**逐字**对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DropReason {
    /// 发件人没有绑定。
    UnboundUser,
    /// 发件人已绑定但不是 workspace 成员。
    NonWorkspaceMember,
    /// 群聊里这条消息没有 @bot / 不是回复 bot。
    NotAddressedInGroup,
    /// 去重命中（重连重投）。
    Duplicate,
    /// 安装已撤销。
    RevokedInstallation,
    /// 事件本身无效（没有匹配的安装行）。
    InvalidEvent,
}

impl DropReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnboundUser => "unbound_user",
            Self::NonWorkspaceMember => "non_workspace_member",
            Self::NotAddressedInGroup => "not_addressed_in_group",
            Self::Duplicate => "duplicate",
            Self::RevokedInstallation => "revoked_installation",
            Self::InvalidEvent => "invalid_event",
        }
    }
}

/// 一条入站消息的路由结论（上游 `Result`；改名成 [`RouteResult`]，见模块文档）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RouteResult {
    pub outcome: Outcome,
    pub drop_reason: Option<DropReason>,
    pub installation_id: Option<Id>,
    pub chat_session_id: Option<Id>,
    pub channel_binding_id: Option<Id>,
    pub channel_route_revision: i64,
    /// 平台原生发件人 id —— 出站回复器用它把绑定卡发回给这个人。
    pub sender: String,
    /// `/issue` 命令的产物（没走命令路径时为 `None`）。
    pub issue: Option<ChannelIssue>,
    /// 渲染好的 issue 标识符（`ABC-42`；没有前缀时降级成 `#42`）。
    pub issue_identifier: String,
    /// workspace slug（Web 深链要它；空 = 查不到身份，回复里不带链接）。
    pub issue_workspace_slug: String,
    /// `/issue` 因重复守卫**没有**建新行（见 [`ChannelIssue`]）。
    pub issue_duplicate: bool,
    /// `/issue` 因重复守卫没有建新 issue（见 [`ChannelIssue::duplicate`]）。
    pub issue_usage_had_media: bool,
    /// Router 内部状态：这次入库是否排了一次普通 chat run（**回复器只读 `outcome`**）。
    pub run_scheduled: bool,
}

impl RouteResult {
    /// 丢弃结论的构造（`installation_id` 允许为 `None`：安装都没解析出来的事件）。
    pub fn dropped(reason: DropReason, installation_id: Option<Id>) -> Self {
        Self {
            outcome: Outcome::Dropped,
            drop_reason: Some(reason),
            installation_id,
            ..Self::default()
        }
    }

    /// 是否被丢弃。
    pub fn is_dropped(&self) -> bool {
        matches!(self.outcome, Outcome::Dropped)
    }
}

// =====================================================================
// 解析结果（上游 `ResolvedInstallation` / `ResolvedIdentity`）
// =====================================================================

/// 路由到的安装上下文 —— Router 需要的最小集合（上游 `ResolvedInstallation`）。
///
/// `platform` 是 adapter 自己的安装值（**不透明**）：同一组的其它端口（replier / typing）
/// 可以复用它免得再查一次库；Router **从不**读它。
#[derive(Clone)]
pub struct ResolvedInstallation {
    pub id: Id,
    pub workspace_id: Id,
    pub agent_id: Id,
    pub installer_user_id: Id,
    /// 是否仍然活跃（`status = 'active'`）；`false` ⇒ `revoked_installation` 丢弃。
    pub active: bool,
    /// 平台判别式（= 注册 `ResolverSet` 时用的 kind）。
    pub kind: ChannelKind,
    /// adapter 自己的安装值；Router 只搬运。`Debug` 输出 `<opaque>`。
    pub platform: Option<Arc<dyn Any + Send + Sync>>,
}

impl fmt::Debug for ResolvedInstallation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedInstallation")
            .field("id", &self.id)
            .field("workspace_id", &self.workspace_id)
            .field("agent_id", &self.agent_id)
            .field("installer_user_id", &self.installer_user_id)
            .field("active", &self.active)
            .field("kind", &self.kind)
            // 不透明：别把平台安装值（可能含凭据）打进日志。
            .field("platform", &self.platform.as_ref().map(|_| "<opaque>"))
            .finish()
    }
}

impl ResolvedInstallation {
    /// 不带平台值的构造（测试与纯出站路径用）。
    pub fn new(
        id: Id,
        workspace_id: Id,
        agent_id: Id,
        installer_user_id: Id,
        kind: ChannelKind,
        active: bool,
    ) -> Self {
        Self {
            id,
            workspace_id,
            agent_id,
            installer_user_id,
            active,
            kind,
            platform: None,
        }
    }
}

/// 发件人映射到的 Multica 用户（上游 `ResolvedIdentity`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedIdentity {
    pub user_id: Id,
}

// =====================================================================
// 会话 / 媒体 / 运行 参数（上游同名结构）
// =====================================================================

/// `SessionBinder::ensure_session` 的入参（上游 `EnsureSessionParams`）。
///
/// `sender` 是**会话创建者**：p2p 就是那个人，群聊是安装者（Router 决定并在这里传）。
#[derive(Debug, Clone)]
pub struct EnsureSessionParams {
    pub installation: ResolvedInstallation,
    pub sender: Id,
    pub message: InboundMessage,
}

/// `/new` 的会话轮换入参（上游 `StartSessionParams`）。
#[derive(Debug, Clone)]
pub struct StartSessionParams {
    pub installation: ResolvedInstallation,
    pub creator: Id,
    pub sender: Id,
    pub message: InboundMessage,
    pub claim_token: Option<Id>,
    pub media_pending_seconds: f64,
    pub persist_message: bool,
}

/// 会话轮换的结果（上游 `StartSessionResult`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartSessionResult {
    pub session_id: Id,
    pub binding_id: Option<Id>,
    pub route_revision: i64,
    pub append: AppendResult,
}

/// 追加一条用户消息的入参（上游 `AppendParams`）。
///
/// `claim_token` 是去重所有权围栏：binder 把 dedup 的 Mark **放进**自己的
/// `chat_message` + session 事务里，让持久化与 Mark 原子提交。
#[derive(Debug, Clone)]
pub struct AppendParams {
    pub session_id: Id,
    pub sender: Id,
    pub installation_id: Id,
    pub message: InboundMessage,
    pub claim_token: Option<Id>,
    pub media_pending_seconds: f64,
}

/// 追加结果（上游 `AppendResult`）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AppendResult {
    /// 落下的 `chat_message` 行（挂附件时用）。
    pub message_id: Option<Id>,
    /// `/issue` 命令（非命令路径为 `None`）。
    pub issue_command: Option<ChannelIssueCommand>,
    /// binder 在自己的事务里 Mark 了去重行 ⇒ Router 跳过流水线后的 finalize。
    pub dedup_marked: bool,
    /// 这条消息拿到的**持久化上下文代际**（去抖键与任务快照按它分界）。
    pub context_revision: i64,
    pub pending_contexts: Vec<PendingContext>,
    /// 首个标题（绑定媒体后才落时用得上）。
    pub initial_title: String,
    /// 这次提交是否把一个隐式渠道会话变成了公开 Chat。
    pub became_visible: bool,
    pub binding_id: Option<Id>,
    pub route_revision: i64,
}

/// 尚有未认领输入的代际（上游 `PendingContext`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingContext {
    pub revision: i64,
    /// 发起人快照；老数据可能缺失 ⇒ 恢复时**失败关闭**（不冒充后来的发件人）。
    pub initiator_user_id: Option<Id>,
}

/// 媒体绑定入参（上游 `BindMediaParams`）。
#[derive(Debug, Clone)]
pub struct BindMediaParams {
    pub message_id: Option<Id>,
    pub session_id: Id,
    pub workspace_id: Id,
    pub sender: Id,
    /// `/issue` 轮次里媒体归 issue；否则归 `message_id`。
    pub issue_id: Option<Id>,
    pub issue_description_base: Option<String>,
    pub issue_command_text: String,
    pub body: String,
    pub media_refs: Vec<MediaRef>,
}

/// 媒体绑定结果（上游 `BindMediaResult`）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BindMediaResult {
    pub initial_title: String,
    pub title_source: String,
}

/// 媒体意图账本写入参数（上游 `RecordPendingMediaObjectParams`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordPendingMediaObjectParams {
    pub storage_key: String,
    pub workspace_id: Id,
    pub chat_message_id: Option<Id>,
    /// 附件行将来携带的 URL（是 key 的纯函数）⇒ 对账器可以据此查持久引用。
    pub storage_url: String,
    /// 只作运维诊断（上游注释逐字）。
    pub installation_id: Option<Id>,
}

/// 一次 chat run 的触发参数（本仓的等价物：上游把它摊在
/// `scheduleRunWithFresh` / `flushChatRun` 的形参里）。
#[derive(Debug, Clone)]
pub struct ChatRunParams {
    pub installation: ResolvedInstallation,
    pub session_id: Id,
    /// 这条消息的发起人（**不是**会话创建者：群聊里创建者是安装者）。
    pub initiator_user_id: Id,
    pub channel_binding_id: Option<Id>,
    pub route_revision: i64,
    /// `/clear` 一类要求开新会话。
    pub force_fresh: bool,
    pub context_revision: i64,
}

/// workspace 的展示身份（`/issue` 的标识符与深链要用）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkspaceIdentity {
    pub issue_prefix: String,
    pub slug: String,
}

// =====================================================================
// 命令词表（上游 `fresh_command.go` / `issue_command.go` 的**结论**）
// =====================================================================

/// 一条入站正文里的命令意图。
///
/// 分类的**实现**（`/new` / `/clear` / `/issue` 的解析）归 M7-2 的 `commands.rs`
/// （上游 `fresh_command.go` + `issue_command.go` 在 M7-2 的上游文件表里）；本枚举在这里
/// 落定，是为了让 Router 与各 adapter 用**同一份**词表，而不是各写一份"什么算 `/issue`"。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandIntent {
    /// 不是命令。
    None,
    /// `/new`：轮换会话路由。`body` 是剥离指令后的正文。
    NewChat { body: String },
    /// `/clear`：开新会话但保留正文语义。`body` 空 = **裸**命令（只记待开新会话）。
    FreshSession { body: String },
    /// `/issue <title> [desc]`。
    Issue { title: String, description: String },
}

/// 命令分类端口（纯函数、无 I/O；实现归 M7-2）。
///
/// `body` 读的是 [`InboundMessage::command_source_text`]：adapter 若富化过 `text`，
/// **必须**自己先写好 `command_text`，否则富化前缀会被当成命令（上游 #8058 那条回归）。
pub trait CommandClassifier: Send + Sync {
    fn classify(&self, body: &str) -> CommandIntent;
}

/// 一个"不认任何命令"的分类器（默认实现；测试与纯出站路径用）。
///
/// ⚠️ 它**不是**降级替身：装了它 `/issue` 就是普通聊天文本 —— 这正是"没接命令面"的诚实形态。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoCommands;

impl CommandClassifier for NoCommands {
    fn classify(&self, _body: &str) -> CommandIntent {
        CommandIntent::None
    }
}

// =====================================================================
// 端口（trait）：M7-1 定契约，实现分散在 M7-2…M7-20 的写集
// =====================================================================

/// 把一条入站消息路由到它的安装行（上游 `InstallationResolver`）。
///
/// adapter 从 `source` 或 `raw` 里读自己要的平台路由键。没有匹配 ⇒
/// `Err(Pipeline(InstallationNotFound))`；存在但已撤销 ⇒ `active = false`。
#[async_trait]
pub trait InstallationResolver: Send + Sync {
    async fn resolve_installation(
        &self,
        message: &InboundMessage,
    ) -> EngineResult<ResolvedInstallation>;
}

/// 把发件人映射成 Multica 用户，并**重新**校验 workspace 成员资格
/// （上游 `IdentityResolver`；绑定表上没有 member 外键）。
#[async_trait]
pub trait IdentityResolver: Send + Sync {
    async fn resolve_sender(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
    ) -> EngineResult<ResolvedIdentity>;
}

/// 两阶段幂等接缝（上游 `Deduper`）：`claim` 铸所有权令牌（已处理/在飞 ⇒
/// `Err(Pipeline(Duplicate))`）；`mark` / `release` 用令牌围栏（令牌不匹配是 no-op 而非错误）。
#[async_trait]
pub trait Deduper: Send + Sync {
    async fn claim(&self, installation_id: Id, message_id: &str) -> EngineResult<Id>;
    async fn mark(
        &self,
        installation_id: Id,
        message_id: &str,
        claim_token: Id,
    ) -> EngineResult<()>;
    async fn release(
        &self,
        installation_id: Id,
        message_id: &str,
        claim_token: Id,
    ) -> EngineResult<()>;
}

/// 会话绑定接缝（上游 `SessionBinder`）：确保 `chat_session` 并追加消息
/// （含事务内的 dedup Mark）。`append_message` 令牌被轮换时返回
/// `Err(Pipeline(ClaimLost))`。
#[async_trait]
pub trait SessionBinder: Send + Sync {
    async fn ensure_session(&self, params: EnsureSessionParams) -> EngineResult<Id>;

    async fn start_session(&self, params: StartSessionParams) -> EngineResult<StartSessionResult>;

    async fn mark_pending_fresh(&self, session_id: Id, message_id: &str) -> EngineResult<()>;

    async fn append_message(&self, params: AppendParams) -> EngineResult<AppendResult>;

    async fn bind_media(&self, params: BindMediaParams) -> EngineResult<BindMediaResult>;
}

/// 平台媒体解析接缝（上游 `MediaResolver`）：在用户消息与 dedup Mark 已持久化**之后**跑，
/// 脱离 connector 的 ACK 路径；返回的 refs 由 Router 绑定。
///
/// 实现必须**尽力而为**：失败保留正文里的占位文本、**绝不在行内删任何东西** ——
/// 每个上传对象都有 PUT 之前写好的意图账本行，异步对账器事后收尾。
pub trait MediaResolver: Send + Sync {
    /// 同步（ACK 路径上）判断这条消息是否引用了要下载的平台媒体。
    ///
    /// **必须是纯内存检查（无 I/O）**：`false` 让这条消息留在普通入库路径上
    /// （没有 pending 标记、不延后 run、不占并发槽）。
    fn has_media(&self, message: &InboundMessage) -> bool;

    /// 下载平台媒体并上传到对象存储，返回回填了 `media_refs` 的消息副本。
    fn resolve_media(
        &self,
        installation: &ResolvedInstallation,
        sender: &ResolvedIdentity,
        session_id: Id,
        chat_message_id: Option<Id>,
        message: &InboundMessage,
    ) -> InboundMessage;
}

/// 媒体意图账本接缝（上游 `MediaIntentLedger`）：对象写入**之前**落意图行。
///
/// `Ok(false)` = 这个 key 已经离开 `pending`（对账器接管了）⇒ **跳过上传**，
/// 别复活那一行。
#[async_trait]
pub trait MediaIntentLedger: Send + Sync {
    async fn record_pending_media_object(
        &self,
        params: RecordPendingMediaObjectParams,
    ) -> EngineResult<bool>;
}

/// 丢弃审计接缝（上游 `Auditor`）：**只记丢弃，不记正文**。
#[async_trait]
pub trait Auditor: Send + Sync {
    async fn record_drop(
        &self,
        installation_id: Option<Id>,
        message: &InboundMessage,
        reason: DropReason,
    ) -> EngineResult<()>;
}

/// 出站回复器（上游 `OutboundReplier`）：绑定卡 / 离线提示 / `/issue` 确认。
///
/// 可选端口：`None` 关掉出站回复。由 Router **脱离 ACK 关键路径**驱动。
pub trait OutboundReplier: Send + Sync {
    fn reply(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
        result: &RouteResult,
    );
}

/// 打字指示器（上游 `TypingNotifier`）。可选；`None` 关掉。
pub trait TypingNotifier: Send + Sync {
    /// 入库成功后点亮指示器。
    fn on_ingested(
        &self,
        installation: &ResolvedInstallation,
        message: &InboundMessage,
        session_id: Id,
    );
    /// 会话的 run 触发没有产出任务时清除指示器（agent 离线 / 归档 / 入队失败）。
    ///
    /// 那种情况下**永远**不会发布任务生命周期事件，平台自己的"任务结束即清除"也就不会触发 ——
    /// 所以必须在这里清，否则"处理中"会一直粘在用户消息上。幂等。
    fn on_settled(&self, session_id: Id);
}

/// issue 创建接缝（上游 `IssueCreator`）—— Router 只用到 `/issue` 需要的那一小块。
#[async_trait]
pub trait IssueCreator: Send + Sync {
    async fn create_issue(&self, params: ChannelIssueParams) -> EngineResult<ChannelIssueOutcome>;
}

/// `/issue` 建单参数（上游 `service.IssueCreateParams` 的渠道子集）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelIssueParams {
    pub workspace_id: Id,
    pub title: String,
    pub description: String,
    /// 默认指派的 agent（该安装的 agent）。
    pub agent_id: Id,
    pub creator_user_id: Id,
    /// `issue.origin_type` 的渠道标签（Feishu: `lark_chat`）。
    pub origin_type: String,
    /// `issue.origin_id`：产生它的 `chat_session`。
    pub origin_session_id: Id,
    /// 媒体要晚到时，指派 run 的**延后**触发时间。
    pub assigned_run_fire_at: Option<Timestamp>,
}

/// `/issue` 建单结果（上游 `service.IssueCreateResult` 的渠道子集）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelIssueOutcome {
    pub issue: ChannelIssue,
    /// 共享重复守卫找到了活跃的同名 issue ⇒ **没有**建新行。
    pub duplicate: bool,
    /// 指派 agent 的 issue 任务（延后触发时 Router 要在媒体就绪后提升它）。
    pub assigned_task_id: Option<Id>,
}

/// 一条渠道 issue 的展示身份（`db.Issue` 的渠道子集）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelIssue {
    pub id: Id,
    pub number: i64,
    pub title: String,
}

/// `/issue` 命令的解析结果（上游 `IssueCommand`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelIssueCommand {
    pub title: String,
    pub description: String,
}

/// 运行触发接缝（上游 Router 里的去抖 + 任务入队；实现归 M7-2 的 `batcher.rs`）。
#[async_trait]
pub trait RunTriggerer: Send + Sync {
    /// 把一个 run 交给去抖器（或直接入队）。**默认触发普通 chat turn**。
    async fn schedule_chat_run(&self, params: ChatRunParams) -> EngineResult<()>;

    /// 冲刷所有未决窗口并等待在飞的触发收尾（停机路径）。
    async fn drain(&self) -> EngineResult<()>;
}

/// 读侧接缝（上游 `SessionReader` 的渠道子集）：`/issue` 的标识符与深链要 workspace 身份。
#[async_trait]
pub trait SessionReader: Send + Sync {
    async fn workspace_identity(&self, workspace_id: Id) -> EngineResult<WorkspaceIdentity>;
}

// =====================================================================
// 端口包：一台平台一组（上游 `ResolverSet`）
// =====================================================================

/// 每平台一组的端口包（上游 `ResolverSet`）。
///
/// `installation` / `identity` / `dedup` / `session` / `audit` 是**必需**的；
/// `media` / `replier` / `typing` 可选（`None` = 该平台没有这一面）。
/// `origin_type` 是 `/issue` 写给 `issue.origin_type` 的渠道标签。
pub struct ResolverSet {
    pub installation: Arc<dyn InstallationResolver>,
    pub identity: Arc<dyn IdentityResolver>,
    pub dedup: Arc<dyn Deduper>,
    pub session: Arc<dyn SessionBinder>,
    pub audit: Arc<dyn Auditor>,
    pub media: Option<Arc<dyn MediaResolver>>,
    pub replier: Option<Arc<dyn OutboundReplier>>,
    pub typing: Option<Arc<dyn TypingNotifier>>,
    /// `/issue` 的 `origin_type`（Feishu: `lark_chat`）。
    pub origin_type: String,
}

impl fmt::Debug for ResolverSet {
    /// 端口是 trait 对象 ⇒ 只列**存在性**（不含任何端口内部状态）。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolverSet")
            .field("installation", &"<dyn InstallationResolver>")
            .field("identity", &"<dyn IdentityResolver>")
            .field("dedup", &"<dyn Deduper>")
            .field("session", &"<dyn SessionBinder>")
            .field("audit", &"<dyn Auditor>")
            .field("media", &self.media.is_some())
            .field("replier", &self.replier.is_some())
            .field("typing", &self.typing.is_some())
            .field("origin_type", &self.origin_type)
            .finish()
    }
}

impl ResolverSet {
    /// 必填端口的便捷构造（可选端口为 `None`）。
    pub fn new(
        installation: Arc<dyn InstallationResolver>,
        identity: Arc<dyn IdentityResolver>,
        dedup: Arc<dyn Deduper>,
        session: Arc<dyn SessionBinder>,
        audit: Arc<dyn Auditor>,
        origin_type: impl Into<String>,
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
            origin_type: origin_type.into(),
        }
    }

    /// 挂上媒体面。
    #[must_use]
    pub fn with_media(mut self, media: Arc<dyn MediaResolver>) -> Self {
        self.media = Some(media);
        self
    }

    /// 挂上出站回复器。
    #[must_use]
    pub fn with_replier(mut self, replier: Arc<dyn OutboundReplier>) -> Self {
        self.replier = Some(replier);
        self
    }

    /// 挂上打字指示器。
    #[must_use]
    pub fn with_typing(mut self, typing: Arc<dyn TypingNotifier>) -> Self {
        self.typing = Some(typing);
        self
    }
}

#[cfg(test)]
mod tests;
