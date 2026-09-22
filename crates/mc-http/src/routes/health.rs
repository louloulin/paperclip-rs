//! `/api/health` + 通用 placeholder。

use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use mc_db::health::HealthStatus;

use crate::error::ApiResult;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub service: &'static str,
    pub version: &'static str,
    pub db: &'static str,
}

pub async fn health(State(state): State<Arc<AppState>>) -> ApiResult<Json<HealthResponse>> {
    let h = mc_db::health::check(&state.db).await;
    Ok(Json(HealthResponse {
        status: "ok",
        service: "multica-rs",
        version: env!("CARGO_PKG_VERSION"),
        db: match h.status {
            HealthStatus::Healthy => "healthy",
            HealthStatus::Degraded => "degraded",
            HealthStatus::Unhealthy => "unhealthy",
        },
    }))
}

#[derive(Debug, Serialize)]
pub struct DbHealthResponse {
    pub status: String,
    pub latency_ms: u64,
    pub message: Option<String>,
}

pub async fn db_health(State(state): State<Arc<AppState>>) -> ApiResult<Json<DbHealthResponse>> {
    let h = mc_db::health::check(&state.db).await;
    Ok(Json(DbHealthResponse {
        status: format!("{:?}", h.status).to_lowercase(),
        latency_ms: h.latency_ms,
        message: h.message,
    }))
}

/// Placeholder：路由已注册但 handler 暂未实现；返回 501。
pub async fn placeholder() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "code": "not_implemented",
        "message": "this route is reserved; implementation lands in a later milestone",
    }))
}
