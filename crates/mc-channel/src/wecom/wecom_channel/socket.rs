//! aibot 长连接的**传输接缝**：拨号端口 + 读写两半 + `tokio-tungstenite` 的生产实现。
//!
//! 拆出来是**门 ⑩**（800 行硬限）与"一格 = 一个面"的记法纪律的共同要求：本文件是 wecom
//! adapter 里**唯一**碰第三方 WS 库的地方 —— 帧循环与端口都不看库，用例因此不开真 socket
//! （`tokio-tungstenite` 在本仓**没有** `handshake` feature ⇒ 起不了真实 WS 服务端，
//! 与 `dingtalk/stream/tungstenite.rs` 同一条约束）。
//!
//! # 为什么是"两半"而不是一个对象
//!
//! `WsSender`（M7-16）**拥有**写侧（[`WsSink`]）并把它串行化；而读循环是**另一个**任务
//! （上游也是：`gorilla` 的 `ReadMessage` 在一个 goroutine 上，写侧共用一个互斥量）。
//! 一个 `WebSocketStream` 同时是 `Stream` 与 `Sink` ⇒ 拨号后 [`futures_util::StreamExt::split`]
//! 成两半，各自装进本文件的端口。
//!
//! # 错误不得原样透出（凭据纪律，`docs/60` §2.3）
//!
//! 拨号错误里带完整 URL，而那条 URL 是**部署配置**（`wss://openws.work.weixin.qq.com`，不含
//! 一次性票据 —— 与 dingtalk 的 Stream 不同）；即便如此，`tokio-tungstenite` 的错误类型会把它
//! 的**内部形态**（`MaybeTlsStream`、握手响应）也拼进 `Display`，而那些字节不该进日志 ⇒
//! 一律映射成固定文案。写侧同理：失败只报"哪一步"，不带帧内容（帧里是用户正文与 `secret`）。

use std::time::Instant;

use async_trait::async_trait;

use crate::channel::{ChannelError, ChannelResult};
use crate::wecom::ws_sender::{SinkError, WsSink};

/// 一帧从对端读回来的东西（上游 `conn.ReadMessage` 的两个返回值收成一个）。
///
/// 本仓只需要"一段字节"：aibot 的每一帧都是 JSON 文本帧，二进制帧同样当作 JSON 解
/// （上游对两者一视同仁：`typ != TextMessage && typ != BinaryMessage` 才跳过）。
pub type InboundFrame = Vec<u8>;

/// **读**侧（上游 `*gorilla.Conn` 的 `ReadMessage` 那一半）。
///
/// `&mut self`：读与写分属两个任务，各自独占自己那一半，所以这里不需要内部锁。
#[async_trait]
pub trait WsReader: Send {
    /// 取下一帧。
    ///
    /// - `Ok(Some(bytes))` = 一帧文本 / 二进制；
    /// - `Ok(None)` = 对端正常关闭（链路结束，**不是**错误）；
    /// - `Err(_)` = 链路层失败（supervisor 按"这次尝试失败"退避重连）。
    ///
    /// **读截止时刻不在端口上**：调用方（读循环）用 `tokio::time::timeout_at` 包住这一次读，
    /// 于是"静默 90 秒 = socket 死了"这条判据只有**一处**，也不必要求每个替身都实现它。
    ///
    /// # Errors
    ///
    /// 链路层失败（**不带**凭据、不带帧内容）。
    async fn next_message(&mut self) -> ChannelResult<Option<InboundFrame>>;

    /// 关掉 socket（幂等）。
    async fn close(&mut self);
}

/// 一次拨号的结果：同一把 socket 的读写两半。
pub struct DialedConnection {
    /// 写侧（交给 `WsSender`）。
    pub sink: Box<dyn WsSink>,
    /// 读侧（交给读循环）。
    pub reader: Box<dyn WsReader>,
}

impl std::fmt::Debug for DialedConnection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DialedConnection")
            .finish_non_exhaustive()
    }
}

/// 拨号端口（上游 `Dialer`）。
///
/// 做成端口是为了让用例把 `httptest` 换成**内存替身**（上游注释逐字：`tests point it at an
/// httptest server`），本仓更进一步：本仓连真 WS 服务端都起不了（没有 `handshake` feature）
/// ⇒ 替身是唯一可行的那条路。
///
/// # Errors
///
/// [`ChannelError::Transport`]（**固定文案**，不回显 URL 或库的内部形态）。
#[async_trait]
pub trait WsDialer: Send + Sync {
    async fn dial(&self, url: &str) -> ChannelResult<DialedConnection>;
}

/// 生产拨号器：`tokio-tungstenite`。
#[derive(Debug, Default, Clone, Copy)]
pub struct TungsteniteDialer;

