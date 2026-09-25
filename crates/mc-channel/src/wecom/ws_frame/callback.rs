//! aibot 入站帧的 body 形态（上游 `ws_frame.go` 的 `aibotMsgCallback` / `quotedMessage` / `mediaBody` / `mixedItem` / `aibotEventCallback`）。
//!
//! 本文件是 `ws_frame.rs` 的子模块：拆分依据是 `docs/60-M7-PLAN.md` §6.3 的强制拆分
//! 加上门 ⑩ 的 800 行硬限。逐条清单见 `docs/32` §33 的 D10。

use super::*;

// 入站 body 形态（上游 aibotMsgCallback / quotedMessage / mediaBody / mixedItem /
// aibotEventCallback）
// =====================================================================

/// `aibot_msg_callback` 的 body —— 从聊里推给 bot 的一条用户消息（上游 `aibotMsgCallback`）。
///
/// 字段名与 wire **逐字一致**（`msgid` / `aibotid` / `chatid` / `chattype` / `from.userid` /
/// `msgtype` / `text.content` / `voice.content` / `image|file|video{url,aeskey}` /
/// `mixed.msg_item` / `quote`）。
///
/// 手写 `Debug`：`voice.content` 是**转写文本**、`text.content` 是用户原文，两者都是**内容**
/// 而非凭据 ⇒ 不脱敏。但 `quote` / `mixed` 里的媒体 body 带 `aeskey`（**单次使用的密钥**）
/// ⇒ 见 [`MediaBody`] 的脱敏实现。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AibotMsgCallback {
    #[serde(default)]
    pub msgid: String,
    #[serde(default)]
    pub aibotid: String,
    #[serde(default)]
    pub chatid: String,
    /// `"single"` | `"group"`。
    #[serde(default)]
    pub chattype: String,
    #[serde(default)]
    pub from: CallbackSender,
    /// `"text"` | `"image"` | `"voice"` | `"file"` | `"video"` | `"mixed"`。
    #[serde(default)]
    pub msgtype: String,
    #[serde(default)]
    pub text: TextBody,
    /// `voice` 带的是**转写文本**，不是音频：`WeCom` 在自己那侧做完语音识别后只投结果
    /// （上游注释逐字）。
    #[serde(default)]
    pub voice: TextBody,
    #[serde(default)]
    pub image: MediaBody,
    #[serde(default)]
    pub file: MediaBody,
    #[serde(default)]
    pub video: MediaBody,
    /// 图文混排（`msg_item` 按用户编排的顺序）。
    #[serde(default)]
    pub mixed: MixedBody,
    /// 引用（`引用`）：发送者正在回复的那条消息。
    #[serde(default)]
    pub quote: QuotedMessage,
}

/// 发送者（上游匿名内嵌 `struct{ UserID string }`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallbackSender {
    #[serde(default)]
    pub userid: String,
}

/// `{"content": "…"}` —— `text` 与 `voice` 共用（上游两个匿名结构）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextBody {
    #[serde(default)]
    pub content: String,
}

/// 每一种可下载类型都带的 `{url, aeskey}` 对（上游 `mediaBody`）。
///
/// 长连接模式下 `aeskey` 是**按 url 现铸**的，所以它活在消息上而不是配置里。
///
/// 手写 `Debug`：`url` 是五分钟有效的预签名地址（**能直接取回密文**），`aeskey` 是解它的
/// 密钥 —— 两者都不进任何 `{:?}` / 日志 / `assert_eq!` 的失败回显（`docs/60` §2.3）。
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaBody {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub aeskey: String,
}

impl fmt::Debug for MediaBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MediaBody")
            .field("url", &redacted_if_set(&self.url))
            .field("aeskey", &redacted_if_set(&self.aeskey))
            .finish()
    }
}

/// `Some("<redacted>")` / `None` —— 让 `Debug` 既能看出"有没有"，又不带值。
fn redacted_if_set(value: &str) -> Option<&'static str> {
    if value.is_empty() {
        None
    } else {
        Some("<redacted>")
    }
}

/// 图文混排里的一段：一句话、一句说出来的话，或一个附件（上游 `mixedItem`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MixedItem {
    #[serde(default)]
    pub msgtype: String,
    #[serde(default)]
    pub text: TextBody,
    #[serde(default)]
    pub voice: TextBody,
    #[serde(default)]
    pub image: MediaBody,
    #[serde(default)]
    pub file: MediaBody,
    #[serde(default)]
    pub video: MediaBody,
}

/// `mixed.msg_item`（上游匿名结构，出现两次：`aibotMsgCallback.Mixed` 与
/// `quotedMessage.Mixed`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MixedBody {
    #[serde(default)]
    pub msg_item: Vec<MixedItem>,
}

/// 发送者回复的那条消息（上游 `quotedMessage`）。
///
/// 上游在这里做的是 `embedded mixedItem`（Go 的**嵌入**）：`MsgType` 与四个 body 从
/// [`MixedItem`] 平铺上来，`Mixed` 再嵌一层。`serde` 的 `flatten` 会与 `#[serde(default)]`
/// 打架（flat 之后的未知字段处理），所以本仓用**显式字段**把平铺写出来 —— 形态与 wire
/// 逐字一致，代价是四个字段的重复。
///
/// `Debug` 手写成脱敏版（与 [`MixedItem`] 同理：媒体 body 带 `aeskey`）。
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QuotedMessage {
    #[serde(default)]
    pub msgtype: String,
    #[serde(default)]
    pub text: TextBody,
    #[serde(default)]
    pub voice: TextBody,
    #[serde(default)]
    pub image: MediaBody,
    #[serde(default)]
    pub file: MediaBody,
    #[serde(default)]
    pub video: MediaBody,
    /// 引用的图文混排比普通的一段**多嵌一层**（上游注释逐字）。
    #[serde(default)]
    pub mixed: MixedBody,
}

impl fmt::Debug for QuotedMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QuotedMessage")
            .field("msgtype", &self.msgtype)
            .field("text", &self.text)
            .field("voice", &self.voice)
            .field("image", &self.image)
            .field("file", &self.file)
            .field("video", &self.video)
            .field("mixed_items", &self.mixed.msg_item.len())
            .finish()
    }
}

/// `aibot_event_callback` 的 body（上游 `aibotEventCallback`）。
///
/// 上游只看事件类型；具体事件字段（模板卡选择、反馈投票）**还没有**被摊出来 ⇒ 本仓照抄
/// 这条边界，抄的是"没有"本身。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AibotEventCallback {
    #[serde(default)]
    pub event: EventBody,
}

/// `{"eventtype": "…"}`（上游匿名结构）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventBody {
    #[serde(default)]
    pub eventtype: String,
}

// =====================================================================
