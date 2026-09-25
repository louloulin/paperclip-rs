//! `DingTalk` 出站的**目标 + 分片**（上游 `outbound_send.go` 238 行里除发送循环之外的那一半：
//! `sendTarget` / `markdownParam` / `replyMarkdownChunks*`）。
//!
//! - **写者**：M7-8（`docs/32` §22 的 D1；门 ⑩ 的切分，边界取上游文件的边界）。
//! - **引用（`quoteText`）只在群发里用**：直聊回复不引用（上游 `sender.send` 的第一条判断）。

use mc_core::channel::message::{ChatType, InboundMessage};
use mc_repos::channel::delivery::ChannelTaskDeliveryRow;
use serde::{Deserialize, Serialize};

use crate::dingtalk::inbound::{CONV_TYPE_GROUP, CONV_TYPE_P2P};
use crate::dingtalk::markdown::{
    chunk_markdown_with_first_budget, markdown_title, prepend_markdown_quote, MARKDOWN_BYTE_BUDGET,
    MARKDOWN_PAYLOAD_BYTE_BUDGET, MAX_MARKDOWN_FENCE_INFO_BYTES,
};
use crate::dingtalk::resolvers::DingTalkBindingConfig;

// =====================================================================
// 发送目标（上游 `sendTarget`）
// =====================================================================

/// 一条回复的目标（上游 `sendTarget`）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SendTarget {
    /// `"1"` = 直聊，`"2"` = 群（选端点）。空 = 群（上游零值语义）。
    pub conversation_type: String,
    /// 群的 `openConversationId`。
    pub conversation_id: String,
    /// 直聊的收件人 staff id。
    pub staff_id: String,
    /// 被回应的源消息 id（表情回应用）。
    pub source_message_id: String,
    /// 要渲染成引用块的源正文。
    pub quote_text: String,
}

impl SendTarget {
    /// 群发目标（`Channel::send` 的形态：上游 `Send` 只给 `out.ChatID`）。
    #[must_use]
    pub fn group(conversation_id: impl Into<String>) -> Self {
        Self {
            conversation_type: CONV_TYPE_GROUP.to_string(),
            conversation_id: conversation_id.into(),
            ..Self::default()
        }
    }

    /// 直聊目标（绑定卡要**私聊**送达，见 `replier.rs`）。
    #[must_use]
    pub fn direct(staff_id: impl Into<String>) -> Self {
        Self {
            conversation_type: CONV_TYPE_P2P.to_string(),
            staff_id: staff_id.into(),
            ..Self::default()
        }
    }

    /// 从绑定行的出站寻址恢复目标（上游 `outboundTarget` 的**结论**：
    /// `DingTalkBindingConfig` → `sendTarget` 只差一次字段搬运）。
    #[must_use]
    pub fn from_binding_config(config: &DingTalkBindingConfig) -> Self {
        Self {
            conversation_type: clean_conversation_type(&config.conversation_type),
            conversation_id: config.conversation_id.clone(),
            staff_id: config.staff_id.clone(),
            source_message_id: String::new(),
            quote_text: String::new(),
        }
    }

    /// 从 `channel_task_delivery` 行恢复目标（上游
    /// `outboundTarget(bindingFromTaskDelivery(delivery))`）。
    ///
    /// 兜底语义逐字照上游：`config` 解不开 / 半截 ⇒ 退回 `channel_chat_id` 当会话 id、
    /// 会话类型按群。
    #[must_use]
    pub fn from_task_delivery(row: &ChannelTaskDeliveryRow) -> Self {
        let mut config = serde_json::from_value::<DingTalkBindingConfig>(row.config.clone())
            .unwrap_or(DingTalkBindingConfig {
                conversation_type: String::new(),
                conversation_id: String::new(),
                staff_id: String::new(),
            });
        if config.conversation_id.is_empty() {
            config.conversation_id.clone_from(&row.channel_chat_id);
        }
        let mut target = Self::from_binding_config(&config);
        target.source_message_id = row.channel_message_id.clone().unwrap_or_default();
        target
    }

    /// 从入站消息的**路由身份**造目标（上游 `targetFromMessage`：绑定卡 / 状态告知用，
    /// 那时还没有会话绑定）。
    ///
    /// 群聊带可见引用；直聊**不带**引用（上游逐字）。
    #[must_use]
    pub fn from_message(message: &InboundMessage) -> Self {
        if message.source.chat_type == ChatType::P2p {
            return Self {
                conversation_type: CONV_TYPE_P2P.to_string(),
                staff_id: message.source.sender_id.clone(),
                ..Self::default()
            };
        }
        Self {
            conversation_type: CONV_TYPE_GROUP.to_string(),
            conversation_id: message.source.chat_id.clone(),
            quote_text: crate::dingtalk::resolvers::dingtalk_visible_quote_text(message),
            ..Self::default()
        }
    }

