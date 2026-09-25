//! Slack 会话**读**面：`multica chat history`（频道目录）与 `multica chat thread [id]`（单线程）
//! （上游 `internal/integrations/slack/history.go`，737 行）。
//!
//! - **写者**：M7-4（`docs/60-M7-PLAN.md` §3.3）。
//! - **上游定位**（MUL-3871）：两条命令都是**按需拉取**（不是缓存），且**只**读会话自己的频道 ——
//!   频道由服务端从绑定行解析，**永不**接受调用方给的 chat id；`thread id` 只是一个
//!   **频道内**的定位符。没有 Slack 绑定的会话返回 [`HistoryError::NoSlackSession`]
//!   （**空读**，不是失败）。
//!
//! # 四道过滤（一个都不能少，否则 agent 的上下文会被污染）
//!
//! 1. **路由代际**（[`filter_route_generation`]）：`control_ack` 之类的控制回执、
//!    以及**别的**会话绑定发出来的消息，一律从上下文里剔掉。判据优先取消息自带的
//!    出站元数据（`metadata.event_payload.kind` / `binding_id`，见
//!    [`crate::slack::outbound::OUTBOUND_METADATA_EVENT`]），再回落到
//!    `channel_outbound_message` 表；
//! 2. **历史边界**：绑定的 `history_start_message_id` / `history_end_message_id` /
//!    `history_boundary_pending` 把"上一个 agent 上下文"排除掉；
//! 3. **上下文代际**（[`filter_context_generation`]）：代际 > 1 时，**本 bot** 的消息只有在
//!    「这一代里持久化过它的 provider id」时才可信 —— 时间戳**不是**因果边界
//!    （`/clear` 之后才落地的旧回复会把时间序弄乱，上游注释逐字）。未知的本 bot 输出
//!    **对 agent 失败关闭**；
//! 4. **每页上限**（[`clamp_history_limit`]）：默认 20、硬顶 50 —— 一次拉取不能把
//!    无界转录灌进 agent 上下文。
//!
//! # 与上游的三处形态差异（登记 `docs/32` §15）
//!
//! 1. **`allowed_bot_messages` 的粒度**：本仓 `mc-repos` 没有上游的
//!    `ListChannelOutboundMessageIDsForContext`（`channel_outbound_message` **没有**
//!    `channel_context_revision` 列）⇒ 只能用「按 `(binding_id, route_revision)`
//!    列出的出站行」近似 —— 粒度是**路由代际**而不是**上下文代际**，方向是**多**放行
//!    同代际内早前轮次的本 bot 消息（不会少放行）。**已登记**，不是静默降级。
//! 2. **Block Kit 的摊平**走通用 JSON 遍历（按 `type` 分派），不镜像 `slack-go` 的
//!    类型树 —— 语义（哪些块有正文、按什么顺序拼）逐字照搬，类型结构不照搬。
//! 3. **页面类型在本文件内定义**（`mc-core` 没有 `HistoryPage`）：消费方是
//!    `multica chat history` 命令面，**不在 M7 写集内** ⇒ 属于**已登记缺口**
//!    （同 R-M7-6 的手法），不是"漏实现"。
//!
//! # 凭据纪律
//!

//! # 文件布局（门 ⑩）
//!
//! 上游这一个 737 行的文件在本仓拆成四个（切点照上游自身的内聚边界，不是凑数字）：
//!
//! | 文件 | 内容 |
//! | --- | --- |
//! | 本文件 | 模块文档 / 错误 / 页面与 wire 类型 / 端口 / `SlackTarget` / 频道与线程根的解析 |
//! | `history/reader.rs` | `History`：解析 → 拉取 → 过滤 → 归一化 |
//! | `history/text.rs` | 时间窗、游标、页大小、两道过滤（纯函数） |
//! | `history/flatten.rs` | 正文摊平、Block Kit、人名与标签器（纯函数） |
//!
//! 令牌只以形参流动；人名解析失败**不是**错误（缺 `users:read` 作用域就退回 `User N`）。

use async_trait::async_trait;
use mc_core::id::Id;
use mc_repos::channel::installation::{ChannelInstallationRepo, ChannelInstallationRow};
use mc_repos::channel::outbound::ChannelOutboundRepo;
use mc_repos::channel::session::ChannelChatSessionRepo;
use mc_repos::RepoError;
use serde_json::Value;

