//! 引用（选中）消息的渲染与引用上下文（上游 `inbound.go` 的引用段：`dingTalkReplyMetadata` /
//! `renderDingTalkQuotedMessage` / `renderDingTalkQuotedRichText` / `applyDingTalkReplyContext`）。
//!
//! - **写者**：M7-7（`docs/60-M7-PLAN.md` §3.3；本片的写集勘误见 `docs/32` §19）。
//! - 拆出来是**门 ⑩**（800 行硬限）的要求：`inbound.go` 的这三段自成一体（输入 = 一张引用
//!   快照，输出 = 一段引用块 + 媒体的位次），与"当前消息"的归一化只在一处相接
//!   （[`apply_dingtalk_reply_context`]）。
//!
//! # 位次（`inline_index`）是这段代码唯一的**不变量**
//!
//! 引用块会被**拼在**当前正文之前，所以引用块里的每个 [`IMAGE_PLACEHOLDER`] 都会把当前消息的
//! 媒体位次整体右移。三处偏移必须逐处算清（上游注释逐字点名）：
//!
//! 1. 引用块内部的偏移（[`QuotedBody`] 边拼边数）；
//! 2. 富文本引用自己带的起始偏移（上游 `renderDingTalkQuotedRichText(content, placeholderOffset)`）；
//! 3. [`crate::message::format_quoted_message`] 加作者前缀时**新增**的占位符数
//!    （`prefixMarkers`：作者名被转义，理论上不可能含 `[Image]`，但这条**必须**按"数出来的差"
//!    算，不能假设它是 0）。
//!
//! 数错的后果是静默的：正文里的图片会被归到错误的下载码上。

use mc_core::channel::message::{InboundMessage, MessageKind, ReplyCtx};

use super::card;
use super::{
    append_image_placeholder, ref_alt, BotCallbackData, BotCallbackRepliedContent,
    BotCallbackRepliedMessage, BotCallbackReplyMetadata, DingtalkMediaResource, DingtalkRawEvent,
    IMAGE_PLACEHOLDER, IMAGE_UNAVAILABLE,
};

/// 当前轮次的**可见**正文（不含引用历史；上游 `dingtalkCurrentVisibleText`）。
///
/// 它在 bot 寻址的提及被剥掉**之后**、Router 消费控制指令**之前**取值 ⇒ 命令回执引用的是
/// 发送者真正发出来的东西。
#[must_use]
pub fn dingtalk_current_visible_text(message: &InboundMessage) -> String {
    message.text.trim().to_string()
}

/// 读引用元信息（上游 `dingTalkReplyMetadata`）：优先 `text`，缺失字段从 `content` 补。
///
/// 两个位置都**不被**公开收信 schema 保证 ⇒ 这是有界的最佳努力投影：**不**从别的回调字段
/// 推断选中的正文。
///
/// `content` 解不进 [`BotCallbackReplyMetadata`] 时整段丢弃（与上游 `json.Unmarshal` 出错同款）
/// —— 注意 `msgtype=richText` 的引用元信息正是放在 `content` 里的（`quoted_bot_channels.json`
/// 的第二个变体就是这条路径）。
#[must_use]
pub fn dingtalk_reply_metadata(data: &BotCallbackData) -> BotCallbackReplyMetadata {
    let mut metadata = BotCallbackReplyMetadata {
        is_reply_msg: data.text.is_reply_msg,
        replied_msg: data.text.replied_msg.clone(),
    };
    if data.content.is_null() || (metadata.is_reply_msg && metadata.replied_msg.is_some()) {
        return metadata;
    }
    let Ok(from_content) = serde_json::from_value::<BotCallbackReplyMetadata>(data.content.clone())
    else {
        return metadata;
    };
    if !metadata.is_reply_msg {
        metadata.is_reply_msg = from_content.is_reply_msg;
    }
    if metadata.replied_msg.is_none() {
        metadata.replied_msg = from_content.replied_msg;
    }
    metadata
}

