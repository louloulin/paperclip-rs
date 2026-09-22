//! `/api/issues/:id` 的 reactions / metadata / properties（从 `issues.rs` 拆出，
//! R7 单文件 800 行上限）。

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::routes::invitations::require_workspace_member;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use mc_errors::Error;
use mc_repos::issue::IssueReactionRow;
use serde_json::Value as JsonValue;
use std::sync::Arc;

use super::context::{issue_repo, load_issue, resolve_workspace, WorkspaceQuery};
use super::dto::{
    IssueReactionDto, MetadataResponse, PropertiesResponse, ReactionRequest, ValueRequest,
};
use super::helpers::{repo_err, validation};
use super::{METADATA_KEYS_MAX, METADATA_KEY_MAX_LEN};

// ---------------------------------------------------------------------------
// reactions / metadata / properties
pub(crate) fn reaction_dto(row: &IssueReactionRow) -> IssueReactionDto {
    IssueReactionDto {
        id: row.id.to_string(),
        issue_id: row.issue_id.to_string(),
        actor_type: row.actor_type.clone(),
        actor_id: row.actor_id.clone(),
        emoji: row.emoji.clone(),
        created_at: row.created_at.to_rfc3339(),
    }
}

/// `GET /api/issues/:id/reactions`
pub(crate) async fn list_reactions(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Json<Vec<IssueReactionDto>>> {
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    let repo = issue_repo(&state);
    let issue = load_issue(&repo, workspace_id, &raw_id).await?;
    let rows = repo.list_reactions(issue.id()).await.map_err(repo_err)?;
    Ok(Json(rows.iter().map(reaction_dto).collect()))
}

/// `POST /api/issues/:id/reactions`
pub(crate) async fn add_reaction(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    Json(req): Json<ReactionRequest>,
) -> ApiResult<Response> {
    let emoji = req.emoji.trim().to_string();
    if emoji.is_empty() {
        return Err(validation("emoji is required").into());
    }
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let repo = issue_repo(&state);
    let issue = load_issue(&repo, workspace_id, &raw_id).await?;
    // 0004 的 CHECK 只允许 user/agent/system（上游写 `member`）；X-Agent-ID 归 M3
    let row = repo
        .add_reaction(
            workspace_id,
            issue.id(),
            "user",
            &user.id().to_string(),
            &emoji,
        )
        .await
        .map_err(repo_err)?;
    Ok((StatusCode::CREATED, Json(reaction_dto(&row))).into_response())
}

/// `DELETE /api/issues/:id/reactions`（body `{"emoji": "👍"}`）
pub(crate) async fn remove_reaction(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    Json(req): Json<ReactionRequest>,
) -> ApiResult<StatusCode> {
    let emoji = req.emoji.trim().to_string();
    if emoji.is_empty() {
        return Err(validation("emoji is required").into());
    }
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let repo = issue_repo(&state);
    let issue = load_issue(&repo, workspace_id, &raw_id).await?;
    repo.remove_reaction(issue.id(), "user", &user.id().to_string(), &emoji)
        .await
        .map_err(repo_err)?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) fn validate_metadata_key(key: &str) -> Result<(), Error> {
    let trimmed = key.trim();
    if trimmed.is_empty() || trimmed.len() > METADATA_KEY_MAX_LEN {
        return Err(validation(format!(
            "metadata key must be 1..={METADATA_KEY_MAX_LEN} characters"
        )));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(validation(
            "metadata key may only contain letters, digits, '_', '-' and '.'",
        ));
    }
    Ok(())
}

pub(crate) fn validate_metadata_value(value: &JsonValue) -> Result<(), Error> {
    if value.is_array() || value.is_object() {
        return Err(validation(
            "metadata values must be primitives (string, number, boolean, null)",
        ));
    }
    Ok(())
}

/// `GET /api/issues/:id/metadata`
pub(crate) async fn get_metadata(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Json<MetadataResponse>> {
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    let repo = issue_repo(&state);
    let issue = load_issue(&repo, workspace_id, &raw_id).await?;
    let metadata = repo
        .get_metadata(workspace_id, issue.id())
        .await
        .map_err(repo_err)?;
    Ok(Json(MetadataResponse {
        metadata,
        issue_revision: None,
    }))
}

/// `PUT /api/issues/:id/metadata/:key`
pub(crate) async fn set_metadata_key(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((raw_id, key)): Path<(String, String)>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    Json(req): Json<ValueRequest>,
) -> ApiResult<Json<MetadataResponse>> {
    validate_metadata_key(&key)?;
    validate_metadata_value(&req.value)?;
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let repo = issue_repo(&state);
    let current = load_issue(&repo, workspace_id, &raw_id).await?;
    let existing = current.metadata.as_object().map_or(0, serde_json::Map::len);
    let known = current
        .metadata
        .as_object()
        .is_some_and(|map| map.contains_key(&key));
    if !known && existing >= METADATA_KEYS_MAX {
        return Err(validation(format!("metadata cannot exceed {METADATA_KEYS_MAX} keys")).into());
    }

    let metadata = repo
        .set_metadata_key(workspace_id, current.id(), &key, &req.value)
        .await
        .map_err(repo_err)?;
    let row = repo
        .get(workspace_id, current.id())
        .await
        .map_err(repo_err)?;
    Ok(Json(MetadataResponse {
        metadata,
        issue_revision: Some(row.revision),
    }))
}

/// `DELETE /api/issues/:id/metadata/:key`
pub(crate) async fn delete_metadata_key(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((raw_id, key)): Path<(String, String)>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Json<MetadataResponse>> {
    validate_metadata_key(&key)?;
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let repo = issue_repo(&state);
    let current = load_issue(&repo, workspace_id, &raw_id).await?;
    let metadata = repo
        .delete_metadata_key(workspace_id, current.id(), &key)
        .await
        .map_err(repo_err)?;
    Ok(Json(MetadataResponse {
        metadata,
        issue_revision: None,
    }))
}

/// `PUT /api/issues/:id/properties/:propertyId`
///
/// 本仓把 property 值存在 `issue.properties` JSONB 里（以 `:propertyId` 作为 key），
/// 没有 `property` 定义表，因此不做「定义存在 / 类型匹配 / 归档」校验（docs/11 §5）。
pub(crate) async fn set_property(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((raw_id, property_id)): Path<(String, String)>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    Json(req): Json<ValueRequest>,
) -> ApiResult<Json<PropertiesResponse>> {
    if property_id.trim().is_empty() {
        return Err(validation("property id is required").into());
    }
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let repo = issue_repo(&state);
    let current = load_issue(&repo, workspace_id, &raw_id).await?;
    let properties = repo
        .set_property(workspace_id, current.id(), &property_id, &req.value)
        .await
        .map_err(repo_err)?;
    let row = repo
        .get(workspace_id, current.id())
        .await
        .map_err(repo_err)?;
    Ok(Json(PropertiesResponse {
        properties,
        issue_revision: Some(row.revision),
    }))
}

/// `DELETE /api/issues/:id/properties/:propertyId`
pub(crate) async fn delete_property(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((raw_id, property_id)): Path<(String, String)>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Json<PropertiesResponse>> {
    if property_id.trim().is_empty() {
        return Err(validation("property id is required").into());
    }
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let repo = issue_repo(&state);
    let current = load_issue(&repo, workspace_id, &raw_id).await?;
    let properties = repo
        .delete_property(workspace_id, current.id(), &property_id)
        .await
        .map_err(repo_err)?;
    Ok(Json(PropertiesResponse {
        properties,
        issue_revision: None,
    }))
}
