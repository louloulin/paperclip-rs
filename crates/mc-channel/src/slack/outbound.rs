//! Slack 出站面：`chat.postMessage` + Markdown→mrkdwn + 分片 + 线程化
//! （上游 `internal/integrations/slack/outbound.go` 244 行 + `channel.go` 125 行的 **sender 那一半**）。
//!
//! - **写者**：M7-4（`docs/60-M7-PLAN.md` §3.3；`channel.go` 的 sender 归本片 —— M7-3 只落
//!   `slack_channel.go` 的接收循环，并在 `socket.rs` 把 `Channel::send` **失败关闭**
//!   （M7-3-D6），那半条出站链路的落地点就是本文件）。
//! - **上游形态**：`slackSender` 只持**一个**安装的 bot token（`xoxb-`）；入站跑在
//!   每个安装自己的 Socket Mode 连接上（`socket.rs`），出站走 Web API。所以本文件
//!   **不**查库、**不**解析安装 —— 令牌由调用方给。
//!
//! # 三条从上游逐字搬来的形态
//!
//! 1. **先 mrkdwn 再分片**：`format_mrkdwn` 在切分**之前**跑（`channel.go` 的循环体是
//!    `for _, chunk := range chunkMessage(formatMrkdwn(out.Text), maxMessageRunes)`），
//!    否则一个 Markdown 结构会被切在两半；
//! 2. **分片按 rune**（不是一个「按行/按词」的智能切分）：上游是裸的每 `38_000` rune 一段；
//!    本仓逐字照搬，不做「更聪明」的改写（那会改变 wire 形态）；
//! 3. **`MessageID` = 最后一片的 ts**、`MessageIDs` = 全部分片（上游注释逐字）；
//!    与 `mc_core::channel::message::SendResult::chunked` 的「取第一片」**不同** ⇒
//!    这里手工构造，不用那个便捷函数（登记 `docs/32` §15）。
//!
//! # 线程落点（上游 `outboundThreadTS`）
//!
//! 显式引用目标（`reply_to`）优先，否则用入站消息所属线程（`thread_id`）。两者都空 = 会话层发送。
//!
//! # `metadata` 的作用不只是装饰
//!
//! 每条出站帧带 `metadata.event_type = "multica_channel_outbound"` +
//! `{binding_id, route_revision, kind}`。**历史读面靠它区分「谁发的这条」**：
//! `history.rs` 的 `filterRouteGeneration` 正是按 `kind == "control_ack"` 与
//! `binding_id != 本会话绑定` 丢弃别人的控制回执。所以这是协议的一部分，不是可选装饰。
//!
//! # 凭据纪律（`docs/60` §2.3）
//!
//! 明文令牌只以 `&str` 形参在调用链里流动，**不进任何结构体字段**、不进 `Debug`、不进日志；
//! [`SlackApiError`] 的每个变体只带 Slack 自己的错误码（`invalid_auth` 一类），
//! 而**绝不**带请求头。

use std::fmt;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use mc_core::channel::message::OutboundMessage;
use mc_core::id::Id;
use serde_json::{json, Value};

// =====================================================================
// wire 常量（上游 `channel.go` / `outbound.go`）
// =====================================================================

/// Slack Web API 的基址（上游由 SDK 定；本仓显式给出，测试可指向本地替身）。
pub const DEFAULT_API_BASE: &str = "https://slack.com/api";

/// 出站元数据的 `event_type`（上游 `slackOutboundMetadataEvent`，**逐字**）。
///
/// 读面按它认「这条是我们发的」⇒ 值不能改（改了 `history.rs` 的过滤会静默失效）。
pub const OUTBOUND_METADATA_EVENT: &str = "multica_channel_outbound";

/// 单条 `chat.postMessage` 的正文上限（上游 `maxMessageRunes = 38000`，逐字）。
pub const MAX_MESSAGE_RUNES: usize = 38_000;

/// 出站种类字面量（上游调用点给的字面量，读面按它过滤）。
pub mod kind {
    /// agent 回复（`outbound.go`）。
    pub const TASK_REPLY: &str = "task_reply";
    /// 控制回执 / 离线提示 / 绑定卡（`replier.go`）。
    pub const CONTROL_ACK: &str = "control_ack";
    /// `/issue` 确认（`replier.go`）。
    pub const ISSUE_ACK: &str = "issue_ack";
}

// =====================================================================
// API 基址接缝（测试可注入；生产**不得**调用 setter）
// =====================================================================

