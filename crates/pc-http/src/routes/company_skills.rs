//! 公司级 skills (浏览、安装、状态、清单)。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/skills/catalog", get(skills_catalog))
        .route(
            "/api/skills/catalog/:catalog_id/files",
            get(skills_catalog_files),
        )
        .route(
            "/api/skills/catalog/:catalog_id",
            get(skills_catalog_detail),
        )
        .route(
            "/api/companies/:company_id/skills",
            get(list_company_skills).post(install_company_skill),
        )
        .route(
            "/api/companies/:company_id/skills/categories",
            get(skills_categories),
        )
        .route(
            "/api/companies/:company_id/skills/:skill_id",
            get(get_company_skill).delete(remove_company_skill),
        )
        .route(
            "/api/companies/:company_id/skills/:skill_id/config",
            get(get_skill_config).put(put_skill_config),
        )
        .route(
            "/api/companies/:company_id/skills/:skill_id/preview",
            get(skill_preview),
        )
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct InstallBody {
    catalog_id: Option<String>,
    skill_key: Option<String>,
}

async fn skills_catalog(State(_s): State<AppState>) -> Json<Value> {
    Json(json!({ "items": [] }))
}

async fn skills_catalog_files(
    State(_s): State<AppState>,
    Path(catalog_id): Path<String>,
) -> Json<Value> {
    Json(json!({ "catalogId": catalog_id, "files": [] }))
}

async fn skills_catalog_detail(
    State(_s): State<AppState>,
    Path(catalog_id): Path<String>,
) -> Json<Value> {
    Json(json!({ "catalogId": catalog_id }))
}

async fn list_company_skills(
    State(_s): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> Json<Value> {
    Json(json!({ "companyId": company_id, "items": [] }))
}

async fn install_company_skill(
    State(_s): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(_body): Json<InstallBody>,
) -> impl IntoResponse {
    let _ = company_id;
    (
        StatusCode::ACCEPTED,
        Json(json!({ "status": "install-queued" })),
    )
}

async fn skills_categories(
    State(_s): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> Json<Value> {
    Json(json!({ "companyId": company_id, "items": [] }))
}

async fn get_company_skill(
    State(_s): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, String)>,
) -> Json<Value> {
    Json(json!({
        "companyId": company_id,
        "skillId": skill_id
    }))
}

async fn remove_company_skill(
    State(_s): State<AppState>,
    Path((_company_id, skill_id)): Path<(Uuid, String)>,
) -> impl IntoResponse {
    let _ = skill_id;
    (StatusCode::NO_CONTENT, Json(json!({ "deleted": true })))
}

async fn get_skill_config(
    State(_s): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, String)>,
) -> Json<Value> {
    Json(json!({
        "companyId": company_id,
        "skillId": skill_id,
        "config": {}
    }))
}

async fn put_skill_config(
    State(_s): State<AppState>,
    Path((company_id, skill_id)): Path<(Uuid, String)>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    let _ = company_id;
    let _ = skill_id;
    (StatusCode::OK, Json(json!({ "saved": true })))
}

async fn skill_preview(
    State(_s): State<AppState>,
    Path((_company_id, _skill_id)): Path<(Uuid, String)>,
) -> Json<Value> {
    Json(json!({ "preview": null }))
}
