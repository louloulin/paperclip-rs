//! `WeCom` aibot 的 **WebSocket wire 格式**：每一帧都是 `{cmd, headers.req_id, body}`
//! 的信封（上游 `internal/integrations/wecom/ws_frame.go`，**1,187 行**）。
//!
//! - **写者**：M7-16（`LUM-1781` / `docs/60-M7-PLAN.md` §3.3）。
//! - **上游定位**（文件头注释逐字）：`ws_frame.go` 是 aibot 的 wire 层 ——
//!   `inbound`（`aibot_msg_callback` 用户消息 / `aibot_event_callback` 事件）、
//!   `outbound`（`aibot_subscribe` 认证 / `ping` 心跳 / `aibot_send_msg` 推送 /
//!   `aibot_respond_msg` 窗内回复）、`response`（服务端对我们写出的帧的 ack）。
//!   wire 文档：<https://developer.work.weixin.qq.com/document/path/101463>。
//!
//! # 本文件只做「帧编解码」；帧路由与发送在 `ws_sender.rs`
//!
//! `docs/60-M7-PLAN.md` §6.3 要求本片把 1,187 行的上游文件**按「帧编解码 / 帧路由」
//! 拆两文件**（不拆就会撞门 ⑩ 的 800 行硬限）。本片的写集是**逐字的三条路径**，
//! 拆分因此落在**写集内部**的这条缝上：
//!
//! | 半 | 本地文件 | 上游出处 |
//! | --- | --- | --- |
//! | **帧编解码**（信封、各 cmd 的 body 形态、编解码、上限、文本工具） | **本文件** | `ws_frame.go` 的 wire 层 |
//! | **帧路由**（把服务端回帧按 `req_id` 交给等待者）+ 并发写 | `ws_sender.rs` | `ws_sender.go` 的 `routeResponse` / `deliverAck` / `deliverReply` + 写侧 |
//!
//! 两半的接缝是 [`Frame`]（解出来的**类型化**帧）与 [`FrameEnvelope`]（原样信封）：
//! 读循环（M7-19）用 [`decode_frame`] 拿 [`Frame`]，把其中的 `Response` 变体交给
//! `ws_sender::WsSender::route_response` 配对。
//!
//! # 不属于本片的部分（**逐条点名，不默默略过**）
//!
//! 上游 `ws_frame.go` 的后半段（`ownText` / `ownCommandSource` / `attachments` /
//! `quotedContext` / `channelMessageFromCallback` / `stripLeadingMentions` /
//! `normalizeWeComControlLayout` / `isIssueCommand` / `channelMsgType`，合计约
//! **640 行**）是**入站归一化**，本地落点是 M7-19 的
//! `wecom/{inbox_message.rs,wecom_channel.rs,resolvers.rs}`（`docs/60` §3.3 的写集表）——
//! 上游自己也在 `inbox_message.go`（M7-19 的上游文件）里放归一化信封，两者本就同面。
//! 本文件只交出归一化**需要的输入**：[`AibotMsgCallback`] 的逐字段解析。
//! 逐条清单见 `docs/32` §33 的 D2。
//!
//! # 帧大小上限（本仓新增，上游无对应物）
//!
//! 上游**没有**任何显式的帧上限：读侧靠 `gorilla` 的默认（无限制），写侧只靠
//! `streamContentLimit` / `sendMsgContentLimit` 两个**内容**上限。本仓补一条**帧**上限
//! [`MAX_FRAME_BYTES`]，在 [`decode_frame`] 与 `ws_sender` 的写侧各检查一次。
//! 理由：`tokio-tungstenite` 的默认帧上限是 16 MiB，一条被投毒的入站帧可以把读循环的
//! 内存吃到那个数；而 aibot 的合法帧上界由 20 KiB 的内容上限决定，1 MiB 已有三个数量级的
//! 余量。这是**收紧**而非放宽，登记为 `docs/32` §33 的 D3。

use std::fmt;
use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use mc_core::channel::message::ChatType;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::credentials::PlaintextSecret;

mod callback;

pub use callback::*;

// =====================================================================
// 帧命令与事件类型（上游四个 const 块，逐字）
// =====================================================================

