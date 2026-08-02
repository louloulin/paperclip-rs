//! 应用状态。
use axum::extract::FromRef;
use pc_db::Db;
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
    pub config: Arc<ConfigSnapshot>,
    pub telemetry: Arc<TelemetryOptions>,
    pub ws: Arc<WsState>,
    pub realtime: RealtimeHandle,
}

impl FromRef<AppState> for Db {
    fn from_ref(state: &AppState) -> Db {
        state.db.clone()
    }
}

impl AppState {
    pub fn new(
        db: Db,
        config: ConfigSnapshot,
        telemetry: TelemetryOptions,
        ws: Arc<WsState>,
        realtime: RealtimeHandle,
    ) -> Self {
        Self {
            db,
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
