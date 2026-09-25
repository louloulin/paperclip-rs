//! `DingTalk` 入站：**Stream 回调 wire 类型 + 归一化**（上游 `inbound.go` 827 行）。
//!
//! - **写者**：M7-7（`docs/60-M7-PLAN.md` §3.3；本片的写集勘误见 `docs/32` §19）。
//! - **入站恒为 push**（`docs/60` §2.6 第 4 条）：帧循环在
//!   [`crate::dingtalk::stream`] 里**阻塞跑**，把回调交给 per-conversation 串行队列
//!   （[`crate::dingtalk::dispatch`]），队列里的作业再调本文件的归一化，最后交给构造时注入的
//!   `SharedInboundHandler`。engine **从不**轮询 `Channel`。
//!
//! # 三个文件，各自可测（门 ⑩ 的 800 行硬限逼出来的切分）
//!
//! | 文件 | 内容 | 上游 |
//! | --- | --- | --- |
//! | **本文件** | 回调 wire 类型 + 归一化 + 机器人身份（mention）剥离 | `inbound.go` |
//! | [`card`] | 引用卡片的投影（RICHTEXT 子节点）+ 网页 URL 扫描 | `inbound_card.go` |
//! | [`quoted`] | 引用消息的渲染 + 引用上下文（`reply_to` / 正文拼接 / 媒体位次） | `inbound.go` 的引用段 |
//!
//! # 这个回调为什么需要"归一化"这一层
//!
//! `DingTalk` 的机器人回调（`botCallbackData`）**不带 robot code**：路由键要由接收它的那条
//! Stream 连接盖进信封（这就是 [`DingtalkRawEvent::app_id`]，也是本文件的归一化必须**参数化**
//! `app_id` 的原因）。其余平台字段（`conversationTitle` / 媒体下载码 / 引用快照）一律留在
//! [`mc_core::channel::message::InboundMessage::raw`] 里，只有本 adapter 与
//! [`crate::dingtalk::resolvers`] 读它。
//!
//! # 四条"别简化"的语义（上游注释逐字，每条都有用例）
//!
//! 1. **没有发送者 ⇒ 不进核心**（系统消息 / bot 自己发的）：归一化返回 `None`，
//!    而不是一条 `addressed_to_bot=false` 的消息；
//! 2. **读不出来的媒体也要进核心**（`errorCode 20001` 会把 `text` / `content` 整个剥掉）：
//!    换成一个显式的不可用占位符，**不**静默丢弃 —— 发件人是真人，他要看到"我读不到这张图"；
//! 3. **群聊必须被 @**：`isInAtList` 是平台给的归一化结论（私聊恒为 `true`）；
//! 4. **`[Image]` 占位符的位次是承重的**：正文里的第 N 次出现对应
//!    [`DingtalkMediaResource::inline_index`] = N，用户在正文里手打的 `[Image]` 也计数。

use mc_core::channel::message::{ChatType, InboundMessage, MessageKind, Source};
use mc_core::channel::ChannelKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::engine::commands::{parse_control_command, ControlCommandKind};

pub mod card;
pub mod mention;
pub mod quoted;

pub use mention::{
    exact_dingtalk_bot_mention_spans, normalize_dingtalk_bot_mention,
    normalize_dingtalk_rich_text_bot_mention, remove_dingtalk_bot_mention, MentionSpan,
};

pub use quoted::{
    apply_dingtalk_reply_context, dingtalk_current_visible_text, render_dingtalk_quoted_message,
};

/// 本 adapter 的平台判别式（上游 `TypeDingTalk`）。
///
/// 定义在 adapter 里是**故意**的（上游注释逐字）：注册一个新平台不该要求改核心。
pub const TYPE_DINGTALK: ChannelKind = ChannelKind::DingTalk;

