//! Telegram **出站调度**：流式占位消息、节流的编辑、最终答案与失败告知
//! （上游 `internal/integrations/telegram/outbound.go`，1,633 行）。
//!
//! - **写者**：M7-6（`LUM-1771`；`docs/60-M7-PLAN.md` §3.3）。
//!
//! # 上游这一段在解决什么
//!
//! Telegram **没有** stream-update 协议 ⇒ "流式输出"用平台自己的经典手法模拟：第一帧 partial
//! 发**一条占位消息**，之后按**节流**节奏反复 `editMessageText`，最后在完成事件上用**最终编辑**
//! （或补发的分片）收尾。节流按 chat 走 —— Telegram 对编辑有速率预算（单聊约 1 次/秒，群更严），
//! 429 时按平台强制的退避让开。
//!
//! # 与上游的形态差异（**登记** `docs/32` §18）
//!
//! 1. **没有事件总线 ⇒ 入口是显式调用**：上游 `Outbound` 是 `events.Bus` 的订阅者，并自带
//!    terminal worker 池 / 重试最小堆 / 空闲清扫器。本仓**没有**那条进程内总线 ⇒ 本文件只提供
//!    **一次一步**的入口（[`Outbound::push_partial`] / [`Outbound::deliver_answer`] /
//!    [`Outbound::deliver_failure_notice`]），由宿主驱动 —— 与 M7-4 对 `slack/outbound.go`
//!    同一先例。
//! 2. **目标解析 = 纯函数 + 调用方取数**：上游 `resolveTarget` 自己查
//!    `channel_task_delivery` / `channel_installation` 并解密凭据；本仓的 adapter 不直接写 DB
//!    ⇒ 语义在 [`ReplyTarget::from_task_delivery`]（纯函数，表驱动用例），两次读库留在宿主。
//! 3. **一步一回报**：[`Step`] 把上游 `terminalRequestResult{done, retryAt, err}` 的三态原样
//!    搬过来 —— 于是"等多久、还试不试"由调用方决定（用例因此完全不睡真觉）。
//!
//! # 凭据纪律（`docs/60` §2.3）
//!
//! 明文 bot token 只经 [`Sensitive`] 流动（手写 `Debug` 输出 `<redacted>`）；[`ReplyTarget`]
//! 的 `Debug` 也手写。本文件没有任何 `tracing::*` 插值 token / URL。

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_repos::channel::delivery::ChannelTaskDeliveryRow;

use crate::telegram::api::{EditMessageText, SendMessage, TelegramApi};
use crate::telegram::config::Sensitive;
use crate::telegram::delivery::{
    classify_send, is_edit_target_missing, is_not_modified, is_permanent_edit_rejection,
    DeliveryLease, DeliveryLedger, DeliveryOutcome, DeliveryTarget,
};
use crate::telegram::inbound::parse_message_ref;
use crate::telegram::markdown::format_html;
use crate::telegram::sender::{
    chunk_message, is_html_parse_error, utf16_units, Sender, MAX_MESSAGE_UNITS,
};

/// 同一个 chat 上两次 `editMessageText` 之间的**最小**间隔（上游 `editInterval = 2.5s`）。
///
/// 单聊约每秒一次、群里约 20 条/分钟；2.5 秒让一次长生成稳稳落在两个预算里。
pub const EDIT_INTERVAL: Duration = Duration::from_millis(2500);

/// 结果不明的编辑重试间隔（上游 `terminalEditRetryDelay`）。
pub const TERMINAL_EDIT_RETRY_DELAY: Duration = Duration::from_secs(1);

/// 结果不明的编辑**最多**重试几次（上游 `maxAmbiguousEditAttempts = 3`）：有界是必须的 ——
/// terminal 队列是**按会话**的，一条永不收口的回复会挡住同一个 chat 之后每一条答案。
pub const MAX_AMBIGUOUS_EDIT_ATTEMPTS: u32 = 3;

/// 失败告知的编辑尝试上限（上游 `maxNoticeEditAttempts = 2`）：告知跑在**同步**的完成回调上，
/// 重试比答案更紧（这里的延迟会压住实时广播）。
pub const MAX_NOTICE_EDIT_ATTEMPTS: u32 = 2;

/// 第一帧的占位文本（上游 `streamPlaceholder = "…"`）。
pub const STREAM_PLACEHOLDER: &str = "…";