/// 把引用上下文并进当前消息（上游 `applyDingTalkReplyContext`）。
///
/// 四件事：
///
/// 1. **`reply_to`**：父消息 id 取 `repliedMsg.msgId`，退到 `originalMsgId`；两者都空也**照样**
///    记下这次引用关系（可选的平台坐标缺失不该抹掉"用户选了东西"这个事实）；
/// 2. **控制指令**：正文一旦被引用块富化，共享 Router 就再也不能靠"`text` 与 `commandText`
///    比对"来剥前导指令 ⇒ 在这里先剥可见的那一份，`command_text` 保持真值。
///    **只有未被触碰的当前正文**才走这一步（富文本已经重建过布局；再解一次会让一轮对话有
///    两个控制含义）；
/// 3. **媒体位次**：引用块里的占位符把它后面的当前媒体整体右移；
/// 4. **`kind`**：只要最终有媒体引用，消息就是 [`MessageKind::Image`]。
pub fn apply_dingtalk_reply_context(
    data: &BotCallbackData,
    message: &mut InboundMessage,
    raw_event: &mut DingtalkRawEvent,
) {
    let reply = dingtalk_reply_metadata(data);
    let mut replied = reply.replied_msg.clone();
    if !reply.is_reply_msg && replied.is_none() && data.original_msg_id.is_empty() {
        return;
    }
    let mut parent_id = data.original_msg_id.trim().to_string();
    if let Some(snapshot) = replied.as_ref() {
        let snapshot_id = snapshot.msg_id.trim();
        if !snapshot_id.is_empty() {
            parent_id = snapshot_id.to_string();
        }
    }
    message.reply_to = Some(ReplyCtx {
        message_id: parent_id,
        root_id: String::new(),
    });
    if replied.is_none() {
        if !reply.is_reply_msg {
            return;
        }
        // 显式的引用**没有**快照仍然是"选中的输入"：光有一个线程坐标不代表用户选了东西。
        replied = Some(BotCallbackRepliedMessage::default());
    }

    let instruction = message.text.clone();
    let mut visible_instruction = instruction.clone();
    if instruction == message.command_text {
        if let Some(control) = crate::engine::commands::parse_control_command(&instruction) {
            visible_instruction.clone_from(&control.body);
            if control.kind == crate::engine::commands::ControlCommandKind::FreshSession {
                message.force_fresh = true;
            }
        }
    }

    let (block, quoted_media) = render_dingtalk_quoted_message(replied.as_ref());
    let current_media = std::mem::take(&mut raw_event.media);
    let placeholder_offset =
        i32::try_from(block.matches(IMAGE_PLACEHOLDER).count()).unwrap_or(i32::MAX);
    let mut media = quoted_media;
    media.extend(current_media.into_iter().map(|mut resource| {
        resource.inline_index = resource.inline_index.saturating_add(placeholder_offset);
        resource
    }));
    raw_event.media = media;

    message.text.clone_from(&block);
    message.has_selected_context = !block.is_empty();
    if !visible_instruction.is_empty() {
        message.text = format!("{block}\n\n{visible_instruction}");
    }
    if !raw_event.media.is_empty() {
        message.kind = MessageKind::Image;
    }
}

/// 渲染一条引用快照（上游 `renderDingTalkQuotedMessage`）：返回 `(引用块, 它的媒体)`。
///
/// `msgType` 判别式逐条的处置见下面的 `match`；`SenderId` 是**不透明的平台身份**、不是显示名
/// ⇒ 残缺快照（没有 `SenderNick`）保留引用但**不**编造作者。
#[must_use]
pub fn render_dingtalk_quoted_message(
    replied: Option<&BotCallbackRepliedMessage>,
) -> (String, Vec<DingtalkMediaResource>) {
    let Some(replied) = replied else {
        return (String::new(), Vec::new());
    };
    let sender = replied.sender_nick.trim();
    let raw_kind = replied.msg_type.trim();
    let msg_type = if raw_kind.is_empty() {
        "unknown"
    } else {
        raw_kind
    };
    let mut body = QuotedBody::default();

    match msg_type {
        "text" => body.append_text(&card::readable_quoted_text(&replied.content.text)),
        "interactiveCard" => {
            body.append_text(&card::render_dingtalk_quoted_card(
                &replied.content.card_content,
            ));
        }
        "picture" | "image" => {
            body.append_picture(
                &replied.content.download_code,
                &replied.content.picture_download_code,
            );
            // 快照里的 `text` 字段没有文档化的 caption 含义：保留图片，同时**显式**说明
            // supplementary 文本被扣下了。
            if !replied.content.text.is_empty() {
                body.append_text(&format!("\n{}", card::UNAVAILABLE));
            }
        }
        "richText" => {
            let (quoted_body, quoted_media) =
                render_dingtalk_quoted_rich_text(&replied.content, body.placeholders);
            body.append_text(&quoted_body);
            body.media.extend(quoted_media);
        }
        "file" => {
            let name = replied.content.file_name.trim();
            if name.is_empty() {
                body.append_text("[File]");
            } else {
                body.append_text(&format!("[File: {name}]"));
            }
        }
        "audio" => {
            let recognition = replied.content.recognition.trim();
            if recognition.is_empty() {
                body.append_text("[Audio message]");
            } else {
                body.append_text(&card::readable_quoted_text(recognition));
            }
        }
        "video" => body.append_text("[Video message]"),
        _ => body.append_text(card::UNAVAILABLE),
    }

    let quoted_body = if body.body.trim().is_empty() {
        card::UNAVAILABLE.to_string()
    } else {
        body.body.trim().to_string()
    };
    let block = crate::message::format_quoted_message(sender, &quoted_body);
    // 最终 Markdown 才是媒体位置的权威。格式化只加了作者前缀与引用标记 ⇒ 把"由前缀引入的
    // 占位符数"补到每一个位次上（用数出来的差，不假设它是 0）。
    let prefix_markers = i32::try_from(block.matches(IMAGE_PLACEHOLDER).count())
        .unwrap_or(i32::MAX)
        .saturating_sub(
            i32::try_from(quoted_body.matches(IMAGE_PLACEHOLDER).count()).unwrap_or(i32::MAX),
        );
    let media = body
        .media
        .into_iter()
        .map(|mut resource| {
            resource.inline_index = resource.inline_index.saturating_add(prefix_markers);
            resource
        })
        .collect();
    (block, media)
}

