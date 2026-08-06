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

use crate::routes::realtime_stream;
use crate::AppState;
use pc_repos::agent::AgentRepo;
use pc_repos::company_member::CompanyMemberRepo;

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

#[derive(Debug, Deserialize)]
struct AuthQuery {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    company_id: Option<Uuid>,
    /// 重连 resume 起点：客户端上一次收到的 event_id。
    /// 服务器会先重放 resume_buffer 中 event_id > resume 的事件，再切换到实时广播。
    #[serde(default)]
    resume: Option<u64>,
    /// R256: 仅订阅 `at >= since` 的事件（ISO8601 / RFC3339 时间戳）。
    #[serde(default)]
    since: Option<chrono::DateTime<chrono::Utc>>,
    /// R256: 仅订阅 `at <= until` 的事件（ISO8601 / RFC3339 时间戳）。
    #[serde(default)]
    until: Option<chrono::DateTime<chrono::Utc>>,
}

async fn handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(query): axum::extract::Query<AuthQuery>,
) -> impl IntoResponse {
    use axum::http::StatusCode;
    let token = query.token;
    let company_id = query.company_id;
    let authorized = match authorize_ws(&state, token.as_deref(), company_id).await {
        Ok(true) => true,
        Ok(false) => {
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(json!({"error": "unauthorized"})),
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

    // R255: rate limit + connection count limit
    let ip_rate_limiter = std::sync::Arc::clone(&state.ws.ip_rate_limiter);
    let connection_limiter = std::sync::Arc::clone(&state.ws.connection_limiter);
    if let Some(ip) = realtime_stream::extract_client_ip(&headers) {
        if !ip_rate_limiter.try_acquire(ip, 1) {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                axum::Json(json!({
                    "error": "rate_limited",
                    "detail": "too many connections from your IP"
                })),
            )
                .into_response();
        }
    }
    let connection_guard = if let Some(cid) = company_id {
        match connection_limiter.try_acquire(cid) {
            Some(g) => g,
            None => {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    axum::Json(json!({
                        "error": "connection_limit",
                        "detail": "too many connections for this company"
                    })),
                )
                    .into_response();
            }
        }
    } else {
        // 没传 company_id 时不占用 slot；用临时 guard 占位（never 类型无法构造）
        // 我们用 `connection_limiter.try_acquire(Uuid::nil())` 占位，连接关闭时自动释放
        match connection_limiter.try_acquire(uuid::Uuid::nil()) {
            Some(g) => g,
            None => {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    axum::Json(json!({"error": "connection_limit"})),
                )
                    .into_response();
            }
        }
    };
    let ws_state = state.ws.clone();
    let resume_from = query.resume;
    let since = query.since;
    let until = query.until;
    ws.on_upgrade(move |socket| {
        // 把 guard move 进 task；guard 在 socket drop 时自动 release。
        let _guard = connection_guard;
        handle_socket(socket, ws_state, company_id, resume_from, since, until)
    })
}

/// Authorize a WebSocket upgrade. Mirrors Node `authorizeUpgrade` in
/// `services/realtime/live-events-ws.ts`. Returns `true` when the request
/// carries a valid agent API key matching the requested company, the
/// request is a logged-in user with a session, or the server is in
/// `local_trusted` mode (anonymous board context).
pub(super) async fn authorize_ws(
    state: &AppState,
    token: Option<&str>,
    company_id: Option<Uuid>,
) -> Result<bool, String> {
    // Mirror Node `DEPLOYMENT_MODE === "local_trusted"` check.
    let local_trusted = matches!(
        std::env::var("PAPERCLIP_DEPLOYMENT_MODE").as_deref(),
        Ok("local_trusted") | Ok("local-trusted")
    );
    let token = token.map(str::trim).filter(|value| !value.is_empty());
    match (token, local_trusted) {
        (Some(token), _) => {
            // Hash the token and look up the agent API key.
            // Mirrors Node `live-events-ws.ts`: it queries `agentApiKeys` joined
            // with `companyMemberships` / `instanceUserRoles` to resolve board vs.
            // agent context. `agent_api_keys` is the agent-scoped table (with
            // `company_id`) and is the correct analog here.
            let token_hash = pc_auth::hash_token(token);
            let row = AgentRepo::new(&state.db)
                .find_api_key_id_company_by_hash(&token_hash)
                .await
                .map_err(|err| err.to_string())?;
            if let Some((_, key_company_id)) = row {
                if let Some(requested) = company_id {
                    if key_company_id != requested {
                        return Ok(false);
                    }
                }
                Ok(true)
            } else {
                // Fall back to session token
                if let Some((user_id, _)) = pc_auth::resolve_session(&state.db, token)
                    .await
                    .map_err(|err| err.to_string())?
                {
                    if let Some(requested) = company_id {
                        Ok(CompanyMemberRepo::new(&state.db)
                            .is_active_member(&user_id, requested)
                            .await
                            .map_err(|err| err.to_string())?)
                    } else {
                        Ok(true)
                    }
                } else {
                    Ok(false)
                }
            }
        }
        (None, true) => Ok(true),
        (None, false) => Ok(false),
    }
}

