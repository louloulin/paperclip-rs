//! Telegram Bot API 的**端口 + 最小 HTTP 客户端**（上游 `internal/integrations/telegram/api.go`，
//! 304 行）。
//!
//! - **写者**：M7-5（`LUM-1770`；`docs/60-M7-PLAN.md` §3.3 把 `api.rs` 记在 M7-6 名下，本片
//!   **先落它的入站/校验/判决回复半边**并登记写集勘误 `docs/32` §17.1 —— 理由见下）。
//! - **为什么由 M7-5 落**：M7-5 的三处调用（入站长轮询 `getUpdates`、安装校验 `getMe` +
//!   `getWebhookInfo`、判决回复 `sendMessage`）都在这条传输上，而 **M7-6 的硬前置是 M7-5**
//!   ⇒ 依赖方向只能是"M7-5 落传输、M7-6 在同一文件里补出站流式那一半"（与 M7-3 先落
//!   `slack/socket.rs`、M7-4 再接线 `send` 是同一条先例）。anchor 的 `mc-channel/Cargo.toml`
//!   注释里也已经把出站 HTTP 的点名写成 "telegram `api.rs`"。
//! - **交给 M7-6 的缺口（逐条，**已由 `LUM-1771` 补齐**）**：`editMessageText`（流式编辑）、
//!   `sendMessage` 的 `parse_mode=HTML` 形态与分片、429 的"一次重试"包装
//!   （[`send_message_with_retry_after`]，上游 `sendMessageWithRetryAfter`）、以及 `sender.rs`
//!   的 UTF-16 分片。本文件**已经把** [`TelegramApi::retry_after`] 与 `parse_mode` 字段摆好，
//!   M7-6 只**加方法**，没有改任何既有方法的 wire 形态。
//!
//! # 端口为什么是**一个**五方法 trait
//!
//! 上游的 `botAPI` 就是一个结构体上的五个方法（`getMe` / `getWebhookInfo` / `getUpdates` /
//! `sendMessage` / `sendChatAction`），且**每个安装的 token 是形参**（不是字段）：
//! 本仓照这个形态落，于是「令牌不进结构体字段」这条凭据纪律在**类型层面**成立
//! （同一个客户端服务 5 个平台的任意多个安装）。
//!
//! # 基址接缝（**测试**可注入；生产不得调用 setter）
//!
//! 与 `crate::slack::outbound::{set_api_base,…}` / M8-1 的 `GITHUB_API_BASE` 同款：本文件的
//! 用例与 `crates/mc-http/tests/channels/telegram.rs` 的端到端回路都跑**真** HTTP 对本地
//! 替身，而不是把 [`TelegramApi`] 换成一个返回常量的假实现。进程全局 ⇒ 依赖它的用例必须
//! 串行。
//!
//! # 凭据纪律（`docs/60` §2.3）
//!
//! Bot API 的请求 URL 里**带 bot token**（`/bot<token>/<method>`），所以：
//!
//! 1. [`ApiError`] 的每个变体只带**方法名** / Telegram 自己的错误码与描述，**绝不**带 URL
//!    —— 传输失败的原始 `reqwest` 错误**被丢弃**（它的 `Display` 会印出整条 URL；登记为
//!    `docs/32` §17.2 的偏离，与 `slack::outbound::SlackApiError::Transport` 同款）；
//! 2. 本文件**没有任何** `tracing::*` 插值 token / URL；
//! 3. `Debug` 派生是安全的：[`JsonBotApi`] 只持两个 `reqwest::Client`。

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;

use crate::telegram::inbound::{Message, Update, User};

/// 生产 Bot API 主机（上游 `defaultAPIBase`）。测试把它指向本地替身。
pub const DEFAULT_API_BASE: &str = "https://api.telegram.org";

/// `getUpdates` 的服务端挂起秒数（上游 `longPollTimeoutSecs = 50`）。
///
/// 50s 既在常见的 60s 代理 / LB 空闲超时之内，又能把请求数压得很低。
pub const LONG_POLL_TIMEOUT_SECS: u16 = 50;

/// 长轮询客户端的总超时：必须**大于** [`LONG_POLL_TIMEOUT_SECS`]，否则我们会先于 Telegram
/// 掐掉自己（上游默认客户端的 65s 同量级）。
pub const POLL_TIMEOUT: Duration = Duration::from_secs(65);

