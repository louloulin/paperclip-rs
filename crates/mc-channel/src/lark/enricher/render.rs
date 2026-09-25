//! 三个渲染块 + 发言人标签器（上游 `inbound_enricher.go` 的
//! `renderRecentContextBlock` / `renderQuotedBlock` / `renderForwardedItems` /
//! `flattenMessage` / `restMentionsToEvent` / `wrapQuoted` / `quotedErrorBlock` /
//! `forwardedErrorBlock` / `senderOpenIDs` / `speakerLabeler`）。
//!
//! - **写者**：M7-12（`crate::lark::enricher` 的子模块，见 `docs/32` §29）。
//! - **拆出来的理由**：门 ⑩ 的 800 行硬限 —— 渲染是纯函数，与"取回 / 重试"的时序逻辑
//!   没有关系，分开后两块各自都能整屏读完。
//! - **本文件零 I/O、零 `tracing::*`**：输入是已经取回的消息，输出是要拼进正文的字符串。
//!   正文（用户的话）**只**进返回值，不进日志 —— 见 `enricher.rs` 的凭据一段。

use std::collections::HashMap;
use std::fmt::Write as _;

use super::super::client::ApiError;
use super::super::content_flatten::{
    flatten_content, mentions_from_rest, resolve_mentions, MSG_TYPE_MERGE_FORWARD,
};
use super::super::types::LarkMessage;

/// 发言人的显示名映射（`open_id` → 显示名）。
pub type SpeakerNames = HashMap<String, String>;

/// 一组消息里**互异**的非 app 发言人（上游 `senderOpenIDs`）。
///
/// 按首次出现顺序返回 —— 这是 Contact 查名的输入集合。
#[must_use]
pub fn sender_open_ids(messages: &[LarkMessage]) -> Vec<String> {
    let mut seen = std::collections::HashSet::with_capacity(messages.len());
    let mut out = Vec::with_capacity(messages.len());
    for message in messages {
        if message.sender_type == "app" || message.sender_id.is_empty() {
            continue;
        }
        if seen.insert(message.sender_id.clone()) {
            out.push(message.sender_id.clone());
        }
    }
    out
}

/// 把一条取回的消息摊平成纯文本（上游 `flattenMessage`）。
///
/// 先按 `msg_type` 结构摊平，再拿消息**自己**的提及数组解 `@_user_N` 占位。
/// **不**剥 bot 提及（与入站解码器相反）：引用 / 转发的消息是历史上下文而不是新的触发，
/// 所以两个 bot 标识都传空串 ⇒ 每一条 `@` 都渲染成可读的 `@名字`。
#[must_use]
pub fn flatten_message(message: &LarkMessage) -> String {
    if message.deleted {
        return "[deleted message]".to_string();
    }
    let raw = flatten_content(&message.message_type, &message.content);
    if raw.is_empty() {
        return String::new();
    }
    // REST 形状的提及（裸 `open_id`）经 `mentions_from_rest` 归一到共用形态
    // （上游 `restMentionsToEvent`）—— 一份提及实现服务两条入站路径。
    resolve_mentions(&raw, &mentions_from_rest(&message.mentions), "", "")
}

/// 给一个块里的发言人分配稳定、可读的标签（上游 `speakerLabeler`）。
///
/// Lark 的消息项只带一个发言人 id（载荷里没有显示名），所以装配器用 Contact API 旁路解析
/// 出真名，再以 `sender_id → name` 映射传进来。在映射里的发言人用真名；不在的
/// （通讯录范围受限、用户停用、查名失败）按**首次出现顺序**退化成 `User 1` / `User 2` …。
/// app 发言人恒为 `Bot`。
#[derive(Debug)]
pub struct SpeakerLabeler {
    names: Option<SpeakerNames>,
    seen: HashMap<String, String>,
    index: usize,
}

impl SpeakerLabeler {
    /// 用一份（可能为 `None` 的）显示名映射造标签器。
    #[must_use]
    pub fn new(names: Option<&SpeakerNames>) -> Self {
        Self {
            names: names.cloned(),
            seen: HashMap::new(),
            index: 0,
        }
    }

    /// 一条消息的标签。
    pub fn label(&mut self, message: &LarkMessage) -> String {
        if message.sender_type == "app" {
            return "Bot".to_string();
        }
        let key = if message.sender_id.is_empty() {
            "unknown".to_string()
        } else {
            message.sender_id.clone()
        };
        if let Some(label) = self.seen.get(&key) {
            return label.clone();
        }
        let resolved = self
            .names
            .as_ref()
            .and_then(|names| names.get(&key))
            .filter(|name| !name.is_empty())
            .cloned();
        let label = resolved.unwrap_or_else(|| {
            self.index += 1;
            format!("User {}", self.index)
        });
        self.seen.insert(key, label.clone());
        label
    }
}

/// 把周围一段会话渲染成 `<recent_context>` 块（上游 `renderRecentContextBlock`）：
/// 每条消息一行 `[<发言人>]: <正文>`，最旧在前，发言人用真名（解析不出退化成 `User N`）。
///
/// 调用方保证 `kept` 非空。
#[must_use]
pub fn render_recent_context_block(kept: &[LarkMessage], names: Option<&SpeakerNames>) -> String {
    let mut labeler = SpeakerLabeler::new(names);
    let lines: Vec<String> = kept
        .iter()
        .map(|message| {
            let label = labeler.label(message);
            let text = if message.message_type == MSG_TYPE_MERGE_FORWARD {
                "[merge_forward, expand manually]".to_string()
            } else {
                let text = flatten_message(message);
                if text.is_empty() {
                    "[empty message]".to_string()
                } else {
                    text
                }
            };
            format!("[{label}]: {text}")
        })
        .collect();
    let mut out = String::new();
    let _ = write!(
        out,
        "<recent_context count=\"{}\">\n{}\n</recent_context>",
        kept.len(),
        lines.join("\n")
    );
    out
}