/// 客户端发出的帧命令（上游 `cmdSubscribe` / `cmdPing` / `cmdSendMsg` / `cmdRespondMsg`）。
pub const CMD_SUBSCRIBE: &str = "aibot_subscribe";
pub const CMD_PING: &str = "ping";
pub const CMD_SEND_MSG: &str = "aibot_send_msg";
pub const CMD_RESPOND_MSG: &str = "aibot_respond_msg";

/// 服务端发出的帧命令 —— 读循环就 switch 这几个（上游 `cmdMsgCallback` … `cmdPong`）。
pub const CMD_MSG_CALLBACK: &str = "aibot_msg_callback";
pub const CMD_EVENT_CALLBACK: &str = "aibot_event_callback";
/// 服务端也会主动 ping（上游 `cmdServerPing`，与客户端的 `CMD_PING` **同字面量**）。
pub const CMD_SERVER_PING: &str = "ping";
pub const CMD_PONG: &str = "pong";

/// `aibot_event_callback.body.event.eventtype` 的事件类型（上游四条）。
pub const EVENT_DISCONNECTED: &str = "disconnected_event";
pub const EVENT_ENTER_CHAT: &str = "enter_chat";
pub const EVENT_TEMPLATE_CARD: &str = "template_card_event";
pub const EVENT_FEEDBACK: &str = "feedback_event";

/// `aibot_send_msg` 的接收者形态：`WeCom` 用**整数**，不是字符串（上游 `chatTypeSingleInt`
/// / `chatTypeGroupInt`）。
pub const CHAT_TYPE_SINGLE_INT: i32 = 1;
pub const CHAT_TYPE_GROUP_INT: i32 = 2;

/// 心跳间隔（上游 `pingInterval`，在 `wecom_channel.go`）：`WeCom` 会杀掉静默超过 ~90s 的
/// socket，所以每 30s 一次 ping。
pub const PING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// 一次握手的 TCP + WS dial 预算（上游 `handshakeTimeout`，在 `wecom_channel.go`）。
pub const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// 单帧的写预算（上游 `writeDeadline`，在 `wecom_channel.go`）。
pub const WRITE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// 本仓新增的**帧**上限（见模块文档；上游无对应物）。
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

// =====================================================================
// 信封与头部
// =====================================================================

/// 每帧的相关 id（上游 `frameHeaders`）。服务端的 ack 会把 `req_id` 原样回显，客户端据此
/// 把请求与响应配对。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameHeaders {
    /// 缺省为空串（上游是 Go 的零值 `""`，`omitempty` **不在**这里：上游两个字段都没有 tag
    /// 的 omit，所以空串会**照常序列化**）。
    #[serde(default)]
    pub req_id: String,
}

/// 服务端推来的每一帧的外壳（上游 `frameEnvelope`）。
///
/// `body` **保持原样**（上游 `json.RawMessage`）：让下游按具体的 cmd 各自解一次，
/// 而不是先把外壳解成一个统一形态再重解一次。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FrameEnvelope {
    #[serde(default)]
    pub cmd: String,
    #[serde(default)]
    pub headers: FrameHeaders,
    /// 缺省 `Value::Null`（对应上游 `nil` 的 `json.RawMessage`）。
    #[serde(default)]
    pub body: Value,
    /// 服务端 ack 我们写出的帧时才有（上游 `ErrCode`）。
    #[serde(default)]
    pub errcode: i32,
    /// 同上的文案（上游 `ErrMsg`）。
    #[serde(default, rename = "errmsg")]
    pub error_message: String,
}

impl FrameEnvelope {
    /// 这一帧是不是一次 ack（上游 `routeResponse` 只看这两个字段）。
    #[must_use]
    pub fn is_ack(&self) -> bool {
        self.errcode != 0 || !self.error_message.is_empty()
    }
}

/// 解出来的一帧。
///
/// 上游的读循环是一个 `switch env.Cmd`（`wecom_channel.go` 的 `dispatchFrame`），这里把它
/// 变成**类型化**的枚举：M7-19 的读循环拿到的是已经分好类的帧，不必再自己 switch 一遍
/// 字符串。
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    /// 用户消息回调（`aibot_msg_callback`）。解析失败**不**在这里报错 ——
    /// 上游对不合形态的 body 是"跳过这一帧"，形态判定归 M7-19。
    MsgCallback {
        req_id: String,
        callback: Box<AibotMsgCallback>,
    },
    /// 事件回调（`aibot_event_callback`）。
    EventCallback {
        req_id: String,
        event: AibotEventCallback,
    },
    /// 服务端对我们写出的帧的应答（含 `errcode` / `errmsg`，可能有 body）。
    Response(FrameEnvelope),
    /// 服务端主动 ping（`cmd=ping`）。
    ServerPing { req_id: String },
    /// `pong`。
    Pong { req_id: String },
    /// 这个 adapter 不认识的命令。**不是**错误：`WeCom` 会加新命令，而读循环跳过
    /// 不认识的帧正是上游的行为。
    Unknown { cmd: String, req_id: String },
}