/// 校验类调用（`getMe` / `getWebhookInfo`）的超时（上游 `credentialVerificationTimeout = 15s`）。
pub const CREDENTIAL_TIMEOUT: Duration = Duration::from_secs(15);

// =====================================================================
// API 基址接缝
// =====================================================================

/// 进程内可注入的 API 基址（`None` = [`DEFAULT_API_BASE`]）。
static API_BASE: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn api_base_slot() -> &'static Mutex<Option<String>> {
    API_BASE.get_or_init(|| Mutex::new(None))
}

/// 当前生效的 API 基址（末尾无 `/`）。
#[must_use]
pub fn api_base() -> String {
    api_base_slot()
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
        .unwrap_or_else(|| DEFAULT_API_BASE.to_string())
        .trim_end_matches('/')
        .to_string()
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

/// Bot API 失败（上游 `*apiError` / `*requestError` / 解码错误的合并投影）。
///
/// 变体**只带方法名与 Telegram 自己的错误码 / 描述**：请求 URL（带 token）在类型层面就进不了
/// 这里（凭据纪律第 1 条）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiError {
    /// 传输层失败（DNS / 连接 / 超时 / 响应体读不出来）。**不带 URL**（见模块文档）。
    #[error("telegram: {method} request failed")]
    Transport { method: &'static str },
    /// 响应不是 JSON envelope，或 `result` 的形状与本 adapter 的 wire 类型对不上。
    #[error("telegram: {method} returned a malformed body")]
    Malformed { method: &'static str },
    /// `{"ok": false, …}`（只带 Telegram 自己的错误码与描述；描述里没有凭据）。
    #[error("telegram api: {code} {description}")]
    Api {
        method: &'static str,
        code: u16,
        description: String,
        /// Telegram 在 `parameters.retry_after` 里给的强制退避（秒）；只有 429 会带。
        retry_after: Option<u64>,
    },
    /// `getUpdates` 的 **409**：同一个 bot token 正被**另一个**消费者轮询（另一副本、另一个
    /// workdir、或一个外部进程）。退避**修不好**这件事，但运维能 —— 所以它是一个**独立**的
    /// 变体，让轮询回路能给出准确文案而不是无休止重连。
    #[error("telegram: bot is already being polled by another instance (409 conflict)")]
    Conflict,
}

impl ApiError {
    /// 方法名（日志 / 诊断用；**不含**凭据）。
    #[must_use]
    pub fn method(&self) -> &'static str {
        match self {
            Self::Transport { method } | Self::Malformed { method } | Self::Api { method, .. } => {
                method
            }
            Self::Conflict => "getUpdates",
        }
    }

    /// Telegram 强制的一次退避（上游 `retryAfter`）：只有 429 会给出 `Some`。
    ///
    /// 429 但没带 `parameters.retry_after` 时按 1 秒处理（上游逐字）。
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Api {
                code: 429,
                retry_after,
                ..
            } => {
                let secs = retry_after.unwrap_or(1).max(1);
                Some(Duration::from_secs(secs))
            }
            _ => None,
        }
    }

    /// Bot API 给出的 HTTP 状态码（`Transport` / `Malformed` / `Conflict` 无）。
    #[must_use]
    pub fn http_code(&self) -> Option<u16> {
        match self {
            Self::Api { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// 是否是"另一个消费者在轮询这个 bot"（409）。
    #[must_use]
    pub fn is_conflict(&self) -> bool {
        matches!(self, Self::Conflict)
    }
}

/// 本文件的 `Result` 别名。
pub type ApiResult<T> = Result<T, ApiError>;

// =====================================================================
// 端口
// =====================================================================

/// 一个安装的 **webhook 状态**（上游 `WebhookInfo` 的子集）。
///
/// 长轮询与 webhook **互斥**：装的时候发现 `url` 非空就必须拒（[`crate::telegram::install`]）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct WebhookInfo {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub pending_update_count: i64,
}

/// `sendMessage` 的入参（上游 `sendMessageParams`）。
///
/// `parse_mode` 由**调用方**决定：判决回复走纯文本（留空），出站回复器（M7-6）走
/// `HTML`。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SendMessage {
    pub chat_id: i64,
    pub text: String,
    /// `"HTML"` / `"MarkdownV2"`；空 = 纯文本。
    pub parse_mode: String,
    /// 论坛话题（0 = 不带）。
    pub message_thread_id: i64,
    /// 引用回复的 platform message id（0 = 不引用）。
    pub reply_to_message_id: i64,
    /// 被引用消息被删时仍然发出去（上游 `AllowSendingWithoutReply`）。
    pub allow_sending_without_reply: bool,
}

