//! 历史读面的**正文摊平与命名**（上游 `flattenSlackText` / `attachmentText` /
//! `flattenBlocks` / `richTextBlockText` / `truncateRunes` / `resolveUserNames` /
//! `historyLabeler`）。
//!
//! - **写者**：M7-4。切分理由见 `super` 的「文件布局」小节。
//! - **告警卡也要读得出来**：Grafana / incoming-webhook 一类 bot 把正文全放在
//!   `attachments` 或 Block Kit 里，`text` 是空的；没有这条回落，那种消息会与
//!   join / 系统标记无法区分而被丢掉（上游 MUL-3931 / #4803）。
//! - **人名解析失败不是错误**：缺 `users:read` 作用域或传输抖动 ⇒ 空表，标签器退回
//!   位置序号的 `User N`（上游逐字：would rather block the read —— 这里**不**阻断）。

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::{array_at, HistoryApi, SlackMessage, SlackUser, MAX_DERIVED_TEXT_CHARS};

// =====================================================================
// 纯函数：摊平与命名
// =====================================================================

/// 把一条 Slack 消息渲染成历史契约承诺的纯文本正文（上游 `flattenSlackText`）。
///
/// 顺序逐字照上游：顶层正文 → 各附件的渲染文本 → **最后手段**的 fallback → 兜底的块摊平。
/// 都渲染不出东西 ⇒ 空串（那是**真的**系统标记，例如 join / edit）。
#[must_use]
pub fn flatten_slack_text(message: &SlackMessage) -> String {
    if !message.text.trim().is_empty() {
        return message.text.trim().to_string();
    }
    let mut parts: Vec<String> = Vec::with_capacity(message.attachments.len() + 1);
    for attachment in &message.attachments {
        let text = attachment_text(attachment);
        if !text.is_empty() {
            parts.push(text);
        }
    }
    if parts.is_empty() {
        let text = flatten_blocks(&message.blocks);
        if !text.is_empty() {
            parts.push(text);
        }
    }
    truncate_chars(parts.join("\n").trim(), MAX_DERIVED_TEXT_CHARS)
}

/// 一个附件的摘要（上游 `attachmentText`）。
///
/// `pretext` → `title` → `text` → 各 `fields`；都空才用 `fallback`，再空才摊平它的块。
#[must_use]
pub fn attachment_text(attachment: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    for key in ["pretext", "title", "text"] {
        if let Some(value) = attachment.get(key).and_then(Value::as_str) {
            if !value.trim().is_empty() {
                parts.push(value.trim().to_string());
            }
        }
    }
    if let Some(fields) = attachment.get("fields").and_then(Value::as_array) {
        for field in fields {
            let title = field
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim();
            let value = field
                .get("value")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim();
            let joined = format!("{title} {value}").trim().to_string();
            if !joined.is_empty() {
                parts.push(joined);
            }
        }
    }
    if !parts.is_empty() {
        return parts.join("\n");
    }
    if let Some(fallback) = attachment.get("fallback").and_then(Value::as_str) {
        if !fallback.trim().is_empty() {
            return fallback.trim().to_string();
        }
    }
    let blocks = array_at(attachment, "blocks");
    flatten_blocks(&blocks)
}

/// Block Kit 块的**尽力而为**摊平（上游 `flattenBlocks` + `richTextBlockText`）。
///
/// 只看有正文的常见块（`section` / `header` / `context` / `markdown` / `rich_text`），
/// 跳过互动与媒体块。
#[must_use]
pub fn flatten_blocks(blocks: &[Value]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for block in blocks {
        match block
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "section" | "header" => {
                push_trimmed(
                    &mut parts,
                    block.pointer("/text/text").and_then(Value::as_str),
                );
                if let Some(fields) = block.get("fields").and_then(Value::as_array) {
                    for field in fields {
                        push_trimmed(&mut parts, field.get("text").and_then(Value::as_str));
                    }
                }
            }
            "markdown" => push_trimmed(&mut parts, block.get("text").and_then(Value::as_str)),
            "context" => {
                if let Some(elements) = block.pointer("/elements").and_then(Value::as_array) {
                    for element in elements {
                        push_trimmed(&mut parts, element.get("text").and_then(Value::as_str));
                    }
                }
            }
            "rich_text" => {
                let text = rich_text_lines(block);
                if !text.is_empty() {
                    parts.push(text);
                }
            }
            _ => {}
        }
    }
    parts.join("\n")
}

fn push_trimmed(parts: &mut Vec<String>, value: Option<&str>) {
    if let Some(value) = value {
        if !value.trim().is_empty() {
            parts.push(value.trim().to_string());
        }
    }
}