/// agent 运行**彻底失败**时的告知文案（上游 `taskFailedText`，逐字）。
pub const TASK_FAILED_TEXT: &str = "❌ The agent run failed. Please try again.";

/// 一个出站目标的解析结果（上游 `replyTarget`）。
pub struct ReplyTarget {
    /// 流状态表的键（本仓取 task id 的字符串形态）。
    pub stream_key: String,
    /// 限速状态表的键（本仓取 installation id 的字符串形态）。
    pub bot_key: String,
    /// 数值 chat id。
    pub chat_id: i64,
    /// 论坛话题（0 = 不带）。
    pub thread_id: i64,
    /// 触发消息的 platform message id（0 = 不引用）。
    pub reply_to: i64,
    /// 明文 bot token（**手写脱敏**类型；绝不出现在日志 / `Debug` 里）。
    pub bot_token: Sensitive,
    /// 投递账要的行身份。
    pub delivery: DeliveryTarget,
}

impl fmt::Debug for ReplyTarget {
    /// 手写脱敏：令牌字段只说明**配没配**（`docs/60` §2.3 第 1 条）。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReplyTarget")
            .field("stream_key", &self.stream_key)
            .field("bot_key", &self.bot_key)
            .field("chat_id", &self.chat_id)
            .field("thread_id", &self.thread_id)
            .field("reply_to", &self.reply_to)
            .field("bot_token", &self.bot_token)
            .field("delivery", &self.delivery)
            .finish()
    }
}

impl ReplyTarget {
    /// 从一条**任务投递快照**解析目标（上游 `resolveTarget` + `outboundTarget` 的语义部分）。
    ///
    /// - `None` = 这条快照不是本渠道的。任务来自 Web / Desktop / Mobile 时**不许**把它送进外部
    ///   会话，哪怕它所在的 chat 曾经有过这条路由（上游注释逐字）；
    /// - 数值 chat id 优先取**绑定 config 里的** `chat_id`（绑定键可能是复合的
    ///   `"chat:thread"`），回落才用 `channel_chat_id` 本身；
    /// - `thread` / `last_message_id` 缺席 ⇒ 0（上游 `Valid` 的零值语义）。
    #[must_use]
    pub fn from_task_delivery(
        row: &ChannelTaskDeliveryRow,
        bot_token: Sensitive,
        kind: ChannelKind,
    ) -> Option<Self> {
        if ChannelKind::from_storage_str(&row.channel_type) != Some(kind) {
            return None;
        }
        let raw = config_chat_id(&row.config).unwrap_or_else(|| row.channel_chat_id.clone());
        let chat_id = raw.parse::<i64>().unwrap_or(0);
        let thread_id = row
            .channel_thread_id
            .as_deref()
            .and_then(|raw| raw.parse::<i64>().ok())
            .unwrap_or(0);
        let reply_to = row
            .channel_message_id
            .as_deref()
            .map_or(0, parse_message_ref);
        Some(Self {
            stream_key: row.task_id.to_string(),
            bot_key: row.installation_id.to_string(),
            chat_id,
            thread_id,
            reply_to,
            bot_token,
            delivery: DeliveryTarget {
                task_id: Id(row.task_id),
                binding_id: Id(row.binding_id),
                installation_id: Id(row.installation_id),
                kind,
                chat_id: row.channel_chat_id.clone(),
            },
        })
    }
}

/// 从绑定 config 里读 `chat_id`（上游 `telegramBindingConfig.ChatID`）。
#[must_use]
fn config_chat_id(config: &serde_json::Value) -> Option<String> {
    config
        .get("chat_id")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// 一次投递**步骤**的结果（上游 `terminalRequestResult` 的三态）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// 这一步完成了；带一句稳定原因（`delivered` / `empty_reply` / `edit_rejected` …）。
    Done {
        /// 稳定原因（收口原因或诊断标签；**不含**凭据）。
        reason: String,
    },
    /// 现在做不了，间隔之后再试这一步（429 退避 / 节流 / 被别的路径占着）。
    RetryAfter(Duration),
    /// 这一步**失败**且不该继续（上游把这种情况记成日志 + `done`）。
    Failed {
        /// 不含凭据的说明。
        message: String,
    },
}

impl Step {
    /// 完成。
    #[must_use]
    pub fn done(reason: impl Into<String>) -> Self {
        Self::Done {
            reason: reason.into(),
        }
    }

