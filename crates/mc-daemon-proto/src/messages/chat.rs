//! 会话（chat）面载荷 —— 上游 `server/pkg/protocol/messages.go` L75–L81、
//! L186–L200、L235–L382 冻结。
//!
//! 这一组里有**唯一**一个三重可选字段（[`ChatSessionUpdatedPayload::project_id`]），
//! 也是本 crate 最容易被改错的地方 ——
//! `**string` 与 `*string` 的区别就是「显式 `null`」与「没提这件事」的区别。
//!
//! 上游把三种状态压进一个字段，是因为同一个事件由三条不同路径产生（改名 / 置顶 /
//! 归档 / 改项目），每条路径只该动自己那个字段：
//!
//! | 线上形状 | Rust | Go | 语义 |
//! |----------|------|----|------|
//! | 键不存在 | `None` | `nil` | 本次变更**没提**这个字段，接收方保持原值 |
//! | `"key": null` | `Some(None)` | 指向 nil 的指针 | 显式清空（如移出项目） |
//! | `"key": "x"` | `Some(Some("x"))` | 指向值的指针 | 设成 `x` |
//!
//! 把 `**string` 简化成 `Option<String>` 会把「显式清空」与「没提」合并成一件事 ——
//! 那正是这个字段存在的理由，所以它必须保持三态。

use serde::{Deserialize, Serialize};

use super::omit;

/// 会话消息种类（`chat_message.message_kind`，`messages.go:248`–L262）。
///
/// **增量**：未知值在老读者上退化成普通消息 —— 上游把未知 kind 当普通消息渲染，
/// 而不是报错（`messages.go:246` `Additive: unknown values degrade to
/// ChatMessageKindMessage on older readers`）。
pub mod message_kind {
    /// 普通 user/assistant 消息（`messages.go:248`）。
    pub const MESSAGE: &str = "message";
    /// 一次「完成了但没有文字回复」的直聊回合（`messages.go:252`，MUL-4351）：可见的
    /// 终止态，而不是静默丢弃。
    pub const NO_RESPONSE: &str = "no_response";
    /// server 自己写的隐藏首回合（`messages.go:257`），用来启动 onboarding 对话。
    pub const ONBOARDING_KICKOFF: &str = "onboarding_kickoff";
    /// onboarding kickoff 产生的 assistant 回复（`messages.go:262`）。
    pub const ONBOARDING_OPENING: &str = "onboarding_opening";

    /// 已知 kind 全表。
    pub const KNOWN: [&str; 4] = [MESSAGE, NO_RESPONSE, ONBOARDING_KICKOFF, ONBOARDING_OPENING];

    /// 已知 kind 判定。未知值合法（渲染成普通消息），故只给谓词不做枚举。
    #[must_use]
    pub fn is_known(kind: &str) -> bool {
        KNOWN.contains(&kind)
    }
}

/// 取消失效化（`chat:cancel_finalized`）的两种结果（`messages.go:297`–L301）。
pub mod cancel_outcome {
    /// transcript 非空 → 已持久化一条 "Stopped." assistant 消息。
    pub const STOPPED: &str = "stopped";
    /// transcript 仍为空 → 触发那条用户消息被删除，内容应还原进输入框草稿。
    pub const RESTORED: &str = "restored";

    /// 已知 outcome 全表。
    pub const KNOWN: [&str; 2] = [STOPPED, RESTORED];

    /// 已知 outcome 判定。
    #[must_use]
    pub fn is_known(outcome: &str) -> bool {
        KNOWN.contains(&outcome)
    }
}

/// 一条 assistant 回复上的后续建议（`messages.go:77`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatQuickAction {
    /// 芯片上的短文本。
    pub label: String,
    /// 点下去的完整用户回合。
    pub prompt: String,
    /// 是否是主推荐（`omitempty`）。
    #[serde(skip_serializing_if = "omit::boolean")]
    pub primary: bool,
}