/// `origin_type`：`DingTalk` `/issue` 建出来的 issue 的来源标签（**逐字** `dingtalk_chat`，
/// 与 `issue.origin_type` 的 `CHECK` 约束、以及既有的 `lark_chat` / `slack_chat` 同一形态）。
pub const ORIGIN_DINGTALK_CHAT: &str = "dingtalk_chat";

/// 丢弃审计用的粗粒度事件标签（上游在 auditor 里写死 `"message"`）。
pub const EVENT_TYPE_MESSAGE: &str = "message";

/// `conversationType` 的直聊判别式（`1`）。
pub const CONV_TYPE_P2P: &str = "1";
/// `conversationType` 的群聊判别式（`2`）。**除 `1` 之外的一切都算群**（见
/// [`dingtalk_chat_type`]）。
pub const CONV_TYPE_GROUP: &str = "2";

/// adapter 自己生成的图片占位标记（上游 `dingtalkImagePlaceholder`）。
///
/// 与 `mc_core::channel::message` 的既有约定一致：它出现在**正文里**，`inline_index` 数它的
/// 第几次出现。**用户手打的同一串也算**（上游逐字：`including identical user-authored text`）。
pub const IMAGE_PLACEHOLDER: &str = "[Image]";

/// 读不出来的图片（拿不到下载码 / `content` 被剥）时正文里的占位。
pub const IMAGE_UNAVAILABLE: &str = "[Image unavailable]";
/// 读不出来的富文本（同上）时正文里的占位。
pub const RICH_TEXT_UNAVAILABLE: &str = "[rich-text content unavailable]";

// =====================================================================
// 平台原始载荷（留在 `InboundMessage::raw` 里，只有 adapter 与 resolver 读）
// =====================================================================

/// 跨平台信封装不下的 `DingTalk` 专有字段（上游 `dingtalkRawEvent`）。
///
/// `app_id` 由**接收这条回调的那条连接**盖章（回调自己不带 robot code）⇒ 安装路由键；
/// `media` 是正文里每个内联媒体的下载码与位次（真正的字节取回归 M7-8 的媒体面）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DingtalkRawEvent {
    /// 路由键：本连接所属安装的 `AppKey`（`config->>'app_id'`）。
    #[serde(default)]
    pub app_id: String,
    /// 群标题（仅群聊有；空 = 缺席）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub conversation_title: String,
    /// **引用上下文拼进来之前**的当前可见正文（含 adapter 生成的媒体占位符、按原序）。
    /// 出站命令回执引用它，而不是 Router 改写过的 `text`。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub current_text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<DingtalkMediaResource>,
}

/// 正文里一个内联媒体的平台引用（上游 `dingtalkMediaResource`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DingtalkMediaResource {
    /// 下载码（`downloadCode` 优先，退到 `pictureDownloadCode`；见 [`ref_alt`]）。
    #[serde(rename = "ref")]
    pub reference: String,
    /// 备选下载码（平台同时给了两个时才有）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub alt: String,
    /// 占位符在可见正文里的第几次出现（0 起，含用户手打的同一串）。
    #[serde(default, rename = "inline_index", skip_serializing_if = "is_zero_i32")]
    pub inline_index: i32,
}

