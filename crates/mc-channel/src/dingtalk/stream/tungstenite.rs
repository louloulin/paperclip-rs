//! `tokio-tungstenite` 承载的**生产传输**（上游那份手写 `gorilla` 拨号器的等价物）。
//!
//! - **写者**：M7-7（`docs/60-M7-PLAN.md` §3.3；本片的写集勘误见 `docs/32` §19）。
//! - 拆出来是**门 ⑩**（800 行硬限）的要求：这里是**唯一**碰第三方 WS 库的地方 ——
//!   帧循环与端口（[`super::WsConnection`] / [`super::WsDialer`]）都只看端口，用例因此
//!   不开真 socket。
//!
//! # 两处"错误不得原样透出"
//!
//! 1. **拨号错误**：`connect_async` 的错误里带完整 URL，而那条 URL 自带一次性 Stream
//!    ticket（等价于凭据）⇒ 一律映射成固定文案（上游逐字）；
//! 2. **写错误**：只报"哪一步失败"，不带帧内容（帧里是用户正文）。

use async_trait::async_trait;

use crate::channel::{ChannelError, ChannelResult};

use super::{WsConnection, WsDialer, WsEvent};

/// 生产拨号器：`tokio-tungstenite`。
#[derive(Debug, Default, Clone)]
pub struct TungsteniteDialer;

#[async_trait]
impl WsDialer for TungsteniteDialer {
    async fn dial(&self, dial_url: &str) -> ChannelResult<Box<dyn WsConnection>> {
        // 错误**不**原样透出（`connect_async` 的错里带完整 URL，而那条 URL 自带票据）。
        let (stream, _response) =
            tokio_tungstenite::connect_async(dial_url)
                .await
                .map_err(|_| ChannelError::Transport {
                    message: "dingtalk stream: websocket handshake failed".to_string(),
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
        use futures_util::StreamExt as _;
        use tokio_tungstenite::tungstenite::Message;

        match self.stream.next().await? {
            Ok(Message::Text(text)) => Some(Ok(WsEvent::Text(text.clone()))),
            Ok(Message::Binary(bytes)) => Some(Ok(WsEvent::Binary(bytes.clone()))),
            Ok(Message::Ping(_)) => Some(Ok(WsEvent::Ping)),
            Ok(Message::Pong(_) | Message::Frame(_)) => Some(Ok(WsEvent::Pong)),
            Ok(Message::Close(_)) => Some(Ok(WsEvent::Closed)),
            Err(_) => Some(Err(ChannelError::Transport {
                message: "dingtalk stream: socket read failed".to_string(),
            })),
        }
    }

    async fn send_text(&mut self, text: &str) -> ChannelResult<()> {
        use futures_util::SinkExt as _;

        self.stream
            .send(tokio_tungstenite::tungstenite::Message::text(text))
            .await
            .map_err(|_| ChannelError::Transport {
                message: "dingtalk stream: socket write failed".to_string(),
            })
    }

    async fn send_ping(&mut self) -> ChannelResult<()> {
        use futures_util::SinkExt as _;

        self.stream
            .send(tokio_tungstenite::tungstenite::Message::Ping(Vec::new()))
            .await
            .map_err(|_| ChannelError::Transport {
                message: "dingtalk stream: ping write failed".to_string(),
            })
    }

    async fn close(&mut self) {
        let _ = self.stream.close(None).await;
    }
}
