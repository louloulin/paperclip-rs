//! 应用状态。
use std::sync::Arc;
use axum::extract::FromRef;
use pc_db::Db;
use pc_telemetry::TelemetryOptions;

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
}

impl FromRef<AppState> for Db {
    fn from_ref(state: &AppState) -> Db { state.db.clone() }
}

impl AppState {
    pub fn new(db: Db, config: ConfigSnapshot, telemetry: TelemetryOptions) -> Self {
        Self { db, config: Arc::new(config), telemetry: Arc::new(telemetry) }
    }
}
