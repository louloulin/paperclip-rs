//! `GET /api/live-events` WebSocket 路由：把 pc-realtime 的事件流桥接到客户端。

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use pc_realtime::WsState;
use serde::Deserialize;
use serde_json::json;
use tracing::{info, warn};
use uuid::Uuid;

use crate::AppState;

pub fn router() -> axum::Router<AppState> {
    axum::Router::new().route("/api/live-events", get(handler))
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientFrame {
    Subscribe {
        #[serde(default)]
        company_id: Option<Uuid>,
    },
    Ping,
}

async fn handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    let ws_state = state.ws.clone();
    ws.on_upgrade(move |socket| handle_socket(socket, ws_state))
}

async fn handle_socket(socket: WebSocket, state: Arc<WsState>) {
    let client_id = Uuid::new_v4();
    info!(%client_id, "ws connected");

    let (mut sender, mut receiver) = socket.split();
    let mut rx = state.realtime.subscribe();

    let welcome =
        json!({"type":"welcome","client_id":client_id,"server":&state.server_name}).to_string();
    if sender.send(Message::Text(welcome)).await.is_err() {
        return;
    }

    let mut company_filter: Option<Uuid> = None;
    loop {
        tokio::select! {
            evt = rx.recv() => {
                match evt {
                    Ok(arc_evt) => {
                        if let Some(cid) = company_filter {
                            if arc_evt.company_id != Some(cid) { continue; }
                        }
                        let frame = json!({"type":"event","event":&*arc_evt}).to_string();
                        if sender.send(Message::Text(frame)).await.is_err() { break; }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        let msg = json!({"type":"error","message":format!("lagged {n}")}).to_string();
                        let _ = sender.send(Message::Text(msg)).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            ctrl = receiver.next() => {
                match ctrl {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ClientFrame>(&text) {
                            Ok(ClientFrame::Subscribe { company_id }) => {
                                company_filter = company_id;
                                let ack = json!({"type":"ack","filter":company_id}).to_string();
                                let _ = sender.send(Message::Text(ack)).await;
                            }
                            Ok(ClientFrame::Ping) => {
                                let pong = json!({"type":"pong"}).to_string();
                                let _ = sender.send(Message::Text(pong)).await;
                            }
                            Err(e) => {
                                let err = json!({"type":"error","message":e.to_string()}).to_string();
                                let _ = sender.send(Message::Text(err)).await;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(p))) => { let _ = sender.send(Message::Pong(p)).await; }
                    Some(Err(e)) => { warn!(error=%e, "ws client error"); break; }
                    _ => {}
                }
            }
        }
    }
    info!(%client_id, "ws disconnected");
}