/// 补充一个已完成回合的后续建议（`messages.go:186`，`chat:quick_actions`）。
///
/// **空 `quick_actions` 是有意义的终态**：它表示「这一轮没有建议」，用来解开客户端的
/// 等待骨架屏。所以这个字段**没有** `omitempty`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatQuickActionsPayload {
    /// 会话 id。
    pub chat_session_id: String,
    /// 该回合的任务 id。
    pub task_id: String,
    /// 被补充的 assistant 消息 id。
    pub message_id: String,
    /// 建议列表；空数组 = 明确「无建议」。
    pub quick_actions: Vec<ChatQuickAction>,
    /// `true` = 这次补充是**失败**收敛（生成或投递失败），`quick_actions` 里是原样
    /// 未变的旧建议，客户端应提示「刷新失败」而不是当成功（MUL-5149，`omitempty`）。
    #[serde(skip_serializing_if = "omit::boolean")]
    pub failed: bool,
}

/// 新会话消息广播（`messages.go:235`，`chat:message`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatMessagePayload {
    /// 会话 id。
    pub chat_session_id: String,
    /// 消息 id。
    pub message_id: String,
    /// `user` / `assistant` / `system`。
    pub role: String,
    /// 正文。
    pub content: String,
    /// 产生这条消息的任务（`omitempty`：用户自己发的消息没有任务）。
    #[serde(skip_serializing_if = "omit::string")]
    pub task_id: String,
    /// 创建时间（无 `omitempty`）。
    pub created_at: String,
}

/// agent 完成一次会话回合（`messages.go:278`，`chat:done`）。
///
/// 携带**刚刚持久化的** assistant 消息，让客户端直接写进消息缓存 —— 免掉
/// 「实时流 → 正式消息」交接时的一次重取（上游注释指向 #2123 的闪烁）。
///
/// `message_id`/`content`/`created_at`/`elapsed_ms` 的 `omitempty` 只对**遗留广播路径**
/// 生效：直聊完成现在必然写恰好一行 assistant 记录，所以这些字段在直聊上总是有值。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatDonePayload {
    /// 会话 id。
    pub chat_session_id: String,
    /// 任务 id。
    pub task_id: String,
    /// 持久化的 assistant 消息 id（`omitempty`）。
    #[serde(skip_serializing_if = "omit::string")]
    pub message_id: String,
    /// 正文（`omitempty`）。
    #[serde(skip_serializing_if = "omit::string")]
    pub content: String,
    /// 耗时毫秒（`omitempty`：0 = 未提供）。
    #[serde(skip_serializing_if = "omit::i64")]
    pub elapsed_ms: i64,
    /// 创建时间（`omitempty`）。
    #[serde(skip_serializing_if = "omit::string")]
    pub created_at: String,
    /// [`message_kind`] 之一（`omitempty`：缺失 = 老语义，即普通消息）。
    #[serde(skip_serializing_if = "omit::string")]
    pub message_kind: String,
    /// 随回合一起给出的建议（`omitempty`）。
    #[serde(skip_serializing_if = "omit::vec_is_empty")]
    pub quick_actions: Vec<ChatQuickAction>,
    /// `true` = 稍后还会有 `chat:quick_actions` 补充，先渲染占位。
    /// **与 `quick_actions` 非空互斥**（`omitempty`）。
    #[serde(skip_serializing_if = "omit::boolean")]
    pub quick_actions_pending: bool,
}

