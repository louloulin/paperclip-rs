//! Telegram 入站：**Bot API wire 类型 + 更新归一化**（上游
//! `internal/integrations/telegram/inbound.go` 306 行 + `api.go` 的类型段）。
//!
//! - **写者**：M7-5（`docs/60-M7-PLAN.md` §3.3；本片的写集勘误见 `docs/32` §17.1）。
//! - **入站是 push，不是 poll**（`docs/60` §2.6 第 4 条）：接收循环在
//!   [`crate::telegram::TelegramChannel::connect`] 里**阻塞跑**，把归一化消息交给构造时注入的
//!   `SharedInboundHandler`。engine **从不**轮询 `Channel` —— 五个平台的"取一条"语义完全不同
//!   （Socket Mode / 自建 WS / Stream / aibot / `getUpdates`），轮询会强迫每个平台把长连接
//!   包装成队列。
//!
//! # 两半，各自可测
//!
//! | 半 | 落点 | 形态 | 用例形态 |
//! | --- | --- | --- | --- |
//! | wire 类型 + 归一化（[`inbound_from_update`] 一族） | **本文件** | **纯函数**，参数化 bot id / 用户名 | 用例直接喂 `Update` 断言 `InboundMessage` 字段 |
//! | HTTP 传输（`getUpdates` / `sendMessage`…） | `crate::telegram::api` | 端口 + `reqwest` 实现 | 本地替身 / 端口替身 |
//! | 轮询回路 + 工厂 | `crate::telegram`（`mod.rs`） | `Channel` 实现 | 端口替身（不睡真觉） |
//!
//! 拆成三个文件是**门 ⑩**（800 行硬限）的要求，不是风格选择；边界正好是上游
//! `api.go` / `inbound.go` / `telegram_channel.go` 的边界。
//!
//! # BYO 安装：一个安装 = 一条轮询回路
//!
//! 每个 `channel_installation` 带自己的 bot（自己的 token），所以每条回路只服务
//! **一个** bot id（= `config->>'app_id'`）。engine 的 `Supervisor` 按活跃安装各建一条，
//! 并管租约 / 重连。Telegram 对同一 bot token 的**第二个** `getUpdates` 消费者回 **409**
//! —— 那正是 Supervisor 的"每安装至多一条活跃回路"保证要防的事（见 `api::ApiError::Conflict`）。
//!
//! # 平台原始载荷
//!
//! 跨平台信封装不下的 Telegram 字段全部留在 [`InboundMessage::raw`]（[`RawEvent`]：
//! `bot_id` / `event_type` / `sender_name`）：只有本 adapter 与
//! [`crate::telegram::resolvers`] 读它，engine **从不**读。

use mc_core::channel::message::{ChatType, InboundMessage, MessageKind, ReplyCtx, Source};
use mc_core::channel::ChannelKind;
use serde::{Deserialize, Serialize};

use crate::engine::commands::{parse_control_command, ControlCommandKind};

/// 本 adapter 的平台判别式（上游 `TypeTelegram`）。
///
/// 定义在 adapter 里是**故意**的（上游注释逐字）：注册一个新平台不该要求改核心。
pub const TYPE_TELEGRAM: ChannelKind = ChannelKind::Telegram;

/// `origin_type`：Telegram `/issue` 建出来的 issue 的来源标签（`telegram_chat`）。
pub const ORIGIN_TELEGRAM_CHAT: &str = "telegram_chat";

/// 丢弃审计用的粗粒度事件标签（上游 `telegramRawEvent.EventType`）。
pub const EVENT_TYPE_MESSAGE: &str = "message";

// =====================================================================
// 平台原始载荷（留在 `InboundMessage::raw` 里，只有 adapter 与 resolver 读）
// =====================================================================

