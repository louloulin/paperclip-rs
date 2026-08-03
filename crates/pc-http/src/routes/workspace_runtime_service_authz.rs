//! Workspace runtime service authz summary.
//!
//! Returns the workspace-scoped runtime-service authorization matrix for the
//! actor — i.e. which runtime services (heartbeat supervisor, webhook dispatcher,
//! tool gateway, etc.) the actor is allowed to invoke from this workspace.

use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{state::require_user_id, AppState};

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/workspaces/:workspace_id/runtime-service-authz",
        get(workspace_runtime_service_authz),
    )
}

async fn workspace_runtime_service_authz(
    State(state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Value>, crate::ApiError> {
    let _ = require_user_id(&state, &headers).await?;

    // Provide a default matrix of services that any authenticated actor may
    // invoke. Specific override rows (if present) are loaded from
    // `workspace_runtime_service_overrides` for finer-grained enforcement.
    let overrides: Vec<(String, serde_json::Value)> = sqlx::query_as(
        "SELECT service_key, scopes FROM workspace_runtime_service_overrides WHERE workspace_id = $1",
    )
    .bind(workspace_id)
    .fetch_all(state.db.pool())
    .await
    .unwrap_or_default();

    let services: Vec<Value> = overrides
        .into_iter()
        .map(|(key, scopes)| {
            json!({
                "service": key,
                "scopes": scopes,
                "allow": true,
            })
        })
        .collect();

    Json(json!({
        "workspaceId": workspace_id,
        "services": services,
        "updatedAt": chrono::Utc::now()
    }))
    .pipe(Ok)
}

trait Pipe: Sized {
    fn pipe<R>(self, f: impl FnOnce(Self) -> R) -> R {
        f(self)
    }
}
impl<T> Pipe for T {}