impl Frame {
    /// 这一帧的 `req_id`（上游代码里到处都是 `env.Headers.ReqID`）。
    #[must_use]
    pub fn req_id(&self) -> &str {
        match self {
            Self::MsgCallback { req_id, .. }
            | Self::EventCallback { req_id, .. }
            | Self::ServerPing { req_id }
            | Self::Pong { req_id }
            | Self::Unknown { req_id, .. } => req_id,
            Self::Response(envelope) => &envelope.headers.req_id,
        }
    }
}

// =====================================================================
// 帧编解码
// =====================================================================

/// 解帧失败。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    /// 帧比 [`MAX_FRAME_BYTES`] 大。
    #[error("wecom: frame of {len} bytes exceeds the {limit} byte cap")]
    TooLarge { len: usize, limit: usize },
    /// 不是合法 JSON（或形态与信封不符）。
    #[error("wecom: malformed frame: {message}")]
    Malformed { message: String },
    /// 一帧的**命令**是我们自己才发的（写侧误用读侧的解码器）。
    #[error("wecom: {cmd} is a client-to-server command, not an inbound frame")]
    NotInbound { cmd: String },
}

/// 把一段 wire 字节解成 [`Frame`]。
///
/// 两条判据在上游分散着（`SetReadLimit` 一类并不存在 + 读循环的 switch）：
///
/// 1. **大小**先于解析：一条超限的帧**不**进 `serde_json`（上游没有这条，见模块文档）；
/// 2. `body` 按 cmd 再解一次，解不动就是 [`FrameError::Malformed`] ——
///    与上游 `json.Unmarshal` 失败同义。
///
/// # Errors
///
/// [`FrameError::TooLarge`] / [`FrameError::Malformed`]。
pub fn decode_frame(raw: &[u8]) -> Result<Frame, FrameError> {
    if raw.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge {
            len: raw.len(),
            limit: MAX_FRAME_BYTES,
        });
    }
    let envelope: FrameEnvelope =
        serde_json::from_slice(raw).map_err(|error| FrameError::Malformed {
            message: error.to_string(),
        })?;
    let req_id = envelope.headers.req_id.clone();

    // 服务端对我们写出的帧的应答：**先**判 `errcode`/`errmsg` 存在的帧（上游
    // `routeResponse` 也是先看这两个字段），因为 ack 的 `cmd` 可能为空。
    match envelope.cmd.as_str() {
        CMD_MSG_CALLBACK => {
            let callback: AibotMsgCallback =
                serde_json::from_value(envelope.body).map_err(|error| FrameError::Malformed {
                    message: format!("{CMD_MSG_CALLBACK}: {error}"),
                })?;
            Ok(Frame::MsgCallback {
                req_id,
                callback: Box::new(callback),
            })
        }
        CMD_EVENT_CALLBACK => {
            let event: AibotEventCallback =
                serde_json::from_value(envelope.body).map_err(|error| FrameError::Malformed {
                    message: format!("{CMD_EVENT_CALLBACK}: {error}"),
                })?;
            Ok(Frame::EventCallback { req_id, event })
        }
        CMD_SERVER_PING => Ok(Frame::ServerPing { req_id }),
        CMD_PONG => Ok(Frame::Pong { req_id }),
        // 空 cmd + 有 ack 字段 ⇒ 一次应答（上游 ack 帧就是这样：只有 headers + errcode）。
        "" if !envelope.is_ack() && envelope.body.is_null() => Ok(Frame::Response(envelope)),
        "" => Ok(Frame::Response(envelope)),
        CMD_SUBSCRIBE | CMD_SEND_MSG | CMD_RESPOND_MSG => {
            Err(FrameError::NotInbound { cmd: envelope.cmd })
        }
        other => Ok(Frame::Unknown {
            cmd: other.to_owned(),
            req_id,
        }),
    }
}