/// 被取消的会话任务延迟收敛后的广播（`messages.go:313`，`chat:cancel_finalized`）。
///
/// 取消的 HTTP 响应**带不了**这个结果（要等 daemon 把 transcript 刷完才知道），所以前端
/// 靠这个事件：`stopped` 插入 assistant 消息，`restored` 删掉已删的用户消息并让发起方去
/// 取持久化草稿。**草稿正文与附件刻意不上广播**（会话是 workspace 级可见的）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatCancelFinalizedPayload {
    /// [`cancel_outcome`] 之一。
    pub outcome: String,
    /// 会话 id。
    pub chat_session_id: String,
    /// 任务 id。
    pub task_id: String,
    /// 触发取消的人；只有这个用户的客户端需要去取草稿（`omitempty`；
    /// 客户端把缺失当作「不是我」）。
    #[serde(skip_serializing_if = "omit::string")]
    pub initiator_user_id: String,
    /// `stopped` 时的 assistant 消息 id（`omitempty`）。
    #[serde(skip_serializing_if = "omit::string")]
    pub message_id: String,
    /// 仅 `outcome == "stopped"` 有值，形状与 [`ChatDonePayload`] 一致（`omitempty`）。
    #[serde(skip_serializing_if = "omit::string")]
    pub content: String,
    /// 仅 `stopped` 有值（`omitempty`）。
    #[serde(skip_serializing_if = "omit::string")]
    pub message_kind: String,
    /// 仅 `stopped` 有值（`omitempty`）。
    #[serde(skip_serializing_if = "omit::string")]
    pub created_at: String,
    /// 仅 `stopped` 有值（`omitempty`）。
    #[serde(skip_serializing_if = "omit::i64")]
    pub elapsed_ms: i64,
}

/// 会话被标记已读（`messages.go:334`，`chat:session_read`）：让其他设备同步未读数。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatSessionReadPayload {
    /// 会话 id。
    pub chat_session_id: String,
}

/// 新会话建立（`messages.go:338`，`chat:session_created`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatSessionCreatedPayload {
    /// 所属 workspace。
    pub workspace_id: String,
    /// 会话 id。
    pub chat_session_id: String,
    /// 参与会话的 agent。
    pub agent_id: String,
    /// 创建者。
    pub creator_id: String,
    /// 标题。
    pub title: String,
    /// 渠道来源（无 `omitempty`：始终是值结构体，不是指针 → 永远不会是 `null`）。
    pub channel_source: ChatSessionChannelSource,
    /// 当前请求是否正好来自该渠道路由。
    pub is_current_channel_route: bool,
}

/// 会话的来源渠道三元组（`messages.go:348`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatSessionChannelSource {
    /// 渠道类型（`web` / `slack` / ……）。
    pub channel_type: String,
    /// 渠道安装 id。
    pub installation_id: String,
    /// 路由版本号（单调递增，用于丢弃乱序更新）。
    pub route_revision: i64,
}

/// 会话被硬删除（`messages.go:357`，`chat:session_deleted`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatSessionDeletedPayload {
    /// 会话 id。
    pub chat_session_id: String,
}

/// 会话可编辑字段变化（`messages.go:365`，`chat:session_updated`）。
///
/// 三条路径（改名 / 置顶 / 归档 / 改项目）共用这一个事件，各自只填自己的字段，
/// 其余字段**缺失**表示「没提这件事，保持原值」。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatSessionUpdatedPayload {
    /// 会话 id。
    pub chat_session_id: String,
    /// 新标题。
    pub title: String,
    /// **三态**（`**string`，上游刻意为之，见本模块文档）：
    /// `None` 缺失 = 没提；`Some(None)` = 显式 `null`（移出项目）；
    /// `Some(Some(v))` = 设为 `v`。
    ///
    /// `skip_serializing_if` 用 `omit::option`：`None` 省略键，`Some(None)` 输出 `null` ——
    /// 这正是 Go `**string` + `omitempty` 的行为。
    ///
    /// 入站必须用 [`super::double_option::deserialize`]：serde 对 `Option<Option<T>>`
    /// 的默认实现会把 `null` 吃在外层，三态塌缩成两态（详见该模块文档）。
    #[serde(
        skip_serializing_if = "omit::option",
        deserialize_with = "super::double_option::deserialize"
    )]
    pub project_id: Option<Option<String>>,
    /// `Some(v)` = 置顶路径设的值；`None` = 改名路径没碰置顶状态（`omitempty`）。
    #[serde(skip_serializing_if = "omit::option")]
    pub pinned: Option<bool>,
    /// `Some("active"/"archived")` = 归档路径；`None` = 没碰（`omitempty`）。
    #[serde(skip_serializing_if = "omit::option")]
    pub status: Option<String>,
    /// 更新时间（无 `omitempty`）。
    pub updated_at: String,
}
