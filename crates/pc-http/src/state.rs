//! 应用状态。
use axum::{
    extract::FromRef,
    http::{header, HeaderMap},
};
use pc_adapter_api::AdapterRegistry;
use pc_core::actor_runtime::kameo_api::ActorRef;
use pc_core::ActorRegistry;
use pc_db::Db;
use pc_heartbeat::HeartbeatSupervisor;
use pc_realtime::RealtimeHandle;
use pc_realtime::WsState;
use pc_telemetry::TelemetryOptions;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct ConfigSnapshot {
    pub host: String,
    pub port: u16,
    pub session_cookie: String,
    pub api_key_header: String,
    pub csrf_header: String,
}

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub actors: ActorRegistry,
    pub heartbeat: ActorRef<HeartbeatSupervisor>,
    pub adapters: AdapterRegistry,
    pub config: Arc<ConfigSnapshot>,
    pub telemetry: Arc<TelemetryOptions>,
    pub ws: Arc<WsState>,
    pub realtime: RealtimeHandle,
}

#[derive(Clone)]
pub struct RuntimeHandles {
    pub actors: ActorRegistry,
    pub heartbeat: ActorRef<HeartbeatSupervisor>,
    pub adapters: AdapterRegistry,
}

impl FromRef<AppState> for Db {
    fn from_ref(state: &AppState) -> Db {
        state.db.clone()
    }
}

impl AppState {
    pub fn new(
        db: Db,
        runtime: RuntimeHandles,
        config: ConfigSnapshot,
        telemetry: TelemetryOptions,
        ws: Arc<WsState>,
        realtime: RealtimeHandle,
    ) -> Self {
        Self {
            db,
            actors: runtime.actors,
            heartbeat: runtime.heartbeat,
            adapters: runtime.adapters,
            config: Arc::new(config),
            telemetry: Arc::new(telemetry),
            ws,
            realtime,
        }
    }
}

impl FromRef<AppState> for Arc<WsState> {
    fn from_ref(input: &AppState) -> Self {
        input.ws.clone()
    }
}

pub async fn require_user_id(state: &AppState, headers: &HeaderMap) -> crate::ApiResult<String> {
    if let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    {
        if let Some((_, user_id)) = pc_auth::resolve_api_key(&state.db, token)
            .await
            .map_err(|error| crate::ApiError::Internal(error.to_string()))?
        {
            return Ok(user_id);
        }
        if let Some((user_id, _)) = pc_auth::resolve_session(&state.db, token)
            .await
            .map_err(|error| crate::ApiError::Internal(error.to_string()))?
        {
            return Ok(user_id);
        }
    }
    if let Some(cookie) = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
    {
        let prefix = format!("{}=", state.config.session_cookie);
        for part in cookie.split(';').map(str::trim) {
            if let Some(token) = part.strip_prefix(&prefix) {
                if let Some((user_id, _)) = pc_auth::resolve_session(&state.db, token)
                    .await
                    .map_err(|error| crate::ApiError::Internal(error.to_string()))?
                {
                    return Ok(user_id);
                }
            }
        }
    }
    Err(crate::ApiError::Unauthorized(
        "user authentication required".into(),
    ))
}