/// 把一个 `{cmd, headers, body}` 编成 wire 字节，并检查帧上限。
///
/// # Errors
///
/// 序列化失败，或编出来的帧超过 [`MAX_FRAME_BYTES`]（写侧的最后一关）。
pub fn encode_frame(frame: &Value) -> Result<Vec<u8>, FrameError> {
    let encoded = serde_json::to_vec(frame).map_err(|error| FrameError::Malformed {
        message: error.to_string(),
    })?;
    if encoded.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge {
            len: encoded.len(),
            limit: MAX_FRAME_BYTES,
        });
    }
    Ok(encoded)
}

/// 把 `body` 包成 `{cmd, headers:{req_id}, body}`。
///
/// `body` 取**所有权**：每个调用点交出来的都是一个刚构造好的 `Value`
/// （[`SubscribeBody::into_value`] 的产物、`Value::Null`、或一个 `json!` 字面量），
/// 而 `json!` 内部无论如何都要把它变成一个 `Value` —— 收 `&Value` 只会多一次深拷贝。
#[allow(clippy::needless_pass_by_value)] // 见上：调用点交出的都是刚铸的 `Value`
#[must_use]
pub fn frame_with(req_id: &str, cmd: &str, body: Value) -> Value {
    json!({
        "cmd": cmd,
        "headers": { "req_id": req_id },
        "body": body,
    })
}

/// 我们的帧自己的 `req_id`（上游 `newReqID`，在 `wecom_channel.go`）。
///
/// 8 个随机字节的十六进制；系统随机源不可用时退回时间戳（上游同款兜底 ——
/// 一个可预测的 `req_id` 只是**配对**能力变弱，不是安全缺陷：真正的凭据是 `secret`）。
#[must_use]
pub fn new_req_id() -> String {
    let mut buf = [0_u8; 8];
    if getrandom(&mut buf) {
        return hex_encode(&buf);
    }
    format!("wecom-{}", unix_nanos())
}

/// 铸一个流式气泡的 id（上游 `newStreamID`）。复用同一个 id = **替换**那条消息的正文；
/// 换一个 = 开一条新气泡。
#[must_use]
pub fn new_stream_id() -> String {
    let mut buf = [0_u8; 12];
    if getrandom(&mut buf) {
        return format!("s{}", hex_encode(&buf));
    }
    format!("wecom-stream-{}", unix_nanos())
}

fn unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |delta| delta.as_nanos())
}

/// 从系统随机源取字节。返回是否成功。
///
/// 本仓**不新增依赖**（`Cargo.toml` 由 M7-0 一次性接好，`docs/60` §3.1）⇒ 这里用
/// `std` 的 `/dev/urandom`，而不是 `getrandom` crate。失败只有两种可能：平台没有
/// `/dev/urandom`，或 fd 耗尽 —— 两者都由调用方退回时间戳。
fn getrandom(buf: &mut [u8]) -> bool {
    use std::io::Read as _;
    let Ok(mut file) = std::fs::File::open("/dev/urandom") else {
        return false;
    };
    file.read_exact(buf).is_ok()
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // 写进 String 不会失败（`fmt::Write for String` 的 `write_str` 是 infallible）。
        let _ = write!(out, "{byte:02x}");
    }
    out
}

// =====================================================================
// 出站 body 构造（上游 subscribeBody / sendMsgTextBody /
// aibotChatTypeFromChannel / respondStreamBody）
// =====================================================================

/// `aibot_subscribe` 的 body（上游 `subscribeBody`）。成功时服务端回显 `req_id` 且
/// `errcode` 为 0。
///
/// `secret` 收 [`PlaintextSecret`] 而不是 `&str`：本函数是**唯一**会把明文写进一个
/// `serde_json::Value` 的地方，而那个 `Value` 一旦被 `{:?}` 打印就绕过了
/// [`PlaintextSecret`] 的脱敏 ⇒ 用类型把出口钉在这里，并且返回的 `Value` 由调用方
/// **立刻**编码成 wire 字节。`Debug` 打印它仍然会漏（`Value` 不脱敏）⇒ 见下面的
/// [`Debug for SubscribeBody`]，本函数返回的就是它。
#[must_use]
pub fn subscribe_body(bot_id: &str, secret: &PlaintextSecret) -> SubscribeBody {
    SubscribeBody {
        bot_id: bot_id.to_owned(),
        secret: secret.clone(),
    }
}

