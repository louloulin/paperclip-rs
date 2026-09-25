//! Slack 入站：**事件归一化**（上游 `internal/integrations/slack/inbound.go`，241 行）。
//!
//! - **写者**：M7-3（`docs/60-M7-PLAN.md` §3.3；本片的写集勘误见 `slack/mod.rs`）。
//! - **入站是 push，不是 poll**（`docs/60` §2.6 第 4 条）：接收循环在
//!   [`crate::slack::socket::SlackChannel::connect`] 里**阻塞跑**，把归一化消息交给构造时注入的
//!   `SharedInboundHandler`。engine **从不**轮询 `Channel` —— 五个平台的"取一条"语义完全不同
//!   （Socket Mode / 自建 WS / Stream / aibot / `getUpdates`），轮询会强迫每个平台把长连接包装成队列。
//!
//! # 两半，各自可测（本文件 = 第一半）
//!
//! | 半 | 落点 | 形态 | 用例形态 |
//! | --- | --- | --- | --- |
//! | 事件归一化（[`inbound_from_event`] 一族） | **本文件** | **纯函数**，参数化 bot user id | 用例直接喂 JSON 断言 `InboundMessage` 字段 |
//! | Socket Mode 信封与接收循环 | `crate::slack::socket` | 帧解析是纯函数；传输是端口 | 帧文本 / 替身传输 |
//!
//! 拆成两个文件是**门 ⑩**（800 行硬限）的要求，不是风格选择；按"归一化 / 传输"切，边界正好是
//! 上游 `inbound.go` 与 `slack_channel.go` 的边界。
//!
//! # BYO 安装：一个安装 = 一条连接
//!
//! 每个 `channel_installation` 带自己的 Slack app（自己的 `xapp-`），所以每条连接只服务于
//! **一个** `app_id`（= `config->>'app_id'` = 入站事件的 `api_app_id`）。engine 的 `Supervisor`
//! 按活跃安装各建一条并管租约 / 重连。
//!
//! # 平台原始载荷
//!
//! 跨平台信封装不下的 Slack 字段全部留在 [`InboundMessage::raw`]（`team_id` / `api_app_id` /
//! `files`…）：只有本 adapter 与 [`crate::slack::resolvers`] 读它，engine **从不**读。

use mc_core::channel::message::{ChatType, InboundMessage, MessageKind, ReplyCtx, Source};
use mc_core::channel::ChannelKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 本 adapter 的平台判别式（上游 `TypeSlack`）。
///
/// 定义在 adapter 里是**故意**的（上游注释逐字）：注册一个新平台不该要求改核心。
pub const TYPE_SLACK: ChannelKind = ChannelKind::Slack;

/// `origin_type`：Slack `/issue` 建出来的 issue 的来源标签（`slack_chat`）。
pub const ORIGIN_SLACK_CHAT: &str = "slack_chat";

// =====================================================================
// 平台原始载荷（留在 `InboundMessage::raw` 里，只有 adapter 与 resolver 读）
// =====================================================================

/// 跨平台信封装不下的 Slack 专有字段（上游 `slackRawEvent`）。
///
/// `team_id` 用于把安装与 Slack **工作区**对上（BYO 下一个 app 可能被装进别人的工作区，
/// 事件带的是**同一个** `api_app_id`）；`api_app_id` 是路由键；`files` 是媒体解析的输入。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawEvent {
    #[serde(default)]
    pub team_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api_app_id: String,
    #[serde(default)]
    pub event_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sub_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub channel_type: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<RawFile>,
}

/// 媒体解析真正能取回的文件（上游 `slackRawFile`）。
///
/// 下载 URL 是 Slack 自己托管的 `url_private(_download)`，要本安装的 bot token 才取得到 ——
/// 它**只**在 `raw` 里，绝不交给引擎、也不持久化到别的列。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawFile {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mimetype: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub size: i64,
    #[serde(default)]
    pub download_url: String,
}

/// serde 的 `omitempty` 等价物（`size` 为 0 时不落进 `raw`）。
///
/// 收 `&i64` 是 `skip_serializing_if` 的签名要求（它只拿得到引用）。
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(value: &i64) -> bool {
    *value == 0
}