impl SendMessage {
    /// 纯文本发送（判决回复的形态）。
    #[must_use]
    pub fn text(chat_id: i64, text: impl Into<String>) -> Self {
        Self {
            chat_id,
            text: text.into(),
            ..Self::default()
        }
    }

    /// 带上论坛话题（`message_thread_id != 0` 才有意义）。
    #[must_use]
    pub fn in_thread(mut self, message_thread_id: i64) -> Self {
        self.message_thread_id = message_thread_id;
        self
    }

    /// 引用回复（`allow_sending_without_reply` 由上游的 `optionalReplyParameters` 固定为真；
    /// `0` ⇒ 不加引用参数，与上游零值语义一致）。
    #[must_use]
    pub fn with_reply_to(mut self, message_id: i64) -> Self {
        if message_id != 0 {
            self.reply_to_message_id = message_id;
            self.allow_sending_without_reply = true;
        }
        self
    }
}

/// `editMessageText` 的入参（上游 `editMessageTextParams`）。
///
/// 这是**流式输出**的原语：Telegram 没有 stream-update 协议，"流式"= 先发一条占位消息，
/// 再按节流节奏反复替换它的正文。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditMessageText {
    pub chat_id: i64,
    /// 要替换的那条消息的 platform message id。
    pub message_id: i64,
    pub text: String,
    /// `"HTML"` / `"MarkdownV2"`；空 = 纯文本。
    pub parse_mode: String,
}

impl EditMessageText {
    /// 纯文本替换。
    #[must_use]
    pub fn text(chat_id: i64, message_id: i64, text: impl Into<String>) -> Self {
        Self {
            chat_id,
            message_id,
            text: text.into(),
            parse_mode: String::new(),
        }
    }

    /// 走 HTML parse mode（出站流式那条路径用的形态）。
    #[must_use]
    pub fn html(mut self) -> Self {
        self.parse_mode = "HTML".to_string();
        self
    }
}

/// Telegram Bot API 的**端口**（上游 `botAPI` 的五个方法）。
///
/// 每个方法的第一个形参都是**本安装的 bot token**（不是结构体字段）：入站回路、安装服务与
/// 判决回复器共用同一个实现，而"令牌不进字段"是凭据纪律的结构性保证。
#[async_trait]
pub trait TelegramApi: Send + Sync {
    /// `getMe`：校验 token 并拿 bot 自己的身份（安装时的第一步校验）。
    async fn get_me(&self, bot_token: &str) -> ApiResult<User>;

    /// `getWebhookInfo`：检测会让 `getUpdates` 不可用的 outgoing webhook。
    async fn get_webhook_info(&self, bot_token: &str) -> ApiResult<WebhookInfo>;

    /// `getUpdates`：长轮询（服务端最多挂 [`LONG_POLL_TIMEOUT_SECS`] 秒）。
    ///
    /// 只订阅 `message` 更新（更少的唤醒，且编辑 / reaction / 频道帖永不进入流水线）。
    async fn get_updates(&self, bot_token: &str, offset: i64) -> ApiResult<Vec<Update>>;

    /// `sendMessage`：发一条消息（返回平台消息对象）。
    async fn send_message(&self, bot_token: &str, params: &SendMessage) -> ApiResult<Message>;

    /// `sendChatAction`：显示 Telegram 原生的"typing…"指示（约 5 秒后自动消失）。
    async fn send_chat_action(
        &self,
        bot_token: &str,
        chat_id: i64,
        message_thread_id: i64,
    ) -> ApiResult<()>;

    /// `editMessageText`：替换**已发出**消息的正文（M7-6 补的流式原语）。
    ///
    /// 返回 `()`：Bot API 对它的应答是 `{"ok":true,"result":true}`（`result` **不是**消息
    /// 对象），所以调用方只关心成功/失败。"message is not modified" / "message to edit not
    /// found" 这类 400 由调用方按自己的语义吸收（见 `sender.rs` / `outbound.rs`）。
    async fn edit_message_text(&self, bot_token: &str, params: &EditMessageText) -> ApiResult<()>;
}

// =====================================================================
// 生产实现
// =====================================================================