use crate::slack::outbound::{ApiResult, HttpSlackApi, SlackApiError, OUTBOUND_METADATA_EVENT};

/// 未指定时的页大小（上游 `defaultHistoryLimit`）。
pub const DEFAULT_HISTORY_LIMIT: i64 = 20;
/// 单页硬顶（上游 `maxHistoryLimit`）。
pub const MAX_HISTORY_LIMIT: i64 = 50;
/// 从附件 / 块里**回落**出来的正文上限（上游 `maxDerivedTextLen`）。
pub const MAX_DERIVED_TEXT_CHARS: usize = 4000;
/// 出站元数据里的两个字段名（上游字面量）。
pub(super) const META_KIND: &str = "kind";
pub(super) const META_BINDING_ID: &str = "binding_id";

// =====================================================================
// 错误
// =====================================================================

/// 读面失败。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HistoryError {
    /// 该会话没有 Slack 频道绑定（**空读**语义，不是失败）。
    #[error("slack: session has no slack channel binding")]
    NoSlackSession,
    /// 存储层故障。
    #[error("slack: history store failure: {message}")]
    Store { message: String },
    /// 解密失败 / 凭据缺失（**不**回显任何字节）。
    #[error("slack: installation credentials unavailable ({code})")]
    Credentials { code: &'static str },
    /// Slack Web API 失败。
    #[error("slack: {source}")]
    Api {
        #[from]
        source: SlackApiError,
    },
}

impl HistoryError {
    /// 是否"不是 Slack 会话"（调用方据此回**空**而不是失败）。
    #[must_use]
    pub fn is_no_slack_session(&self) -> bool {
        matches!(self, Self::NoSlackSession | Self::Credentials { .. })
    }
}

// =====================================================================
// 页面类型（上游 `channel.History*`；见模块文档差异 3）
// =====================================================================

/// 一页读的入参（上游 `channel.HistoryOptions`）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HistoryOptions {
    /// 向更早翻页的游标（空 = 从最新开始）。
    pub before: String,
    /// 时间窗下界（与绑定的 `history_start_message_id` 取较晚者）。
    pub after: String,
    /// 时间窗上界（与绑定的 `history_end_message_id` 取较早者）。
    pub until: String,
    /// 页大小（≤0 ⇒ [`DEFAULT_HISTORY_LIMIT`]；> [`MAX_HISTORY_LIMIT`] ⇒ 取硬顶）。
    pub limit: i64,
    /// 调用方声明"边界还没准备好" ⇒ 直接空读（上游同）。
    pub boundary_pending: bool,
    /// 调用方的上下文代际（>1 时启用第 3 道过滤）。
    pub context_revision: i64,
}

/// 消息角色（上游 `channel.HistoryRole*`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HistoryRole {
    /// 人 / 第三方 bot 发的。
    #[default]
    User,
    /// **本** bot 发的。
    Assistant,
}

/// 一条归一化的历史消息（上游 `channel.HistoryMessage`）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HistoryMessage {
    pub id: String,
    pub author: String,
    pub author_id: String,
    pub role: HistoryRole,
    pub text: String,
    pub ts: String,
    /// 概览模式里、这条消息**起了**一个线程时给线程 id（= 它的 ts）。
    pub thread_id: String,
    pub reply_count: i64,
    pub latest_reply: String,
}

/// 一页历史（上游 `channel.HistoryPage`）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HistoryPage {
    pub messages: Vec<HistoryMessage>,
    pub next_cursor: String,
    pub channel_type: String,
    pub thread_id: String,
}

// =====================================================================
// wire 类型
// =====================================================================

/// 一条 Slack 消息（上游 `slack.Message` 的**契约子集**）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SlackMessage {
    pub ts: String,
    pub user: String,
    pub username: String,
    pub bot_id: String,
    pub text: String,
    pub reply_count: i64,
    pub latest_reply: String,
    /// 附件（原样 JSON：摊平只看 `pretext`/`title`/`text`/`fields`/`fallback`/`blocks`）。
    pub attachments: Vec<Value>,
    /// Block Kit 块（原样 JSON）。
    pub blocks: Vec<Value>,
    /// `metadata.event_type`。
    pub metadata_event_type: String,
    /// `metadata.event_payload`（原样 JSON）。
    pub metadata_event_payload: Value,
}