/// 把已经取回的 `GetMessage(parent_id)` 结果渲染成 `<quoted_message>` 块
/// （上游 `renderQuotedBlock`）。
///
/// 本身就是 `merge_forward` 的父消息会在引用块里**嵌套**一份 `<forwarded_messages>`
/// 记录（`GetMessage` 的响应已经同时带着转发哨兵与它的子消息）。取回失败 / 空 / 已删的父
/// 消息退化成文档化的错误块。发言人用共享的（已经解析好的）`names`，解析不出退化成 `User N`。
#[must_use]
pub fn render_quoted_block(
    parent_id: &str,
    items: &[LarkMessage],
    error: Option<&ApiError>,
    names: Option<&SpeakerNames>,
    max_children: usize,
) -> String {
    if error.is_some() || items.is_empty() {
        return quoted_error_block(parent_id);
    }
    let parent = &items[0];
    if parent.deleted {
        return quoted_error_block(parent_id);
    }

    let mut labeler = SpeakerLabeler::new(names);
    let sender = labeler.label(parent);

    if parent.message_type == MSG_TYPE_MERGE_FORWARD {
        // 嵌套转发：上限与顶层一致（上游把同一个 `maxForwardChildren` 传下去）。
        return wrap_quoted(
            parent_id,
            &sender,
            MSG_TYPE_MERGE_FORWARD,
            &render_forwarded_items(items, parent_id, names, max_children),
        );
    }
    let text = flatten_message(parent);
    let text = if text.is_empty() {
        "[empty message]".to_string()
    } else {
        text
    };
    wrap_quoted(parent_id, &sender, &parent.message_type, &text)
}

/// 渲染一条转发的子消息（上游 `renderForwardedItems`）。
///
/// 子消息按时间排序、截断到 `max_children`，每条渲染成 `[<发言人>]: <正文>`；
/// **本身也是转发**的子消息不再递归（它只拿到一个"手动展开"占位），
/// 这样入站 ACK 敏感路径上的 HTTP 扇出保持有界。
#[must_use]
pub fn render_forwarded_items(
    items: &[LarkMessage],
    forward_id: &str,
    names: Option<&SpeakerNames>,
    max_children: usize,
) -> String {
    // 已核实的契约是 `GetMessage(forward_id)` 返回**一层**打包：`[哨兵, 直接子消息…]`。
    // 因此这里把每一个非哨兵项都当成直接子消息。按 id 过滤（**不**按
    // `upper_message_id == forwardID`）是刻意的：严格的 upper_message_id 匹配会在 Lark
    // 偶尔不填那个字段时**静默丢掉**一条真子消息。
    let mut children: Vec<&LarkMessage> = items
        .iter()
        .filter(|item| item.message_id != forward_id)
        .collect();
    let total = children.len();
    if total == 0 {
        return "<forwarded_messages count=\"0\">\n[no forwarded content available]\n</forwarded_messages>"
            .to_string();
    }

    children.sort_by_key(|item| super::parse_lark_millis(&item.create_time));

    let truncated = total.saturating_sub(max_children);
    if truncated > 0 {
        children.truncate(max_children);
    }

    let mut labeler = SpeakerLabeler::new(names);
    let lines: Vec<String> = children
        .iter()
        .map(|child| {
            let label = labeler.label(child);
            let text = if child.message_type == MSG_TYPE_MERGE_FORWARD {
                "[nested merge_forward, expand manually]".to_string()
            } else {
                let text = flatten_message(child);
                if text.is_empty() {
                    "[empty message]".to_string()
                } else {
                    text
                }
            };
            format!("[{label}]: {text}")
        })
        .collect();
    let mut body = lines.join("\n");
    if truncated > 0 {
        let _ = write!(body, "\n... ({truncated} more truncated)");
    }
    format!("<forwarded_messages count=\"{total}\">\n{body}\n</forwarded_messages>")
}

/// 引用块的包壳（上游 `wrapQuoted`）—— 属性顺序与引号形态逐字照搬。
#[must_use]
pub fn wrap_quoted(message_id: &str, sender: &str, msg_type: &str, inner: &str) -> String {
    format!(
        "<quoted_message message_id={message_id:?} sender={sender:?} type={msg_type:?}>\n{inner}\n</quoted_message>"
    )
}

/// 取不到父消息时的引用块（上游 `quotedErrorBlock`）。
#[must_use]
pub fn quoted_error_block(message_id: &str) -> String {
    format!("<quoted_message message_id={message_id:?} type=\"error\">[unable to fetch]</quoted_message>")
}

/// 取不到转发内容时的块（上游 `forwardedErrorBlock`）。
#[must_use]
pub fn forwarded_error_block() -> String {
    "<forwarded_messages type=\"error\">[unable to fetch]</forwarded_messages>".to_string()
}

#[cfg(test)]
mod tests;