/// `reqwest` 实现（上游 `newBotAPI` 的 `net/http` 那份）。
///
/// 两个客户端：长轮询的那个超时必须大于服务端挂起时间，校验类的那个要短（否则一条不可达的
/// 安装校验会把管理 API 挂住 65 秒）。
#[derive(Debug, Clone)]
pub struct JsonBotApi {
    client: reqwest::Client,
    poll_client: reqwest::Client,
}

impl Default for JsonBotApi {
    fn default() -> Self {
        Self::new()
    }
}

impl JsonBotApi {
    /// 生产形态（两个超时见模块常量）。
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(CREDENTIAL_TIMEOUT)
                .build()
                .unwrap_or_default(),
            poll_client: reqwest::Client::builder()
                .timeout(POLL_TIMEOUT)
                .build()
                .unwrap_or_default(),
        }
    }

    /// 打一次 Bot API 方法，返回 `result` 那一格。
    ///
    /// **URL 带 token ⇒ 任何一步失败都只回方法名**（见模块文档的凭据纪律）。
    async fn call_raw(
        &self,
        long_poll: bool,
        method: &'static str,
        bot_token: &str,
        params: Option<Value>,
    ) -> ApiResult<Value> {
        let url = format!("{}/bot{bot_token}/{method}", api_base());
        let client = if long_poll {
            &self.poll_client
        } else {
            &self.client
        };
        let mut request = client.post(url);
        if let Some(params) = params {
            request = request.json(&params);
        }
        let response = request
            .send()
            .await
            .map_err(|_| ApiError::Transport { method })?;
        let status = response.status().as_u16();
        let body = response
            .text()
            .await
            .map_err(|_| ApiError::Transport { method })?;
        let envelope: Envelope =
            serde_json::from_str(&body).map_err(|_| ApiError::Malformed { method })?;
        if !envelope.ok {
            // Telegram 的 `error_code` 是权威；缺了就回落 HTTP 状态（本仓的小加固，
            // 上游只读 `error_code`）。
            let code = if envelope.error_code == 0 {
                status
            } else {
                envelope.error_code
            };
            if method == "getUpdates" && code == 409 {
                return Err(ApiError::Conflict);
            }
            return Err(ApiError::Api {
                method,
                code,
                description: envelope.description,
                retry_after: envelope.parameters.map(|parameters| parameters.retry_after),
            });
        }
        Ok(envelope.result)
    }
}

/// Bot API 的响应信封（上游 `envelope`）。
#[derive(Debug, Deserialize)]
struct Envelope {
    ok: bool,
    #[serde(default)]
    result: Value,
    #[serde(default)]
    error_code: u16,
    #[serde(default)]
    description: String,
    #[serde(default)]
    parameters: Option<EnvelopeParameters>,
}

/// `envelope.parameters` 里本 adapter 唯一读的字段（上游匿名结构体的 `retry_after`）。
#[derive(Debug, Deserialize)]
struct EnvelopeParameters {
    #[serde(default)]
    retry_after: u64,
}

/// 把 `result` 那一格解成具体类型（形状不对 ⇒ [`ApiError::Malformed`]）。
fn decode_result<T: DeserializeOwned>(value: Value, method: &'static str) -> ApiResult<T> {
    serde_json::from_value(value).map_err(|_| ApiError::Malformed { method })
}

/// `getMe` 的 wire 方法名（错误文案与调用点共用一处字面量）。
const METHOD_GET_ME: &str = "getMe";
/// `getWebhookInfo` 的 wire 方法名。
const METHOD_GET_WEBHOOK_INFO: &str = "getWebhookInfo";
/// `getUpdates` 的 wire 方法名。
const METHOD_GET_UPDATES: &str = "getUpdates";
/// `sendMessage` 的 wire 方法名。
const METHOD_SEND_MESSAGE: &str = "sendMessage";
/// `sendChatAction` 的 wire 方法名。
const METHOD_SEND_CHAT_ACTION: &str = "sendChatAction";
/// `editMessageText` 的 wire 方法名（M7-6）。
const METHOD_EDIT_MESSAGE_TEXT: &str = "editMessageText";

#[async_trait]
impl TelegramApi for JsonBotApi {
    async fn get_me(&self, bot_token: &str) -> ApiResult<User> {
        let result = self.call_raw(false, METHOD_GET_ME, bot_token, None).await?;
        decode_result(result, METHOD_GET_ME)
    }