impl SlackMessage {
    /// 从一条消息 JSON 解出。
    #[must_use]
    pub fn from_json(value: &Value) -> Self {
        let text = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let metadata = value.get("metadata").cloned().unwrap_or(Value::Null);
        Self {
            ts: text("ts"),
            user: text("user"),
            username: text("username"),
            bot_id: text("bot_id"),
            text: text("text"),
            reply_count: value
                .get("reply_count")
                .and_then(Value::as_i64)
                .unwrap_or_default(),
            latest_reply: text("latest_reply"),
            attachments: array_at(value, "attachments"),
            blocks: array_at(value, "blocks"),
            metadata_event_type: metadata
                .get("event_type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            metadata_event_payload: metadata
                .get("event_payload")
                .cloned()
                .unwrap_or(Value::Null),
        }
    }

    /// 出站元数据里的一个字段（读面靠它过滤控制回执 / 别人的绑定）。
    #[must_use]
    pub fn metadata_field(&self, key: &str) -> String {
        self.metadata_event_payload
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// 是否带**本仓**的出站元数据。
    #[must_use]
    pub fn is_our_outbound(&self) -> bool {
        self.metadata_event_type == OUTBOUND_METADATA_EVENT
    }
}

impl std::fmt::Debug for SlackTarget {
    /// 手写脱敏（凭据纪律第 1 条）：`bot_token` 只报存在性。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SlackTarget")
            .field("bot_token", &"<redacted>")
            .field("binding_id", &self.binding_id)
            .field("channel_id", &self.channel_id)
            .field("thread_root", &self.thread_root)
            .field("bot_user_id", &self.bot_user_id)
            .field("route_revision", &self.route_revision)
            .finish_non_exhaustive()
    }
}

pub(super) fn array_at(value: &Value, key: &str) -> Vec<Value> {
    value
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// 一个 Slack 用户（上游 `slack.User` 的契约子集）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SlackUser {
    pub id: String,
    /// `profile.display_name`。
    pub display_name: String,
    pub real_name: String,
    /// `name`（handle）。
    pub name: String,
}

// =====================================================================
// 端口
// =====================================================================

/// Slack 读面的三个 Web API 方法（上游 `historyClient`）。
#[async_trait]
pub trait HistoryApi: Send + Sync {
    /// `conversations.history`。
    async fn conversation_history(
        &self,
        token: &str,
        channel: &str,
        window: &HistoryWindow,
    ) -> ApiResult<Vec<SlackMessage>>;

    /// `conversations.replies`（`window.ts` 是线程根）。
    async fn conversation_replies(
        &self,
        token: &str,
        channel: &str,
        window: &HistoryWindow,
    ) -> ApiResult<Vec<SlackMessage>>;

    /// `users.info`（可能一次问多个 id）。
    async fn users_info(&self, token: &str, ids: &[String]) -> ApiResult<Vec<SlackUser>>;
}

/// 一次拉取的时间窗（上游 `GetConversationHistoryParameters` 的四项）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HistoryWindow {
    pub channel: String,
    pub ts: String,
    pub latest: String,
    pub oldest: String,
    pub inclusive: bool,
    pub limit: i64,
}

impl HistoryWindow {
    fn to_body(&self, method: &'static str) -> Value {
        let _ = method;
        let mut body = serde_json::json!({
            "channel": self.channel,
            "limit": self.limit,
            "inclusive": self.inclusive,
            "include_all_metadata": true,
        });
        if let Some(map) = body.as_object_mut() {
            if !self.ts.is_empty() {
                map.insert("ts".to_string(), Value::String(self.ts.clone()));
            }
            if !self.latest.is_empty() {
                map.insert("latest".to_string(), Value::String(self.latest.clone()));
            }
            if !self.oldest.is_empty() {
                map.insert("oldest".to_string(), Value::String(self.oldest.clone()));
            }
        }
        body
    }
}

/// 生产实现：三个方法直连（基址见 [`crate::slack::outbound::api_base`]）。
#[derive(Debug, Default, Clone)]
pub struct HttpHistoryApi;

