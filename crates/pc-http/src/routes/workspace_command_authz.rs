//! workspace command 授权。

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
        "/api/workspaces/:workspace_id/command-authz",
        get(workspace_command_authz),
    )
}

async fn workspace_command_authz(
    State(_state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
) -> Json<Value> {
    Json(json!({
        "workspaceId": workspace_id,
        "allow": ["read", "write"],
        "deny": [],
        "updatedAt": null
    }))
}