impl DingtalkMediaResource {
    /// 就位构造。
    #[must_use]
    pub fn at(reference: impl Into<String>, alt: impl Into<String>, inline_index: i32) -> Self {
        Self {
            reference: reference.into(),
            alt: alt.into(),
            inline_index,
        }
    }
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde 的 `skip_serializing_if` 只收 `&T`
fn is_zero_i32(value: &i32) -> bool {
    *value == 0
}

/// 解出 `raw` 里的平台事件字段（解不开 ⇒ **基础设施失败**：`raw` 是本 adapter 自己写的）。
///
/// # Errors
///
/// `raw` 为 `null`（没有盖过 `app_id` 的路径）或形态不符 ⇒ [`crate::engine::EngineError::Infra`]。
pub fn decode_dingtalk_raw(
    message: &InboundMessage,
) -> crate::engine::EngineResult<DingtalkRawEvent> {
    if message.raw.is_null() {
        return Err(crate::engine::EngineError::infra(
            "dingtalk: inbound message raw is empty",
        ));
    }
    serde_json::from_value::<DingtalkRawEvent>(message.raw.clone()).map_err(|error| {
        crate::engine::EngineError::infra(format!("dingtalk: decode inbound raw: {error}"))
    })
}

// =====================================================================
// Stream 回调的 wire 类型（上游 `botCallbackData` 一族）
// =====================================================================

/// 机器人消息回调 —— `CALLBACK` 帧 `data` 字段里的那个 JSON（上游 `botCallbackData`）。
///
/// 只声明本翻译真正读的字段；DingTalk 还会送更多（一律忽略）。缺失字段一律取零值
/// （上游的 Go 结构体没有指针，除 `repliedMsg` / `content` 两处外"缺席 = 零值"）。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct BotCallbackData {
    #[serde(rename = "conversationId", default)]
    pub conversation_id: String,
    #[serde(rename = "conversationTitle", default)]
    pub conversation_title: String,
    /// `1` = 直聊，`2` = 群（见 [`dingtalk_chat_type`]）。
    #[serde(rename = "conversationType", default)]
    pub conversation_type: String,
    #[serde(rename = "atUsers", default)]
    pub at_users: Vec<BotCallbackAtUser>,
    #[serde(rename = "chatbotUserId", default)]
    pub chatbot_user_id: String,
    /// 发送者的 staff id：**空 ⇒ 这条不进核心**（系统消息 / bot 自己发的）。
    #[serde(rename = "senderStaffId", default)]
    pub sender_staff_id: String,
    #[serde(rename = "msgId", default)]
    pub msg_id: String,
    /// 被引用消息的 id（**快照里没有 id 时的退路**，见 [`quoted::apply_dingtalk_reply_context`]）。
    #[serde(rename = "originalMsgId", default)]
    pub original_msg_id: String,
    #[serde(rename = "msgtype", default)]
    pub msgtype: String,
    /// 平台给的"这条在 @ 列表里"结论（群聊里必须为 `true` 才进核心）。
    #[serde(rename = "isInAtList", default)]
    pub is_in_at_list: bool,
    #[serde(default)]
    pub text: BotCallbackText,
    /// `msgtype` 判别式下的载荷（picture / richText）。**按 msgtype 惰性解码**；`errorCode
    /// 20001` 的超额回调会把 `text` / `content` 整个剥掉 ⇒ 缺席就是 `null`。
    #[serde(default)]
    pub content: Value,
}

impl BotCallbackData {
    /// `content` 是否**存在**（不是"零值"）：超额回调会整个剥掉它。
    #[must_use]
    pub fn has_content(&self) -> bool {
        !self.content.is_null()
    }

    /// 回调里能拿到的群标题（去空白）。
    #[must_use]
    pub fn trimmed_title(&self) -> String {
        self.conversation_title.trim().to_string()
    }
}

/// 一条 @ 提及（上游 `botCallbackAtUser`）：本 adapter 只用于诊断，不参与判定
/// （判定读 `isInAtList`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotCallbackAtUser {
    #[serde(rename = "dingtalkId", default)]
    pub dingtalk_id: String,
    #[serde(rename = "staffId", default)]
    pub staff_id: String,
}

/// 回调的 `text` 段（上游 `botCallbackText`）。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct BotCallbackText {
    #[serde(default)]
    pub content: String,
    #[serde(rename = "isReplyMsg", default)]
    pub is_reply_msg: bool,
    /// 用户**显式引用**了另一条消息时，DingTalk 嵌进来的快照。
    ///
    /// `DingTalk` 的公开收信 schema **没有**文档化这些字段 ⇒ 每个字段可选、解码必须容忍残缺
    /// 快照（见 [`BotCallbackRepliedMessage`] 的宽容解码）。
    #[serde(rename = "repliedMsg", default)]
    pub replied_msg: Option<BotCallbackRepliedMessage>,
}

