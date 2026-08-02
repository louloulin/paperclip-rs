//! `/health` 健康检查。
use std::time::Instant;
use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use serde_json::json;
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/health", get(handler))
}

async fn handler(State(state): State<AppState>) -> impl IntoResponse {
    let start = Instant::now();
    let db_result = sqlx::query("SELECT 1").execute(state.db.pool()).await;
    #[allow(clippy::cast_possible_truncation)] let latency_ms = start.elapsed().as_millis() as u64;
    match db_result {
        Ok(_) => (StatusCode::OK, Json(json!({
            "status": "ok",
            "version": env!("CARGO_PKG_VERSION"),
            "db": { "ok": true, "latency_ms": latency_ms, "error": null }
        }))),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, Json(json!({
            "status": "degraded",
            "version": env!("CARGO_PKG_VERSION"),
            "db": { "ok": false, "latency_ms": latency_ms, "error": e.to_string() }
        }))),
    }
}