/// 进程内可注入的 API 基址（`None` = [`DEFAULT_API_BASE`]）。
///
/// 与 `mc_http::routes::github::install` 的 `GITHUB_API_BASE` 同款：本文件的自有用例
/// 用本地 axum 替身跑**真** HTTP，而不是把 `MessageApi` 换成一个返回常量的假实现。
/// 进程全局 ⇒ 依赖它的用例必须串行（见 `outbound/tests.rs` 的锁）。
static API_BASE: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn api_base_slot() -> &'static Mutex<Option<String>> {
    API_BASE.get_or_init(|| Mutex::new(None))
}

/// 当前生效的 API 基址。
#[must_use]
pub fn api_base() -> String {
    api_base_slot()
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
        .unwrap_or_else(|| DEFAULT_API_BASE.to_string())
}

/// 注入基址（**只给测试用**；生产代码不得调用）。
pub fn set_api_base(base: impl Into<String>) {
    if let Ok(mut guard) = api_base_slot().lock() {
        *guard = Some(base.into());
    }
}

/// 清掉注入的基址。
pub fn reset_api_base() {
    if let Ok(mut guard) = api_base_slot().lock() {
        *guard = None;
    }
}

// =====================================================================
// 错误
// =====================================================================

/// Slack Web API 失败（上游 `slack-go` 的 error）。
///
/// 变体**只带 Slack 自己的错误码**（或 HTTP 状态码）—— 这是「错误路径不回显凭据」的
/// 结构性保证：请求头里的 `Bearer xoxb-…` 在类型层面就进不了这里。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SlackApiError {
    /// 传输层失败（DNS / 连接 / 超时）。**不带 URL**：Slack 的 URL 上可能带票据
    /// （`apps.connections.open` 返回的 `wss://` 就自带票据，`socket.rs` 已有同款纪律）。
    #[error("slack: transport failure on {method}")]
    Transport { method: &'static str },
    /// 响应不是 JSON 或形状不对。
    #[error("slack: {method} returned a malformed body")]
    Malformed { method: &'static str },
    /// `{"ok": false, "error": "<code>"}`（只带该错误码）。
    #[error("slack: {method} refused: {code}")]
    Refused { method: &'static str, code: String },
    /// 非 2xx 且 body 不可解析。
    #[error("slack: {method} returned HTTP {status}")]
    Http { method: &'static str, status: u16 },
}

impl SlackApiError {
    /// 稳定码（路由 / 看板聚合用的**类别**；**不含**任何凭据）。
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Transport { .. } => "transport",
            Self::Malformed { .. } => "malformed",
            Self::Refused { .. } => "refused",
            Self::Http { .. } => "http_status",
        }
    }

    /// 给用户看的**具体**原因：Slack 拒绝时是它自己的错误码（`invalid_auth` 一类），
    /// 其余情况是类别码。**永不含**令牌 / URL。
    ///
    /// 单独一个方法（而不是让调用方 `match`）的理由：HTTP 层的 400 文案要"能指路"
    /// ——「check the bot token」只有在带上 Slack 的 `invalid_auth` 时才说得通。
    #[must_use]
    pub fn detail(&self) -> String {
        match self {
            Self::Refused { code, .. } => code.clone(),
            other => other.code().to_string(),
        }
    }
}

/// 端口失败的统一结果别名。
pub type ApiResult<T> = Result<T, SlackApiError>;

// =====================================================================
// 请求 / 响应形状
// =====================================================================

/// 一条出站帧的元数据（上游 `slack.SlackMetadata` 的契约子集）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundMetadata {
    /// 会话绑定 id（`channel_chat_session_binding.id`）。
    pub binding_id: Option<Id>,
    /// 路由代际（上游 `route_revision`）。
    pub route_revision: i64,
    /// 出站种类（见 [`kind`]）。
    pub kind: String,
}

impl OutboundMetadata {
    /// 装配。
    #[must_use]
    pub fn new(binding_id: Option<Id>, route_revision: i64, kind: impl Into<String>) -> Self {
        Self {
            binding_id,
            route_revision,
            kind: kind.into(),
        }
    }

    /// 线的形态（上游 `outboundMetadata`）—— `binding_id` 缺席时是空串（上游零值）。
    #[must_use]
    pub fn to_payload(&self) -> Value {
        json!({
            "event_type": OUTBOUND_METADATA_EVENT,
            "event_payload": {
                "binding_id": self.binding_id.map(|id| id.to_string()).unwrap_or_default(),
                "route_revision": self.route_revision,
                "kind": self.kind,
            }
        })
    }
}