/// 引用元信息的两个可能位置（`text` 下 / `content` 下）。
///
/// **两处都不被公开 schema 保证**；这是有界的最佳努力投影：优先 `text`，缺失字段从 `content` 补，
/// 绝不从别的回调字段**猜**选中的正文。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct BotCallbackReplyMetadata {
    #[serde(rename = "isReplyMsg", default)]
    pub is_reply_msg: bool,
    #[serde(rename = "repliedMsg", default)]
    pub replied_msg: Option<BotCallbackRepliedMessage>,
}

/// 用户显式引用时 `DingTalk` 嵌进来的那条消息快照（上游 `botCallbackRepliedMessage`）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BotCallbackRepliedMessage {
    pub msg_type: String,
    pub msg_id: String,
    pub sender_id: String,
    pub sender_nick: String,
    pub content: BotCallbackRepliedContent,
}

impl BotCallbackRepliedMessage {
    /// **宽容**解码（上游手写 `UnmarshalJSON` 的逐条对应）。
    ///
    /// 三条纪律：整个信封解不开 ⇒ **零值快照**（不是错误 —— 可选快照的存在不该让发送者
    /// 那条本来有效的消息失败）；每个字段**各自**降级；`content` 也独立解码。
    #[must_use]
    pub fn from_value(value: &Value) -> Self {
        let Some(object) = value.as_object() else {
            return Self::default();
        };
        Self {
            msg_type: string_field(object.get("msgType")),
            msg_id: string_field(object.get("msgId")),
            sender_id: string_field(object.get("senderId")),
            sender_nick: string_field(object.get("senderNick")),
            content: object.get("content").map_or_else(
                BotCallbackRepliedContent::default,
                BotCallbackRepliedContent::from_value,
            ),
        }
    }
}

impl<'de> Deserialize<'de> for BotCallbackRepliedMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        Ok(Self::from_value(&value))
    }
}

/// 引用快照的 `content`（上游 `botCallbackRepliedContent`）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BotCallbackRepliedContent {
    pub text: String,
    pub rich_text: Vec<RichTextItem>,
    /// 互动卡片的原始载荷（投影见 [`card::render_dingtalk_quoted_card`]）。
    pub card_content: Value,
    pub download_code: String,
    pub picture_download_code: String,
    pub file_name: String,
    pub recognition: String,
}

impl BotCallbackRepliedContent {
    /// **宽容**解码：非对象 ⇒ 零值；每个字段各自降级（上游手写 `UnmarshalJSON` 的对应）。
    ///
    /// 引用快照的 `text` / `richText` 用的是**与当前消息不同**的包装与别名 ⇒ 那套解码
    /// **只在选中的上下文里**生效（上游逐字）。有序数组（不是兄弟节点里的摘要）才是布局权威。
    #[must_use]
    pub fn from_value(value: &Value) -> Self {
        let Some(object) = value.as_object() else {
            return Self::default();
        };
        Self {
            text: string_field(object.get("text")),
            rich_text: card::rich_text_items(object.get("richText"), true),
            card_content: object.get("cardContent").cloned().unwrap_or(Value::Null),
            download_code: string_field(object.get("downloadCode")),
            picture_download_code: string_field(object.get("pictureDownloadCode")),
            file_name: string_field(object.get("fileName")),
            recognition: string_field(object.get("recognition")),
        }
    }
}

impl<'de> Deserialize<'de> for BotCallbackRepliedContent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        Ok(Self::from_value(&value))
    }
}

/// 读一个**字符串**字段：不是字符串 / 缺席 ⇒ 空串（上游"忽略单字段错误、字段留零值"的对应）。
fn string_field(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_default()
}

// =====================================================================
// 有序富文本（上游 `richTextItem` / `richTextItems`）
// =====================================================================