#[async_trait]
impl WsDialer for TungsteniteDialer {
    async fn dial(&self, url: &str) -> ChannelResult<DialedConnection> {
        // 错误**不**原样透出（`connect_async` 的错里带完整 URL 与库的内部形态）。
        let (stream, _response) =
            tokio_tungstenite::connect_async(url)
                .await
                .map_err(|_| ChannelError::Transport {
                    message: "wecom: websocket handshake failed".to_string(),
                })?;
        let (sink, reader) = futures_util::StreamExt::split(stream);
        Ok(DialedConnection {
            sink: Box::new(TungsteniteSink { sink }),
            reader: Box::new(TungsteniteReader { reader }),
        })
    }
}

type TungsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type TungsSink =
    futures_util::stream::SplitSink<TungsStream, tokio_tungstenite::tungstenite::Message>;
type TungsReader = futures_util::stream::SplitStream<TungsStream>;

/// `tokio-tungstenite` 的写半。
struct TungsteniteSink {
    sink: TungsSink,
}

#[async_trait]
impl WsSink for TungsteniteSink {
    async fn write_text(&mut self, payload: &[u8], deadline: Instant) -> Result<(), SinkError> {
        use futures_util::SinkExt as _;

        // aibot 的每一帧都是 JSON **文本帧**（上游用 `gorilla` 的 `TextMessage` 写）。
        // 编出来的字节不是合法 UTF-8 ⇒ 这是**编码前**就存在的问题，所以归"确定没发出"。
        let Ok(text) = std::str::from_utf8(payload) else {
            return Err(SinkError::before_write("frame is not valid utf-8"));
        };
        let message = tokio_tungstenite::tungstenite::Message::text(text);
        match tokio::time::timeout_at(deadline.into(), self.sink.send(message)).await {
            // 已经进了 `send` ⇒ 对端**可能**已经拿到字节（上游 `writeAttempted` 的语义）。
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(SinkError::write_attempted("socket write failed")),
            // 超时同样发生在 `send` 之内 ⇒ 不能声称"没发出去"。
            Err(_) => Err(SinkError::write_attempted("socket write timed out")),
        }
    }

    async fn close(&mut self) -> Result<(), SinkError> {
        use futures_util::SinkExt as _;

        self.sink
            .close()
            .await
            .map_err(|_| SinkError::write_attempted("socket close failed"))
    }
}

/// `tokio-tungstenite` 的读半。
struct TungsteniteReader {
    reader: TungsReader,
}

#[async_trait]
impl WsReader for TungsteniteReader {
    async fn next_message(&mut self) -> ChannelResult<Option<InboundFrame>> {
        use futures_util::StreamExt as _;
        use tokio_tungstenite::tungstenite::Message;

        let Some(message) = self.reader.next().await else {
            return Ok(None);
        };
        match message {
            Ok(Message::Text(text)) => Ok(Some(text.as_bytes().to_vec())),
            Ok(Message::Binary(bytes)) => Ok(Some(bytes.clone())),
            // `tokio-tungstenite` 自动回 pong（其文档逐字），本层只当它不存在；aibot 读循环
            // 也不 dispatch 它。
            Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => Ok(Some(Vec::new())),
            Ok(Message::Close(_)) => Ok(None),
            Err(_) => Err(ChannelError::Transport {
                message: "wecom: socket read failed".to_string(),
            }),
        }
    }

    async fn close(&mut self) {
        // 读半**没有**自己的关闭动作：关 socket 是写半（`WsSink::close`）的事，而
        // `SplitStream` 上没有 `close`。把这条留成显式的 no-op，而不是让调用方以为它做了什么事。
    }
}

/// 一个**已经记不起**任何东西的拨号器：`None` ⇒ 该安装整体不装配（见模块文档）。
///
/// 它存在的理由与 [`crate::wecom::replier::AdjacencyBreaker`] 同款：让"没接拨号器"这件事有
/// 一个**显式**的值，而不是一个 `todo!()`。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoDialer;

#[async_trait]
impl WsDialer for NoDialer {
    async fn dial(&self, _url: &str) -> ChannelResult<DialedConnection> {
        Err(ChannelError::InvalidConfig {
            kind: "wecom".to_string(),
            reason: "no websocket dialer configured".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 没接拨号器时**显式失败**，而且不回显 URL（错误路径不回显部署配置）。
    #[tokio::test]
    async fn the_no_dialer_refuses_without_echoing_anything() {
        let error = NoDialer
            .dial("wss://openws.work.weixin.qq.com")
            .await
            .expect_err("NoDialer 必须拒绝");
        assert_eq!(error.code(), "channel_invalid_config");
        assert!(!error.to_string().contains("openws"), "{error}");
    }
}