/// `chat.postMessage` 的入参（上游 `slack.MsgOption*` 那几项的**结论**）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostMessageRequest {
    pub channel: String,
    pub text: String,
    /// 非空 = 挂进该线程（上游 `MsgOptionTS`）。
    pub thread_ts: Option<String>,
    /// 非空 = 带 `metadata`（上游 `MsgOptionMetadata`）。
    pub metadata: Option<OutboundMetadata>,
}

/// Slack 的 `metadata` 上限是 50 个键；本仓只用固定两个（上游同）。
impl PostMessageRequest {
    /// 请求体（上游 SDK 的线形态：`channel` / `text` / `thread_ts` / `metadata`；
    /// `unfurl_links`/`unfurl_media` 恒 false，对应 `MsgOptionDisableLinkUnfurl`）。
    #[must_use]
    pub fn to_body(&self) -> Value {
        let mut body = json!({
            "channel": self.channel,
            "text": self.text,
            "unfurl_links": false,
            "unfurl_media": false,
        });
        if let Some(thread_ts) = &self.thread_ts {
            if let Some(map) = body.as_object_mut() {
                map.insert("thread_ts".to_string(), json!(thread_ts));
            }
        }
        if let Some(metadata) = &self.metadata {
            if let Some(map) = body.as_object_mut() {
                map.insert("metadata".to_string(), metadata.to_payload());
            }
        }
        body
    }
}

/// 一条消息的定位（Slack 用 `(channel, ts)` 寻址：**没有** reaction id 可存）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRef {
    pub channel: String,
    pub timestamp: String,
}

// =====================================================================
// 端口
// =====================================================================

/// 发一条消息（上游 `replySender` / `slackSender.Send`）。
///
/// 返回新消息的 `ts`。实现必须**不**把令牌写进错误。
#[async_trait]
pub trait MessageApi: Send + Sync {
    /// `chat.postMessage`。
    async fn post_message(&self, token: &str, req: &PostMessageRequest) -> ApiResult<String>;
}

/// 生产实现：`reqwest` 直连 Slack Web API（基址见 [`api_base`]）。
#[derive(Debug, Default, Clone)]
pub struct HttpSlackApi;

impl HttpSlackApi {
    /// POST 一个 Web API 方法并解开 `{"ok": …}` 信封。
    ///
    /// 公共解析点：四条端口（消息 / 反应 / 安装 / 历史）都走这里，
    /// 于是「错误文案只带 Slack 错误码」只有**一处**实现，不会各写各的。
    pub(crate) async fn call(method: &'static str, token: &str, body: Value) -> ApiResult<Value> {
        let url = format!("{}/{}", api_base(), method);
        let response = reqwest::Client::new()
            .post(&url)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(|_| SlackApiError::Transport { method })?;
        let status = response.status().as_u16();
        let parsed: Option<Value> = response.json().await.ok();
        match parsed {
            Some(value) if value.get("ok").and_then(Value::as_bool) == Some(true) => Ok(value),
            Some(value) => {
                let code = value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown_error")
                    .to_string();
                Err(SlackApiError::Refused { method, code })
            }
            None if status >= 400 => Err(SlackApiError::Http { method, status }),
            None => Err(SlackApiError::Malformed { method }),
        }
    }
}

#[async_trait]
impl MessageApi for HttpSlackApi {
    async fn post_message(&self, token: &str, req: &PostMessageRequest) -> ApiResult<String> {
        let body = Self::call("chat.postMessage", token, req.to_body()).await?;
        body.get("ts")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or(SlackApiError::Malformed {
                method: "chat.postMessage",
            })
    }
}

// =====================================================================
// 分片 / 线程落点（上游纯函数）
// =====================================================================