/// 富文本里的一个**有序**项（上游 `richTextItem`）：文本 run 与图片项按发送顺序交错。
///
/// 公开 schema 只文档化了 `text` 与 `picture` 两类；别的一律得到"不可用"标记
/// （[`RICH_TEXT_UNAVAILABLE`]），**不**递归解释未知的嵌套值。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RichTextItem {
    pub text: String,
    /// `""` / `"text"` / `"picture"`（其余值 ⇒ 整个项变成不可用标记）。
    pub kind: String,
    pub download_code: String,
    pub picture_download_code: String,
}

impl RichTextItem {
    /// 解码一项（上游 `unmarshalJSON(data, quoted)` 的逐条对应）。
    ///
    /// `quoted = true` 时走**引用快照**的额外包装/别名（`msgType` 别名、`content` 包装、
    /// 标量解包）；当前消息从不走这些（上游逐字：`KKeep that decoding scoped to selected
    /// context`）。
    #[must_use]
    pub fn from_value(value: &Value, quoted: bool) -> Self {
        let Some(object) = value.as_object() else {
            // 单个坏节点**就地降级**，让相邻的有效文本与图片活下来。
            return Self {
                text: RICH_TEXT_UNAVAILABLE.to_string(),
                ..Self::default()
            };
        };
        let text_field = object.get("text");
        let content_field = object.get("content");
        let mut kind = string_field(object.get("type"));
        if quoted && kind.is_empty() {
            // `msgType` 只有能解成字符串时才当别名用（上游：解不开 ⇒ 整项不可用）。
            match object.get("msgType") {
                Some(other) if !other.is_null() => match other.as_str() {
                    Some(alias) => kind = alias.to_string(),
                    None => {
                        return Self {
                            text: RICH_TEXT_UNAVAILABLE.to_string(),
                            ..Self::default()
                        }
                    }
                },
                _ => {}
            }
        }
        if !kind.is_empty() && kind != "text" && kind != "picture" {
            return Self {
                text: RICH_TEXT_UNAVAILABLE.to_string(),
                ..Self::default()
            };
        }
        let mut item = Self {
            text: String::new(),
            kind,
            download_code: string_field(object.get("downloadCode")),
            picture_download_code: string_field(object.get("pictureDownloadCode")),
        };
        let text_source: Option<Value> = if quoted && (item.kind.is_empty() || item.kind == "text")
        {
            let mut raw = match text_field {
                Some(value) if !value.is_null() => value.clone(),
                _ => Value::Null,
            };
            if raw.is_null() && item.kind == "text" {
                raw = content_field.cloned().unwrap_or(Value::Null);
            }
            Some(quoted_rich_text_scalar(&raw))
        } else {
            text_field.cloned()
        };
        match text_source {
            Some(value) if !value.is_null() => match value.as_str() {
                Some(text) => item.text = text.to_string(),
                None => item.text = RICH_TEXT_UNAVAILABLE.to_string(),
            },
            _ => {
                if item.kind != "picture"
                    && item.download_code.is_empty()
                    && item.picture_download_code.is_empty()
                {
                    item.text = RICH_TEXT_UNAVAILABLE.to_string();
                }
            }
        }
        item
    }

    /// 这项是否带一张图片（上游 `item.Type == "picture" || DownloadCode != "" ||
    /// PictureDownloadCode != ""`）。
    #[must_use]
    pub fn has_picture(&self) -> bool {
        self.kind == "picture"
            || !self.download_code.is_empty()
            || !self.picture_download_code.is_empty()
    }

    /// 这项的两个下载码排成 `(主, 备)`（见 [`ref_alt`]）。
    #[must_use]
    pub fn codes(&self) -> (String, String) {
        ref_alt(&self.download_code, &self.picture_download_code)
    }
}