/// 渲染富文本引用（上游 `renderDingTalkQuotedRichText`）。
///
/// 只复用 `text` / `picture` 那套**有序** schema（
/// <https://open.dingtalk.com/document/orgapp/receive-message>）：公开 schema 既没定义
/// `repliedMsg`、也没定义"引用的文本摘要与 `richText` 的关系" ⇒ **绝不**把摘要标记与资源配对。
#[must_use]
pub fn render_dingtalk_quoted_rich_text(
    content: &BotCallbackRepliedContent,
    placeholder_offset: usize,
) -> (String, Vec<DingtalkMediaResource>) {
    if content.rich_text.is_empty() {
        return (card::UNAVAILABLE.to_string(), Vec::new());
    }
    let mut body = String::new();
    let mut media: Vec<DingtalkMediaResource> = Vec::new();
    let has_text = content
        .rich_text
        .iter()
        .any(|item| !item.text.trim().is_empty());
    if !has_text && !content.text.trim().is_empty() {
        // 只有媒体的快照可能省掉了散文：把这个损失**说明白**，但不猜预览该落在哪张图之间。
        body.push_str(card::UNAVAILABLE);
        body.push('\n');
    }
    let mut marker_count = placeholder_offset;
    for item in &content.rich_text {
        let text = card::readable_quoted_text(&item.text);
        body.push_str(&text);
        marker_count += text.matches(IMAGE_PLACEHOLDER).count();
        if !item.has_picture() {
            continue;
        }
        let (primary, alt) = ref_alt(&item.download_code, &item.picture_download_code);
        if primary.is_empty() {
            if !body.is_empty() && !body.ends_with('\n') {
                body.push('\n');
            }
            body.push_str(IMAGE_UNAVAILABLE);
            continue;
        }
        append_image_placeholder(&mut body);
        media.push(DingtalkMediaResource::at(
            primary,
            alt,
            super::i32_index(marker_count),
        ));
        marker_count += 1;
    }
    (body.trim().to_string(), media)
}

/// 引用块的拼装状态（正文 + 占位符计数 + 媒体）。
///
/// 抽成结构是为了让"边拼边数"成为**一个**地方（Go 用闭包捕获；Rust 里闭包同时可变借用
/// 三个字段写不出来，结构体是同一件事的显式形态）。
#[derive(Debug, Default)]
struct QuotedBody {
    body: String,
    placeholders: usize,
    media: Vec<DingtalkMediaResource>,
}

impl QuotedBody {
    fn append_text(&mut self, value: &str) {
        self.body.push_str(value);
        self.placeholders += value.matches(IMAGE_PLACEHOLDER).count();
    }

    fn append_picture(&mut self, download_code: &str, picture_download_code: &str) {
        let (primary, alt) = ref_alt(download_code, picture_download_code);
        if primary.is_empty() {
            self.append_text(IMAGE_UNAVAILABLE);
            return;
        }
        append_image_placeholder(&mut self.body);
        self.media.push(DingtalkMediaResource::at(
            primary,
            alt,
            super::i32_index(self.placeholders),
        ));
        self.placeholders += 1;
    }
}

#[cfg(test)]
mod tests;