/// 跨平台信封装不下的 Telegram 专有字段（上游 `telegramRawEvent`）。
///
/// `bot_id` 把消息路由到它的安装（`config->>'app_id'` + 唯一索引）；`event_type` 是丢弃审计
/// 的粗标签；`sender_name` 是群上下文里给发送者署名用的显示名。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawEvent {
    /// 路由键：bot 的数值 id（字符串形态）。
    #[serde(default)]
    pub bot_id: String,
    /// 粗粒度事件标签（本片只产 [`EVENT_TYPE_MESSAGE`]）。
    #[serde(default)]
    pub event_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sender_name: String,
}

/// 解出 `raw` 里的平台事件字段（解不开 ⇒ 基础设施失败：`raw` 是本 adapter 自己写的）。
pub fn decode_raw(message: &InboundMessage) -> crate::engine::EngineResult<RawEvent> {
    if message.raw.is_null() {
        return Err(crate::engine::EngineError::infra(
            "telegram: inbound message raw is empty",
        ));
    }
    serde_json::from_value(message.raw.clone()).map_err(|error| {
        crate::engine::EngineError::infra(format!("telegram: decode inbound raw: {error}"))
    })
}

// =====================================================================
// Bot API wire 类型（上游 `api.go` 的四个对象 + `getUpdates` 的信封）
// =====================================================================

/// Bot API 的 `User` 对象（`getMe` 结果 / 消息发送者）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: i64,
    #[serde(default)]
    pub is_bot: bool,
    #[serde(default)]
    pub first_name: String,
    #[serde(default)]
    pub last_name: String,
    #[serde(default)]
    pub username: String,
}

/// Bot API 的 `Chat` 对象。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chat {
    pub id: i64,
    /// `"private"` / `"group"` / `"supergroup"` / `"channel"`。
    #[serde(rename = "type", default)]
    pub chat_type: String,
}

/// Bot API `Message` 对象里本 adapter 真正读的**子集**（上游同名结构体的逐字段对应）。
///
/// `photo` / `voice` / `video` / `document` 只用来**分类**（文本 vs 哪种媒体）：字节的取回
/// 不在本片写集（媒体面归各自的片）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub message_id: i64,
    #[serde(default)]
    pub from: Option<User>,
    #[serde(default)]
    pub chat: Chat,
    #[serde(default)]
    pub date: i64,
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<MessageEntity>,
    #[serde(default)]
    pub caption: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caption_entities: Vec<MessageEntity>,
    #[serde(default)]
    pub reply_to_message: Option<Box<Message>>,
    #[serde(default)]
    pub message_thread_id: i64,
    #[serde(default)]
    pub is_topic_message: bool,
    /// 只关心**有没有**：非空即图片（元素形状与下载不在本片）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub photo: Vec<serde_json::Value>,
    #[serde(default)]
    pub document: Option<serde_json::Value>,
    #[serde(default)]
    pub voice: Option<serde_json::Value>,
    #[serde(default)]
    pub video: Option<serde_json::Value>,
}

/// Bot API 的 `MessageEntity`：`@提及` / `/命令` / 链接 / 格式的**结构化标注**。
///
/// `offset` 与 `length` 的单位是 **UTF-16 code unit**，不是 UTF-8 字节偏移、也不是
/// rune 下标（上游注释逐字）——见 [`message_entity_text`]。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageEntity {
    #[serde(rename = "type", default)]
    pub entity_type: String,
    #[serde(default)]
    pub offset: i32,
    #[serde(default)]
    pub length: i32,
}

/// `getUpdates` 信封里的一条更新。v1 只消费**新消息**（编辑 / 频道帖 / 回调查询都被
/// `allowed_updates` 挡在门外）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Update {
    pub update_id: i64,
    #[serde(default)]
    pub message: Option<Box<Message>>,
}

// =====================================================================
// 归一化
// =====================================================================

