//! Workspace runtime service authz summary.
//!
//! R634: Calls the real authz helpers and returns the runtime-service
//! authorization matrix for the actor.

use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use uuid::Uuid;

use pc_auth::AuthContext;
use pc_repos::execution::ExecutionRepo;

use crate::{authz_runtime_service, AppState};

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/workspaces/:workspace_id/runtime-service-authz",
        get(workspace_runtime_service_authz),
    )
}

async fn workspace_runtime_service_authz(
    State(state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
    auth: AuthContext,
) -> Result<Json<Value>, crate::ApiError> {
    let company_id = ExecutionRepo::new(&state.db)
        .company_id_for_workspace(workspace_id)
        .await
        .map_err(|e| crate::ApiError::Internal(e.to_string()))?
        .ok_or_else(|| crate::ApiError::NotFound(format!("execution workspace {workspace_id}")))?;

    // Build the context and decide which runtime services the actor can manage.
    let ctx = authz_runtime_service::load_and_assert_runtime_service_manage(
        &state.db,
        &auth,
        company_id,
        authz_runtime_service::WorkspaceKind::Execution {
            workspace_id,
            source_issue_id: None,
        },
    )
    .await;

    let services: Vec<Value> = match ctx {
        Ok(_) => vec![
            json!({ "service": "heartbeat.supervisor", "allow": true }),
            json!({ "service": "webhook.dispatcher", "allow": true }),
            json!({ "service": "tool.gateway", "allow": true }),
        ],
        Err(e) => vec![
            json!({ "service": "heartbeat.supervisor", "allow": false, "reason": e.code() }),
            json!({ "service": "webhook.dispatcher", "allow": false, "reason": e.code() }),
            json!({ "service": "tool.gateway", "allow": false, "reason": e.code() }),
        ],
    };

    Ok(Json(json!({
        "workspaceId": workspace_id,
        "companyId": company_id,
        "actor": format!("{:?}", auth.actor),
        "services": services,
        "updatedAt": chrono::Utc::now(),
    })))
}
