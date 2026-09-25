//! lark 长连接的**事件载荷解码器**：数据帧的 JSON 信封 → 归一化事件
//! （上游 `internal/integrations/lark/ws_frame_decoder.go` 350 行）。
//!
//! - **写者**：M7-11（`docs/60-M7-PLAN.md` §3.3；本片的写集勘误见 `docs/32` §28）。
//! - 拆出来是**门 ⑩**（800 行硬限）的要求，边界就是上游那个文件自己的边界：二进制帧信封与
//!   分片重组在 [`super::ws_frame`]，连接在 [`super::ws_connector`]。
//!
//! # 本文件在解码链上的**位置**（三处交接，别在这里越界）
//!
//! | 层 | 谁 | 干什么 |
//! | --- | --- | --- |
//! | 字节 → [`super::ws_frame::Frame`] | [`super::ws_frame`]（本片） | protobuf 信封 |
//! | `Frame.payload` → [`LarkInboundEvent`] | **本文件**（本片） | JSON 信封 + `im.message.receive_v1` 的字段抽取 |
//! | [`LarkInboundEvent`] → `mc_core::channel::message::InboundMessage` | [`super::super::lark`] 的 adapter（**M7-12** `feishu_channel.rs`） | 摊平正文（`content_flatten.rs`）/ 提及改写（`resolvers.rs`）/ `addressed_to_bot` |
//!
//! 上游把最后一步放在**同一个** Go 文件里（它调 `flattenContent` / `resolveMentions` /
//! `containsMention`）。本仓那五个 helper 属于 **M7-12 的写集**
//! （`docs/60` §3.3：`content_flatten.rs` / `resolvers.rs`，逐文件分配表里 `content_flatten.go`
//! 与 `mention.go` 都归 M7-12）⇒ 本片**不重实现**它们，只把**解码所需的一切**原样交出去：
//! [`LarkInboundEvent::content`]（Lark 双重编码的 JSON 字符串）、[`LarkInboundEvent::mentions`]
//! （WS 形状的提及数组）、[`LarkInboundEvent::raw`]（信封逐字）。
//! ⇒ 交接项记在 `docs/32` §28 的 H 项。
//!
//! # 三种结果（上游逐字的三分支）
//!
//! | 上游返回 | 本仓 | 连接器怎么处置 |
//! | --- | --- | --- |
//! | `(msg, true, nil)` | [`DecodeOutcome::Message`] | emit + ACK 200 |
//! | `(zero, false, nil)` 心跳 / 未订阅的事件类型 | [`DecodeOutcome::Ignored`] | 静默丢弃 + **仍然 ACK 200**（让服务端别再重投） |
//! | `(zero, false, err)` JSON 坏 / 信封坏 | `Err(`[`DecodeError`]`)` | 记日志 + 丢**这一帧** + **仍然 ACK 200**（一个坏载荷不该放大成重连风暴） |
//!
//! # 凭据面（`docs/60` §2.3）
//!
//! 本文件没有任何凭据字段与任何 `tracing::*`。⚠️ [`LarkInboundEvent`] 含**用户正文**
//! （`content` / `raw`）⇒ 派生 `Debug` 只给用例；接线方的日志只插值 `event_type` 与
//! `message_id`（[`super::ws_connector`] 逐条遵守）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::types::{ChatId, ChatType, OpenId};

// =====================================================================
// 词表
// =====================================================================

/// 本 adapter 唯一消费的事件类型（上游逐字：`im.message.receive_v1`）。
pub const EVENT_TYPE_MESSAGE_RECEIVE: &str = "im.message.receive_v1";

/// 老式 webhook v1 信封的 `type` 取值（长连接上**不用**，但上游防御性接受）。
pub const LEGACY_CALLBACK_TYPE: &str = "event_callback";

/// 长连接数据帧的 schema 版本（诊断用；Lark 长连接恒为 `2.0`）。
pub const LONG_CONN_SCHEMA: &str = "2.0";

// =====================================================================
// 信封（上游 `larkEventEnvelope` / `larkEventHeader`）
// =====================================================================