/// [`subscribe_body`] 的产物：**手写 `Debug`**，`secret` 只报 `<redacted>`；
/// [`SubscribeBody::into_value`] 是唯一的取 `Value` 出口（命名刺眼，照
/// [`PlaintextSecret::expose`] 的先例）。
#[derive(Clone, PartialEq, Eq)]
pub struct SubscribeBody {
    bot_id: String,
    secret: PlaintextSecret,
}

impl SubscribeBody {
    /// 明文出口：**唯一**会把 secret 变成 `serde_json::Value` 的地方。
    ///
    /// 调用方**必须**紧接着 [`encode_frame`]，把这个 `Value` 交给 socket（或交给
    /// [`FrameEnvelope`] 的测试替身），**不得**把它记进日志、断言消息或错误值。
    #[must_use]
    pub fn into_value(self) -> Value {
        json!({ "bot_id": self.bot_id, "secret": self.secret.expose() })
    }

    /// 认证身份的 bot id（**不是**凭据，可以照常进日志）。
    #[must_use]
    pub fn bot_id(&self) -> &str {
        &self.bot_id
    }
}

impl fmt::Debug for SubscribeBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SubscribeBody")
            .field("bot_id", &self.bot_id)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// `aibot_send_msg` 的 body 构造错误（上游两个 `errors.New`）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BodyError {
    #[error("wecom: send_msg requires chat_id")]
    MissingChatId,
    #[error("wecom: send_msg chat_type must be 1 (single) or 2 (group)")]
    BadChatType,
    #[error("wecom: stream frame requires a stream id")]
    MissingStreamId,
    #[error("wecom: closing stream frame needs visible content")]
    EmptyClosingFrame,
}

/// `aibot_send_msg` 的 body，携带纯文本内容（上游 `sendMsgTextBody`）。
///
/// `aibot_send_msg` 支持的 msgtype 只有 `markdown` 与 `template_card` ——
/// **text 不被这个 cmd 接受**（`aibot_respond_msg` 才接受 text，上游注释逐字）。
/// 所以这里按 `markdown` 发；`WeCom` 客户端会把纯文本按 markdown 路径渲染，无需转义。
/// `chat_type` 是 1（单聊）/ 2（群聊）。
///
/// # Errors
///
/// [`BodyError::MissingChatId`] / [`BodyError::BadChatType`]。
pub fn send_msg_text_body(
    chat_id: &str,
    chat_type: i32,
    content: &str,
) -> Result<Value, BodyError> {
    if chat_id.is_empty() {
        return Err(BodyError::MissingChatId);
    }
    if chat_type != CHAT_TYPE_SINGLE_INT && chat_type != CHAT_TYPE_GROUP_INT {
        return Err(BodyError::BadChatType);
    }
    Ok(json!({
        "chatid": chat_id,
        "chat_type": chat_type,
        "msgtype": "markdown",
        "markdown": { "content": content },
    }))
}

/// 把 engine 的 [`ChatType`] 映射成 `aibot_send_msg` 要的整数（上游
/// `aibotChatTypeFromChannel`）。
#[must_use]
pub fn aibot_chat_type_from_channel(chat_type: ChatType) -> i32 {
    match chat_type {
        ChatType::Group => CHAT_TYPE_GROUP_INT,
        ChatType::P2p => CHAT_TYPE_SINGLE_INT,
    }
}

// =====================================================================
// 流式回复（上游 ---- streaming replies ---- 段）
// =====================================================================

/// `stream.content` 的上限：20480 个 utf8 字节
/// （<https://developer.work.weixin.qq.com/document/path/101031>）。
///
/// 内容是气泡正文的**全量替换**而非增量 ⇒ 它约束的是**整条回答**，不是某一帧。
pub const STREAM_CONTENT_LIMIT: usize = 20480;

/// 开场帧说的话（上游 `streamThinkingPlaceholder`）。按 101031，带 `<think></think>` 的
/// 内容会渲染成客户端自己的"思考中"动效。腾讯自己的 `OpenClaw` 插件用同一个字面量开场。
pub const STREAM_THINKING_PLACEHOLDER: &str = "<think></think>";

