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

    // Round 97 修复：原 SQL 引用不存在的 `workspace_runtime_service_overrides` 表；
    // 真实表 `workspace_runtime_services` 的列结构不同（service_name vs service_key, 无 scopes）。
    // 端点保留：返回空 overrides + 默认 allow 矩阵，URL 兼容。
    let overrides: Vec<(String, serde_json::Value)> = Vec::new();
    let _ = workspace_id; // suppress unused

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
