//! 机器人身份的**提及剥离**（上游 `inbound.go` 的 `normalizeDingTalk…Mention` 一族，
//! 与 M7-9 的 `bot_identity.go` 面相邻）。
//!
//! - **写者**：M7-7（`docs/60-M7-PLAN.md` §3.3；本片的写集勘误见 `docs/32` §19）。
//! - 拆出来是**门 ⑩**（800 行硬限）的要求：这一族是**纯字符串**判决（无 I/O、无平台调用），
//!   与"归一化"那一半只在一个方向上有依赖（归一化调它）。
//!
//! # 为什么这件事必须失败关闭
//!
//! `DingTalk` **不给**提及跨度：回调只给一个 `isInAtList` 布尔。所以"剥掉 `@bot`"这件事只能靠
//! **经平台 API 校验过的 bot 名**加"两侧必须是空白 / 串端"的边界规则。**少任何一道闸门**都会
//! 把用户正文里的普通文本当成提及删掉：
//!
//! 1. 必须是**群**（直聊里平台不带这个信封）；
//! 2. 必须**被 @**（`isInAtList`）；
//! 3. 必须有**非空**的、经校验的 bot 名（拿不到名字 ⇒ 什么都不剥）；
//! 4. 名字两侧是空白 / 串端（标点**不**算边界：`Bot-DEV` 可能是另一个显示名的前缀）。

use super::{BotCallbackData, RichTextItem, CONV_TYPE_GROUP};

// =====================================================================
// 机器人身份（mention 剥离）
// =====================================================================

/// 在**单个纯文本**里剥掉 bot 的寻址 token（上游 `normalizeDingTalkBotMention`）。
#[must_use]
pub fn normalize_dingtalk_bot_mention(
    data: &BotCallbackData,
    text: &str,
    bot_name: &str,
) -> String {
    let mut runs = vec![text.to_string()];
    remove_dingtalk_bot_mention(data, &mut runs, bot_name);
    runs.into_iter().next().unwrap_or_default()
}

/// 在富文本的**某一 run** 里剥掉 bot 的寻址包装（上游
/// `normalizeDingTalkRichTextBotMention`）：`DingTalk` 可能把那个 run 放在媒体之前或之后，
/// 也可能让它与含控制指令的 run **不是同一个**。
pub fn normalize_dingtalk_rich_text_bot_mention(
    data: &BotCallbackData,
    items: &mut [RichTextItem],
    bot_name: &str,
) {
    let mut runs: Vec<String> = items.iter().map(|item| item.text.clone()).collect();
    remove_dingtalk_bot_mention(data, &mut runs, bot_name);
    for (item, run) in items.iter_mut().zip(runs) {
        item.text = run;
    }
}

/// 一个提及跨度的位置（run 下标 + 字节区间）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MentionSpan {
    pub run: usize,
    pub start: usize,
    pub end: usize,
}

/// 剥掉 bot 提及（上游 `removeDingTalkBotMention`）。
///
/// 四道闸门缺一不可：必须**是群**、必须**被 @**、必须有**经校验的 bot 名**、且只在名字
/// 两侧都是**空白 / 串端**时才认（见 [`exact_dingtalk_bot_mention_spans`]）。
pub fn remove_dingtalk_bot_mention(data: &BotCallbackData, runs: &mut [String], bot_name: &str) {
    let bot_name = bot_name.trim();
    if data.conversation_type != CONV_TYPE_GROUP || !data.is_in_at_list || bot_name.is_empty() {
        return;
    }
    let mentions = exact_dingtalk_bot_mention_spans(runs, bot_name);
    // 从后往前删：先删后面的跨度不会让前面的下标失效。
    for span in mentions.iter().rev() {
        let run = &runs[span.run];
        let prefix = &run[..span.start];
        let suffix = &run[span.end..];
        let (prefix, suffix) = if prefix.trim().is_empty() {
            (String::new(), trim_left_horizontal_space(suffix))
        } else if suffix.trim().is_empty() {
            (trim_right_horizontal_space(prefix), String::new())
        } else {
            (prefix.to_string(), trim_left_horizontal_space(suffix))
        };
        runs[span.run] = format!("{prefix}{suffix}");
    }
}

/// 找一个 run 里 `@<bot 名>` 的**确切**跨度（上游 `exactDingTalkBotMentionSpans`）。
///
/// `DingTalk` **不给**提及跨度 ⇒ 只有空白 / 串端能证明那个完整名字到此为止：
/// 标点可能本身是另一个显示名的一部分（例如 `Bot-DEV`），所以在标点上**失败关闭**。
#[must_use]
pub fn exact_dingtalk_bot_mention_spans(runs: &[String], bot_name: &str) -> Vec<MentionSpan> {
    let literal = format!("@{bot_name}");
    let mut spans = Vec::new();
    for (run, text) in runs.iter().enumerate() {
        let mut offset = 0usize;
        while offset < text.len() {
            let Some(relative) = text[offset..].find(&literal) else {
                break;
            };
            let start = offset + relative;
            let end = start + literal.len();
            if mention_left_boundary(&text[..start]) && mention_right_boundary(&text[end..]) {
                spans.push(MentionSpan { run, start, end });
            }
            offset = end;
        }
    }
    spans
}

fn mention_left_boundary(prefix: &str) -> bool {
    prefix.chars().next_back().is_none_or(char::is_whitespace)
}

fn mention_right_boundary(suffix: &str) -> bool {
    suffix.chars().next().is_none_or(char::is_whitespace)
}

fn trim_left_horizontal_space(value: &str) -> String {
    value
        .trim_start_matches([' ', '\t', '\u{3000}'])
        .to_string()
}

fn trim_right_horizontal_space(value: &str) -> String {
    value.trim_end_matches([' ', '\t', '\u{3000}']).to_string()
}
