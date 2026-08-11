//! WebSocket 桥接层（原 `pc-ws` crate 已下沉），把 pc-realtime 事件流暴露给客户端。
//!
//! 路由：`GET /api/live-events` （upgrade to WebSocket）
//! 协议：客户端发 `{type:"subscribe", company_id?}` 后收到 JSON LiveEvent 流。

use std::sync::Arc;

use crate::{LiveEvent, RealtimeHandle};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{info, warn};
use uuid::Uuid;

/// 客户端 → 服务端控制帧。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientFrame {
    /// 订阅（可选按 company_id 过滤）
    Subscribe {
        #[serde(default)]
        company_id: Option<Uuid>,
    },
    /// 心跳
    Ping,
}

/// 服务端 → 客户端帧。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerFrame<'a> {
    Welcome { client_id: Uuid, server: &'a str },
    Event { event: &'a LiveEvent },
    Pong,
    Error { message: String },
}

pub async fn handler<S>(
    ws: WebSocketUpgrade,
    State(state): State<Arc<WsState>>,
) -> impl IntoResponse
where
    S: Send + Sync + 'static,
{
    let _ = std::marker::PhantomData::<S>;
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

#[derive(Clone)]
pub struct WsState {
    pub realtime: RealtimeHandle,
    pub server_name: String,
}

async fn handle_socket(socket: WebSocket, state: Arc<WsState>) {
    let client_id = Uuid::new_v4();
    info!(%client_id, "ws connected");

    let (mut sender, mut receiver) = socket.split();
    let mut rx = state.realtime.subscribe();

    // 1. 发送 Welcome
    let welcome = ServerFrame::Welcome {
        client_id,
        server: &state.server_name,
    };
    if let Ok(s) = serde_json::to_string(&welcome) {
        if sender.send(Message::Text(s)).await.is_err() {
            return;
        }
    }

    // 2. 主循环：要么从 rx 收事件，要么从 client 收控制帧
    let mut company_filter: Option<Uuid> = None;
    loop {
        tokio::select! {
            // 推事件
            evt = rx.recv() => {
                match evt {
                    Ok(arc_evt) => {
                        if let Some(cid) = company_filter {
                            if arc_evt.company_id != Some(cid) {
                                continue;
                            }
                        }
                        let frame = ServerFrame::Event { event: &arc_evt };
                        match serde_json::to_string(&frame) {
                            Ok(s) => {
                                if sender.send(Message::Text(s)).await.is_err() { break; }
                            }
                            Err(e) => {
                                warn!(error=%e, "ws serialize failed");
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!(%client_id, skipped = n, "ws lagged");
                        let _ = sender.send(Message::Text(
                            json!({"type":"error","message":format!("lagged {n}")}).to_string()
                        )).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            // 收控制帧
            ctrl = receiver.next() => {
                match ctrl {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ClientFrame>(&text) {
                            Ok(ClientFrame::Subscribe { company_id }) => {
                                company_filter = company_id;
                                let _ = sender.send(Message::Text(
                                    json!({"type":"ack","filter":company_id}).to_string()
                                )).await;
                            }
                            Ok(ClientFrame::Ping) => {
                                let _ = sender.send(Message::Text(
                                    serde_json::to_string(&ServerFrame::Pong).unwrap()
                                )).await;
                            }
                            Err(e) => {
                                let _ = sender.send(Message::Text(
                                    serde_json::to_string(&ServerFrame::Error { message: e.to_string() }).unwrap()
                                )).await;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(p))) => {
                        let _ = sender.send(Message::Pong(p)).await;
                    }
                    Some(Err(e)) => {
                        warn!(error=%e, "ws client error");
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
    info!(%client_id, "ws disconnected");
}