/// **只**读 `text` / `content` 两个键的标量包装（上游 `dingTalkQuotedRichTextScalar`）。
///
/// 引用文本可能有一层结构包装；只读它的标量值：**不**遍历任意对象 / 数组 / JSON 里的散文。
/// 尤其：这些只属于引用的别名**不能**变成当前消息的命令。
#[must_use]
pub fn quoted_rich_text_scalar(value: &Value) -> Value {
    let Some(object) = value.as_object() else {
        return value.clone();
    };
    for key in ["text", "content"] {
        if let Some(found) = object.get(key) {
            return found.clone();
        }
    }
    value.clone()
}

/// 把两个下载码排成 `(主, 备)`：主码缺失时把备码提上来（上游 `refAlt`）。
#[must_use]
pub fn ref_alt(download_code: &str, picture_download_code: &str) -> (String, String) {
    if !download_code.is_empty() {
        return (download_code.to_string(), picture_download_code.to_string());
    }
    (picture_download_code.to_string(), String::new())
}

// =====================================================================
// 归一化（上游 `inboundFromCallback` 一族）
// =====================================================================

/// 归一化一条机器人回调（上游 `inboundFromCallback`，**无 bot 名**的入口）。
///
/// `None` 只留给"**根本不该进核心**"的事件：没有发送者 staff id 的（系统 / bot 自己发的）。
/// 文本 / 图片 / 富文本都成为可入库消息；形如 `errorCode 20001` 的媒体载荷**照样**进核心，
/// 只是带一个显式的不可用占位符（不静默丢弃）。
#[must_use]
pub fn inbound_from_callback(
    data: Option<&BotCallbackData>,
    app_id: &str,
) -> Option<InboundMessage> {
    inbound_from_callback_with_bot_name(data, app_id, "")
}

/// 归一化一条回调，并且**只**使用经 `DingTalk` 群 Bot 列表 API 校验过的 bot 名（上游
/// `inboundFromCallbackWithBotName`）。
///
/// 空名字是**故意失败关闭**的：adapter 会把每一个可见提及原样保留，而不是靠空白猜它的跨度。
#[must_use]
pub fn inbound_from_callback_with_bot_name(
    data: Option<&BotCallbackData>,
    app_id: &str,
    bot_name: &str,
) -> Option<InboundMessage> {
    let data = data?;
    if data.sender_staff_id.is_empty() {
        return None;
    }
    let chat_type = dingtalk_chat_type(&data.conversation_type);
    let mut raw_event = DingtalkRawEvent {
        app_id: app_id.to_string(),
        conversation_title: data.trimmed_title(),
        ..DingtalkRawEvent::default()
    };
    let mut message = InboundMessage {
        event_id: data.msg_id.clone(),
        message_id: data.msg_id.clone(),
        source: Source {
            channel_type: TYPE_DINGTALK,
            chat_id: data.conversation_id.clone(),
            chat_type,
            sender_id: data.sender_staff_id.clone(),
            sender_stable_id: String::new(),
            thread_id: String::new(),
        },
        kind: MessageKind::Unknown,
        text: String::new(),
        command_text: String::new(),
        has_selected_context: false,
        media_refs: Vec::new(),
        reply_to: None,
        // 直聊恒为"在跟 bot 说话"；群聊只有被 @ 才算（`isInAtList` 是平台给的结论）。
        addressed_to_bot: chat_type == ChatType::P2p || data.is_in_at_list,
        force_fresh: false,
        skip_agent_run: false,
        raw: Value::Null,
    };

    match data.msgtype.as_str() {
        "text" => {
            message.kind = MessageKind::Text;
            message.text = normalize_dingtalk_bot_mention(data, &data.text.content, bot_name)
                .trim()
                .to_string();
            message.command_text.clone_from(&message.text);
            raw_event.current_text = dingtalk_current_visible_text(&message);
            apply_dingtalk_reply_context(data, &mut message, &mut raw_event);
            Some(with_dingtalk_raw(message, raw_event))
        }
        "picture" => {
            let Some(picture) = card::picture_content(&data.content) else {
                return Some(media_unreadable_message(data, message, raw_event));
            };
            let (primary, alt) = ref_alt(&picture.download_code, &picture.picture_download_code);
            if primary.is_empty() {
                return Some(media_unreadable_message(data, message, raw_event));
            }
            message.kind = MessageKind::Image;
            message.text = IMAGE_PLACEHOLDER.to_string();
            message.command_text.clone_from(&message.text);
            raw_event.media = vec![DingtalkMediaResource::at(primary, alt, 0)];
            raw_event.current_text = dingtalk_current_visible_text(&message);
            apply_dingtalk_reply_context(data, &mut message, &mut raw_event);
            Some(with_dingtalk_raw(message, raw_event))
        }
        "richText" => Some(inbound_rich_text(data, message, raw_event, bot_name)),
        "audio" => {
            message.kind = MessageKind::Audio;
            Some(non_text_message(
                data,
                message,
                raw_event,
                "[Audio message]",
            ))
        }
        "video" => {
            message.kind = MessageKind::Video;
            Some(non_text_message(
                data,
                message,
                raw_event,
                "[Video message]",
            ))
        }
        "file" => {
            message.kind = MessageKind::File;
            Some(non_text_message(data, message, raw_event, "[File]"))
        }
        _ => {
            message.kind = MessageKind::Unknown;
            Some(non_text_message(
                data,
                message,
                raw_event,
                "[Unsupported DingTalk message]",
            ))
        }
    }
}

