//! `DingTalk` **出站面**：机器人发送（文本 / 引用）与任务终态事件的投递计划
//! （上游 `internal/integrations/dingtalk/` 的 `outbound.go` 313 行 + `outbound_send.go`
//! 238 行里**发送循环**那一半）。
//!
//! - **写者**：M7-8（`docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §22 的 D1）。
//! - **上游形态**：出站是一个**订阅者**（`EventChatDone` / `EventTaskFailed` /
//!   `EventTaskCancelled`），它从 `channel_task_delivery` 找回目标、从安装行解凭据，再用
//!   `sender` 往 `/v1.0/robot/{oToMessages/batchSend,groupMessages/send}` POST 一条
//!   `sampleMarkdown`。
//!
//! # 本目录的四个文件（门 ⑩ 的切分，边界取上游文件的边界）
//!
//! | 文件 | 上游 | 内容 |
//! | --- | --- | --- |
//! | `outbound.rs`（本文件） | `outbound.go` + `outbound_send.go` 的 `sender` | 发送循环、判决投影、投递计划 |
//! | `outbound/target.rs` | `outbound_send.go` 的目标与分片 | [`SendTarget`] / [`MarkdownChunk`] |
//! | `outbound/openapi.rs` | `client.go` + `token.go` 的出站两条 | 令牌缓存、`postJSON`、基址接缝 |
//! | `outbound/credentials.rs` | `config.go` 的 `decodeCredentials` | 安装凭据解码 |
//! | `outbound/quote.rs` | `outbound_quote.go` | 引用里的内部图片 → `[Image]` |
//! | `outbound/source.rs` | `reply_source.go` | 提供方坐标缓存（Done 的唯一判据） |
//!
//! # 与上游的形态差异（**逐条登记** `docs/32` §22）
//!
//! 1. **没有进程内事件总线 ⇒ 入口是显式调用**（与 M7-6 对 telegram `outbound.go` 的同一先例）：
//!    本仓只落**发送那一段**与**目标解析的纯函数**（[`SendTarget::from_task_delivery`]），
//!    "哪条任务完成了"由宿主驱动。
//! 2. **令牌缓存与 `postJSON` 在这里（而不是 M7-9 的 `client.rs`）**：M7-8 是出站的唯一写者，
//!    而 M7-9 才落完整的 `Client`（安装 / 吊销面也要它）⇒ 这两条抽成端口
//!    [`OpenApiTransport`] 并给出生产实现 [`HttpOpenApi`]；M7-9 收敛时只换实现，
//!    发送端的校验 / 分片 / 401 重试语义一行不动。
//! 3. **错误不带平台的 `message` 字段**：上游 `apiRequestError.Error()` 会带上 `message`，
//!    而网关的错误体会**回声请求体**（`accessToken` 的请求体里就是 `appSecret`）⇒ 本仓只保留
//!    机器可读的 `code`（`docs/60` §2.3 第 2/3 条；M7-7 的 `ReqwestOpener` 已按同一判据处理）。
//! 4. **没有 `singleflight`**：并发未命中用**按 `AppKey` 的异步互斥**折叠成一次铸造
//!    （本仓不许新增依赖，`tokio::sync::Mutex` 已能表达同一条 fan-in）。
//! 5. **引用源的图片识别不用 `goldmark`**（见 `outbound/quote.rs` 的模块文档）。
//!
//! # 凭据纪律（`docs/60` §2.3）
//!
//! 明文 `AppSecret` 只以 [`AppSecret`]（手写 `Debug` 输出 `<redacted>`）在结构体里流动，
//! 且只在铸令牌那一步经 [`AppSecret::expose`] 取出；[`Credentials`] 与 [`Sender`] 都手写
//! `Debug`。本文件**没有**任何 `tracing::*`，错误变体里也不含任何凭据 / URL。

pub mod credentials;
pub mod openapi;
pub mod quote;
pub mod source;
pub mod target;