fn parse_bearer_token(raw: Option<&str>) -> Option<String> {
    let value = raw?.trim();
    let lower = value.to_ascii_lowercase();
    let token = lower
        .strip_prefix("bearer")
        .map(|rest| rest.trim_start())
        .unwrap_or(value);
    if token.is_empty() {
        None
    } else {
        Some(token.to_owned())
    }
}

async fn handle_socket(
    socket: WebSocket,
    state: Arc<WsState>,
    initial_company_id: Option<Uuid>,
    resume_from: Option<u64>,
    since: Option<chrono::DateTime<chrono::Utc>>,
    until: Option<chrono::DateTime<chrono::Utc>>,
) {
    let client_id = Uuid::new_v4();
    info!(%client_id, ?initial_company_id, ?resume_from, "ws connected");

    let (mut sender, mut receiver) = socket.split();

    // 重连 resume：先把 resume_buffer 中 event_id > resume 的事件重放给客户端。
    // R257: 同时应用 since / until 时间窗口过滤。
    // 再切换到 broadcast 订阅。
    let (mut rx, replayed) = match resume_from {
        Some(last_id) => {
            let (replay, rx) = state.realtime.subscribe_with_resume(last_id);
            let mut count: usize = 0;
            for arc_evt in replay {
                if let Some(cid) = initial_company_id {
                    if arc_evt.company_id != Some(cid) {
                        continue;
                    }
                }
                if let Some(since_ts) = since {
                    if arc_evt.at < since_ts {
                        continue;
                    }
                }
                if let Some(until_ts) = until {
                    if arc_evt.at > until_ts {
                        continue;
                    }
                }
                let frame = json!({"type":"event","event":&*arc_evt}).to_string();
                if sender.send(Message::Text(frame)).await.is_err() {
                    return;
                }
                count += 1;
            }
            // 通知客户端 resume 边界（实际回放数 = 经过时间窗口过滤后）
            let ack =
                json!({"type":"resumed","last_event_id": last_id,"replayed":count}).to_string();
            if sender.send(Message::Text(ack)).await.is_err() {
                return;
            }
            (rx, count)
        }
        None => {
            let rx = state.realtime.subscribe();
            (rx, 0)
        }
    };
    info!(%client_id, replayed, "ws resume complete");

    let welcome = json!({
        "type":"welcome",
        "client_id":client_id,
        "server":&state.server_name,
        "next_event_id": state.realtime.next_event_id(),
    })
    .to_string();
    if sender.send(Message::Text(welcome)).await.is_err() {
        return;
    }

    let mut company_filter: Option<Uuid> = initial_company_id;
    loop {
        tokio::select! {
            evt = rx.recv() => {
                match evt {
                    Ok(arc_evt) => {
                        if let Some(cid) = company_filter {
                            if arc_evt.company_id != Some(cid) { continue; }
                        }
                        if let Some(since_ts) = since {
                            if arc_evt.at < since_ts { continue; }
                        }
                        if let Some(until_ts) = until {
                            if arc_evt.at > until_ts { continue; }
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

#[cfg(test)]
mod tests {
    use super::parse_bearer_token;

    #[test]
    fn parse_bearer_token_handles_bearer_prefix() {
        assert_eq!(
            parse_bearer_token(Some("Bearer abc123")),
            Some("abc123".to_owned())
        );
        assert_eq!(
            parse_bearer_token(Some("bearer xyz")),
            Some("xyz".to_owned())
        );
        assert_eq!(
            parse_bearer_token(Some("   Bearer tok   ")),
            Some("tok".to_owned())
        );
    }

    #[test]
    fn parse_bearer_token_passes_through_plain_token() {
        assert_eq!(
            parse_bearer_token(Some("plain-token")),
            Some("plain-token".to_owned())
        );
    }

    #[test]
    fn parse_bearer_token_rejects_empty_inputs() {
        assert_eq!(parse_bearer_token(None), None);
        assert_eq!(parse_bearer_token(Some("")), None);
        assert_eq!(parse_bearer_token(Some("   ")), None);
        assert_eq!(parse_bearer_token(Some("Bearer ")), None);
    }
}