    /// 指向**源消息自身**的目标（上游 `reactionTargetFromMessage`）：表情回应只需要会话
    /// 与消息 id。
    #[must_use]
    pub fn reaction_from_message(message: &InboundMessage) -> Self {
        Self::reaction(&message.source, &message.message_id)
    }

    /// 同上，但只需要路由身份与消息 id（引用源缓存里只有这两样）。
    #[must_use]
    pub fn reaction(source: &mc_core::channel::message::Source, message_id: &str) -> Self {
        let mut target = Self {
            conversation_type: CONV_TYPE_GROUP.to_string(),
            conversation_id: source.chat_id.clone(),
            source_message_id: message_id.to_string(),
            ..Self::default()
        };
        if source.chat_type == ChatType::P2p {
            target.conversation_type = CONV_TYPE_P2P.to_string();
            target.staff_id.clone_from(&source.sender_id);
        }
        target
    }

    /// 这是群发吗（引用**只**在群发里出现）。
    #[must_use]
    pub fn is_group(&self) -> bool {
        self.conversation_type != CONV_TYPE_P2P
    }
}

/// 空串 / 认不出的会话类型一律按群处理（上游零值 + 默认语义）。
fn clean_conversation_type(raw: &str) -> String {
    if raw == CONV_TYPE_P2P {
        CONV_TYPE_P2P.to_string()
    } else {
        CONV_TYPE_GROUP.to_string()
    }
}

// =====================================================================
// 分片（上游 `markdownParam` / `replyMarkdownChunk*`）
// =====================================================================

/// `sampleMarkdown` 的 `msgParam` 载荷（上游 `markdownParam`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkdownParam {
    pub title: String,
    pub text: String,
}

/// 一片待发送的 Markdown（上游 `replyMarkdownChunk`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownChunk {
    pub text: String,
    pub title: String,
}

/// 一片的**序列化后**载荷长度（判据是它，不是正文长度）。
fn payload_len(chunk: &MarkdownChunk) -> Option<usize> {
    serde_json::to_string(&MarkdownParam {
        title: chunk.title.clone(),
        text: chunk.text.clone(),
    })
    .ok()
    .map(|payload| payload.len())
}

/// 按**序列化后**的载荷预算分片（上游 `replyMarkdownChunks`）。
///
/// 预算从 14000 起一路折半到 512：转义会把正文放大，装不下就**重切更小**，好让代码围栏
/// 保持完整。所有预算都装不下 ⇒ [`crate::dingtalk::outbound::DingTalkApiError::PayloadBudget`]。
///
/// # Errors
///
/// 见上。
pub fn reply_markdown_chunks(
    text: &str,
    quote: &str,
) -> Result<Vec<MarkdownChunk>, crate::dingtalk::outbound::DingTalkApiError> {
    let mut budget = MARKDOWN_BYTE_BUDGET;
    while budget >= 512 {
        let chunks = reply_markdown_chunks_with_budget(text, quote, budget);
        if chunks
            .iter()
            .all(|chunk| payload_len(chunk).is_some_and(|len| len <= MARKDOWN_PAYLOAD_BYTE_BUDGET))
        {
            return Ok(chunks);
        }
        budget /= 2;
    }
    Err(crate::dingtalk::outbound::DingTalkApiError::PayloadBudget)
}

/// 用显式预算分片（上游 `replyMarkdownChunksWithBudget`）：引用前缀只进**首片**。
#[must_use]
pub fn reply_markdown_chunks_with_budget(
    text: &str,
    quote: &str,
    byte_budget: usize,
) -> Vec<MarkdownChunk> {
    let prefix = prepend_markdown_quote("", quote);
    // 多行引用可能比正文预算还长，而它在 wire 上仍然装得下：首片预算里给"一条回答 +
    // 续片围栏"留位；调用方随后用**序列化后**的载荷校验（上游逐字）。
    let reserved = MAX_MARKDOWN_FENCE_INFO_BYTES + 32;
    let first_budget = byte_budget.saturating_sub(prefix.len()).max(reserved);
    chunk_markdown_with_first_budget(text, first_budget, byte_budget)
        .into_iter()
        .enumerate()
        .map(|(index, body)| MarkdownChunk {
            title: markdown_title(&body),
            text: if index == 0 {
                format!("{prefix}{body}")
            } else {
                body
            },
        })
        .collect()
}
