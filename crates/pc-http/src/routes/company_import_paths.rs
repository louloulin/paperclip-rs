//! 公司可导入路径配置。

use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/companies/:company_id/import-paths", get(import_paths))
}

async fn import_paths(State(_state): State<AppState>, Path(company_id): Path<Uuid>) -> Json<Value> {
    Json(json!({
        "companyId": company_id,
        "paths": [],
        "updatedAt": null
    }))
}
