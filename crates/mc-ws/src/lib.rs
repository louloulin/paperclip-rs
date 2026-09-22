//! Multica WebSocket handler：`/live-events` 通道。
//!
//! 协议：
//! - server → client: `EventEnvelope` JSON
//! - client → server: `{ "type": "ping" }` / `{ "type": "resume", "last_event_id": "..." }`
//!
//! 与 multica `server/internal/daemonws/*` + `apps/web/.../live-events.ts` 等价。

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
