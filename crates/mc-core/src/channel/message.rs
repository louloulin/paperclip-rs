//! 归一化的**跨渠道**消息信封：`InboundMessage` / `OutboundMessage`（上游
//! `server/internal/integrations/channel/message.go` 的逐条移植）。
//!
//! - **写者**：M7-0 anchor（本片；`docs/60-M7-PLAN.md` §2.1）。落地后**各片只读**。
//! - **为什么要有这一层**（`docs/60` §2.6 第 1 条）：engine 不知道平台、adapter 不知道 DB。
//!   两者的交点就是本文件的几个结构 —— 平台自己的原始载荷必须留在 [`InboundMessage::raw`]，
//!   只有 adapter 自己读它。
//! - **本仓约定**：`raw` 用 `serde_json::Value`（不是 `Vec<u8>`）：本仓的 JSON 全链路都走
//!   `serde_json`，上游的 `json.RawMessage` 等价物就是它。
//! - **不做什么**：
//!   - 不做 dedup / 绑定 / 会话判定（那些是 engine 与仓储）；
//!   - 不做富卡片 / 媒体上传的建模：上游明确**不**把富输出放进跨平台信封
//!     （`OutboundMessage` 只有文本 + 线程 + 引用；富输出是 adapter 自己的类型）；
//!   - 不在这里放平台字段（`open_id` / `team_id` / `app_id`…）—— 它们属于 `raw`。
//!
//! # 形态纪律（两条，来自上游注释，别丢）
//!
//! 1. [`InboundMessage::media_refs`] 是 **engine 的输出通道**（`MediaResolver` 下载/上传后
//!    回填）：adapter **不得**预填它，inbound 消息到达时它必须是空的。
//! 2. [`InboundMessage::addressed_to_bot`] 只对群聊有意义（p2p 里 engine 忽略它）；它是
//!    **归一化后的布尔**，不是平台数据（mention 数组 / parent id 留在 `raw`）。

use serde::{Deserialize, Serialize};

/// 直聊还是群聊。wire 取值与 `channel_chat_session_binding.chat_type` 的 `CHECK` 逐字一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatType {
    /// 与 bot 的一对一会话（上游 `ChatTypeP2P`）。
    P2p,
    /// 多人群聊（上游 `ChatTypeGroup`）。
    Group,
}

impl ChatType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::P2p => "p2p",
            Self::Group => "group",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "p2p" => Some(Self::P2p),
            "group" => Some(Self::Group),
            _ => None,
        }
    }

    /// 群聊才需要 `addressed_to_bot` 判定。
    pub fn is_group(self) -> bool {
        matches!(self, Self::Group)
    }
}

/// 归一化后的消息种类（上游 `channel.MsgType`）。
///
/// 平台的原始类型串（Lark 的 `post` / `merge_forward` / `interactive`…）**不在**这里：
/// engine 只需要知道"文本还是媒体、哪种媒体"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Text,
    Image,
    File,
    Audio,
    Video,
    /// adapter 认不出的平台类型：engine 按"非文本、不可动作"处理。
    Unknown,
}

impl MessageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
            Self::File => "file",
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Unknown => "unknown",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "text" => Some(Self::Text),
            "image" => Some(Self::Image),
            "file" => Some(Self::File),
            "audio" => Some(Self::Audio),
            "video" => Some(Self::Video),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

/// 入站消息的**跨平台路由身份**（上游 `channel.Source`）。
///
/// 每个字段在每个平台上都成立；平台专有的路由键（Lark 的 `app_id`、Slack 的 `team_id`）
/// 由 adapter 解析成 installation，**不**出现在这里。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// 消息来自哪个平台；等于拥有该连接的 adapter 的 kind。
    pub channel_type: crate::channel::ChannelKind,
    /// 平台会话 id。一个 `chat_id` 经 `channel_chat_session_binding` 映射到一个 `chat_session`。
    pub chat_id: String,
    pub chat_type: ChatType,
    /// 平台原生、**安装内**稳定的发件人 id（Lark `open_id`、Slack user id…）。
    /// 绑定行就存在这个键上；**跨安装不可比**。
    pub sender_id: String,
    /// 平台给出的**跨安装**稳定身份（Lark `union_id`…），没有就空。
    pub sender_stable_id: String,
    /// 所属线程 / 话题；空 = 顶层消息（出站回复要按它回帖）。
    pub thread_id: String,
}

