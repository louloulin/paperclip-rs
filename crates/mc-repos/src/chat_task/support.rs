//! M4-4 共享项：`agent_task_queue` 的 chat 投影、状态字面量与错误类型。
//!
//! 归属：M4-4（LUM-1475）。本文件只被 `super::{send,queue,onboarding}` 引用，
//! 不对外导出（`super` 用 `pub use` 转发需要的部分）。

use chrono::{DateTime, Utc};
use sqlx::FromRow;
use uuid::Uuid;

use crate::RepoError;

/// 排队态字面量（上游每条 chat 查询里的 `status IN (...)` 原文）。
///
/// 上游有**七份逐字拷贝**（`HasActiveChatTaskForSession` / `HasPendingChatTurnForSession` /
/// `GetPendingChatTask` / `ListPendingChatTasksForSession` / `PrioritizeQueuedChatTask` ×2 处 /
/// `CancelQueuedAgentTasksForSession` 的 head CTE / `ReanchorNextQueuedDirectChatInput`），
/// 本仓收成一个常量 —— 与 `mc_task::status::TaskStatus` 的 CHECK 取值同源
/// （`contracts/upstream-schema.sql:1056`；本仓 `migrations/0001_init.up.sql:230` 的 CHECK 是错的）。
pub(super) const PENDING_STATUSES: &str =
    "'queued', 'dispatched', 'running', 'waiting_local_directory', 'deferred'";

/// 「可见头」排序（上游每条 pending 查询共享的 `ORDER BY`）。
///
/// 上游注释把这份排序与 `chat.sql` 里 `ListChatMessages` 上方的 visible-head 不变式
/// 绑在一起：**改一处必须同 patch 改所有 selector**。本仓把它收成一个常量。
pub(super) const VISIBLE_HEAD_ORDER: &str = "CASE \
       WHEN task.status IN ('dispatched', 'running', 'waiting_local_directory') THEN 0 \
       WHEN task.status = 'deferred' THEN 1 \
       ELSE 2 \
     END, \
     task.priority DESC, task.created_at ASC, task.id ASC";

/// `chat` 任务行实际用到的列投影。
///
/// **只列本片读到的列**（`sqlx::FromRow` 会忽略结果集里多出来的列），因此不需要上游
/// `RETURNING *` 的 60 列全投影。
pub(super) const CHAT_TASK_COLUMNS: &str = "id, agent_id, runtime_id, status, priority, \
     chat_session_id, chat_input_task_id, created_at, wait_reason, completed_at, \
     channel_context_revision, regenerate_quick_actions_for";

/// `agent_task_queue` 的 chat 子集行。
#[derive(Debug, Clone, FromRow)]
pub struct ChatTaskRow {
    /// 任务主键（`dbid.NewV7()` 生成 ⇒ v7）。
    pub id: Uuid,
    /// 执行该任务的 agent。
    pub agent_id: Uuid,
    /// 认领的 runtime；chat 任务创建时**必有**（`carrier.runtime_id`）。
    pub runtime_id: Option<Uuid>,
    /// `queued` / `deferred` / … 见 [`PENDING_STATUSES`]。
    pub status: String,
    /// chat 任务固定 `2`（`PRIORITY_CHAT`）；`prioritize` 会改成 `4`，把旧的 `4`
    /// 降回 `3`。
    pub priority: i32,
    /// 所属会话。
    pub chat_session_id: Option<Uuid>,
    /// 本轮输入批次的归属任务 id（`SetChatTaskInputOwnerSelf` 置为自身）。
    pub chat_input_task_id: Option<Uuid>,
    /// 创建时间（发送响应里的 `created_at` 取它，不是消息的）。
    pub created_at: DateTime<Utc>,
    /// 仅在 `waiting_local_directory` 时有意义（上游只在该状态下渲染）。
    pub wait_reason: Option<String>,
    /// 终态时间。
    pub completed_at: Option<DateTime<Utc>>,
    /// 渠道上下文版本（渠道分支用；direct chat 为 `NULL`）。
    pub channel_context_revision: Option<i64>,
    /// 背景 quick-actions 重生成轮的目标 assistant 消息（对 UI 不可见）。
    pub regenerate_quick_actions_for: Option<Uuid>,
}

