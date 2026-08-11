//! `GET /api/companies/:company_id/events/ws` 公司范围 WebSocket 路由。
//!
//! R576：补齐 Node 上游 UI client 真实调用但 Rust 端未实现的路径。
//!
//! 与 `/api/live-events` 的区别：
//! - 路径含 `:company_id`，服务器**强制**按公司过滤事件
//! - 不需要客户端发 `subscribe { company_id }` 帧（路径即订阅）
//! - 鉴权失败时直接 401 close，不进入 WS upgrade
//!
//! 设计：
//! - **路径 = scope**: `:company_id` 在 URL 里，避免客户端漏发 subscribe 帧
//! - **强制公司隔离**: 事件的 `company_id` 与路径不匹配时直接丢弃
//! - **复用 realtime bus**: 与 `live_events` 共用 `pc_realtime::WsState` /
//!   `RealtimeHandle`，无独立事件流

#![forbid(unsafe_code)]

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tracing::{info, warn};
use uuid::Uuid;

use crate::routes::live_events::authorize_ws;
use crate::AppState;
use pc_realtime::{LiveEvent, WsState};

pub fn router() -> axum::Router<AppState> {
    axum::Router::new().route("/api/companies/:company_id/events/ws", get(handler))
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WsQuery {
    /// 鉴权 token
    /// Internal field — public for cross-module testing（API key / session cookie token）。
    #[serde(default)]
    pub token: Option<String>,
    /// 重连 resume 起点：客户端上一次收到的 event_id。
    #[serde(default)]
    pub resume: Option<u64>,
}

async fn handler(
    Path(company_id): Path<Uuid>,
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<WsQuery>,
) -> impl IntoResponse {
    use axum::http::StatusCode;

    // 1. 鉴权（复用 live_events 的 authorize_ws）
    let authorized = match authorize_ws(&state, query.token.as_deref(), Some(company_id)).await {
        Ok(true) => true,
        Ok(false) => {
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(json!({"error": "unauthorized", "companyId": company_id})),
            )
                .into_response();
        }
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(json!({"error": error.to_string()})),
            )
                .into_response();
        }
    };
    if !authorized {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }

    let ws_state = state.ws.clone();
    let resume_from = query.resume;

    ws.on_upgrade(move |socket| handle_socket(socket, ws_state, company_id, resume_from))
}

/// WebSocket 主循环：订阅 realtime bus，过滤为指定 company_id。
async fn handle_socket(
    socket: WebSocket,
    ws_state: Arc<WsState>,
    company_id: Uuid,
    resume_from: Option<u64>,
) {
    let client_id = Uuid::new_v4();
    info!(%client_id, company_id = %company_id, ?resume_from, "company events ws connected");

    let (mut sender, mut receiver) = socket.split();

    // 订阅 realtime bus（带可选 resume 重放）
    let (replay, mut subscriber_rx) = match resume_from {
        Some(from_id) => ws_state.realtime.subscribe_with_resume(from_id),
        None => (Vec::new(), ws_state.realtime.subscribe()),
    };

    // 先发送 replay 事件（仅保留 company_id 匹配的）
    let mut replay_sent = 0usize;
    for event in replay {
        if event_company_id_matches(&event, company_id) {
            if let Ok(text) = serde_json::to_string(&event) {
                if sender.send(Message::Text(text)).await.is_err() {
                    warn!(%client_id, "ws send failed during replay; closing");
                    return;
                }
                replay_sent += 1;
            }
        }
    }
    if resume_from.is_some() {
        info!(%client_id, replayed = replay_sent, "company events ws resume complete");
    }

    // 主循环：读 WS 帧（ping/pong）+ 推送实时事件
    loop {
        tokio::select! {
            // 客户端帧（用于 keepalive ping）
            ws_frame = receiver.next() => {
                match ws_frame {
                    Some(Ok(Message::Close(_))) | None => {
                        info!(%client_id, "company events ws client closed");
                        break;
                    }
                    Some(Ok(Message::Ping(bytes))) => {
                        if sender.send(Message::Pong(bytes)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Text(_))) | Some(Ok(Message::Binary(_))) => {
                        // 客户端消息协议：仅支持 ping/pong；其它静默忽略
                    }
                    Some(Err(e)) => {
                        warn!(%client_id, error = %e, "ws client error");
                        break;
                    }
                    _ => {}
                }
            }
            // 服务器事件
            event = subscriber_rx.recv() => {
                match event {
                    Ok(live_event) => {
                        if !event_company_id_matches(&live_event, company_id) {
                            continue; // 公司不匹配，丢弃
                        }
                        match serde_json::to_string(&live_event) {
                            Ok(text) => {
                                if sender.send(Message::Text(text)).await.is_err() {
                                    break;
                                }
                            }
                            Err(error) => {
                                warn!(%client_id, ?error, "event serialize failed");
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // 客户端处理太慢，跳过中间事件
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        // 通道关闭
                        break;
                    }
                }
            }
        }
    }

    info!(%client_id, "company events ws disconnected");
}

/// 提取 LiveEvent 的 company_id 用于过滤。
///
/// LiveEvent 直接带 `company_id: Option<Uuid>` 字段（不是嵌在 payload 里）。
/// 过滤规则：路径的 company_id == event.company_id。
fn event_company_id_matches(event: &LiveEvent, company_id: Uuid) -> bool {
    event.company_id == Some(company_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r576_match_when_company_id_equals() {
        let cid = Uuid::new_v4();
        let event = LiveEvent::new("test.event", "test", Uuid::nil()).with_company(cid);
        assert!(event_company_id_matches(&event, cid));
    }

    #[test]
    fn r576_mismatch_when_company_id_differs() {
        let cid = Uuid::new_v4();
        let other = Uuid::new_v4();
        let event = LiveEvent::new("test.event", "test", Uuid::nil()).with_company(other);
        assert!(!event_company_id_matches(&event, cid));
    }

    #[test]
    fn r576_mismatch_when_company_id_missing() {
        let cid = Uuid::new_v4();
        let event = LiveEvent::new("test.event", "test", Uuid::nil());
        // event has no company_id set
        assert!(!event_company_id_matches(&event, cid));
    }

    #[test]
    fn r576_router_exposes_path() {
        let _r = router();
    }
}