/// `rich_text` 块的摊平：每个 section / quote / preformatted / list 项拼成一行
/// （上游 `richTextBlockText`；提及与 emoji 跳过 —— 这是 agent 要的纯正文）。
#[must_use]
pub fn rich_text_lines(block: &Value) -> String {
    let mut lines: Vec<String> = Vec::new();
    let elements = block
        .get("elements")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for element in &elements {
        collect_rich_text(element, &mut lines);
    }
    lines.join("\n")
}

fn collect_rich_text(element: &Value, lines: &mut Vec<String>) {
    match element
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "rich_text_section" | "rich_text_quote" | "rich_text_preformatted" => {
            let mut buffer = String::new();
            if let Some(inner) = element.get("elements").and_then(Value::as_array) {
                for run in inner {
                    match run.get("type").and_then(Value::as_str).unwrap_or_default() {
                        "text" => buffer
                            .push_str(run.get("text").and_then(Value::as_str).unwrap_or_default()),
                        "link" => {
                            let label = run.get("text").and_then(Value::as_str).unwrap_or_default();
                            if label.is_empty() {
                                buffer.push_str(
                                    run.get("url").and_then(Value::as_str).unwrap_or_default(),
                                );
                            } else {
                                buffer.push_str(label);
                            }
                        }
                        _ => {}
                    }
                }
            }
            if !buffer.trim().is_empty() {
                lines.push(buffer.trim().to_string());
            }
        }
        "rich_text_list" => {
            if let Some(inner) = element.get("elements").and_then(Value::as_array) {
                for item in inner {
                    collect_rich_text(item, lines);
                }
            }
        }
        _ => {}
    }
}

/// 截到 `max` 个 **char**，截断时补省略号（上游 `truncateRunes`）。
#[must_use]
pub fn truncate_chars(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_string();
    }
    let mut truncated: String = chars[..max].iter().collect();
    truncated.push('…');
    truncated
}

/// Slack 用户最友好的名字（上游 `slackDisplayName`）。
#[must_use]
pub fn slack_display_name(user: &SlackUser) -> String {
    if !user.display_name.is_empty() {
        return user.display_name.clone();
    }
    if !user.real_name.is_empty() {
        return user.real_name.clone();
    }
    user.name.clone()
}

/// 一次页面内的稳定人名标签（上游 `historyLabeler`）。
///
/// 本 bot 是 `Bot`；解析出来的人用真名；没解析出来的退化到**位置序号** `User N`；
/// 第三方 bot 用它的 posted username。
#[derive(Debug, Default)]
pub struct HistoryLabeler {
    names: HashMap<String, String>,
    seen: HashMap<String, String>,
    positional: usize,
}

impl HistoryLabeler {
    /// 装配（`names` 是 `users.info` 的结果）。
    #[must_use]
    pub fn new(names: HashMap<String, String>) -> Self {
        Self {
            names,
            seen: HashMap::new(),
            positional: 0,
        }
    }

    /// 给一条消息一个标签。
    pub fn label(&mut self, message: &SlackMessage, own: bool) -> String {
        if own {
            return "Bot".to_string();
        }
        let key = if message.user.is_empty() {
            if !message.username.is_empty() {
                return message.username.clone();
            }
            format!("bot:{}", message.bot_id)
        } else {
            message.user.clone()
        };
        if let Some(label) = self.seen.get(&key) {
            return label.clone();
        }
        let label = if let Some(name) = self.names.get(&message.user) {
            if name.is_empty() {
                self.fallback_label(message)
            } else {
                name.clone()
            }
        } else {
            self.fallback_label(message)
        };
        self.seen.insert(key, label.clone());
        label
    }

    fn fallback_label(&mut self, message: &SlackMessage) -> String {
        if !message.username.is_empty() {
            return message.username.clone();
        }
        self.positional += 1;
        format!("User {}", self.positional)
    }
}

/// 批量解析人名（上游 `resolveUserNames`）：失败 ⇒ 空表（标签器退回 `User N`），**不报错**。
pub async fn resolve_user_names(
    api: &dyn HistoryApi,
    bot_token: &str,
    messages: &[SlackMessage],
    bot_user_id: &str,
) -> HashMap<String, String> {
    let mut seen = HashSet::new();
    let mut ids: Vec<String> = Vec::new();
    for message in messages {
        if message.user.is_empty() || message.user == bot_user_id {
            continue;
        }
        if seen.insert(message.user.clone()) {
            ids.push(message.user.clone());
        }
    }
    if ids.is_empty() {
        return HashMap::new();
    }
    match api.users_info(bot_token, &ids).await {
        Ok(users) => users
            .into_iter()
            .filter_map(|user| {
                let name = slack_display_name(&user);
                if name.is_empty() {
                    None
                } else {
                    Some((user.id, name))
                }
            })
            .collect(),
        Err(error) => {
            tracing::warn!(
                ids = ids.len(),
                code = error.code(),
                "slack history: user name resolution failed"
            );
            HashMap::new()
        }
    }
}
