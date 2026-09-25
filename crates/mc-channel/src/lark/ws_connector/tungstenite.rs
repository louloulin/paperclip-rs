//! `tokio-tungstenite` 承载的**生产传输**（上游那份手写 `gorilla` 拨号器 + `WSConn` 适配的等价物）。
//!
//! - **写者**：M7-11（`docs/60-M7-PLAN.md` §3.3；本片的写集勘误见 `docs/32` §28）。
//! - 拆出来是**门 ⑩**（800 行硬限）与"唯一碰第三方 WS 库的地方"两条要求：帧循环与端口
//!   （[`super::WsConnection`] / [`super::WsDialer`]）都只看端口，用例因此不开真 socket。
//!
//! # 两处"错误不得原样透出"（`docs/60` §2.3 第 3 条）
//!
//! 1. **拨号错误**：`connect_async` 的错误里带完整 URL，而那条 URL 自带一次性的 `device_id`
//!    （等价于凭据）⇒ 一律映射成固定文案；
//! 2. **读 / 写错误**：只报"哪一步失败"，不带帧内容（帧里是事件正文 / 用户文本）。
//!
//! # 应用层 ping vs 协议层 ping（上游逐字，别搞混）
//!
//! Lark 的心跳是**应用层**的二进制 [`Frame`]（`type=ping`），走 [`WsConnection::send_binary`]；
//! WebSocket **协议层**的 PING 会被 lark 服务端无视（上游为此专门留了注释）。
//! `tokio-tungstenite` 会自动回协议层 pong（其文档逐字），本文件因此**不**实现 `send_ping`。

use async_trait::async_trait;
use futures_util::{SinkExt as _, StreamExt as _};
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::Message;

use crate::channel::{ChannelError, ChannelResult};

use super::super::ws_endpoint::WsEndpoint;
use super::{WsConnection, WsDialer, WsEvent};

/// 生产拨号器：`tokio-tungstenite`（上游 `GorillaDialer` 的等价物）。
#[derive(Debug, Default, Clone)]
pub struct TungsteniteDialer;

impl TungsteniteDialer {
    /// 造一个（无状态）。
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl WsDialer for TungsteniteDialer {
    async fn dial(&self, endpoint: &WsEndpoint) -> ChannelResult<Box<dyn WsConnection>> {
        // 先按 URL 造握手请求（tungstenite 会补齐 `Host` / `Upgrade` / `Sec-WebSocket-*` 那几个
        // 承重头），再把端点带来的握手头并进去。
        let mut request =
            endpoint
                .url
                .as_str()
                .into_client_request()
                .map_err(|_| ChannelError::Transport {
                    message: "lark ws: the endpoint url cannot start a websocket handshake"
                        .to_string(),
                })?;
        for (name, value) in &endpoint.headers {
            request.headers_mut().insert(name.clone(), value.clone());
        }
        // 错误**不**原样透出（`connect_async` 的错里带完整 URL，而那条 URL 自带 device_id）。
        let (stream, _response) =
            tokio_tungstenite::connect_async(request)
                .await
                .map_err(|_| ChannelError::Transport {
                    message: "lark ws: websocket handshake failed".to_string(),
                })?;
        Ok(Box::new(TungsteniteConnection { stream }))
    }
}

type TungsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// `tokio-tungstenite` 承载的会话。
struct TungsteniteConnection {
    stream: TungsStream,
}

#[async_trait]
impl WsConnection for TungsteniteConnection {
    async fn next_event(&mut self) -> Option<ChannelResult<WsEvent>> {
        match self.stream.next().await? {
            Ok(Message::Binary(bytes)) => Some(Ok(WsEvent::Binary(bytes.clone()))),
            Ok(Message::Text(text)) => Some(Ok(WsEvent::Text(text.clone()))),
            Ok(Message::Ping(_)) => Some(Ok(WsEvent::Ping)),
            Ok(Message::Pong(_) | Message::Frame(_)) => Some(Ok(WsEvent::Pong)),
            Ok(Message::Close(_)) => Some(Ok(WsEvent::Closed)),
            Err(_) => Some(Err(ChannelError::Transport {
                message: "lark ws: socket read failed".to_string(),
            })),
        }
    }

    async fn send_binary(&mut self, bytes: &[u8]) -> ChannelResult<()> {
        self.stream
            .send(Message::binary(bytes.to_vec()))
            .await
            .map_err(|_| ChannelError::Transport {
                message: "lark ws: socket write failed".to_string(),
            })
    }

    async fn close(&mut self) {
        let _ = self.stream.close(None).await;
    }
}