/// Lark 包在每个推送外面的信封（上游 `larkEventEnvelope`）：`{schema, type, header, event}`。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LarkEventEnvelope {
    /// 恒为 `"2.0"`（长连接）。
    #[serde(default)]
    pub schema: String,
    /// 老式回调判别式；长连接上为空串。
    #[serde(rename = "type", default)]
    pub envelope_type: String,
    /// 事件头。
    #[serde(default)]
    pub header: LarkEventHeader,
    /// 事件体（**原样**留在 [`LarkInboundEvent::raw`] 里给解析器读）。
    #[serde(default)]
    pub event: Option<Value>,
}

/// 信封头（上游 `larkEventHeader`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LarkEventHeader {
    /// 平台投递 id（幂等键的一半，与 `message_id` 一起构成 dedup 键）。
    #[serde(default)]
    pub event_id: String,
    /// 事件类型（我们只认 [`EVENT_TYPE_MESSAGE_RECEIVE`]）。
    #[serde(default)]
    pub event_type: String,
    /// 平台给的创建时间（字串，纪元毫秒）。
    #[serde(default)]
    pub create_time: String,
    /// 收到这条事件的**机器人自身**的 `app_id` —— 多机器人部署下按它选安装行。
    #[serde(default)]
    pub app_id: String,
    /// 租户键。
    #[serde(default)]
    pub tenant_key: String,
}

// =====================================================================
// `im.message.receive_v1` 的载荷（上游 `larkMessageReceiveEvent`）
// =====================================================================

/// `im.message.receive_v1` 的事件体（上游 `larkMessageReceiveEvent`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LarkMessageReceiveEvent {
    /// 发送者。
    #[serde(default)]
    pub sender: LarkEventSender,
    /// 消息本体。
    #[serde(default)]
    pub message: LarkEventMessage,
}

/// 事件里的发送者（上游匿名结构体那一层）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LarkEventSender {
    /// 三种 id 形态的嵌套对象 —— `open_id` 是**按应用**的，`union_id` 才是跨应用稳定的。
    #[serde(default)]
    pub sender_id: LarkSenderId,
    /// `user` / `app` / `anonymous` / …
    #[serde(default)]
    pub sender_type: String,
    /// 租户键。
    #[serde(default)]
    pub tenant_key: String,
}

/// 一组 Lark 标识（上游 `sender_id` / `mentions[].id` 的同一个嵌套形状）。
///
/// ⚠️ **与 REST 形状不同**：REST 侧（[`super::types::LarkMessageMention`]）的 `id` 是裸
/// `open_id` 字串，只有 WS 事件才有这个三字段对象（M7-10 在 `types.rs` 里逐字记过这条）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct LarkSenderId {
    /// 按应用的 `open_id`。
    #[serde(default)]
    pub open_id: String,
    /// 租户内跨应用稳定的 `union_id`。
    #[serde(default)]
    pub union_id: String,
    /// 老式 `user_id`（多数应用拿不到）。
    #[serde(default)]
    pub user_id: String,
}

/// 事件里的消息体（上游 `larkMessageReceiveEvent.message`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LarkEventMessage {
    /// 平台消息 id。
    #[serde(default)]
    pub message_id: String,
    /// 会话 id。
    #[serde(default)]
    pub chat_id: String,
    /// `p2p` / `group`。
    #[serde(default)]
    pub chat_type: String,
    /// `text` / `post` / `image` / `merge_forward` / …
    #[serde(default)]
    pub message_type: String,
    /// Lark 双重编码的正文（按 `message_type` 而定的 JSON **字符串**）——原样透传给 M7-12。
    #[serde(default)]
    pub content: String,
    /// 提及数组（WS 形状，见 [`LarkEventMention`]）。
    #[serde(default)]
    pub mentions: Vec<LarkEventMention>,
    /// 纪元毫秒的字串。
    #[serde(default)]
    pub create_time: String,
    /// 直接引用的那条消息 id（只在回复场景有）。
    #[serde(default)]
    pub parent_id: String,
    /// 回复树的根 id。
    #[serde(default)]
    pub root_id: String,
    /// 话题（`thread_id`）；话题外的消息为空串。
    #[serde(default)]
    pub thread_id: String,
}