impl HttpHistoryApi {
    fn messages_of(body: &Value) -> Vec<SlackMessage> {
        body.get("messages")
            .and_then(Value::as_array)
            .map(|items| items.iter().map(SlackMessage::from_json).collect())
            .unwrap_or_default()
    }
}

#[async_trait]
impl HistoryApi for HttpHistoryApi {
    async fn conversation_history(
        &self,
        token: &str,
        _channel: &str,
        window: &HistoryWindow,
    ) -> ApiResult<Vec<SlackMessage>> {
        let body = HttpSlackApi::call(
            "conversations.history",
            token,
            window.to_body("conversations.history"),
        )
        .await?;
        Ok(Self::messages_of(&body))
    }

    async fn conversation_replies(
        &self,
        token: &str,
        _channel: &str,
        window: &HistoryWindow,
    ) -> ApiResult<Vec<SlackMessage>> {
        let body = HttpSlackApi::call(
            "conversations.replies",
            token,
            window.to_body("conversations.replies"),
        )
        .await?;
        Ok(Self::messages_of(&body))
    }

    async fn users_info(&self, token: &str, ids: &[String]) -> ApiResult<Vec<SlackUser>> {
        let body = HttpSlackApi::call(
            "users.info",
            token,
            serde_json::json!({ "users": ids.join(",") }),
        )
        .await?;
        let members = body
            .get("users")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(members
            .iter()
            .map(|member| SlackUser {
                id: member
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                display_name: member
                    .pointer("/profile/display_name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                real_name: member
                    .get("real_name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                name: member
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
            .collect())
    }
}

/// 会话绑定的一行（上游 `db.ChannelChatSessionBinding` 的读面子集）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BindingSnapshot {
    pub id: Id,
    pub installation_id: Id,
    /// 复合隔离键 `channel#threadRoot`（M7-3-D1）。
    pub channel_chat_id: String,
    /// 隔离键前缀里的**真实**频道 id。
    pub channel_id: String,
    pub last_thread_id: String,
    pub history_start_message_id: String,
    pub history_end_message_id: String,
    pub history_boundary_pending: bool,
    pub route_revision: i64,
}

/// 安装行的一行（读面只关心密文 config 与是否还活跃）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InstallationSnapshot {
    pub config: Value,
    pub active: bool,
}

/// 一条出站记账（上游 `ListChannelOutboundMessagesByIDs` 的那三列）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundRow {
    pub channel_message_id: String,
    pub outbound_kind: String,
    pub binding_id: Id,
}

/// 读面要的三类存储。
#[async_trait]
pub trait HistoryStore: Send + Sync {
    /// 会话当前的 Slack 绑定；没有 ⇒ `Ok(None)`。
    async fn current_binding(&self, session_id: Id) -> Result<Option<BindingSnapshot>, String>;
    /// 安装行；不存在 ⇒ `Ok(None)`。
    async fn installation(
        &self,
        installation_id: Id,
    ) -> Result<Option<InstallationSnapshot>, String>;
    /// 某个绑定 + 路由代际下已投递的出站消息。
    async fn outbound_for_binding(
        &self,
        binding_id: Id,
        route_revision: i64,
    ) -> Result<Vec<OutboundRow>, String>;
}

/// 生产形态：三个泛化渠道仓储（**零新 SQL** —— 三条查询都已存在）。
///
/// 不派生 `Debug`（仓储不实现它，且它没有任何"要打印才有用"的状态）。
#[derive(Clone)]
pub struct RepoHistoryStore {
    sessions: ChannelChatSessionRepo,
    installations: ChannelInstallationRepo,
    outbound: ChannelOutboundRepo,
}

impl RepoHistoryStore {
    /// 装配。
    #[must_use]
    pub fn new(
        sessions: ChannelChatSessionRepo,
        installations: ChannelInstallationRepo,
        outbound: ChannelOutboundRepo,
    ) -> Self {
        Self {
            sessions,
            installations,
            outbound,
        }
    }
}

#[async_trait]
impl HistoryStore for RepoHistoryStore {
    async fn current_binding(&self, session_id: Id) -> Result<Option<BindingSnapshot>, String> {
        let found = self
            .sessions
            .get_current_binding_by_session(session_id)
            .await
            .map_err(|error| error.to_string())?;
        let Some(row) = found else {
            return Ok(None);
        };
        Ok(Some(BindingSnapshot {
            id: row.id(),
            installation_id: row.installation_id(),
            channel_id: binding_channel_id(&row.channel_chat_id, &row.config),
            channel_chat_id: row.channel_chat_id.clone(),
            last_thread_id: row.last_thread_id.clone().unwrap_or_default(),
            history_start_message_id: row.history_start_message_id.clone().unwrap_or_default(),
            history_end_message_id: row.history_end_message_id.clone().unwrap_or_default(),
            history_boundary_pending: row.history_boundary_pending,
            route_revision: row.route_revision,
        }))
    }

