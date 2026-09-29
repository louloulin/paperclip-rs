use super::{
    async_trait, AppendParams, AppendResult, BindMediaParams, BindMediaResult, ChatRunParams,
    DropReason, EngineResult, EnsureSessionParams, Id, InboundMessage,
    RecordPendingMediaObjectParams, ResolvedIdentity, ResolvedInstallation, RouteResult,
    StartSessionParams, StartSessionResult, Timestamp, WorkspaceIdentity,
};

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