// 分片的实现在 `outbound/target.rs`；出站面的公开名字在这里**统一再导出**
// （调用方只 `use crate::dingtalk::outbound::…`，不必知道文件是怎么切的）。
pub use credentials::{decode_credentials, Credentials, OutboundConfig};
pub use openapi::{
    api_base, reset_api_base, set_api_base, DingTalkApiError, HttpOpenApi, OpenApiTransport,
    ACCESS_TOKEN_PATH, DEFAULT_API_BASE, HTTP_UNAUTHORIZED, MAX_CACHED_TOKENS,
    MESSAGE_FILES_DOWNLOAD_PATH, MSG_KEY_MARKDOWN, PATH_SEND_GROUP, PATH_SEND_P2P,
    TOKEN_MINT_TIMEOUT, TOKEN_SAFETY_MARGIN,
};
pub use quote::sealed_input_quote;
pub use source::{ReplySource, ReplySourceCache, MAX_REPLY_SOURCES};
pub use target::{
    reply_markdown_chunks, reply_markdown_chunks_with_budget, MarkdownChunk, MarkdownParam,
    SendTarget,
};

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::dingtalk::emotion::{
    set_emoji_reaction as emotion_set_emoji_reaction, Emotion, EmotionError, EmotionRequest,
    EmotionTransport, PATH_RECALL_EMOTION, PATH_REPLY_EMOTION,
};
use crate::dingtalk::inbound::CONV_TYPE_P2P;
use crate::dingtalk::stream::AppSecret;

// =====================================================================
// 脱离任务（engine 的两个同步接缝用）
// =====================================================================

/// 把一段 future 推到**脱离任务**上（有运行时 ⇒ 起任务；没有 ⇒ 打一条 warn）。
///
/// engine 的两个**同步**接缝（[`crate::engine::OutboundReplier`] 与
/// [`crate::engine::TypingNotifier`]）在这一侧都落到网络 I/O 上，而引擎的调用点绝不应该阻塞在
/// 那里（`docs/60` §2.6 第 5 条：出站不阻塞 ACK）⇒ 与 `telegram::spawn_detached` 同款。
/// 调用方：`replier.rs`（判决回复）与 `ack.rs`（表情回执）。
pub(crate) fn spawn_detached<F>(future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(future);
    } else {
        tracing::warn!("dingtalk: no async runtime; skipping the detached task");
    }
}

// =====================================================================
// 发送器
// =====================================================================

/// 发一条回复的机器人客户端（上游 `sender`）。
///
/// 与上游的一处**形态**差异：上游 `sender` 持一个绑好 token 的 `*Client`，本仓的传输是
/// 端口、凭据逐次传入（“凭据不进无状态端口”在类型层面成立，同 `slack::outbound::Sender`
/// 的先例）。
pub struct Sender {
    transport: Arc<dyn OpenApiTransport>,
    robot_code: String,
    app_key: String,
    app_secret: AppSecret,
}

impl fmt::Debug for Sender {
    /// 手写脱敏。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Sender")
            .field("transport", &"<dyn OpenApiTransport>")
            .field("robot_code", &self.robot_code)
            .field("app_key", &self.app_key)
            .field("app_secret", &self.app_secret)
            .finish()
    }
}

impl Sender {
    /// 装配。
    #[must_use]
    pub fn new(
        transport: Arc<dyn OpenApiTransport>,
        robot_code: impl Into<String>,
        app_key: impl Into<String>,
        app_secret: AppSecret,
    ) -> Self {
        Self {
            transport,
            robot_code: robot_code.into(),
            app_key: app_key.into(),
            app_secret,
        }
    }

    /// 从凭据 + 端口装配（出站回复的每条路径都用这一条）。
    #[must_use]
    pub fn from_credentials(
        transport: Arc<dyn OpenApiTransport>,
        credentials: &Credentials,
    ) -> Self {
        Self::new(
            transport,
            credentials.robot_code.clone(),
            credentials.app_key.clone(),
            credentials.app_secret.clone(),
        )
    }

    /// 生产形态：真 `reqwest` 端口。
    #[must_use]
    pub fn http(credentials: &Credentials) -> Self {
        Self::from_credentials(Arc::new(HttpOpenApi::new()), credentials)
    }

    /// 本安装的 `AppKey`（诊断 / 令牌缓存键）。
    #[must_use]
    pub fn app_key(&self) -> &str {
        &self.app_key
    }

    /// 本安装的 robot code。
    #[must_use]
    pub fn robot_code(&self) -> &str {
        &self.robot_code
    }