/// 音频 / 视频 / 文件 / 未知类型：有占位正文、无媒体引用（上游那四个 case 的共同尾巴）。
fn non_text_message(
    data: &BotCallbackData,
    mut message: InboundMessage,
    mut raw_event: DingtalkRawEvent,
    text: &str,
) -> InboundMessage {
    message.text = text.to_string();
    message.command_text = text.to_string();
    raw_event.current_text = dingtalk_current_visible_text(&message);
    apply_dingtalk_reply_context(data, &mut message, &mut raw_event);
    with_dingtalk_raw(message, raw_event)
}

/// `msgtype=richText` 的归一化（上游那个最长 case 的逐条对应）。
///
/// 三件事值得单独点出：**单个 item 可能同时带文本与图片码** ⇒ 两类载荷各自独立处理
/// （不是 `switch`），谁都不会被静默丢掉；`command_text` 只装文本 run（不含图片占位），
/// 因为命令分类器读它；布局冻结在 Router 消费控制指令**之前**。
fn inbound_rich_text(
    data: &BotCallbackData,
    mut message: InboundMessage,
    mut raw_event: DingtalkRawEvent,
    bot_name: &str,
) -> InboundMessage {
    let Some(items) = card::rich_text_content(&data.content) else {
        return media_unreadable_message(data, message, raw_event);
    };
    if items.is_empty() {
        return media_unreadable_message(data, message, raw_event);
    }
    let mut items = items;
    normalize_dingtalk_rich_text_bot_mention(data, &mut items, bot_name);

    let mut text = String::new();
    let mut command_text = String::new();
    let mut inline_placeholder_count = 0usize;
    for item in &items {
        if !item.text.is_empty() {
            text.push_str(&item.text);
            command_text.push_str(&item.text);
            inline_placeholder_count += item.text.matches(IMAGE_PLACEHOLDER).count();
        }
        if item.has_picture() {
            let (primary, alt) = item.codes();
            if primary.is_empty() {
                continue; // 带图片语义但没有可用下载码的项
            }
            append_image_placeholder(&mut text);
            raw_event.media.push(DingtalkMediaResource::at(
                primary,
                alt,
                i32_index(inline_placeholder_count),
            ));
            inline_placeholder_count += 1;
        }
    }
    message.kind = if raw_event.media.is_empty() {
        MessageKind::Text
    } else {
        MessageKind::Image
    };
    message.text = text.trim().to_string();
    message.command_text = command_text.trim().to_string();
    // 在 Router 消费 `/clear` / `/new` 之前冻结发送者真正发出去的东西。
    raw_event.current_text.clone_from(&message.text);
    normalize_dingtalk_rich_text_control_layout(
        &mut message,
        &mut items,
        !raw_event.media.is_empty(),
    );
    apply_dingtalk_reply_context(data, &mut message, &mut raw_event);
    with_dingtalk_raw(message, raw_event)
}