/// 按 rune 边界把正文切成 ≤`MAX_MESSAGE_RUNES` 的若干段（上游 `chunkMessage`，逐字）。
///
/// 空串给**一段空串**（上游同：调用方自己保证不空发）。
#[must_use]
pub fn chunk_message(text: &str, max_runes: usize) -> Vec<String> {
    if max_runes == 0 {
        return vec![text.to_string()];
    }
    let runes: Vec<char> = text.chars().collect();
    if runes.len() <= max_runes {
        return vec![text.to_string()];
    }
    runes
        .chunks(max_runes)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

/// 出站回复落到哪个线程（上游 `outboundThreadTS`）：显式引用优先，否则入站线程。
#[must_use]
pub fn outbound_thread_ts(out: &OutboundMessage) -> Option<String> {
    if !out.reply_to.is_empty() {
        return Some(out.reply_to.clone());
    }
    if !out.thread_id.is_empty() {
        return Some(out.thread_id.clone());
    }
    None
}

// =====================================================================
// sender
// =====================================================================

/// 只做「发」的客户端（上游 `slackSender`）：持一批 API 端口，令牌逐次传入。
///
/// 与上游的一处形态差异（**登记 `docs/32` §15**）：上游把 `*slack.Client`（绑好令牌的
/// 客户端）当字段持有；本仓的端口是**无状态**的，令牌作为形参传入 —— 于是同一个
/// `Sender` 可以服务多个安装，且「令牌不进字段」在类型层面成立（凭据纪律第 1 条）。
/// 分片 / mrkdwn / 线程落点 / 元数据这些**语义**逐字不变。
pub struct Sender {
    api: Arc<dyn MessageApi>,
    max_runes: usize,
}

impl fmt::Debug for Sender {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Sender")
            .field("api", &"<dyn MessageApi>")
            .field("max_runes", &self.max_runes)
            .finish()
    }
}

impl Sender {
    /// 装配（显式端口）。
    #[must_use]
    pub fn new(api: Arc<dyn MessageApi>) -> Self {
        Self {
            api,
            max_runes: MAX_MESSAGE_RUNES,
        }
    }

    /// 生产形态（`reqwest` 直连）。
    #[must_use]
    pub fn http() -> Self {
        Self::new(Arc::new(HttpSlackApi))
    }

    /// 分片上限（用例把它压小，好钉住"分片后每片仍然线程化"）。
    #[must_use]
    pub fn with_max_runes(mut self, max_runes: usize) -> Self {
        self.max_runes = max_runes;
        self
    }

    /// 发一条回复（上游 `Send`：无元数据）。
    pub async fn send(&self, token: &str, out: &OutboundMessage) -> ApiResult<OutboundFrame> {
        self.send_with_metadata(token, out, None).await
    }

    /// 发一条带元数据的回复（上游 `SendWithMetadata`）。
    ///
    /// 顺序逐字照上游：**先** mrkdwn、**再**分片、**每片**都带同一份 `metadata` 与
    /// 同一个 `thread_ts`。任一片失败 ⇒ 整体失败（上游同：`return SendResult{}, err`），
    /// 但**已经发出去的那几片不会撤回**（Slack 侧没有回滚），调用方按"部分投递"处理。
    pub async fn send_with_metadata(
        &self,
        token: &str,
        out: &OutboundMessage,
        metadata: Option<&OutboundMetadata>,
    ) -> ApiResult<OutboundFrame> {
        let thread_ts = outbound_thread_ts(out);
        let rendered = crate::slack::mrkdwn::format_mrkdwn(&out.text);
        let mut timestamps = Vec::new();
        for chunk in chunk_message(&rendered, self.max_runes) {
            let request = PostMessageRequest {
                channel: out.chat_id.clone(),
                text: chunk,
                thread_ts: thread_ts.clone(),
                metadata: metadata.cloned(),
            };
            let ts = self.api.post_message(token, &request).await?;
            timestamps.push(ts);
        }
        Ok(OutboundFrame { timestamps })
    }
}

/// 一次发送的结果（上游 `channel.SendResult` 的等价物）。
///
/// `message_id` 是**最后一片**的 ts（上游逐字），`message_ids` 是全部 —— 与
/// `SendResult::chunked` 的「取第一片」不同，所以这里手工构造。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OutboundFrame {
    pub timestamps: Vec<String>,
}

impl OutboundFrame {
    /// 转成 engine 的 `SendResult`（`MessageID` = 末片，`MessageIDs` = 全部）。
    #[must_use]
    pub fn to_send_result(&self) -> mc_core::channel::message::SendResult {
        mc_core::channel::message::SendResult {
            message_id: self.timestamps.last().cloned().unwrap_or_default(),
            message_ids: self.timestamps.clone(),
        }
    }

    /// 平台是否给了至少一个 id（上游 `outbound.go` 在缺失时判基础设施失败）。
    #[must_use]
    pub fn has_any_id(&self) -> bool {
        !self.timestamps.is_empty()
    }
}

#[cfg(test)]
mod tests;
