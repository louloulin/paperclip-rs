use super::{fmt, Any, Arc, ChannelIssueCommand, ChannelKind, Id, InboundMessage, MediaRef};

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
