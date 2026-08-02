//! 当前公司选择的环境。

use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/companies/:company_id/environment-selection",
        get(get_selection),
    )
}

async fn get_selection(
    State(_state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> Json<Value> {
    Json(json!({
        "companyId": company_id,
        "environmentId": null,
        "updatedAt": null
    }))
}