    async fn installation(
        &self,
        installation_id: Id,
    ) -> Result<Option<InstallationSnapshot>, String> {
        match self.installations.get(installation_id).await {
            Ok(row) => Ok(Some(installation_snapshot(&row))),
            Err(RepoError::NotFound) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    async fn outbound_for_binding(
        &self,
        binding_id: Id,
        route_revision: i64,
    ) -> Result<Vec<OutboundRow>, String> {
        self.outbound
            .list_by_binding(binding_id, route_revision)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| {
                        let binding_id = row.binding_id();
                        OutboundRow {
                            channel_message_id: row.channel_message_id,
                            outbound_kind: row.outbound_kind,
                            binding_id,
                        }
                    })
                    .collect()
            })
            .map_err(|error| error.to_string())
    }
}

fn installation_snapshot(row: &ChannelInstallationRow) -> InstallationSnapshot {
    InstallationSnapshot {
        config: row.config.clone(),
        active: row.status == "active",
    }
}

/// 从隔离键与绑定 config 里取**真实**频道 id（上游 `historyTarget` 的前半）。
///
/// 本仓的隔离键是 `channel#threadRoot`（M7-3-D1）⇒ 真实频道 id 取 `#` 之前那一段；
/// 绑定的 `config.channel_id`（上游形态）优先。
#[must_use]
pub fn binding_channel_id(channel_chat_id: &str, config: &Value) -> String {
    if let Some(found) = config.get("channel_id").and_then(Value::as_str) {
        if !found.is_empty() {
            return found.to_string();
        }
    }
    channel_chat_id
        .split_once('#')
        .map_or_else(|| channel_chat_id.to_string(), |(head, _)| head.to_string())
}

/// 单线程的根（上游 `historyTarget` 的后半）：记录线程优先，否则隔离键的后半。
#[must_use]
pub fn binding_thread_root(binding: &BindingSnapshot) -> String {
    if !binding.last_thread_id.is_empty() {
        return binding.last_thread_id.clone();
    }
    binding
        .channel_chat_id
        .split_once('#')
        .map_or_else(String::new, |(_, tail)| tail.to_string())
}

// =====================================================================
// 读面
// =====================================================================

/// 一次读的解析上下文（上游 `slackTarget`）。
#[derive(Clone, PartialEq)]
pub struct SlackTarget {
    /// 本安装的明文 bot token（**绝不**进日志；`Debug` 由 `#[derive]` 打印它 —— 故本结构
    /// **不**得出现在任何日志调用里，这也是它为什么叫 "target" 而不是 "binding"）。
    pub bot_token: String,
    pub binding_id: Id,
    pub channel_id: String,
    pub thread_root: String,
    pub bot_user_id: String,
    pub history_start: String,
    pub history_end: String,
    pub boundary_pending: bool,
    pub route_revision: i64,
}
// =====================================================================
// 子模块（门 ⑩：单文件 ≤800 行 ⇒ 按「读面 / 窗口与过滤 / 摊平与命名」拆开）
// =====================================================================

mod flatten;
mod reader;
mod text;

pub use flatten::{
    attachment_text, flatten_blocks, flatten_slack_text, resolve_user_names, rich_text_lines,
    slack_display_name, truncate_chars, HistoryLabeler,
};
pub use reader::History;
pub use text::{
    clamp_history_limit, filter_context_generation, filter_route_generation, history_bounds,
    history_cursor_reached_start, history_next_cursor, history_window, slack_ts_less,
    strip_bot_mention,
};

#[cfg(test)]
mod tests;