/// 事件里的提及项（上游 `larkMention`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct LarkEventMention {
    /// 正文里的占位键（`@_user_1` …）。
    #[serde(default)]
    pub key: String,
    /// 被提及者的三种 id。
    #[serde(default)]
    pub id: LarkSenderId,
    /// 显示名（可能为空）。
    #[serde(default)]
    pub name: String,
}

// =====================================================================
// 归一化后的入站事件（解码器的**输出**）
// =====================================================================

/// 一条**已解码**的入站事件 —— 解码器与世界之间的唯一形态。
///
/// 它是上游 lark 包的局部 `InboundMessage` **减去** M7-12 那三个派生字段
/// （`Body` / `CommandBody` / `AddressedToBot`，见模块文档的交接表）。字段名与上游逐条对应，
/// 便于两边的用例逐字段比对。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LarkInboundEvent {
    /// 事件类型（恒为 [`EVENT_TYPE_MESSAGE_RECEIVE`]）。
    pub event_type: String,
    /// 平台投递 id。
    pub event_id: String,
    /// 机器人自身的 `app_id`（多机器人部署下按它选安装）。
    pub app_id: String,
    /// 租户键。
    pub tenant_key: String,
    /// 会话 id。
    pub chat_id: ChatId,
    /// 归一化后的会话类型（未知取值**失败关闭**成 [`ChatType::Group`]，见 [`normalize_chat_type`]）。
    pub chat_type: ChatType,
    /// 平台消息 id。
    pub message_id: String,
    /// 发送者的按应用 id。
    pub sender_open_id: OpenId,
    /// 发送者的跨应用稳定 id（空 = 平台没给）。
    pub sender_union_id: String,
    /// 平台原始消息类型（`text` / `post` / `image` / …）。
    pub message_type: String,
    /// 原样的正文（Lark 双重编码的 JSON 字符串）——**摊平归 M7-12**。
    pub content: String,
    /// 原样的提及数组（WS 形状）——**改写归 M7-12**。
    pub mentions: Vec<LarkEventMention>,
    /// 平台给的创建时间（字串）。
    pub create_time: String,
    /// 直接引用的消息 id。
    pub parent_id: String,
    /// 回复树根 id。
    pub root_id: String,
    /// 话题 id（空 = 顶层消息）。
    pub thread_id: String,
    /// 信封**逐字**（`serde_json::Value`）：解析器要读的 `app_id` / `event_type` /
    /// `create_time` 都在这里，且平台加字段不会丢。
    pub raw: Value,
}

impl LarkInboundEvent {
    /// 是否在话题里（`thread_id` 非空）。
    #[must_use]
    pub fn is_threaded(&self) -> bool {
        !self.thread_id.is_empty()
    }

    /// 是否是对某条消息的回复 / 引用（`parent_id` 或 `root_id` 非空）。
    #[must_use]
    pub fn is_reply(&self) -> bool {
        !self.parent_id.is_empty() || !self.root_id.is_empty()
    }

    /// 是否来自 p2p 直聊。
    #[must_use]
    pub fn is_direct(&self) -> bool {
        self.chat_type == ChatType::P2p
    }
}

/// `chat_type` → 归一化（上游 `normalizeChatType` 的本地形态）。
///
/// 上游把未知取值原样塞进 `ChatType(t)`；本仓的 [`ChatType`] 只有两个变体 ⇒ 按本 crate 的
/// 既有先例（`dingtalk/inbound.rs` 的 `dingtalk_chat_type`）**失败关闭到群**：群的入站要走
/// engine 的"必须 @ bot"过滤，最坏情况是漏一条闲聊，而不是把群消息当成私聊提示词。
/// 比较是大小写不敏感的（上游 `strings.ToLower`）。
#[must_use]
pub fn normalize_chat_type(raw: &str) -> ChatType {
    if raw.eq_ignore_ascii_case("p2p") {
        ChatType::P2p
    } else {
        ChatType::Group
    }
}

// =====================================================================
// 解码结果与端口
// =====================================================================

/// 解码器的三种结果里的两种"成功"（第三种是 `Err(`[`DecodeError`]`)`）。
#[derive(Debug, Clone, PartialEq)]
pub enum DecodeOutcome {
    /// 认得出、且是要交给 engine 的事件。
    Message(Box<LarkInboundEvent>),
    /// 心跳形状的 JSON / 未订阅的事件类型 / 空载荷 —— **静默丢弃但照常 ACK**。
    Ignored,
}

