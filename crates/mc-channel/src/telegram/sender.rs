//! Telegram 出站发送器（上游 `internal/integrations/telegram/sender.go`，194 行）。
//!
//! - **写者**：M7-6（`LUM-1771`；`docs/60-M7-PLAN.md` §3.3）。
//! - **上游定位**：`sender` 只做"发"这一半 —— 目标身份（chat / thread / reply）由调用方
//!   （`outbound.rs` 的调度器，或 `mod.rs` 的 `Channel::send`）解析好；本文件**不**查库、
//!   **不**解密安装。
//!
//! # 一次发送的五条形态（逐条照上游，别各自发明）
//!
//! 1. **先分片、再渲染**：`chunk_message` 切的是**原始 Markdown**，每一片**各自**
//!    [`format_html`] —— 于是代码围栏不会跨片（`markdown.rs` 是逐行的）；
//! 2. **分片按 UTF-16 码元**（不是 rune、更不是字节）：Telegram 的 4096 上限是实体解析**之后**
//!    的 UTF-16 码元数，星面字符（emoji）算 2 ⇒ 计数必须按 UTF-16（[`utf16_units`]）；
//!    上限取 3500 留出渲染余量；
//! 3. **优先在换行处断开**，但只在断开后剩下的第一片仍然"够大"（> 上限的一半）时才这么切，
//!    否则会切出一堆碎渣；
//! 4. **HTML 被拒 ⇒ 回落纯文本再发一次**：**只**对 Telegram 明确说"解析实体失败"的那几种
//!    描述回落（[`is_html_parse_error`]）；传输失败与其它 API 错误**不**重试（第一次请求可能
//!    已经到达 Telegram，重发就是重复）；回落时**不带** `parse_mode`；
//! 5. **只有第一片引用回复**（上游 `replyTo = 0 // only the first chunk quotes`），
//!    而 `SendResult` 带的是**最后一片**的 id（上游逐字；与 slack 的"取第一片"不同，
//!    登记 `docs/32` §18）。
//!
//! # 凭据纪律（`docs/60` §2.3）
//!
//! 明文 bot token 只以 `&str` 形参在调用链里流动，**不进结构体字段**、不进 `Debug`、
//! 不进日志；[`SendError`] / [`ApiError`] 的每个变体只带方法名与 Telegram 自己的错误码
//! 与描述，**绝不**带请求 URL（URL 里就嵌着 token）。

use std::fmt;
use std::sync::Arc;

use mc_core::channel::message::{OutboundMessage, SendResult};

use crate::telegram::api::{send_message_with_retry_after, ApiError, SendMessage, TelegramApi};
use crate::telegram::inbound::{message_key, parse_message_ref};
use crate::telegram::markdown::format_html;

/// 单条 `sendMessage` 正文的上限（上游 `maxMessageUnits = 3500`，逐字）。
///
/// Telegram 在实体解析之后硬性上限 **4096 UTF-16 码元**；3500 给 Markdown 转换留出余量，
/// 且计数按 UTF-16（星面字符算 2）而不是 rune。
pub const MAX_MESSAGE_UNITS: usize = 3500;

/// 出站发送失败。
///
/// 分两类是为了让调用方能**只**对一类做处置：目标解析失败是**产品性**的（配置/路由错了），
/// API 失败是**传输/平台**性的。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SendError {
    /// `chat_id` 不是 Telegram 的数值 chat id（上游 `telegram: bad chat id %q`）。
    #[error("telegram: bad chat id {value:?}")]
    BadChatId {
        /// 收到的那个值（**不是**凭据：bot token 从不进这里）。
        value: String,
    },
    /// Bot API 拒绝或够不着（变体内部已保证不含凭据）。
    #[error(transparent)]
    Api(#[from] ApiError),
}

/// 本文件的 `Result` 别名。
pub type SendResultOf<T> = Result<T, SendError>;