/// 把一条 Telegram update 归一化（上游 `inboundFromUpdate`）。
///
/// `None` 表示这条更新**不得**进入核心：bot 自己的消息、`is_bot` 发件人、频道帖
/// （广播频道，没有可交互的发件人上下文）、或认不出的 `chat.type`。
///
/// **群内寻址策略**（上游注释逐字，与 Slack v1 同形）：只有**显式 `@bot` 提及**或**直接
/// 回复 bot 的一条消息**才算在跟 bot 对话。Privacy mode 保持开启，所以 Telegram 已经不会把
/// 未被指名的群聊推给 bot；这里的判定是"管理员在 `BotFather` 里关掉了 privacy mode"时
/// 的**纵深防御**。
#[must_use]
pub fn inbound_from_update(
    update: &Update,
    bot_id: i64,
    bot_username: &str,
) -> Option<InboundMessage> {
    let message = update.message.as_deref()?;
    let from = message.from.as_ref()?;
    if from.is_bot || from.id == bot_id {
        return None;
    }
    let chat_type = telegram_chat_type(&message.chat.chat_type)?;

    let text = if message.text.is_empty() {
        message.caption.as_str()
    } else {
        message.text.as_str()
    };
    let kind = classify_message(message);

    let mentioned = mentions_bot(message, bot_username);
    let replied_to_bot = message
        .reply_to_message
        .as_deref()
        .and_then(|quoted| quoted.from.as_ref())
        .is_some_and(|quoted_from| quoted_from.id == bot_id);
    let addressed = chat_type == ChatType::P2p || mentioned || replied_to_bot;

    let cleaned = normalize_text(text, bot_username);
    let command_text = cleaned.clone();
    let mut body = cleaned;
    let mut force_fresh = false;
    if let Some(control) = parse_control_command(&body) {
        body = control.body;
        force_fresh = control.kind == ControlCommandKind::FreshSession;
    }

    let quoted_human = message
        .reply_to_message
        .as_deref()
        .and_then(|quoted| quoted.from.as_ref())
        .is_some_and(|quoted_from| !quoted_from.is_bot);
    let has_selected_context = chat_type == ChatType::Group && mentioned && quoted_human;
    let agent_text = if has_selected_context {
        message
            .reply_to_message
            .as_deref()
            .map_or(body.clone(), |quoted| {
                enrich_with_quoted_human_message(&body, message.chat.id, quoted)
            })
    } else {
        body
    };

    let sender_id = from.id.to_string();
    let chat_id = message.chat.id.to_string();
    let thread_id = if message.is_topic_message && message.message_thread_id != 0 {
        message.message_thread_id.to_string()
    } else {
        String::new()
    };

    let raw = serde_json::to_value(RawEvent {
        bot_id: bot_id.to_string(),
        event_type: EVENT_TYPE_MESSAGE.to_string(),
        sender_name: sender_display_name(from),
    })
    .unwrap_or(serde_json::Value::Null);

    let reply_to = message.reply_to_message.as_deref().map(|quoted| ReplyCtx {
        message_id: message_key(message.chat.id, quoted.message_id),
        root_id: thread_id.clone(),
    });

    Some(InboundMessage {
        event_id: update.update_id.to_string(),
        // Telegram 的 message id **只在单个 chat 内**唯一，所以去重键
        // `(installation, message_id)` 用 `chat:message` 的复合形态。
        message_id: message_key(message.chat.id, message.message_id),
        source: Source {
            channel_type: TYPE_TELEGRAM,
            chat_id,
            chat_type,
            sender_id: sender_id.clone(),
            // Telegram 的用户 id 是**全局**的 ⇒ 每安装的 id 同时也是跨安装的稳定 id。
            sender_stable_id: sender_id,
            thread_id,
        },
        kind,
        text: agent_text,
        command_text,
        has_selected_context,
        // engine 的**输出通道**（见 `InboundMessage` 的文档）：adapter 必须留空。
        media_refs: Vec::new(),
        reply_to,
        addressed_to_bot: addressed,
        force_fresh,
        // 渠道入站**不需要**"只落库不跑 agent"这条（那是 wecom 的独立 `/issue` 形态）。
        skip_agent_run: false,
        raw,
    })
}

