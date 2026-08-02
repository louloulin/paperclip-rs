//! Teams catalog (团队/技能目录) — 浏览、安装、预览。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/teams/catalog", get(list_teams_catalog))
        .route("/api/teams/catalog/:catalog_id/files", get(catalog_files))
        .route("/api/teams/catalog/:catalog_id", get(catalog_detail))
        .route(
            "/api/companies/:company_id/teams/catalog/installed",
            get(installed_teams),
        )
        .route(
            "/api/companies/:company_id/teams/catalog/:catalog_id/preview",
            get(catalog_preview),
        )
        .route(
            "/api/companies/:company_id/teams/catalog/:catalog_id/install",
            post(install_team),
        )
}

async fn list_teams_catalog(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({
        "items": [
            { "catalogId": "core-team", "name": "Core Team", "description": "Default team setup", "version": "1.0.0" },
            { "catalogId": "research-team", "name": "Research Team", "description": "Research-focused team", "version": "0.1.0" }
        ]
    }))
}

async fn catalog_files(
    State(_state): State<AppState>,
    Path(catalog_id): Path<String>,
) -> Json<Value> {
    Json(json!({
        "catalogId": catalog_id,
        "files": []
    }))
}

async fn catalog_detail(
    State(_state): State<AppState>,
    Path(catalog_id): Path<String>,
) -> Json<Value> {
    Json(json!({
        "catalogId": catalog_id,
        "name": catalog_id,
        "description": "Catalog entry",
        "version": "0.1.0"
    }))
}

async fn installed_teams(
    State(_state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> Json<Value> {
    Json(json!({
        "companyId": company_id,
        "items": []
    }))
}

async fn catalog_preview(
    State(_state): State<AppState>,
    Path((company_id, catalog_id)): Path<(Uuid, String)>,
) -> Json<Value> {
    Json(json!({
        "companyId": company_id,
        "catalogId": catalog_id,
        "preview": { "skills": [], "agents": [] }
    }))
}

async fn install_team(
    State(_state): State<AppState>,
    Path((company_id, catalog_id)): Path<(Uuid, String)>,
) -> impl IntoResponse {
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "companyId": company_id,
            "catalogId": catalog_id,
            "status": "install-queued"
        })),
    )
}