/// Slack 事件里的一个文件对象（上游 `slack.File` 的**子集**）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct SlackFile {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub mimetype: String,
    #[serde(default)]
    pub size: i64,
    #[serde(default)]
    pub url_private: String,
    #[serde(default)]
    pub url_private_download: String,
}

/// 把事件里的文件对象映射进 `raw`，**只留媒体解析真能取回的**（上游 `rawFilesFrom`）。
///
/// 丢掉两类：还在处理中的上传（没有 URL），以及指向 Google Drive / Dropbox 的"外部"文件
/// （`url_private` 在别人域名上）—— 解析器**不会**把 bot token 发到站外，带着它们只会让
/// `has_media` 承诺一份永远绑不上的媒体（白白推迟一次 run + 一行注定失败的意图账本）。
#[must_use]
pub fn raw_files_from(files: &[SlackFile]) -> Vec<RawFile> {
    let mut out = Vec::new();
    for file in files {
        let download_url = if file.url_private_download.is_empty() {
            file.url_private.clone()
        } else {
            file.url_private_download.clone()
        };
        if !crate::slack::media::is_fetchable_slack_file_url(&download_url) {
            continue;
        }
        out.push(RawFile {
            id: file.id.clone(),
            name: file.name.clone(),
            mimetype: file.mimetype.clone(),
            size: file.size,
            download_url,
        });
    }
    out
}

// =====================================================================
// 事件解析
// =====================================================================

/// Events API 事件的 `event` 对象（上游 `slackevents` 里本 adapter 读到的字段）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct EventBody {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub subtype: String,
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub channel_type: String,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub ts: String,
    #[serde(default)]
    pub thread_ts: String,
    #[serde(default)]
    pub bot_id: String,
    #[serde(default)]
    pub files: Vec<SlackFile>,
}

/// `event_callback` 信封（上游 `slackevents.EventsAPIEvent` 的三个字段）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct EventsApiEvent {
    #[serde(default)]
    pub team_id: String,
    #[serde(default)]
    pub api_app_id: String,
    #[serde(default)]
    pub event: EventBody,
}

/// 解析 `events_api` 帧的 `payload`（`event_callback`）。
///
/// `null` / 非对象 / 缺 `event` ⇒ `None`（上游按"认不出的事件"丢弃，**不是**基础设施失败）。
#[must_use]
pub fn parse_events_api(payload: &Value) -> Option<EventsApiEvent> {
    if !payload.is_object() {
        return None;
    }
    let parsed: EventsApiEvent = serde_json::from_value(payload.clone()).ok()?;
    (!parsed.event.kind.is_empty()).then_some(parsed)
}

/// 一条已解析事件 → 归一化入站消息（上游 `dispatchEventsAPI` 的判决表）。
///
/// 只认 `message`（含 `file_share` / `thread_broadcast`）与 `app_mention` 两种内部类型；
/// 其余（`reaction_added`、`channel_join`…）返回 `None` ⇒ 丢弃且**不报错**。
#[must_use]
pub fn inbound_from_event(event: &EventsApiEvent, bot_user_id: &str) -> Option<InboundMessage> {
    let mention = MentionRe::new(bot_user_id);
    match event.event.kind.as_str() {
        "message" => inbound_from_message(event, &event.event, mention.as_ref()),
        "app_mention" => inbound_from_app_mention(event, &event.event, mention.as_ref()),
        _ => None,
    }
}

/// `message` 事件（上游 `inboundFromMessage`）。
///
/// 必须**不**进核心的几类：bot 自己的消息、别的 bot 的消息（环守卫）、
/// 编辑 / 删除 / 入群一类带 subtype 的系统消息（只收全新的用户消息）。
///
/// **群寻址策略**（v1，上游注释逐字）：群消息只有带显式 `<@bot>` 提及时才算"在对 bot 说话"；
/// 线程里 bot 已经参与过的无提及追问**不**在这里自动寻址 —— "回复了 bot 的消息"是会话状态，
/// 属于会话感知的共享层 / resolver，不属于 per-connection 的 adapter 记忆。
/// p2p（DM）每条消息都收，不变。
#[must_use]
pub fn inbound_from_message(
    event: &EventsApiEvent,
    body: &EventBody,
    mention: Option<&MentionRe>,
) -> Option<InboundMessage> {
    if !body.bot_id.is_empty() || body.subtype == "bot_message" {
        return None;
    }
    if body.user.is_empty() || mention.is_some_and(|m| m.bot_user_id() == body.user) {
        return None;
    }
    if !is_ingestable_subtype(&body.subtype) {
        return None;
    }
    let chat_type = slack_chat_type(&body.channel, &body.channel_type);
    let addressed = chat_type == ChatType::P2p || mentions_bot(&body.text, mention);
    Some(build_inbound(
        event,
        &BuildInboundParams {
            event_type: "message",
            sub_type: &body.subtype,
            channel_id: &body.channel,
            user_id: &body.user,
            text: &body.text,
            ts: &body.ts,
            thread_ts: &body.thread_ts,
            chat_type,
            addressed,
            files: raw_files_from(&body.files),
        },
        mention,
    ))
}