/// 每安装唯一的消息 id：`"chat:message"`（上游 `messageKey`）。
#[must_use]
pub fn message_key(chat_id: i64, message_id: i64) -> String {
    format!("{chat_id}:{message_id}")
}

/// 从复合消息引用里取出**裸** message id（上游 `parseMessageRef`；`sender.go` 归 M7-6，
/// 但入站面（`reply_to` 的拼装）与判决回复都要它）。
#[must_use]
pub fn parse_message_ref(reference: &str) -> i64 {
    let tail = reference
        .rsplit_once(':')
        .map_or(reference, |(_, after)| after);
    tail.parse::<i64>().unwrap_or(0)
}

/// 映射 Telegram 的 `chat.type`。**频道帖不进站**（广播频道，没有可交互的发件人上下文）。
#[must_use]
pub fn telegram_chat_type(raw: &str) -> Option<ChatType> {
    match raw {
        "private" => Some(ChatType::P2p),
        "group" | "supergroup" => Some(ChatType::Group),
        _ => None,
    }
}

/// 把消息载荷映射成归一化的 [`MessageKind`]。v1 只有文本可动作（与 Feishu / Slack 对齐）；
/// 媒体种类要报出来，好让调用方回一句"暂不支持"而不是装死。
#[must_use]
pub fn classify_message(message: &Message) -> MessageKind {
    if !message.text.is_empty() {
        return MessageKind::Text;
    }
    if !message.photo.is_empty() {
        return MessageKind::Image;
    }
    if message.voice.is_some() {
        return MessageKind::Audio;
    }
    if message.video.is_some() {
        return MessageKind::Video;
    }
    if message.document.is_some() {
        return MessageKind::File;
    }
    MessageKind::Unknown
}

/// 消息文本里是否含 `@botusername`。
///
/// Telegram 用 `entities` 标注提及，但对 bot 用户名来说**字面量匹配是等价的**（它们全局唯一
/// 且在文本里总是以 `@` 开头），而且不依赖 entity 的顺序。所以两条路都走：先看 entity
/// （这是 Telegram **正常**给的那条路），再用带边界判定的字面量兜底（老 fixture 与不完整
/// 网关的兼容）。
#[must_use]
pub fn mentions_bot(message: &Message, bot_username: &str) -> bool {
    if bot_username.is_empty() {
        return false;
    }
    let want_mention = format!("@{bot_username}");
    for (text, entities) in [
        (message.text.as_str(), &message.entities),
        (message.caption.as_str(), &message.caption_entities),
    ] {
        for entity in entities {
            if entity.entity_type != "mention" && entity.entity_type != "bot_command" {
                continue;
            }
            let Some(value) = message_entity_text(text, entity) else {
                continue;
            };
            if value.eq_ignore_ascii_case(&want_mention)
                || command_targets_bot(&value, bot_username)
            {
                return true;
            }
        }
    }
    contains_bot_mention(&message.text, bot_username)
        || contains_bot_mention(&message.caption, bot_username)
}

/// 剥掉 bot 提及词但**保留** `/clear`、`/issue` 这类共享命令，供 engine 的命令解析器消费
/// （上游 `normalizeText`）。
#[must_use]
pub fn normalize_text(text: &str, bot_username: &str) -> String {
    let cleaned = if bot_username.is_empty() {
        text.to_string()
    } else {
        remove_bot_mentions(text, bot_username)
    };
    cleaned.trim().to_string()
}