/// adapter **已经**持久化到对象存储的媒体引用（上游 `channel.MediaRef`）。
///
/// engine 只拿引用、不拿字节，信封才能保持小且平台中立。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaRef {
    pub message_kind: MessageKind,
    /// Multica 对象存储里的键。
    pub storage_key: String,
    /// 存储后端返回的对象 URL（附件下载端点靠它重开）。
    pub storage_url: String,
    /// 原始文件名（平台给才有）。
    pub filename: String,
    pub mime_type: String,
    /// 对象字节数；未知为 0。
    pub size_bytes: i64,
    /// 正文里的**精确**内联占位标记（空 = 附件独立存在，保持平台既有行为）。
    pub inline_placeholder: String,
    /// [`MediaRef::inline_placeholder`] 的第几次出现（0 起）：部分媒体失败时不会串位。
    pub inline_index: i32,
}

/// 入站消息引用 / 回复的上下文（上游 `channel.ReplyCtx`）。不是回复时为 `None`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyCtx {
    /// 被引用（直接父）消息的平台 id。
    pub message_id: String,
    /// 平台给出的线程 / 根锚点 id。
    pub root_id: String,
}

/// engine 消费的**唯一**归一化入站形态（上游 `channel.InboundMessage`）。
///
/// router / dedup / 身份校验 / 持久化**只**读这些字段；平台专有的一切留在 [`Self::raw`]。
///
/// 五个 `bool` 是**上游契约的一部分**（`addressed_to_bot` / `has_selected_context` /
/// `force_fresh` / `skip_agent_run` 是四个独立语义开关，不是可以打包的状态位）⇒ 这里
/// 显式豁免 `struct_excessive_bools`，而**不是**为了躲 lint 把它们塞进一个位图
/// （那会让 20 个切片的构造点全部改写成位运算）。
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboundMessage {
    /// 平台的投递/事件 id 与消息 id。二者一起支撑幂等层（重连时平台会重投，
    /// dedup 按 `(installation, message_id)` 键）。
    pub event_id: String,
    pub message_id: String,
    pub source: Source,
    pub kind: MessageKind,
    /// adapter 摊平出来的 agent 可读正文。
    pub text: String,
    /// 命令剥离 / 上下文富化**之前**的原始文本；空 = 与 [`Self::text`] 相同。
    pub command_text: String,
    /// 文本里是否含发送者**显式选中**的引用/转发（不含自动近况与裸回复坐标）。
    pub has_selected_context: bool,
    /// **engine 的输出通道**（见模块文档形态纪律第 1 条）：adapter 必须留空。
    pub media_refs: Vec<MediaRef>,
    /// 被引用 / 回复的上下文。
    pub reply_to: Option<ReplyCtx>,
    /// adapter 给出的"群聊里这条是否在跟 bot 互动"归一化结论（p2p 无意义）。
    pub addressed_to_bot: bool,
    /// 要求 engine 开新会话（`/clear` 一类），而不是续用旧会话。
    pub force_fresh: bool,
    /// **持久化但不触发 agent run**（例如 wecom 的独立 `/issue` 命令）。
    pub skip_agent_run: bool,
    /// 未经触碰的平台原始载荷（JSON）；**只有 adapter 读它**。
    pub raw: serde_json::Value,
}

impl InboundMessage {
    /// dedup 键（上游 `(installation_id, message_id)` 的另一半）。
    pub fn dedup_message_id(&self) -> &str {
        &self.message_id
    }

    /// 命令分类器要读的文本：`command_text` 为空时退回 `text`（上游 `CommandText` 的空值语义）。
    pub fn command_source_text(&self) -> &str {
        if self.command_text.is_empty() {
            &self.text
        } else {
            &self.command_text
        }
    }
}

/// engine 能让**任何** adapter 投递的最小出站消息（上游 `channel.OutboundMessage`）。
///
/// 富卡片 / 媒体上传 / 出站 webhook **故意**不建模：支持更丰富输出的 adapter 用自己的
/// 类型暴露，不往这个跨平台信封里加字段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboundMessage {
    /// 目标会话（平台 chat id）。
    pub chat_id: String,
    /// 消息正文。
    pub text: String,
    /// 非空则把回复挂进该线程 / 话题；空 = 会话层发送。
    pub thread_id: String,
    /// 非空则引用回复该平台消息 id。
    pub reply_to: String,
}

