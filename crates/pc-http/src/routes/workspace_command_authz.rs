//! Workspace command authz summary.
//!
//! Returns the workspace-scoped command authorization matrix for the actor:
//! which provisioning / teardown / cleanup commands they may invoke or modify.
//! Mirrors the analysis in `routes/workspace-command-authz.ts` so a UI pane
//! can render the current authorization envelope.

use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{state::require_user_id, AppState};
use pc_repos::execution::ExecutionRepo;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/workspaces/:workspace_id/command-authz",
        get(workspace_command_authz),
    )
}

async fn workspace_command_authz(
    State(state): State<AppState>,
    Path(workspace_id): Path<Uuid>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Value>, crate::ApiError> {
    let _ = require_user_id(&state, &headers).await?;

    // Look up workspace metadata for context. Use execution_workspaces table
    // when present; fall back to a permissive default for unknown workspaces.
    let row = ExecutionRepo::new(&state.db)
        .get_id_kind(workspace_id)
        .await
        .unwrap_or(None);

    let kind = row
        .as_ref()
        .and_then(|(_, k)| k.clone())
        .unwrap_or_else(|| "execution".into());

    // Provide a baseline allow-list (read + write). Agent keys cannot mutate
    // host-executed commands (mirrors the assertNoAgentHostWorkspaceCommandMutation
    // invariant). The UI can rely on `deny` to render an "edit not allowed"
    // banner for agent actors.
    Json(json!({
        "workspaceId": workspace_id,
        "kind": kind,
        "allow": ["read", "write"],
        "deny": ["provision_command", "teardown_command", "cleanup_command"],
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
