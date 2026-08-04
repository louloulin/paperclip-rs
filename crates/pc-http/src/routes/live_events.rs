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

#[derive(Debug, Deserialize)]
struct AuthQuery {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    company_id: Option<Uuid>,
}

async fn handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
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
    let ws_state = state.ws.clone();
    ws.on_upgrade(move |socket| handle_socket(socket, ws_state, company_id))
}

/// Authorize a WebSocket upgrade. Mirrors Node `authorizeUpgrade` in
/// `services/realtime/live-events-ws.ts`. Returns `true` when the request
/// carries a valid agent API key matching the requested company, the
/// request is a logged-in user with a session, or the server is in
/// `local_trusted` mode (anonymous board context).
async fn authorize_ws(
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
            // Hash the token and look up the agent API key
            let token_hash = pc_auth::hash_token(token);
            let row: Option<(Uuid, Uuid)> = sqlx::query_as(
                "SELECT id, company_id FROM board_api_keys                  WHERE key_hash = $1 AND revoked_at IS NULL",
            )
            .bind(&token_hash)
            .fetch_optional(state.db.pool())
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
                if let Some((user_id, _)) =
                    pc_auth::resolve_session(&state.db, token)
                        .await
                        .map_err(|err| err.to_string())?
                {
                    if let Some(requested) = company_id {
                        let row: Option<(Uuid,)> = sqlx::query_as(
                            "SELECT company_id FROM company_memberships                              WHERE user_id = $1 AND company_id = $2 AND status = 'active'",
                        )
                        .bind(&user_id)
                        .bind(requested)
                        .fetch_optional(state.db.pool())
                        .await
                        .map_err(|err| err.to_string())?;
                        Ok(row.is_some())
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
) {
    let client_id = Uuid::new_v4();
    info!(%client_id, ?initial_company_id, "ws connected");

    let (mut sender, mut receiver) = socket.split();
    let mut rx = state.realtime.subscribe();

    let welcome =
        json!({"type":"welcome","client_id":client_id,"server":&state.server_name}).to_string();
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
