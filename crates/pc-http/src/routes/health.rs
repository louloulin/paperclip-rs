//! `/health` 健康检查。
use crate::AppState;
use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use serde_json::json;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/health", get(handler))
        // ── Round 44: /api root index alias (node health mounted at /api) ──
        .route("/api", get(handler))
        .route("/api/health", get(handler))
}

async fn handler(State(state): State<AppState>) -> impl IntoResponse {
    // Round 189: use pc_db::health::HealthCheck which encapsulates SELECT 1 ping.
    let db_result = pc_db::health::HealthCheck::check(&state.db).await;
    #[allow(clippy::cast_possible_truncation)]
    let latency_ms = db_result.latency_ms as u64;
    if db_result.ok {
        (
            StatusCode::OK,
            Json(json!({
                "status": "ok",
                "version": env!("CARGO_PKG_VERSION"),
                "db": { "ok": true, "latency_ms": latency_ms, "error": null }
            })),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "status": "degraded",
                "version": env!("CARGO_PKG_VERSION"),
                "db": { "ok": false, "latency_ms": latency_ms, "error": db_result.error }
            })),
        )
    }
}