    /// 是否已经收口。
    #[must_use]
    pub fn is_done(&self) -> bool {
        matches!(self, Self::Done { .. })
    }
}

/// 一次流式帧的处理结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamStep {
    /// 占位消息刚刚发出（`message_id` 是新的那条）。
    PlaceholderCreated {
        /// 新占位消息的 platform message id。
        message_id: i64,
    },
    /// 占位消息被编辑（内容与现状相同、被 Telegram 判为 not-modified 也算）。
    Edited,
    /// 这一帧**什么都没做**（被 429 挡住 / 轮次已收口 / 前情未知）。
    Idle {
        /// 稳定原因，便于用例与运维分辨。
        reason: &'static str,
    },
    /// 节流窗口（调用方到点再来）。
    RetryAfter(Duration),
    /// 失败（**不**冒泡成错误：流式帧是装饰，最终答案会照常到达）。
    Failed {
        /// 不含凭据的说明。
        message: String,
    },
}

/// 一条最终答案的投递进度（上游 `terminalReply` 里那几个字段）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnswerProgress {
    /// 已经可编辑的那条消息（0 = 还没有）。
    pub streamed_message_id: i64,
    /// 下一片的下标。
    pub chunk_index: usize,
    /// 占位消息是否已经被最终编辑过（`false` ⇒ 第一片走**编辑**而不是新发）。
    pub placeholder_edited: bool,
    /// 目标消息确认没了 ⇒ 另发一条新消息（**唯一**允许"新发"的编辑失败）。
    pub fresh_send: bool,
    /// 结果不明的编辑已经试了几次（答案与失败告知**共用**这个计数，上游同）。
    pub edit_attempts: u32,
}

/// 出站调度器：把"该发什么"落到 Telegram，每一步都经投递租约。
pub struct Outbound {
    sender: Sender,
    api: Arc<dyn TelegramApi>,
    ledger: Arc<DeliveryLedger>,
    edit_interval: Duration,
}

impl fmt::Debug for Outbound {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Outbound")
            .field("sender", &self.sender)
            .field("api", &"<dyn TelegramApi>")
            .field("ledger", &self.ledger)
            .field("edit_interval", &self.edit_interval)
            .finish()
    }
}

mod construct;

mod reply;

/// 一次 `editMessageText` 结果的归类（上游 `editStreamedReply` 那道 `switch` 的**顺序**）。
///
/// **顺序就是语义**：三个可恢复的 400 共用状态码，所以必须按"not-modified（良性）→ 标记被拒
/// （换纯文本重试同一目标）→ 目标没了（唯一允许新发）→ 永久拒绝（停手）→ 其余（结果不明，有界
/// 重试）"这条阶梯走。把顺序抽成一个纯函数，是为了让它**可测**（`outbound/tests.rs` 有一张表）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditVerdict {
    /// 编辑生效了。
    Applied,
    /// 快照与现状相同 —— Telegram 说"没改"是**良性**的。
    NotModified,
    /// Telegram 拒的是**标记**：同一个目标换纯文本可以再试。
    MarkupRefused,
    /// Telegram 确认那条消息**没了**：唯一值得另发一条新消息的编辑失败。
    TargetMissing,
    /// 重试修不好（封禁 / 失权 / 消息不再可编）。
    PermanentRejection,
    /// 结果不明（编辑**可能**已经生效）。
    Ambiguous,
}

/// 归类一次编辑的结果（429 的退避由调用方**先**处理）。
#[must_use]
pub fn classify_edit(result: &Result<(), crate::telegram::api::ApiError>) -> EditVerdict {
    match result {
        Ok(()) => EditVerdict::Applied,
        Err(error) if is_not_modified(error) => EditVerdict::NotModified,
        Err(error) if is_html_parse_error(error) => EditVerdict::MarkupRefused,
        Err(error) if is_edit_target_missing(error) => EditVerdict::TargetMissing,
        Err(error) if is_permanent_edit_rejection(error) => EditVerdict::PermanentRejection,
        Err(_) => EditVerdict::Ambiguous,
    }
}

/// 非空取首（上游 `firstNonEmpty`）。
#[must_use]
pub fn first_non_empty(primary: &str, fallback: &str) -> String {
    if primary.is_empty() {
        fallback.to_string()
    } else {
        primary.to_string()
    }
}

#[cfg(test)]
mod tests;
