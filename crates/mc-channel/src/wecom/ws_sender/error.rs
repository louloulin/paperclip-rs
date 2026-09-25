//! 发送侧的失败分类（上游 `ws_sender.go` 的一组哨兵错误 + `wecomAPIError` + `streamError`）。
//!
//! 本文件是 `ws_sender.rs` 的子模块：拆分的理由是 `docs/60-M7-PLAN.md` §6.3 要求把
//! 1,187 行的上游 `ws_frame.go` 按「帧编解码 / 帧路由」拆开，加上门 ⑩ 的 800 行硬限。
//! 逐条清单见 `docs/32` §33 的 D10。

use super::SinkError;
use crate::wecom::ws_frame::{FrameError, StreamError};

// =====================================================================
// 错误
// =====================================================================

/// 发送侧的失败（上游 `ws_sender.go` 的一整组哨兵错误 + `wecomAPIError` +
/// `streamError`）。
///
/// 变体名与上游哨兵的**一一对应**写在每个变体的文档里；`is_not_attempted` / `unusable`
/// 是两个分类器（上游 `provablyNotSent` / `streamUnusable` 各自要问的那**一个**问题）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SenderError {
    /// 上游 `errNotAttempted`：这次发送在本进程**一个字节都没出去**之前就结束了。
    /// 这是这条路径上唯一一个"确定没投递"的记号。
    #[error("wecom: nothing was written")]
    NotAttempted,

    /// 上游 `errChatBusy`：等这个聊的轮次时预算先到了，且**一个字节**都没出去。
    #[error("wecom: nothing was written; the wait for this chat's turn ended first")]
    ChatBusy,

    /// 上游 `errAckTimeout`：帧出去了，判决没回来。**与拒绝不同** —— 消息很可能已经投递。
    #[error("wecom: timed out waiting for the server verdict")]
    AckTimeout,

    /// 上游 `errAckAbandoned`：帧出去了，调用方的预算在判决回来之前用完了。
    #[error("wecom: the wait for the verdict was cut short after the frame went out ({cause})")]
    AckAbandoned { cause: String },

    /// 上游 `errWriteAttempted`：失败由 socket 写**自身**引发，而不是写之前的某一步。
    /// 过了这一点就不再是"没投递"的证明。
    #[error("wecom: frame write attempted ({cause})")]
    WriteAttempted { cause: String },

    /// 上游 `errPartiallySent`：一条长回答的**后一段**在**前一段已被服务端接受**之后失败。
    #[error("wecom: an earlier piece of this answer was already accepted ({cause})")]
    PartiallySent { cause: String },

    /// 上游 `errStreamBusy`：上一帧还没 ack，所以这一帧（非收尾）让了。
    /// 这就是官方 SDK 说的 `replyStreamNonBlocking`：非收尾帧让位。
    #[error("wecom: previous stream frame still unacked")]
    StreamBusy,

    /// 上游 `errStreamAckTimeout`：流帧出去了、判决没回来。
    #[error("wecom: stream frame ack timed out")]
    StreamAckTimeout,

    /// 上游 `errStreamSuperseded`：非收尾帧在收尾帧封住流之后才到达写者，被拒。
    #[error("wecom: stream frame superseded by the closing frame")]
    StreamSuperseded,

    /// 上游 `wecomAPIError`：服务端**说出来了**的拒绝。带 `errcode` 而不是一句话，
    /// 调用方才能区分永久拒绝（坏帧、bot 被移出聊）与瞬时拒绝（限流）。
    #[error("wecom: {cmd} rejected errcode={code} errmsg={message}")]
    Api {
        cmd: String,
        code: i32,
        message: String,
    },

    /// 上游 `streamError`（服务端拒绝了一个流帧）。
    #[error(transparent)]
    Stream(#[from] StreamError),

    /// 帧编解码失败（本仓新增的显式一层，上游是 `json.Marshal` 的裸错误）。
    #[error("wecom: {0}")]
    Frame(#[from] FrameError),

    /// 帧/内容超出上限。
    #[error("wecom: frame of {len} bytes exceeds the {limit} byte cap")]
    FrameTooLarge { len: usize, limit: usize },

    /// body 构造失败（上游 `sendMsgTextBody` / `respondStreamBody` 的 `errors.New`）。
    #[error("wecom: {0}")]
    Body(#[from] crate::wecom::ws_frame::BodyError),

    /// 流帧没有回显回调的 `req_id`（上游 `respondStreamFrame` 的第一道检查）。
    #[error("wecom: stream frame requires the callback req_id")]
    MissingCallbackReqId,

    /// 这个 `req_id` 已经被另一个等待者占着（上游 `awaitReply` 的 `false`）。
    #[error("wecom: {cmd} req_id {req_id} is already awaiting a response")]
    ReqIdTaken { cmd: String, req_id: String },

    /// socket 层失败（已按 [`SinkFailure`] 分类）。
    #[error("wecom: {0}")]
    Sink(#[from] SinkError),
}

impl SenderError {
    /// 上游 `provablyNotSent` 要问的**那一个问题**：这次发送确定没把任何字节交给对端吗
    /// （`relay_outbound.go:1519`）。
    ///
    /// 上游那张表列的是**不**可证明的那几条，其余一建 `true`（含 `default: true`）；
    /// Rust 的枚举是封闭的，所以这里把同一张表翻成存在否分支 —— **不在列表里的就是 `true`**，
    /// 逐条对应如下：
    ///
    /// | 上游 | 本变体 | 结果 |
    /// | --- | --- | :-: |
    /// | `errNotAttempted`（含 `errChatBusy`） | [`Self::NotAttempted`] / [`Self::ChatBusy`] | true |
    /// | `errStreamBusy` | [`Self::StreamBusy`] | **true** |
    /// | `errStreamSuperseded`（走 default） | [`Self::StreamSuperseded`] | true |
    /// | `wecomAPIError` | [`Self::Api`] | false |
    /// | `errAckTimeout` | [`Self::AckTimeout`] | false |
    /// | `errStreamAckTimeout` | [`Self::StreamAckTimeout`] | false |
    /// | `errAckAbandoned`（包 ctx） | [`Self::AckAbandoned`] | false |
    /// | `errPartiallySent` | [`Self::PartiallySent`] | false |
    /// | `errWriteAttempted` | [`Self::WriteAttempted`] | false |
    /// | `streamError`（服务端判决） | [`Self::Stream`] | false |
    /// | 其余 / default | `Body` / `Frame` / `FrameTooLarge` / `MissingCallbackReqId` / `ReqIdTaken` / `Sink(BeforeWrite)` | true |
    ///
    /// 两个容易看错的格子：
    ///
    /// - [`Self::StreamBusy`] 是 **true** —— 门把帧拦下了，一个字节都没写，
    ///   所以退回普通消息是免费的（上游逐字）；
    /// - [`Self::AckAbandoned`] 是 **false** —— 它看上与 [`Self::NotAttempted`] 一样都是
    ///   "预算用完了"，但事实相反：帧已上 socket。上游 2026-09-03 之前就是在这里读错的
    ///   （把它当成 `errNotAttempted`）⇒ 一条从未发出的消息被当成"结果未知"，
    ///   而那是**唯一**不该重发的结局。
    #[must_use]
    pub fn is_not_attempted(&self) -> bool {
        !matches!(
            self,
            Self::Api { .. }
                | Self::Stream(_)
                | Self::AckTimeout
                | Self::StreamAckTimeout
                | Self::AckAbandoned { .. }
                | Self::PartiallySent { .. }
                | Self::WriteAttempted { .. }
        )
    }

    /// 上游 `streamUnusable`：**只有服务端的判决**算数。写失败、ack 没来、
    /// 注册表里没有这把 socket —— 都不算，因为它们都没有说这条流的事。
    #[must_use]
    pub fn stream_unusable(&self) -> bool {
        matches!(self, Self::Stream(error) if error.unusable())
    }
}
