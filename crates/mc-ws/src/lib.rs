//! Multica WebSocket handlers。
//!
//! 两条互不相干的通道：
//!
//! - **`/live-events`**（前端实时事件，[`live_events_handler`]）：server → client 是
//!   `EventEnvelope` JSON，client → server 是 `{"type":"ping"}` /
//!   `{"type":"resume","last_event_id":"..."}`。与 multica
//!   `apps/web/.../live-events.ts` 等价。
//! - **`/api/daemon/ws`**（daemon 控制通道，[`hub`]）：双向的 daemon 协议帧
//!   （心跳、RPC、唤醒提示），协议在 [`mc_daemon_proto`]。本 crate 只提供**传输层**：
//!   连接注册表、扇出、去重、慢客户端驱逐、读/写泵。路由注册与身份鉴权在 M3-7。
//!
//! [`identity`] 描述调用方注入的连接身份，[`frames`] 描述帧与 RPC 契约类型。

mod connection;
pub mod frames;
pub mod hub;
pub mod identity;
mod pump;

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use mc_realtime::RealtimeHandle;

#[allow(clippy::unused_async)] // axum WebSocket handler 形状；本切片尚未挂载到 router。
pub async fn live_events_handler(
    ws: WebSocketUpgrade,
    State(handle): State<Arc<RealtimeHandle>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_connection(socket, handle))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Ping,
    Resume { last_event_id: Option<String> },
    Pong,
}

async fn handle_connection(socket: WebSocket, handle: Arc<RealtimeHandle>) {
    let (mut sender, mut receiver) = socket.split();
    let mut subscription = handle.subscribe();

    let send_task = tokio::spawn(async move {
        while let Some(env) = subscription.recv().await {
            let json = match serde_json::to_string(&env) {
                Ok(s) => s,
                Err(e) => {
                    warn!(error = %e, "encode envelope");
                    continue;
                }
            };
            if sender.send(Message::Text(json)).await.is_err() {
                break;
            }
        }
    });

    let recv_task = tokio::spawn(async move {
        while let Some(msg) = receiver.next().await {
            match msg {
                Ok(Message::Text(s)) => match serde_json::from_str::<ClientMessage>(&s) {
                    Ok(ClientMessage::Ping) => debug!("ws ping"),
                    Ok(ClientMessage::Resume { last_event_id }) => {
                        debug!(?last_event_id, "ws resume requested");
                    }
                    Ok(ClientMessage::Pong) => debug!("ws pong"),
                    Err(e) => warn!(error = %e, "ws bad client message"),
                },
                Ok(Message::Close(_)) => break,
                Ok(_) => {}
                Err(e) => {
                    warn!(error = %e, "ws receive error");
                    break;
                }
            }
        }
    });

    tokio::select! {
        _ = send_task => {},
        _ = recv_task => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc_realtime::EventEnvelope;

    #[test]
    fn client_message_round_trip() {
        let m = ClientMessage::Ping;
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"type\":\"ping\""));
        let back: ClientMessage = serde_json::from_str(&json).unwrap();
        matches!(back, ClientMessage::Ping);
    }

    #[test]
    fn envelope_serializable() {
        let env = EventEnvelope::new("test", "x", None, serde_json::json!({}));
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("event_id"));
    }
}
