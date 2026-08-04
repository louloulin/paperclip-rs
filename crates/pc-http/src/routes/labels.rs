//! `/api/labels*` 路由：
//!
//! | Method | Path | Node 等价 | 备注 |
//! |---|---|---|---|
//! | GET    | `/api/companies/:company_id/labels` | ✅ | list by company |
//! | POST   | `/api/companies/:company_id/labels` | ✅ | create |
//! | PATCH  | `/api/labels/:label_id` | ✅ | update name / color |
//! | DELETE | `/api/labels/:label_id` | ✅ | delete |

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use pc_repos::label::{LabelPatch, LabelRepo, NewLabel};

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/companies/:company_id/labels",
            get(list_labels).post(create_label),
        )
        .route(
            "/api/labels/:label_id",
            patch(patch_label).delete(delete_label),
        )
}

#[derive(Debug, Deserialize)]
struct CreateBody {
    name: String,
    color: String,
}

#[derive(Debug, Deserialize)]
struct PatchBody {
    name: Option<String>,
    color: Option<String>,
}

async fn list_labels(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = LabelRepo::new(&state.db)
        .list_by_company(company_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn create_label(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    let input = NewLabel {
        company_id,
        name: body.name,
        color: body.color,
    };
    let row = LabelRepo::new(&state.db)
        .create(&input)
        .await
        .map_err(|e| match e.to_string().as_str() {
            s if s.contains("unique") || s.contains("23505") => {
                ApiError::Conflict("label name already exists in company".into())
            }
            _ => ApiError::Internal(e.to_string()),
        })?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(row).unwrap_or_default()),
    ))
}

async fn patch_label(
    State(state): State<AppState>,
    Path(label_id): Path<Uuid>,
    Json(body): Json<PatchBody>,
) -> ApiResult<Json<Value>> {
    let patch = LabelPatch {
        name: body.name,
        color: body.color,
    };
    let row = LabelRepo::new(&state.db)
        .patch(label_id, &patch)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    match row {
        Some(r) => Ok(Json(serde_json::to_value(r).unwrap_or_default())),
        None => Err(ApiError::NotFound(format!("label {label_id}"))),
    }
}

async fn delete_label(
    State(state): State<AppState>,
    Path(label_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let deleted = LabelRepo::new(&state.db)
        .delete(label_id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if !deleted {
        return Err(ApiError::NotFound(format!("label {label_id}")));
    }
    Ok(Json(json!({ "deleted": true, "labelId": label_id })))
}