/// 载荷解不开（上游 `(zero, false, err)` 那一支）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// JSON 信封坏。
    #[error("lark ws decode: malformed event envelope")]
    Envelope,
    /// 事件体坏（信封是好的、`event` 解不出 `im.message.receive_v1`）。
    #[error("lark ws decode: malformed {event_type} payload")]
    Event {
        /// 出问题的事件类型（**不含**载荷内容）。
        event_type: String,
    },
    /// `event_callback` 但 `event` 为空（上游逐字：`event_callback with empty event payload`）。
    #[error("lark ws decode: event_callback with an empty event payload")]
    EmptyEvent,
}

/// 一个数据帧载荷 → 事件 / 忽略 / 错误（上游 `FrameDecoder` 接口）。
///
/// 端口化的理由与 engine 的其它端口一致：连接器的用例不该依赖真 JSON 语义。
pub trait FrameDecoder: Send + Sync {
    /// 解一帧载荷。
    ///
    /// # Errors
    ///
    /// JSON 坏 / 事件体坏 / 空事件体 ⇒ [`DecodeError`]（连接器按"丢这一帧 + ACK 200"处置）。
    fn decode(&self, payload: &[u8]) -> Result<DecodeOutcome, DecodeError>;
}

/// 生产解码器（上游 `LarkJSONFrameDecoder`）：无状态、可并发共享。
#[derive(Debug, Default, Clone, Copy)]
pub struct LarkJsonFrameDecoder;

impl LarkJsonFrameDecoder {
    /// 造一个（无状态，纯为对齐上游的构造习惯）。
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl FrameDecoder for LarkJsonFrameDecoder {
    fn decode(&self, payload: &[u8]) -> Result<DecodeOutcome, DecodeError> {
        // 空载荷是"心跳形状"的一种 ⇒ 忽略而不是错误（上游逐字）。
        if payload.is_empty() {
            return Ok(DecodeOutcome::Ignored);
        }
        let envelope: LarkEventEnvelope =
            serde_json::from_slice(payload).map_err(|_| DecodeError::Envelope)?;
        // `raw` 是信封**逐字**（不是重新序列化的结构）：解析器读到的 app_id / event_type /
        // create_time 与平台发来的逐字一致，平台加字段也不会在这里丢掉。
        let raw: Value = serde_json::from_slice(payload).map_err(|_| DecodeError::Envelope)?;

        // 长连接上的数据帧恒是 v2 信封；老式 `event_callback` 防御性接受（上游逐字）。
        if !envelope.envelope_type.is_empty() && envelope.envelope_type != LEGACY_CALLBACK_TYPE {
            return Ok(DecodeOutcome::Ignored);
        }
        if envelope.header.event_type != EVENT_TYPE_MESSAGE_RECEIVE {
            return Ok(DecodeOutcome::Ignored);
        }
        let Some(event) = envelope.event.clone() else {
            return Err(DecodeError::EmptyEvent);
        };
        let receive: LarkMessageReceiveEvent =
            serde_json::from_value(event).map_err(|_| DecodeError::Event {
                event_type: envelope.header.event_type.clone(),
            })?;
        let message = receive.message;
        Ok(DecodeOutcome::Message(Box::new(LarkInboundEvent {
            event_type: envelope.header.event_type,
            event_id: envelope.header.event_id,
            app_id: envelope.header.app_id,
            tenant_key: envelope.header.tenant_key,
            chat_id: ChatId::new(message.chat_id),
            chat_type: normalize_chat_type(&message.chat_type),
            message_id: message.message_id,
            sender_open_id: OpenId::new(receive.sender.sender_id.open_id),
            sender_union_id: receive.sender.sender_id.union_id,
            message_type: message.message_type,
            content: message.content,
            mentions: message.mentions,
            create_time: message.create_time,
            parent_id: message.parent_id,
            root_id: message.root_id,
            thread_id: message.thread_id,
            raw,
        })))
    }
}

#[cfg(test)]
mod tests;