/// `app_mention` 事件（上游 `inboundFromAppMention`）。
///
/// 它按定义就是"在跟 bot 说话"、且发生在频道（群）里。同一个频道 `@` 也会以**同一个 ts** 的
/// `message` 事件到达 ⇒ engine 的 `(installation, message_id=ts)` 去重会把这一对收成一条，
/// 这里**不需要**特判。
#[must_use]
pub fn inbound_from_app_mention(
    event: &EventsApiEvent,
    body: &EventBody,
    mention: Option<&MentionRe>,
) -> Option<InboundMessage> {
    if !body.bot_id.is_empty()
        || body.user.is_empty()
        || mention.is_some_and(|m| m.bot_user_id() == body.user)
    {
        return None;
    }
    Some(build_inbound(
        event,
        &BuildInboundParams {
            event_type: "app_mention",
            sub_type: "",
            channel_id: &body.channel,
            user_id: &body.user,
            text: &body.text,
            ts: &body.ts,
            thread_ts: &body.thread_ts,
            chat_type: ChatType::Group,
            addressed: true,
            files: raw_files_from(&body.files),
        },
        mention,
    ))
}

/// [`build_inbound`] 的形参袋（上游 `buildInboundParams`）。
struct BuildInboundParams<'a> {
    event_type: &'a str,
    sub_type: &'a str,
    channel_id: &'a str,
    user_id: &'a str,
    text: &'a str,
    ts: &'a str,
    thread_ts: &'a str,
    chat_type: ChatType,
    addressed: bool,
    files: Vec<RawFile>,
}

/// 组装归一化信封（上游 `buildInbound`）。
fn build_inbound(
    event: &EventsApiEvent,
    params: &BuildInboundParams<'_>,
    mention: Option<&MentionRe>,
) -> InboundMessage {
    let raw = RawEvent {
        team_id: event.team_id.clone(),
        api_app_id: event.api_app_id.clone(),
        event_type: params.event_type.to_string(),
        sub_type: params.sub_type.to_string(),
        channel_type: params.chat_type.as_str().to_string(),
        files: params.files.clone(),
    };
    // 线程回复（`thread_ts` 存在且不等于本条 ts）才带引用上下文；顶层消息不带。
    let reply_to =
        (!params.thread_ts.is_empty() && params.thread_ts != params.ts).then(|| ReplyCtx {
            message_id: params.thread_ts.to_string(),
            root_id: params.thread_ts.to_string(),
        });
    let text = clean_text(params.text, mention);
    InboundMessage {
        event_id: params.ts.to_string(),
        message_id: params.ts.to_string(),
        source: Source {
            channel_type: TYPE_SLACK,
            chat_id: params.channel_id.to_string(),
            chat_type: params.chat_type,
            sender_id: params.user_id.to_string(),
            sender_stable_id: String::new(),
            thread_id: params.thread_ts.to_string(),
        },
        kind: MessageKind::Text,
        command_text: text.clone(),
        text,
        has_selected_context: false,
        media_refs: Vec::new(),
        reply_to,
        addressed_to_bot: params.addressed,
        force_fresh: false,
        skip_agent_run: false,
        raw: serde_json::to_value(&raw).unwrap_or(Value::Null),
    }
}

