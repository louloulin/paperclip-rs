//! `GET /api/daemon/workspaces` 及其 `:workspaceId/repos` /
//! `:workspaceId/runtime-profiles` 三个读面。模块文档见父模块（`mod.rs`）。

use axum::extract::{Path, State};
use axum::Json;
use mc_repos::daemon::DaemonRepo;
use serde_json::{json, Value};
use std::sync::Arc;

use super::super::scope::{
    db_err, not_found, parse_path_id, require_workspace_access, DaemonActor, DaemonAuth,
};
use crate::error::ApiResult;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// GET /api/daemon/workspaces
// ---------------------------------------------------------------------------

/// upstream `ListDaemonWorkspaces`（`daemon_workspace.go:27`）。
///
/// daemon token 只看得到 token 绑定的那一个 workspace；用户身份看全部 membership。
pub(crate) async fn list_workspaces(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
) -> ApiResult<Json<Value>> {
    let repo = DaemonRepo::new(&state.db);
    let rows = match &auth.actor {
        DaemonActor::Daemon { workspace_id, .. } => {
            let name = repo
                .workspace_name(*workspace_id)
                .await
                .map_err(db_err)?
                .ok_or_else(|| not_found("workspace not found"))?;
            vec![(*workspace_id, name)]
        }
        DaemonActor::User { user_id, .. } => repo
            .list_workspaces_for_user(*user_id)
            .await
            .map_err(db_err)?,
    };
    let workspaces: Vec<Value> = rows
        .into_iter()
        .map(|(id, name)| json!({ "id": id.to_string(), "name": name }))
        .collect();
    Ok(Json(json!({ "workspaces": workspaces })))
}

// ---------------------------------------------------------------------------
// GET /api/daemon/workspaces/:workspaceId/repos
// ---------------------------------------------------------------------------

/// upstream `GetDaemonWorkspaceRepos`（`daemon.go:905`）。
pub(crate) async fn workspace_repos(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path(workspace_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let workspace_id = parse_path_id("workspace_id", &workspace_id)?;
    require_workspace_access(&state, &auth, workspace_id, "workspace not found").await?;
    let repos = DaemonRepo::new(&state.db)
        .workspace_repos(workspace_id)
        .await
        .map_err(db_err)?
        .ok_or_else(|| not_found("workspace not found"))?;
    Ok(Json(json!({
        "repos": repos.repos,
        "repos_version": repos.repos_version,
        "settings": repos.settings.unwrap_or_else(|| json!({})),
    })))
}

// ---------------------------------------------------------------------------
// GET /api/daemon/workspaces/:workspaceId/runtime-profiles
// ---------------------------------------------------------------------------

/// upstream `DaemonListRuntimeProfiles`（`runtime_profile.go:640`）。
///
/// 只回**启用**的 profile（daemon 拿它来决定要不要拉起自定义命令）。
pub(crate) async fn runtime_profiles(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path(workspace_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let workspace_id = parse_path_id("workspace_id", &workspace_id)?;
    require_workspace_access(&state, &auth, workspace_id, "workspace not found").await?;
    let profiles = DaemonRepo::new(&state.db)
        .list_runtime_profiles(workspace_id)
        .await
        .map_err(db_err)?;
    Ok(Json(json!({ "runtime_profiles": profiles })))
}