/// 流跑过了自己的窗口、服务端不再接受它的帧（上游 `errcodeStreamExpired`）。
///
/// 上游 2026-08-09 对**活的租户**实测（不是推断）：一个每三十秒发一帧的流被服务端用这个码
/// 拒掉，`errmsg` 是 `stream message update expired (>10 minutes), cannot update`。
pub const ERRCODE_STREAM_EXPIRED: i32 = 846_608;

/// 这个 `req_id` 不允许承载流（上游 `errcodeStreamBadReqID`）。
///
/// 上游**明确标注为未证实的假设**：这个数是从腾讯 `OpenClaw` 插件源码读来的、没有钉版本，
/// 而且不像 846608 那样出现在 `WeCom` 的公开错误表里。事件回调的 `req_id` 看着可用、
/// 实际不行 —— 只有消息回调的可以，所以事件路径必须走 `aibot_send_msg`。
pub const ERRCODE_STREAM_BAD_REQ_ID: i32 = 846_605;

/// 服务端对一个流帧的拒绝（上游 `streamError`），带 `errcode` 以便调用方区分
/// "这个气泡没救了"与"那一帧没落地"。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("wecom: stream frame rejected errcode={code} errmsg={message}")]
pub struct StreamError {
    pub code: i32,
    pub message: String,
}

impl StreamError {
    /// 这个拒绝是否意味着这条流**再也写不进去**了 —— 调用方必须退回普通消息而不是重试
    /// （上游 `Unusable`）。
    #[must_use]
    pub fn unusable(&self) -> bool {
        self.code == ERRCODE_STREAM_EXPIRED || self.code == ERRCODE_STREAM_BAD_REQ_ID
    }
}

/// `aibot_respond_msg` 的 body，携带流式回复的**一帧**（上游 `respondStreamBody`）。
/// `finish=false` 画/更新气泡；`finish=true` 封口，此后消息不可变。
///
/// 空收尾帧的检查是 wire 格式里唯一**不显然**的那条规则：`WeCom` 会忽略没有任何可见内容的
/// 内容，所以一帧全是空格的收尾什么也封不住、留给用户一个永远转圈的气泡。在这里拒掉，
/// 每个调用方就都继承了这条检查。
///
/// # Errors
///
/// [`BodyError::MissingStreamId`] / [`BodyError::EmptyClosingFrame`]。
pub fn respond_stream_body(
    stream_id: &str,
    content: &str,
    finish: bool,
) -> Result<Value, BodyError> {
    if stream_id.is_empty() {
        return Err(BodyError::MissingStreamId);
    }
    let mut content = content.to_owned();
    if finish {
        content = defuse_think_tags(&content);
    }
    let content = truncate_stream_content(&content);
    if finish && !has_visible_char(&content) {
        return Err(BodyError::EmptyClosingFrame);
    }
    Ok(json!({
        "msgtype": "stream",
        "stream": {
            "id": stream_id,
            "finish": finish,
            "content": content,
        },
    }))
}

// =====================================================================
// 文本工具（上游 defuseThinkTags / truncateStreamContent / hasVisibleChar /
// splitForWire / wireCutPoint）
// =====================================================================

/// 零宽空格（U+200B）—— [`defuse_think_tags`] 插在 `<` 之后的那一个字符。
const ZERO_WIDTH_SPACE: char = '\u{200b}';

