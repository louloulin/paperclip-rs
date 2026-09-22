//! Routes 聚合：所有 router 注册点。

use axum::routing::{get, post};
use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

pub mod health;
pub mod openapi;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/health", get(health::health))
        .route("/api/health/db", get(health::db_health))
        .route("/api/openapi.json", get(openapi::openapi_json))
        .route("/api/auth/login", post(health::placeholder))
        .route("/api/auth/logout", post(health::placeholder))
        .route("/api/auth/session", get(health::placeholder))
        .route("/api/workspaces", get(health::placeholder).post(health::placeholder))
        .route("/api/workspaces/{id}", get(health::placeholder))
        .route("/api/workspaces/{id}/members", get(health::placeholder))
        .route("/api/issues", get(health::placeholder).post(health::placeholder))
        .route("/api/issues/{id}", get(health::placeholder))
        .route("/api/agents", get(health::placeholder).post(health::placeholder))
        .route("/api/runtimes", get(health::placeholder).post(health::placeholder))
        .route("/api/chat/sessions", get(health::placeholder).post(health::placeholder))
        .route("/api/inbox", get(health::placeholder))
        .route("/api/skills", get(health::placeholder).post(health::placeholder))
        .route("/api/plugins", get(health::placeholder).post(health::placeholder))
        .route("/api/autopilots", get(health::placeholder).post(health::placeholder))
        .route("/api/squads", get(health::placeholder).post(health::placeholder))
        .route("/api/projects", get(health::placeholder).post(health::placeholder))
        .route("/api/comments", get(health::placeholder).post(health::placeholder))
        .route("/api/feature-flags", get(health::placeholder))
}