/// 把 adapter 读不出来的媒体变成**显式占位**（上游 `mediaUnreadableMsg`）。
///
/// 没有可下载的引用 ⇒ 共享媒体解析器不参与，普通渠道轮次携带这个降级信号。
fn media_unreadable_message(
    data: &BotCallbackData,
    mut message: InboundMessage,
    mut raw_event: DingtalkRawEvent,
) -> InboundMessage {
    message.kind = MessageKind::Image;
    message.text = IMAGE_UNAVAILABLE.to_string();
    if data.msgtype == "richText" {
        message.kind = MessageKind::Text;
        message.text = RICH_TEXT_UNAVAILABLE.to_string();
    }
    message.command_text.clone_from(&message.text);
    raw_event.current_text.clone_from(&message.text);
    apply_dingtalk_reply_context(data, &mut message, &mut raw_event);
    with_dingtalk_raw(message, raw_event)
}

/// 富文本正文里的控制指令：剥掉**可见**正文里的那一条，把命令留给共享 Router（上游
/// `normalizeDingTalkRichTextControlLayout`）。
///
/// 为什么要这一层：正文已经被规范化（含交错的图片占位），Router 无法从 `command_text`
/// 重建它。adapter **不**在这里做 `/new` 的路由轮换、也不重分类剩余正文。
fn normalize_dingtalk_rich_text_control_layout(
    message: &mut InboundMessage,
    items: &mut [RichTextItem],
    has_media: bool,
) {
    let Some(control) = parse_control_command(&message.command_text) else {
        return;
    };
    if control.body.is_empty() && !has_media {
        return;
    }
    let Some(first_text) = items.iter().position(|item| !item.text.trim().is_empty()) else {
        return;
    };
    let Some(first_control) = parse_control_command(&items[first_text].text) else {
        return;
    };
    if first_control.kind != control.kind {
        return;
    }
    if control.kind == ControlCommandKind::FreshSession {
        message.force_fresh = true;
    }
    items[first_text].text = first_control.body;
    let mut visible = String::new();
    for item in items.iter() {
        visible.push_str(&item.text);
        if item.has_picture() {
            let (primary, _) = item.codes();
            if !primary.is_empty() {
                append_image_placeholder(&mut visible);
            }
        }
    }
    message.text = visible.trim().to_string();
}

/// 把归一化结果与平台原始载荷合起来（上游 `withDingTalkRaw`）。
fn with_dingtalk_raw(message: InboundMessage, raw_event: DingtalkRawEvent) -> InboundMessage {
    let mut message = message;
    message.raw = serde_json::to_value(raw_event).unwrap_or(Value::Null);
    message
}

/// `conversationType` → 归一化的 [`ChatType`]（上游 `dingtalkChatType`）。
///
/// `"1"` 是直聊；**其余一切**（群 `"2"`，以及未来的新取值）都是群 —— 群会走 engine 的
/// "必须 @ bot" 过滤，这是失败关闭的方向。
#[must_use]
pub fn dingtalk_chat_type(conversation_type: &str) -> ChatType {
    if conversation_type == CONV_TYPE_P2P {
        ChatType::P2p
    } else {
        ChatType::Group
    }
}

/// 在一个 builder 末尾追加图片占位（上游 `appendImagePlaceholder`）。
pub fn append_image_placeholder(body: &mut String) {
    if !body.is_empty() {
        body.push('\n');
    }
    body.push_str(IMAGE_PLACEHOLDER);
    body.push('\n');
}

/// 占位符计数（`usize`）→ `inline_index`（`i32`）。
fn i32_index(count: usize) -> i32 {
    i32::try_from(count).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests;