/// 阻止一条**谈论** `<think>` 的回答被当成 `<think>` 读（上游 `defuseThinkTags`）。
///
/// 这个标签是客户端的，不是我们的：按 101031，被 `<think></think>` 包起来的流正文会渲染成
/// `WeCom` 自己的折叠"思考"控件 —— 那正是开场帧的构成。一条恰好包含这个字面量的回答
/// （引用提示词、解释这个功能本身、粘贴 XML）会得到同样的待遇，于是半条回复消失进一个
/// 折叠里，既没有编辑也没有撤回可以把它弄回来。
///
/// 在尖括号后插一个零宽空格就够：扫描器不再匹配，读者看到的字符与原来一样。**只**动标签
/// 自己的开头，所以回答里其它地方的比较、泛型、HTML 样例都原样通过。调用方只在**收尾帧**
/// 上调用它 —— 开场帧**就是**那个动效。
#[must_use]
pub fn defuse_think_tags(text: &str) -> String {
    if !text.contains('<') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len() + 4);
    let bytes = text.as_bytes();
    let mut last = 0;
    // 直接按**字节**扫 `text`，绝不去扫一份 case-fold 过的副本：折叠**不保长**。
    // U+212A（KELVIN SIGN）是三字节，折叠成单字节的 "k"，所以折叠副本可能比原串**短**，
    // 而用原串算出的偏移可以越过它的末尾。`"KK<x"`（两个 Kelvin 号 + 一个尖括号）就足以
    // 切出界，而这里扫的是 **agent 自己的回答** —— 任何用户能哄 agent 复述的文本都能把
    // 后端带下水（上游注释逐字，是这条实现的全部理由）。
    for index in 0..bytes.len() {
        if bytes[index] != b'<' {
            continue;
        }
        let mut cursor = index + 1;
        if cursor < bytes.len() && bytes[cursor] == b'/' {
            cursor += 1;
        }
        if cursor + 5 > bytes.len() || !bytes[cursor..cursor + 5].eq_ignore_ascii_case(b"think") {
            continue;
        }
        if !text.is_char_boundary(index + 1) {
            // 不可能：`<` 是 ASCII，它后面永远是字符边界。留着是为了不写出一段
            // "按约定成立"的 `expect`（那会在真出问题时 panic 在读循环上）。
            continue;
        }
        out.push_str(&text[last..=index]);
        out.push(ZERO_WIDTH_SPACE);
        last = index + 1;
    }
    if last == 0 {
        return text.to_owned();
    }
    out.push_str(&text[last..]);
    out
}

/// 截断时用的省略号（[`truncate_stream_content`] / [`split_for_wire`] 共用）。
const ELLIPSIS: &str = "…";

/// 把内容按协议的字节上限截断，且切在**字符边界**上（上游 `truncateStreamContent`）。
/// 被裁短的回答仍然是一个回答；被服务端因长度拒掉的不是。
#[must_use]
pub fn truncate_stream_content(text: &str) -> String {
    if text.len() <= STREAM_CONTENT_LIMIT {
        return text.to_owned();
    }
    let mut cut = STREAM_CONTENT_LIMIT - ELLIPSIS.len();
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut out = text[..cut].to_owned();
    out.push_str(ELLIPSIS);
    out
}

/// `text` 里是否有一个既不是空白也不是控制字符的 rune（上游 `hasVisibleChar`）。
/// 这是一条完成要变成一条消息**必须**过的判据：客户端渲染成空白的正文仍然占着聊里一个
/// 气泡，而一个全是换行的完成就是这样的正文。
///
/// **不**等于"客户端会渲染出东西"，而且是**故意**的：格式类 rune（U+200B 零宽空格、
/// U+FEFF、软连字符）既不是空白也不是控制字符，所以一份只由它们组成的正文会通过这里、
/// 却仍然显示为空。上游也不拒这种正文 —— 它以空气泡到达聊里，而这条判据不是拦住它的那道
/// 关。线画在这里是为了不把一张 Unicode 分类表搬进 adapter（上游注释逐字）。
#[must_use]
pub fn has_visible_char(text: &str) -> bool {
    text.chars()
        .any(|character| !character.is_whitespace() && !character.is_control())
}

/// 一条 `aibot_send_msg` markdown body 的上限：与流帧一样的 20480 utf8 字节
/// （<https://developer.work.weixin.qq.com/document/path/101138>）。
///
/// 超限的 body 被**整条**拒绝（服务端不裁），拒绝以 ack 上的 `errcode 45002` 到达 ——
/// 所以在 [`split_for_wire`] 之前，一条长回答就是干脆不出现在聊里。
pub const SEND_MSG_CONTENT_LIMIT: usize = 20480;