/// 一次逻辑回复的发送结果：**每一条**被平台收下的消息的复合键（`"chat:message"`）。
///
/// 复合键就是本 adapter 在 `channel_reply_delivery.message_id` 里存的那个形态
/// （[`message_key`]）⇒ 之后要编辑这条消息时不用再拼一次。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SendFrame {
    /// 按发送顺序排列的复合键。
    pub message_ids: Vec<String>,
}

impl SendFrame {
    /// 本次发送是否一条都没落地（上游在缺失 id 时判基础设施失败）。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.message_ids.is_empty()
    }

    /// 转成 engine 的 `SendResult`：**末片**的 id（上游 `SendResult{MessageID: lastID}`）。
    ///
    /// ⚠️ 上游 telegram 的 `Send` **不**填 `MessageIDs`（只有 slack 那半边填），所以这里
    /// 也留空 —— 与 `SendResult::chunked` 的"取第一片 + 填全部"是**两种**形态
    /// （登记 `docs/32` §18 的偏离）。
    #[must_use]
    pub fn to_send_result(&self) -> SendResult {
        SendResult::single(self.message_ids.last().cloned().unwrap_or_default())
    }
}

/// 只做"发"的客户端（上游 `sender`）：持一个无状态的 API 端口，令牌逐次传入。
///
/// 与上游的一处形态差异（同 `slack::outbound::Sender`，登记 `docs/32` §18）：上游的
/// `sender` 持一个绑好令牌的 `*botAPI`；本仓的端口是**无状态**的，令牌作为形参传入
/// ⇒ 同一个 `Sender` 服务任意多个安装，且"令牌不进字段"在类型层面成立。
pub struct Sender {
    api: Arc<dyn TelegramApi>,
    max_units: usize,
}

impl fmt::Debug for Sender {
    /// 手写脱敏：只有一个 trait 对象与一个数字，**没有**令牌。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Sender")
            .field("api", &"<dyn TelegramApi>")
            .field("max_units", &self.max_units)
            .finish()
    }
}

impl Sender {
    /// 装配（显式端口）。
    #[must_use]
    pub fn new(api: Arc<dyn TelegramApi>) -> Self {
        Self {
            api,
            max_units: MAX_MESSAGE_UNITS,
        }
    }

    /// 生产形态（`reqwest` 直连 Bot API）。
    #[must_use]
    pub fn http() -> Self {
        Self::new(Arc::new(crate::telegram::api::JsonBotApi::new()))
    }

    /// 分片上限（用例把它压小，好钉住"分片后每片都在上限内"）。
    #[must_use]
    pub fn with_max_units(mut self, max_units: usize) -> Self {
        self.max_units = max_units;
        self
    }

    /// 发一条回复（上游 `sender.Send`）。
    ///
    /// **顺序逐字照上游**：分片 → 每片 render HTML → 发 → 失败且是 HTML 解析错误就回落纯文本。
    /// 任一片**最终**失败 ⇒ 整体失败，但**已经发出去的那几片不会撤回**（Telegram 侧没有回滚），
    /// 调用方按"部分投递"处理。
    pub async fn send(&self, bot_token: &str, out: &OutboundMessage) -> SendResultOf<SendFrame> {
        let chat_id = out
            .chat_id
            .parse::<i64>()
            .map_err(|_| SendError::BadChatId {
                value: out.chat_id.clone(),
            })?;
        let thread_id = out.thread_id.parse::<i64>().unwrap_or(0);
        let mut reply_to = parse_message_ref(&out.reply_to);

        let mut message_ids = Vec::new();
        for chunk in chunk_message(&out.text, self.max_units) {
            let params = SendMessage {
                chat_id,
                text: format_html(&chunk),
                parse_mode: "HTML".to_string(),
                message_thread_id: thread_id,
                reply_to_message_id: 0,
                allow_sending_without_reply: false,
            };
            let params = if reply_to != 0 {
                params.with_reply_to(reply_to)
            } else {
                params
            };
            let message =
                match send_message_with_retry_after(self.api.as_ref(), bot_token, &params).await {
                    Ok(message) => message,
                    Err(error) => {
                        // HTML 被拒 ⇒ 把**原始 Markdown**当纯文本再发一次。传输失败与其它 API 错误
                        // 不重试：第一次请求可能已经到达 Telegram（上游逐字）。
                        if !is_html_parse_error(&error) {
                            return Err(SendError::Api(error));
                        }
                        let plain = SendMessage {
                            text: chunk.clone(),
                            parse_mode: String::new(),
                            allow_sending_without_reply: false,
                            ..params.clone()
                        };
                        send_message_with_retry_after(self.api.as_ref(), bot_token, &plain)
                            .await
                            .map_err(SendError::Api)?
                    }
                };
            message_ids.push(message_key(chat_id, message.message_id));
            // 只有第一片引用触发消息（上游逐字）。
            reply_to = 0;
        }
        Ok(SendFrame { message_ids })
    }
}

