//! Workspace member 路由：list / update / delete。
//!
//! 协议与 multica upstream `handler/workspace.go` 对齐：
//! - `GET /api/workspaces/{id}/members` 列出 workspace 下的所有 member。
//! - `PATCH /api/workspaces/{id}/members/{memberId}` 调整 role。
//! - `DELETE /api/workspaces/{id}/members/{memberId}` 移除 member。
//! - `POST /api/workspaces/{id}/members` 等价于创建邀请 —— 实际委托给
//!   invitation 路由（POST /api/workspaces/{id}/invitations），此处只占位。

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use mc_core::id::Id;
use mc_core::workspace::WorkspaceRole;
use mc_errors::Error;
use mc_repos::{MemberRepo, MemberRow, MemberUpdate};

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct MemberResponse {
    pub id: String,
    pub workspace_id: String,
    pub user_id: String,
    pub role: String,
    pub created_at: String,
    pub updated_at: String,
}

impl MemberResponse {
    pub fn from_row(row: &MemberRow) -> Self {
        Self {
            id: row.id.as_string(),
            workspace_id: row.workspace_id.as_string(),
            user_id: row.user_id.as_string(),
            role: row.role.as_str().to_string(),
            created_at: row.created_at.to_rfc3339(),
            updated_at: row.updated_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdateMemberRequest {
    pub role: WorkspaceRole,
}

fn parse_path_ids(workspace_id: &str, member_id: &str) -> Result<(Id, Id), ApiError> {
    Ok((
        Id::parse(workspace_id).map_err(|e| ApiError(Error::Validation {
            message: e.to_string(),
            details: vec![],
        }))?,
        Id::parse(member_id).map_err(|e| ApiError(Error::Validation {
            message: e.to_string(),
            details: vec![],
        }))?,
    ))
}

/// `GET /api/workspaces/{id}/members`
pub async fn list(
    State(state): State<Arc<AppState>>,
    _headers: HeaderMap,
    Path(workspace_id): Path<String>,
) -> Result<Json<Vec<MemberResponse>>, ApiError> {
    let workspace_id = Id::parse(&workspace_id).map_err(|e| ApiError(Error::Validation {
        message: e.to_string(),
        details: vec![],
    }))?;
    let rows = state
        .repos
        .members
        .list_for_workspace(workspace_id)
        .await
        .map_err(|e| ApiError(Error::Internal(format!("list members: {e}"))))?;
    Ok(Json(rows.iter().map(MemberResponse::from_row).collect()))
}

/// `PATCH /api/workspaces/{id}/members/{memberId}`
pub async fn update(
    State(state): State<Arc<AppState>>,
    _headers: HeaderMap,
    Path((workspace_id, member_id)): Path<(String, String)>,
    Json(body): Json<UpdateMemberRequest>,
) -> Result<Json<MemberResponse>, ApiError> {
    let (workspace_id, member_id) = parse_path_ids(&workspace_id, &member_id)?;
    let _ = workspace_id; // member uniqueness enforced by id + service logic

    let updated = state
        .repos
        .members
        .update(
            member_id,
            MemberUpdate {
                role: Some(body.role),
            },
        )
        .await
        .map_err(|e| match e {
            mc_repos::RepoError::Invalid(msg) => ApiError(Error::Validation {
                message: msg,
                details: vec![],
            }),
            mc_repos::RepoError::NotFound => ApiError(Error::NotFound { resource: "member".into() }),
            other => ApiError(Error::Internal(format!("update member: {other}"))),
        })?;
    Ok(Json(MemberResponse::from_row(&updated)))
}

/// `DELETE /api/workspaces/{id}/members/{memberId}`
pub async fn delete(
    State(state): State<Arc<AppState>>,
    _headers: HeaderMap,
    Path((workspace_id, member_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (workspace_id, member_id) = parse_path_ids(&workspace_id, &member_id)?;
    let _ = workspace_id; // uniqueness check could be added in M2

    state
        .repos
        .members
        .delete(member_id)
        .await
        .map_err(|e| match e {
            mc_repos::RepoError::Invalid(msg) => ApiError(Error::Validation {
                message: msg,
                details: vec![],
            }),
            mc_repos::RepoError::NotFound => ApiError(Error::NotFound { resource: "member".into() }),
            other => ApiError(Error::Internal(format!("delete member: {other}"))),
        })?;
    Ok(Json(serde_json::json!({ "ok": true })))
}