/// 把一条回答切成平台会接受的若干段；已经放得下时**原样**返回（上游 `splitForWire`）——
/// 这是绝大多数情况，所以常见路径不多分配。
///
/// **切分而非截断**才是重点：长回答是代码评审、粘贴的日志、文档草稿，尾部不是填充物 ——
/// 一条被服务端整条拒掉的回答、一条停在省略号上再也读不到后文的回答，都不是回答。切点优先
/// 取行边界、其次取 rune 边界，所以一段从不切在字符中间、也很少切在行中间。
///
/// 每段带一个标记，让读者知道回答还有下文。这是 adapter 唯一在 agent 自己的文本上加字的
/// 地方，所以标记是一个**光秃秃的计数**而不是一句话：它不属于任何语言，因此不需要翻译，
/// 也不可能与某一种语言写成的回答矛盾。
#[must_use]
pub fn split_for_wire(content: &str) -> Vec<String> {
    if content.len() <= SEND_MSG_CONTENT_LIMIT {
        return vec![content.to_owned()];
    }

    let mut pieces: Vec<String> = Vec::new();
    let mut remaining = content;
    while !remaining.is_empty() {
        // 给这一段最宽的可能标记留位置。总数要等切完才知道，所以先拿占位符顶着：
        // "…" 是三字节，够覆盖到三位数的总数 —— 远超任何能走到这里的回答。
        let marker = format!("\n\n({}/…)", pieces.len() + 1);
        let budget = SEND_MSG_CONTENT_LIMIT - marker.len();
        if remaining.len() <= SEND_MSG_CONTENT_LIMIT {
            pieces.push(remaining.to_owned());
            break;
        }
        let cut = wire_cut_point(remaining, budget);
        // **接缝处不丢任何东西**：切点是一个索引，它两侧都保留 —— 切点选中的那个换行属于
        // 它终止的那一段，所以把各段去掉标记拼回去，逐字节就是原回答。上游早先的版本在这里
        // 裁过行首的换行，结果是每个长到要切开的日志与代码块都被悄悄吃掉一个段落分隔。
        pieces.push(remaining[..cut].to_owned());
        remaining = &remaining[cut..];
    }

    // 没有任何可见内容的一段不发：一条以连续空行结尾的长回答会把那段空行单独切成一段
    // （最后一段不带标记，所以没有别的东西让它可见），而它到达聊里就是一个空气泡 ——
    // 正是 `has_visible_char` 在各调用点上要防的事情。丢掉它对读者零成本：丢的是本来会独占
    // 一条消息的空白。在**上标记之前**过滤，所以编号数的是人真正收到的段。
    pieces.retain(|piece| has_visible_char(piece));

    // 总数要等切完才知道，所以标记最后上。最后一段不带：它后面没有东西可以承诺，
    // 而读者自己看得见。
    let total = pieces.len();
    for (index, piece) in pieces.iter_mut().enumerate() {
        if index + 1 == total {
            continue;
        }
        // 写进 `String` 不会失败（`fmt::Write for String` 的 `write_str` 是 infallible）。
        let _ = write!(piece, "\n\n({}/{})", index + 1, total);
    }
    pieces
}

/// 选一段的结束位置：预算内最后一个**值得用**的换行，否则最后一个 rune 边界
/// （上游 `wireCutPoint`）。
///
/// ⚠️ **上游 `s[:budget]` 是字节切片**：Go 允许在任何字节处切一个 string（哪怕那个位置把
/// 一个 rune 劈成两半），而它只拿这个前缀做一次 `LastIndexByte`。Rust 的 `&str` 切片
/// **必须**落在字符边界上，否则 panic ⇒ 这里先往回到一个安全的前缀再找换行。
/// 语义不变：找换行只看**预算之内**的字节，而回退最多退到那个 rune 的开头。
#[must_use]
pub fn wire_cut_point(text: &str, budget: usize) -> usize {
    if budget >= text.len() {
        return text.len();
    }
    let mut head = budget;
    while head > 0 && !text.is_char_boundary(head) {
        head -= 1;
    }
    // 落在预算**后四分之一**里的换行才值得取；靠近开头的那个会浪费掉大半帧。
    // 切点在它**之后**，所以那个换行留在它终止的那一段末尾，而不是掉进两帧的缝里。
    if let Some(newline) = text[..head].rfind('\n') {
        if newline > budget * 3 / 4 {
            return newline + 1;
        }
    }
    let mut cut = budget;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    if cut == 0 {
        // 预算比一个 rune 还窄：往**上**找一个边界。
        //
        // 上游这里返回 `budget` —— 在 Go 里合法（字节切片，最多产出一段非法 UTF-8，而
        // 调用方只是把它拼回去）。在 Rust 里那是一个 panic，而返回 0 会让
        // [`split_for_wire`] 死循环 ⇒ 唯一的合法答案是多要一个 rune：
        // 一段比上限多三字节的正文仍然是一段**能被服务端接受**的正文
        // （而 `budget` 在生产里恒为 20472）。
        let mut up = budget;
        while up < text.len() && !text.is_char_boundary(up) {
            up += 1;
        }
        return up;
    }
    cut
}

#[cfg(test)]
mod tests;