/// `ListPendingChatTasksForSession` 的一行（上游 `GetPendingChatTask` 的 legacy
/// 单行版共用同一投影）。
#[derive(Debug, Clone, FromRow)]
pub struct PendingChatTaskRowData {
    /// 任务 id（上游 `task.id`）。
    pub task_id: Uuid,
    /// 任务状态。
    pub status: String,
    /// 任务创建时间（StatusPill 的计时锚点）。
    pub created_at: DateTime<Utc>,
    /// `waiting_local_directory` 专用原因文本（其余状态下是过期值）。
    pub wait_reason: Option<String>,
    /// 该任务输入批次里第一条 user 消息的 id（lateral join，可空）。
    pub message_id: Option<Uuid>,
    /// 该消息正文；上游 `COALESCE(message.content, '')::text` ⇒ 非空串。
    pub content: String,
}

/// `ListPendingChatTasksByCreator` 的一行。
#[derive(Debug, Clone, FromRow)]
pub struct CreatorPendingChatTaskRow {
    /// 任务 id。
    pub task_id: Uuid,
    /// 任务状态。
    pub status: String,
    /// 所属会话（JOIN 保证非空，但列可空）。
    pub chat_session_id: Option<Uuid>,
    /// 会话的 agent，用来按调用方可见的 agent 集合过滤（私有 agent 收回后要掉出列表）。
    pub agent_id: Uuid,
}

/// `PrioritizeQueuedChatTask` 的一行（CTE 的最终 `SELECT`）。
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct PrioritizedChatTaskRow {
    /// 被提升的任务 id。
    pub task_id: Uuid,
    /// 当前可见头任务 id（“send now”之后客户端要取消的那条）。
    pub active_task_id: Option<Uuid>,
}

/// `PrioritizeQueuedChatTask` 的三种结局（把上游 handler 里的
/// `ErrNoRows` + 回读区分逻辑收回仓储）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PriorityOutcome {
    /// CAS 成功：目标被提到 `priority = 4`。
    Prioritized(PrioritizedChatTaskRow),
    /// 目标仍是本会话的 `queued` 行，但可见头还没被认领 —— 没有可替换的活跃回复
    /// （上游 409 `"there is no active reply to replace"`）。
    NoActiveReply,
    /// 目标已不在队列（被 daemon 提升 / 取消 / 换了会话）
    /// （上游 409 `"task is no longer queued"`）。
    NotQueued,
}

/// 发送直聊消息的事务结局错误。
///
/// 前三个变体与上游 `ErrChatSessionArchived` / `ErrChatTaskAgentArchived` /
/// `ErrChatTaskAgentNoRuntime` 一一对应（handler 映射 409 / 409 / 409）；其余落
/// 500 `"failed to send chat message: {e}"`。
#[derive(Debug, thiserror::Error)]
pub enum ChatSendError {
    /// 锁内重读发现会话已归档（并发归档的孪生检查；handler 的 `gate` 已先查一次）。
    #[error("chat session is archived")]
    SessionArchived,
    /// 锁内重读发现 agent 已归档。
    #[error("chat agent is archived")]
    AgentArchived,
    /// agent 没有绑定 runtime（上游 `ErrChatTaskAgentNoRuntime`）。
    #[error("chat agent has no runtime")]
    NoRuntime,
    /// 其余数据库 / 归属栅栏错误。
    #[error("{0}")]
    Repo(#[from] RepoError),
}

/// 直聊发送的入参（一次事务的全部写面）。
pub struct DirectChatSend<'a> {
    /// 会话 id（handler 已 gate 过归属）。
    pub session_id: Uuid,
    /// 会话的 agent id（handler 已加载并 gate 过）。
    pub agent_id: Uuid,
    /// 发起人（web 面 = 请求用户；同时是 originator / accountable）。
    pub initiator_user_id: Uuid,
    /// 正文。
    pub content: &'a str,
    /// 客户端请求的附件 id（已 `parseUUIDSlice` 校验过）。
    pub attachment_ids: &'a [Uuid],
    /// 附件上传者类型（web 面 = `member`）。
    pub uploader_type: &'a str,
    /// 附件上传者 id。
    pub uploader_id: Uuid,
    /// `chattitle.Derive` 的注入点（`chat.sql:180` 的标题 CAS 与 `chat.sql:210` 的
    /// 媒体标题 CAS 都要它）。
    ///
    /// 领域实现在 `mc-chat`（纯字符串扫描）。本仓储**不引 `mc-chat` 依赖边**
    /// （`crate::chat_session` 的先例），所以由 `crates/mc-http` 侧传入
    /// `mc_chat::task::derive_title` —— 单一真值，不在仓储里复刻第二份算法。
    pub derive_title: fn(&str) -> String,
}

