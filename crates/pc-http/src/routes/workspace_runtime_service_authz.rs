//! workspace runtime service 授权。

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
        "/api/workspaces/:workspace_id/runtime-service-authz",
        get(workspace_runtime_service_authz),
    )
}

async fn workspace_runtime_service_authz(
    State(_state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
) -> Json<Value> {
    Json(json!({
        "workspaceId": workspace_id,
        "services": [],
        "updatedAt": null
    }))
}
