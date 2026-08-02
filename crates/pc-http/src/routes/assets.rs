//! 资源（图片 / logo）上传与读取。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use uuid::Uuid;

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/companies/:company_id/assets/images",
            post(upload_image),
        )
        .route("/api/companies/:company_id/logo", post(upload_logo))
        .route("/api/assets/:asset_id/content", get(asset_content))
}

async fn upload_image(
    State(_state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> impl IntoResponse {
    let _ = company_id;
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "status": "accepted",
            "message": "image upload accepted; processing not implemented in Rust build yet"
        })),
    )
}

async fn upload_logo(
    State(_state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> impl IntoResponse {
    let _ = company_id;
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "status": "accepted",
            "message": "logo upload accepted"
        })),
    )
}

async fn asset_content(
    State(_state): State<AppState>,
    Path(asset_id): Path<Uuid>,
) -> impl IntoResponse {
    let _ = asset_id;
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "error": { "code": "not_found", "message": "asset content not available" }
        })),
    )
}