/// 去掉所有 bot 提及并 trim（上游 `cleanText`）：核心看到的是用户的**真**提示词，
/// 而不是 `"<@U123> hi"`。
#[must_use]
pub fn clean_text(text: &str, mention: Option<&MentionRe>) -> String {
    match mention {
        Some(mention) => mention.strip(text).trim().to_string(),
        None => text.trim().to_string(),
    }
}

/// 文本里是否含对本 bot 的提及（上游 `mentionsBot`）。
#[must_use]
pub fn mentions_bot(text: &str, mention: Option<&MentionRe>) -> bool {
    mention.is_some_and(|mention| mention.is_match(text))
}

/// Slack 的 channel id / `channel_type` → 归一化 [`ChatType`]（上游 `slackChatType`）。
///
/// 只有一对一私聊（`im`，或 `D…` 的 channel id）是 p2p；其余 —— 公开 / 私有频道**以及多人
/// 私聊**（`mpim`，那是多人会话）—— 都是群。群要走 engine 的"必须 @bot"过滤，
/// 所以多人 DM 里的闲聊不会被当成对 bot 的提示词。
#[must_use]
pub fn slack_chat_type(channel_id: &str, channel_type: &str) -> ChatType {
    match channel_type {
        "im" => ChatType::P2p,
        "mpim" | "channel" | "group" | "private_channel" => ChatType::Group,
        _ => {
            if channel_id.starts_with('D') {
                ChatType::P2p
            } else {
                ChatType::Group
            }
        }
    }
}

/// 该 subtype 是否是核心应当摄入的**全新用户消息**（上游 `isIngestableSubtype`）。
///
/// 空 subtype 是常态；`thread_broadcast` 与 `file_share` 是真实用户消息；
/// 其余（`message_changed` / `message_deleted` / `channel_join`…）是系统 / 编辑事件。
#[must_use]
pub fn is_ingestable_subtype(sub_type: &str) -> bool {
    matches!(sub_type, "" | "thread_broadcast" | "file_share")
}

// =====================================================================
// 提及匹配器（上游 `compileMentionRe`，手写扫描：本 crate 不引 `regex`）
// =====================================================================

/// `<@<bot>(\|[^>]*)?>` 的匹配器。
///
/// bot user id 为空（安装还没解析出来 / 还不认识）⇒ [`MentionRe::new`] 返回 `None`，
/// 提及判定于是变成 no-op —— 这是**安全**的：DM 与 `app_mention` 都不依赖它，
/// 而路由不出去的 team 在安装解析处就被丢掉了。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionRe {
    bot_user_id: String,
}

impl MentionRe {
    /// 空 id ⇒ `None`。
    #[must_use]
    pub fn new(bot_user_id: &str) -> Option<Self> {
        (!bot_user_id.is_empty()).then(|| Self {
            bot_user_id: bot_user_id.to_string(),
        })
    }

    /// 本匹配器的 bot user id。
    #[must_use]
    pub fn bot_user_id(&self) -> &str {
        &self.bot_user_id
    }

    /// 从 `from` 起（含）的**左最先**匹配区间。
    #[must_use]
    pub fn find(&self, text: &str, from: usize) -> Option<(usize, usize)> {
        let marker = format!("<@{}", self.bot_user_id);
        let bytes = text.as_bytes();
        let mut search = from;
        while search < text.len() {
            let at = text.get(search..)?.find(&marker)? + search;
            let tail = at + marker.len();
            match bytes.get(tail) {
                Some(b'>') => return Some((at, tail + 1)),
                Some(b'|') => {
                    // `(\|[^>]*)?>`：名字里的一切（含换行）直到第一个 `>`。
                    if let Some(rel) = text.get(tail..)?.find('>') {
                        return Some((at, tail + rel + 1));
                    }
                }
                _ => {}
            }
            search = at + 1;
        }
        None
    }

    /// 文本里是否有提及。
    #[must_use]
    pub fn is_match(&self, text: &str) -> bool {
        self.find(text, 0).is_some()
    }

    /// 去掉**所有**提及（`ReplaceAllString(text, "")` 的等价物）。
    #[must_use]
    pub fn strip(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut cursor = 0usize;
        while let Some((start, end)) = self.find(text, cursor) {
            out.push_str(&text[cursor..start]);
            cursor = end;
        }
        out.push_str(&text[cursor..]);
        out
    }
}

#[cfg(test)]
mod tests;
