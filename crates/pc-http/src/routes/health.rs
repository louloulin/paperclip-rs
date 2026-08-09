//! `/health` 健康检查。
use crate::AppState;
use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use serde_json::json;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(handler))
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

    // M23: include deploymentMode / bootstrapStatus so the React UI's
    // CloudAccessGate can decide whether to redirect to /auth.
    //
    // - `local_trusted`: anonymous, no auth required.
    // - `authenticated`: requires a valid session; UI redirects to /auth.
    //
    // For paperclip-rs v1 we default to `authenticated` because every
    // protected route already checks the session cookie. Future config can
    // gate this via env (PAPERCLIP_DEPLOYMENT_MODE).
    let deployment_mode = std::env::var("PAPERCLIP_DEPLOYMENT_MODE")
        .ok()
        .and_then(|v| match v.as_str() {
            "local_trusted" | "local-trusted" => Some("local_trusted"),
            "authenticated" => Some("authenticated"),
            _ => None,
        })
        .unwrap_or("authenticated");

    // M23: bootstrapStatus reflects whether the first admin claim flow is
    // pending. For now we always report `ready` (bootstrap is a follow-up).
    let bootstrap_status = "ready";

    if db_result.ok {
        (
            StatusCode::OK,
            Json(json!({
                "status": "ok",
                "version": env!("CARGO_PKG_VERSION"),
                "deploymentMode": deployment_mode,
                "bootstrapStatus": bootstrap_status,
                "authReady": true,
                "db": { "ok": true, "latency_ms": latency_ms, "error": null }
            })),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "status": "degraded",
                "version": env!("CARGO_PKG_VERSION"),
                "deploymentMode": deployment_mode,
                "bootstrapStatus": bootstrap_status,
                "authReady": false,
                "db": { "ok": false, "latency_ms": latency_ms, "error": db_result.error }
            })),
        )
    }
}
