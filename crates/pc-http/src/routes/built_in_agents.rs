//! 公司内置 agent（系统自带的 skill/template agent）。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/companies/:company_id/built-in-agents",
            get(list_built_in),
        )
        .route(
            "/api/companies/:company_id/built-in-agents/:key/status",
            get(get_built_in_status),
        )
        .route(
            "/api/companies/:company_id/built-in-agents/:key/reconcile",
            post(reconcile_built_in),
        )
        .route(
            "/api/companies/:company_id/built-in-agents/:key/install",
            post(install_built_in),
        )
        .route(
            "/api/companies/:company_id/built-in-agents/:key/reset",
            post(reset_built_in),
        )
        .route(
            "/api/companies/:company_id/built-in-agents/:key/archive",
            post(archive_built_in),
        )
        .route(
            "/api/companies/:company_id/built-in-agents/:key/restore",
            post(restore_built_in),
        )
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct EmptyBody {}

async fn list_built_in(
    State(_state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> Json<Value> {
    Json(json!({
        "companyId": company_id,
        "items": [],
        "available": [
            { "key": "code-reviewer", "name": "Code Reviewer", "description": "Reviews code for issues", "installed": false },
            { "key": "doc-writer", "name": "Doc Writer", "description": "Writes documentation", "installed": false },
            { "key": "issue-triager", "name": "Issue Triager", "description": "Triages issues", "installed": false }
        ]
    }))
}

async fn get_built_in_status(
    State(_state): State<AppState>,
    Path((company_id, key)): Path<(Uuid, String)>,
) -> Json<Value> {
    Json(json!({
        "companyId": company_id,
        "key": key,
        "status": "available",
        "agentId": null
    }))
}

async fn reconcile_built_in(
    State(_state): State<AppState>,
    Path((company_id, key)): Path<(Uuid, String)>,
    Json(_body): Json<EmptyBody>,
) -> impl IntoResponse {
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "companyId": company_id,
            "key": key,
            "status": "reconciling"
        })),
    )
}

async fn install_built_in(
    State(_state): State<AppState>,
    Path((company_id, key)): Path<(Uuid, String)>,
) -> impl IntoResponse {
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "companyId": company_id,
            "key": key,
            "status": "install-queued"
        })),
    )
}

async fn reset_built_in(
    State(_state): State<AppState>,
    Path((company_id, key)): Path<(Uuid, String)>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "companyId": company_id,
            "key": key,
            "status": "reset-queued"
        })),
    )
}

async fn archive_built_in(
    State(_state): State<AppState>,
    Path((company_id, key)): Path<(Uuid, String)>,
) -> impl IntoResponse {
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "companyId": company_id,
            "key": key,
            "status": "archive-queued"
        })),
    )
}

async fn restore_built_in(
    State(_state): State<AppState>,
    Path((company_id, key)): Path<(Uuid, String)>,
) -> impl IntoResponse {
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "companyId": company_id,
            "key": key,
            "status": "restore-queued"
        })),
    )
}