/// `SendDirectChatMessage` 事务落库结果（上游 `DirectChatSendResult`）。
#[derive(Debug, Clone)]
pub struct DirectChatSendResult {
    /// 新建的 chat 任务行。
    pub task: ChatTaskRow,
    /// 新建的 user 消息行。
    pub message: crate::chat_message::ChatMessageRow,
    /// 服务端**实际绑定**的附件 id（请求了但没绑上的不在里面）。
    pub bound_attachment_ids: Vec<Uuid>,
    /// 本次发送是否是追问（插入前检查的队列位置）。
    pub queued: bool,
    /// 本事务初始化的标题（CAS 命中才有值）。
    pub initial_title: String,
}

/// Mika onboarding 开门（`OpenMikaOnboardingChat`）的落库结果。
#[derive(Debug, Clone)]
pub struct OnboardingOpenResult {
    /// 隐藏的 kickoff 行（role = user，`message_kind = 'onboarding_kickoff'`，无 task）。
    pub kickoff: crate::chat_message::ChatMessageRow,
    /// 门面开场白行（role = assistant，`created_at = kickoff + 1µs`）。
    pub opening: crate::chat_message::ChatMessageRow,
}

/// `OpenMikaOnboardingChat` 的三种结局（另两个不是错误，是幂等 / 归档语义）。
#[derive(Debug, Clone)]
pub enum StartOnboardingOutcome {
    /// 首次开门：落 kickoff + opening 两行。
    Started(Box<OnboardingOpenResult>),
    /// 会话已经有 user 消息 ⇒ 上游 `ErrChatSessionAlreadyStarted`（handler 200
    /// `{started:false}`）。
    AlreadyStarted,
    /// 锁内重读发现会话已归档 ⇒ 上游 `ErrChatSessionArchived`。
    SessionArchived,
}

/// 把 `sqlx::Error` 包进 [`ChatSendError::Repo`]。
pub(super) fn send_repo(err: sqlx::Error) -> ChatSendError {
    ChatSendError::Repo(crate::workspace::map_sqlx_err(err))
}

/// `PrioritizeQueuedChatTask` handler 的逐步 500 文案。
///
/// 上游在事务的四个阶段各自 `writeError`，文案不同（全部 500）；`Display` 就是那四句
/// 上游原文，DB 细节留在 `source()` 里不入响应体。
#[derive(Debug, thiserror::Error)]
pub enum PriorityError {
    /// 事务未开起来。
    #[error("failed to start prioritize transaction")]
    Begin(#[source] RepoError),
    /// agent 行锁失败（或 agent 不存在）。
    #[error("failed to lock chat agent")]
    LockAgent(#[source] RepoError),
    /// `PrioritizeQueuedChatTask` 本身失败。
    #[error("failed to prioritize queued task")]
    Query(#[source] RepoError),
    /// 回读区分「过期行」与「无活跃回复」时失败。
    #[error("failed to load queued task")]
    LoadTask(#[source] RepoError),
    /// 提交失败。
    #[error("failed to commit queued task priority")]
    Commit(#[source] RepoError),
}