/// `Channel::send` 的结果（上游 `channel.SendResult`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendResult {
    /// 平台为**这条**消息给的 id。
    pub message_id: String,
    /// 一条逻辑回复被拆成多条时，**每一条**的平台 id；不分片的 adapter 可以留空。
    pub message_ids: Vec<String>,
}

impl SendResult {
    /// 单条发送的便捷构造（`message_ids` 留空 —— 上游注释允许）。
    pub fn single(message_id: impl Into<String>) -> Self {
        Self {
            message_id: message_id.into(),
            message_ids: Vec::new(),
        }
    }

    /// 分片发送：`message_id` 取第一条，其余全部进 `message_ids`。
    pub fn chunked(message_ids: Vec<String>) -> Self {
        let message_id = message_ids.first().cloned().unwrap_or_default();
        Self {
            message_id,
            message_ids,
        }
    }

    /// 是否分成了多条。
    pub fn is_chunked(&self) -> bool {
        self.message_ids.len() > 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::ChannelKind;

    fn source(chat_type: ChatType) -> Source {
        Source {
            channel_type: ChannelKind::Slack,
            chat_id: "C1".into(),
            chat_type,
            sender_id: "U1".into(),
            sender_stable_id: String::new(),
            thread_id: String::new(),
        }
    }

    fn inbound() -> InboundMessage {
        InboundMessage {
            event_id: "Ev1".into(),
            message_id: "Msg1".into(),
            source: source(ChatType::P2p),
            kind: MessageKind::Text,
            text: "hello".into(),
            command_text: String::new(),
            has_selected_context: false,
            media_refs: Vec::new(),
            reply_to: None,
            addressed_to_bot: false,
            force_fresh: false,
            skip_agent_run: false,
            raw: serde_json::json!({ "native": "payload" }),
        }
    }

    /// 两个封闭词表的 wire 取值与 DB 的 `CHECK` 逐字一致，且往返闭合。
    #[test]
    fn closed_vocabularies_round_trip() {
        for chat_type in [ChatType::P2p, ChatType::Group] {
            assert_eq!(ChatType::from_str_opt(chat_type.as_str()), Some(chat_type));
        }
        assert_eq!(ChatType::from_str_opt("channel"), None);
        for kind in [
            MessageKind::Text,
            MessageKind::Image,
            MessageKind::File,
            MessageKind::Audio,
            MessageKind::Video,
            MessageKind::Unknown,
        ] {
            assert_eq!(MessageKind::from_str_opt(kind.as_str()), Some(kind));
        }
        assert_eq!(MessageKind::from_str_opt("post"), None);
        assert!(ChatType::Group.is_group());
        assert!(!ChatType::P2p.is_group());
    }

    /// `command_source_text` 的空值语义（命令分类器读它，别读被改写的 `text`）。
    #[test]
    fn command_text_falls_back_to_text() {
        let mut message = inbound();
        assert_eq!(message.command_source_text(), "hello");
        message.command_text = "/issue fix it".into();
        assert_eq!(message.command_source_text(), "/issue fix it");
        assert_eq!(message.dedup_message_id(), "Msg1");
    }

    /// 入站信封的**形态纪律**：`media_refs` 到达时必须是空的（adapter 不得预填）。
    #[test]
    fn inbound_arrives_without_media_refs() {
        let message = inbound();
        assert!(message.media_refs.is_empty());
        assert!(message.reply_to.is_none());
        // 平台载荷原样保留在 raw 里（engine 不读，adapter 读）。
        assert_eq!(message.raw["native"], "payload");
    }

    /// `SendResult` 的两种构造（单条 / 分片）。
    #[test]
    fn send_result_single_and_chunked() {
        let single = SendResult::single("m1");
        assert_eq!(single.message_id, "m1");
        assert!(single.message_ids.is_empty());
        assert!(!single.is_chunked());

        let chunked = SendResult::chunked(vec!["m1".into(), "m2".into()]);
        assert_eq!(chunked.message_id, "m1");
        assert!(chunked.is_chunked());

        // 空分片不是 panic（message_id 为空串）。
        let empty = SendResult::chunked(Vec::new());
        assert_eq!(empty.message_id, "");
        assert!(!empty.is_chunked());
    }
}