    async fn get_webhook_info(&self, bot_token: &str) -> ApiResult<WebhookInfo> {
        let result = self
            .call_raw(false, METHOD_GET_WEBHOOK_INFO, bot_token, None)
            .await?;
        decode_result(result, METHOD_GET_WEBHOOK_INFO)
    }

    async fn get_updates(&self, bot_token: &str, offset: i64) -> ApiResult<Vec<Update>> {
        // `offset` 显式给出（上游用 `omitempty` 省略 0；`offset=0` 与缺省同义：
        // "从最早的未确认更新开始返回"）。`timeout=0` 会把它变成忙轮询 ⇒ 固定 50s。
        let params = serde_json::json!({
            "offset": offset,
            "timeout": LONG_POLL_TIMEOUT_SECS,
            "allowed_updates": ["message"],
        });
        let result = self
            .call_raw(true, METHOD_GET_UPDATES, bot_token, Some(params))
            .await?;
        decode_result(result, METHOD_GET_UPDATES)
    }

    async fn send_message(&self, bot_token: &str, params: &SendMessage) -> ApiResult<Message> {
        let mut body = serde_json::json!({
            "chat_id": params.chat_id,
            "text": params.text,
        });
        if !params.parse_mode.is_empty() {
            body["parse_mode"] = Value::String(params.parse_mode.clone());
        }
        if params.message_thread_id != 0 {
            body["message_thread_id"] = Value::from(params.message_thread_id);
        }
        if params.reply_to_message_id != 0 {
            let mut reply = serde_json::json!({ "message_id": params.reply_to_message_id });
            if params.allow_sending_without_reply {
                reply["allow_sending_without_reply"] = Value::Bool(true);
            }
            body["reply_parameters"] = reply;
        }
        let result = self
            .call_raw(false, METHOD_SEND_MESSAGE, bot_token, Some(body))
            .await?;
        decode_result(result, METHOD_SEND_MESSAGE)
    }

    async fn send_chat_action(
        &self,
        bot_token: &str,
        chat_id: i64,
        message_thread_id: i64,
    ) -> ApiResult<()> {
        let mut body = serde_json::json!({ "chat_id": chat_id, "action": "typing" });
        if message_thread_id != 0 {
            body["message_thread_id"] = Value::from(message_thread_id);
        }
        self.call_raw(false, METHOD_SEND_CHAT_ACTION, bot_token, Some(body))
            .await
            .map(|_| ())
    }

    async fn edit_message_text(&self, bot_token: &str, params: &EditMessageText) -> ApiResult<()> {
        let mut body = serde_json::json!({
            "chat_id": params.chat_id,
            "message_id": params.message_id,
            "text": params.text,
        });
        if !params.parse_mode.is_empty() {
            body["parse_mode"] = Value::String(params.parse_mode.clone());
        }
        // 短超时客户端：编辑是**交互式**调用（节流节奏由调用方控制），用不上长轮询的那个超时。
        self.call_raw(false, METHOD_EDIT_MESSAGE_TEXT, bot_token, Some(body))
            .await
            .map(|_| ())
    }
}

// =====================================================================
// 429 的"一次重试"包装（上游 `sendMessageWithRetryAfter`）
// =====================================================================

/// 发一条消息，并按 Telegram **强制的** `retry_after` **重试一次**。
///
/// 三个刻意的边界（上游注释逐字）：
///
/// 1. **只重试 429**：传输层错误与其它 API 错误**不**重试 —— 丢掉的响应可能意味着 Telegram
///    已经收下了这条消息，重发就是重复投递（`docs/60` §2.3 的同一纪律）；
/// 2. **只重试一次**：Telegram 的 `retry_after` 是它的协议下界（通常 1 秒），睡完再试一次；
/// 3. 睡的是 `retry_after`（缺省 1 秒），不是固定退避 —— 见 [`ApiError::retry_after`]。
///
/// 这里的睡眠是这个出站路径上**唯一**的真实等待（时延只有协议的退避与人为的节流）。
pub async fn send_message_with_retry_after(
    api: &dyn TelegramApi,
    bot_token: &str,
    params: &SendMessage,
) -> ApiResult<Message> {
    match api.send_message(bot_token, params).await {
        Ok(message) => Ok(message),
        Err(error) => match error.retry_after() {
            Some(wait) => {
                tokio::time::sleep(wait).await;
                api.send_message(bot_token, params).await
            }
            None => Err(error),
        },
    }
}

#[cfg(test)]
mod tests;