    /// 发一条（或多条）有界 Markdown 回复；返回**最后一片**的 `processQueryKey`。
    ///
    /// 引用只在群发里出现（上游逐字：直聊回复不引用）。
    ///
    /// # Errors
    ///
    /// 目标不完整、分片超预算、铸造令牌失败、平台拒绝、传输失败。
    pub async fn send(&self, target: &SendTarget, text: &str) -> Result<String, DingTalkApiError> {
        if text.is_empty() {
            return Ok(String::new());
        }
        let quote = if target.is_group() {
            target.quote_text.as_str()
        } else {
            ""
        };
        let chunks = reply_markdown_chunks(text, quote)?;
        let mut last_key = String::new();
        for chunk in chunks {
            let param = serde_json::to_string(&MarkdownParam {
                title: chunk.title,
                text: chunk.text,
            })
            .map_err(|_| DingTalkApiError::PayloadBudget)?;
            last_key = self.send_one(target, &param).await?;
        }
        Ok(last_key)
    }

    /// 发一片；401 ⇒ 作废缓存后**重试一次**（上游 `sendOne`）。
    async fn send_one(
        &self,
        target: &SendTarget,
        msg_param: &str,
    ) -> Result<String, DingTalkApiError> {
        let (path, body) = self.request(target, msg_param)?;
        for attempt in 0..2 {
            let token = self
                .transport
                .access_token(&self.app_key, &self.app_secret)
                .await?;
            match self.transport.post_json(path, &token, body.clone()).await {
                Ok(value) => {
                    return Ok(value
                        .get("processQueryKey")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string());
                }
                Err(DingTalkApiError::Unauthorized) if attempt == 0 => {
                    self.transport.invalidate(&self.app_key);
                }
                Err(error) => return Err(error),
            }
        }
        Err(DingTalkApiError::Unauthorized)
    }

    /// 端点 + 请求体（上游 `request`）：直聊要收件人，群发要会话 id —— 都缺就是**发之前**拒。
    ///
    /// # Errors
    ///
    /// 目标缺字段 ⇒ [`DingTalkApiError::InvalidTarget`]。
    pub fn request(
        &self,
        target: &SendTarget,
        msg_param: &str,
    ) -> Result<(&'static str, Value), DingTalkApiError> {
        if target.conversation_type == CONV_TYPE_P2P {
            if target.staff_id.is_empty() {
                return Err(DingTalkApiError::InvalidTarget {
                    reason: "1:1 send missing recipient staff id",
                });
            }
            return Ok((
                PATH_SEND_P2P,
                serde_json::json!({
                    "robotCode": self.robot_code,
                    "userIds": [target.staff_id],
                    "msgKey": MSG_KEY_MARKDOWN,
                    "msgParam": msg_param,
                }),
            ));
        }
        if target.conversation_id.is_empty() {
            return Err(DingTalkApiError::InvalidTarget {
                reason: "group send missing conversation id",
            });
        }
        Ok((
            PATH_SEND_GROUP,
            serde_json::json!({
                "robotCode": self.robot_code,
                "openConversationId": target.conversation_id,
                "msgKey": MSG_KEY_MARKDOWN,
                "msgParam": msg_param,
            }),
        ))
    }

    /// 把一个 `downloadCode` 换成短期下载 URL（上游 `messageFileDownloadURL`；401 重试一次）。
    ///
    /// 返回的 URL 是**短期签名链接**（等价于凭据）：立刻取用，别持久化、别记日志。
    ///
    /// # Errors
    ///
    /// 见上；平台没给 `downloadUrl` ⇒ [`DingTalkApiError::Malformed`]。
    pub async fn message_file_download_url(&self, code: &str) -> Result<String, DingTalkApiError> {
        let path = MESSAGE_FILES_DOWNLOAD_PATH;
        let body = serde_json::json!({
            "robotCode": self.robot_code,
            "downloadCode": code,
        });
        let mut token = self
            .transport
            .access_token(&self.app_key, &self.app_secret)
            .await?;
        let mut outcome = self.transport.post_json(path, &token, body.clone()).await;
        if matches!(outcome, Err(DingTalkApiError::Unauthorized)) {
            self.transport.invalidate(&self.app_key);
            token = self
                .transport
                .access_token(&self.app_key, &self.app_secret)
                .await?;
            outcome = self.transport.post_json(path, &token, body).await;
        }
        let value = outcome?;
        match value.get("downloadUrl").and_then(Value::as_str) {
            Some(url) if !url.is_empty() => Ok(url.to_string()),
            _ => Err(DingTalkApiError::Malformed { path }),
        }
    }

