use super::{ChannelError, ChannelIssue, Id};

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