/// Telegram 是否明确说"这条 HTML 解析不了"（上游 `isHTMLParseError`）。
///
/// **只有**这三种描述算：其余 400（比如 `message is not modified`、"chat not found"）都
/// **不**该触发纯文本回落 —— 回落会把"真正的问题"藏进一条更难看但同样发不出去的消息里。
#[must_use]
pub fn is_html_parse_error(error: &ApiError) -> bool {
    if error.http_code() != Some(400) {
        return false;
    }
    let ApiError::Api { description, .. } = error else {
        return false;
    };
    let description = description.to_lowercase();
    description.contains("parse entities")
        || description.contains("unsupported start tag")
        || description.contains("can't find end tag")
}

/// 按 UTF-16 码元把正文切成 ≤`max_units` 的若干片（上游 `chunkMessage`，逐字）。
///
/// 三条形态：
///
/// - 上限 ≤0，或整条都不超上限 ⇒ **一片原文**（不复制、不改动）；
/// - 每一片的"窗口"按 **UTF-16 码元**增长，且**只在 rune 边界**收口（星面字符不能被切开）；
/// - 若窗口内**最后一个换行**之前的长度已经超过上限的一半，就在那里收口
///   （"优先在换行处断开，但不切出碎渣"），并把片尾的换行去掉。
#[must_use]
pub fn chunk_message(text: &str, max_units: usize) -> Vec<String> {
    let runes: Vec<char> = text.chars().collect();
    if max_units == 0 || utf16_units(text) <= max_units {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut rest: &[char] = &runes;
    while !rest.is_empty() {
        let mut units = 0;
        let mut end = 0;
        for (index, ch) in rest.iter().enumerate() {
            let width = if *ch > '\u{FFFF}' { 2 } else { 1 };
            if units + width > max_units {
                break;
            }
            units += width;
            end = index + 1;
        }
        if end == 0 {
            // 单个字符就超上限（上限 < 2 且是星面字符）⇒ 至少吃掉它，否则死循环。
            end = 1;
        }
        // 窗口内最后一个换行，但只在它之前已经"够大"时才在那里收口。
        if let Some(last_newline) = rest[..end].iter().rposition(|ch| *ch == '\n') {
            let head: String = rest[..last_newline].iter().collect();
            if utf16_units(&head) > max_units / 2 {
                end = last_newline + 1;
            }
        }
        let piece: String = rest[..end].iter().collect();
        chunks.push(piece.trim_end_matches('\n').to_string());
        rest = &rest[end..];
    }
    chunks
}

/// 一段文本占多少 **UTF-16 码元**（上游 `utf16Units`）：星面字符（emoji）算 2。
#[must_use]
pub fn utf16_units(text: &str) -> usize {
    text.chars()
        .map(|ch| if ch > '\u{FFFF}' { 2 } else { 1 })
        .sum()
}

#[cfg(test)]
mod tests;