    /// 贴 / 撤一次内置表情（上游 `setEmojiReaction`；校验与重试在
    /// [`crate::dingtalk::emotion`]）。
    ///
    /// # Errors
    ///
    /// 四条前置校验、平台拒绝、链路失败、两次都 401。
    pub async fn set_emoji_reaction(
        &self,
        target: &SendTarget,
        emotion: Emotion,
        recall: bool,
    ) -> Result<(), EmotionError> {
        emotion_set_emoji_reaction(
            self,
            &self.robot_code,
            &target.conversation_id,
            &target.source_message_id,
            emotion,
            recall,
        )
        .await
    }
}

#[async_trait]
impl EmotionTransport for Sender {
    async fn post_emotion(&self, path: &str, request: &EmotionRequest) -> Result<(), EmotionError> {
        let path: &'static str = if path == PATH_REPLY_EMOTION {
            PATH_REPLY_EMOTION
        } else if path == PATH_RECALL_EMOTION {
            PATH_RECALL_EMOTION
        } else {
            return Err(EmotionError::Transport {
                message: "unsupported emotion path".to_string(),
            });
        };
        let body = serde_json::to_value(request).map_err(|_| EmotionError::Transport {
            message: "marshal emotion request failed".to_string(),
        })?;
        let token = self
            .transport
            .access_token(&self.app_key, &self.app_secret)
            .await
            .map_err(|error| EmotionError::Transport {
                message: error.code().to_string(),
            })?;
        self.transport
            .post_json(path, &token, body)
            .await
            .map_err(|error| match error {
                DingTalkApiError::Unauthorized => EmotionError::Unauthorized,
                other => EmotionError::Transport {
                    message: other.code().to_string(),
                },
            })
            .map(|_| ())
    }

    fn invalidate(&self) {
        self.transport.invalidate(&self.app_key);
    }
}

// =====================================================================
// 出站判决（上游 `outbound.go` 的纯函数那一半）
// =====================================================================

/// 一条任务终态事件的**正文投影**（上游 `eventContent` + `eventRetryPending`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboundVerdict {
    /// 投递这条正文。
    Deliver(String),
    /// 保持沉默（重试在飞 / 没有可投递文本）。
    Silent,
}

/// `EventTaskFailed` 的正文投影（上游 `eventContent` 的失败分支）。
///
/// 文案与 Web 转录页的失败 `chat_message` 逐字一致：自动重试在飞时**静默**
/// （重试自己会报结果），带 `error` 时就是“可投递”。
#[must_use]
pub fn failure_notice(error: &str, retry_pending: bool) -> OutboundVerdict {
    if retry_pending {
        return OutboundVerdict::Silent;
    }
    if error.is_empty() {
        return OutboundVerdict::Silent;
    }
    OutboundVerdict::Deliver(format!("⚠️ {error}"))
}

/// `chat:done` 的正文投影（上游 `eventContent` 的 `ChatDonePayload` 分支）。
#[must_use]
pub fn answer_text(content: &str) -> OutboundVerdict {
    if content.is_empty() {
        OutboundVerdict::Silent
    } else {
        OutboundVerdict::Deliver(content.to_string())
    }
}

/// 任务终态事件的**投递计划**（上游 `processEvent` 把引用与正文拼起来的那一步）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundDelivery {
    pub target: SendTarget,
    pub content: String,
}

impl OutboundDelivery {
    /// 从“目标 + 密封输入正文 + 答案”造计划。
    ///
    /// 两条规则逐字照上游：
    /// 1. 引用 = **已拥有**的密封输入（含引用历史），经 [`sealed_input_quote`] 把内部图片
    ///    链接换成占位符；
    /// 2. 只有**群**回复带引用（[`SendTarget::is_group`]）。
    #[must_use]
    pub fn new(target: SendTarget, sealed_input: &str, content: &str) -> Self {
        let mut target = target;
        target.quote_text = sealed_input_quote(sealed_input);
        Self {
            target,
            content: content.to_string(),
        }
    }

    /// 投递（无正文 ⇒ 什么都不发 —— 上游在 `send` 里也短路）。
    ///
    /// # Errors
    ///
    /// 见 [`Sender::send`]。
    pub async fn deliver(&self, sender: &Sender) -> Result<String, DingTalkApiError> {
        sender.send(&self.target, &self.content).await
    }
}

#[cfg(test)]
mod tests;