/// 只把**群成员显式选中**的那条消息（回复 bot 并在文本里提及它）前置进上下文。
///
/// 群里的"近期氛围"**永不**进入 agent 上下文。`command_text` 保持发送者自己的清洗后指令
/// ⇒ 被引用消息里的命令仍然只是历史（上游注释逐字）。
#[must_use]
pub fn enrich_with_quoted_human_message(
    instruction: &str,
    chat_id: i64,
    quoted: &Message,
) -> String {
    let mut quoted_text = quoted.text.as_str();
    if quoted_text.is_empty() {
        quoted_text = quoted.caption.as_str();
    }
    let quoted_text = if quoted_text.trim().is_empty() {
        "[empty or non-text message]"
    } else {
        quoted_text
    };
    let sender = quoted
        .from
        .as_ref()
        .map(sender_display_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Unknown user".to_string());
    let block = format!(
        "<quoted_message message_id={:?} sender={:?} type={:?}>\n{}\n</quoted_message>",
        message_key(chat_id, quoted.message_id),
        sender,
        classify_message(quoted).as_str(),
        quoted_text
    );
    if instruction.is_empty() {
        return block;
    }
    format!("{block}\n\n{instruction}")
}

/// `/cmd@botusername` 形态（上游 `commandTargetsBot`）。
#[must_use]
pub fn command_targets_bot(command: &str, bot_username: &str) -> bool {
    command
        .rfind('@')
        .is_some_and(|at| command[at + 1..].eq_ignore_ascii_case(bot_username))
}

/// 按 **UTF-16 code unit** 语义切出 entity 覆盖的文本（上游 `messageEntityText`）。
///
/// Telegram 的 `offset` / `length` 是 UTF-16 单位（上游注释逐字）；Rust 的 `str` 是 UTF-8，
/// 所以要显式过一趟 UTF-16 编解码。
#[must_use]
pub fn message_entity_text(text: &str, entity: &MessageEntity) -> Option<String> {
    if entity.offset < 0 || entity.length <= 0 {
        return None;
    }
    let units: Vec<u16> = text.encode_utf16().collect();
    let start = usize::try_from(entity.offset).ok()?;
    let length = usize::try_from(entity.length).ok()?;
    let end = start.checked_add(length)?;
    if start > units.len() || end > units.len() {
        return None;
    }
    String::from_utf16(&units[start..end]).ok()
}

/// 带边界判定的字面量提及匹配（bot 用户名大小写不敏感，见上游 `containsBotMention`）。
#[must_use]
pub fn contains_bot_mention(text: &str, bot_username: &str) -> bool {
    let token = format!("@{}", bot_username.to_ascii_lowercase());
    let lower = text.to_ascii_lowercase();
    let mut start = 0usize;
    while let Some(found) = lower[start..].find(&token) {
        let index = start + found;
        let end = index + token.len();
        if end == lower.len() || !is_telegram_username_byte(lower.as_bytes()[end]) {
            return true;
        }
        start = end;
    }
    false
}

/// 剥掉全部 `@botusername`（带边界判定，见上游 `removeBotMentions`）。
#[must_use]
pub fn remove_bot_mentions(text: &str, bot_username: &str) -> String {
    let token = format!("@{}", bot_username.to_ascii_lowercase());
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut start = 0usize;
    while start < text.len() {
        let Some(found) = lower[start..].find(&token) else {
            out.push_str(&text[start..]);
            break;
        };
        let index = start + found;
        let end = index + token.len();
        if end < lower.len() && is_telegram_username_byte(lower.as_bytes()[end]) {
            // 边界不成立（`@botname_x`）：整段照抄，继续往后找。
            out.push_str(&text[start..end]);
            start = end;
            continue;
        }
        out.push_str(&text[start..index]);
        start = end;
    }
    out
}

/// Telegram 用户名允许的字符集（上游 `isTelegramUsernameByte` 的**同一集合**）。
///
/// 注意调用方给的是**已小写化**的字节（两个扫描器都在 `to_ascii_lowercase()` 的副本上找）。
fn is_telegram_username_byte(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

/// 渲染 `First Last`，没有名字就回落到 `username`（上游 `senderDisplayName`）。
#[must_use]
pub fn sender_display_name(user: &User) -> String {
    let name = format!("{} {}", user.first_name.trim(), user.last_name.trim());
    let name = name.trim();
    if name.is_empty() {
        user.username.clone()
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests;
