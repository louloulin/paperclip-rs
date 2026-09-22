//! Workspace CRUD 路由：list / get / create / update / delete。
//!
//! 协议与 multica upstream `handler/workspace.go` 对齐：
//! - `GET /api/workspaces` 列出当前 user 是 member 的 workspace。
//! - `GET /api/workspaces/{id}` 取详情。
//! - `POST /api/workspaces` 新建（current user 顺位为 owner）。
//! - `PUT /api/workspaces/{id}` / `PATCH /api/workspaces/{id}` 修改名字/描述。
//! - `DELETE /api/workspaces/{id}` 仅 owner 可删除（删前权限校验放在 service 层；M2+）。
//!
//! M1 简化：所有权限由调用方传 `X-Multica-User-Id` 头携带身份；完整角色校验在 M2。

use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use mc_core::id::Id;
use mc_core::slug::Slug;
use mc_errors::Error;
use mc_repos::{NewWorkspace, WorkspaceRepo, WorkspaceRow, WorkspaceUpdate};

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct WorkspaceResponse {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub avatar_url: Option<String>,
    pub archived: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl WorkspaceResponse {
    pub fn from_row(row: &WorkspaceRow) -> Self {
        Self {
            id: row.id.as_string(),
            name: row.name.clone(),
            slug: row.slug.clone(),
            description: row.description.clone(),
            avatar_url: row.avatar_url.clone(),
            archived: row.is_archived(),
            created_at: row.created_at.to_rfc3339(),
            updated_at: row.updated_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ListWorkspacesQuery {
    pub archived: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateWorkspaceRequest {
    pub name: String,
    pub slug: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub settings: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Default)]
pub struct UpdateWorkspaceRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub avatar_url: Option<Option<String>>,
    pub settings: Option<serde_json::Value>,
    pub archived: Option<bool>,
}

fn actor_user_id(headers: &HeaderMap) -> Result<Id, ApiError> {
    headers
        .get("x-multica-user-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| Id::parse(s))
        .transpose()
        .map_err(|e| ApiError(Error::Unauthorized { message: e.to_string() }))?
        .ok_or_else(|| ApiError(Error::Unauthorized { message: "no user id".into() }))
}

/// `GET /api/workspaces`
pub async fn list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<ListWorkspacesQuery>,
) -> Result<Json<Vec<WorkspaceResponse>>, ApiError> {
    let user_id = actor_user_id(&headers)?;
    let rows = state
        .repos
        .workspaces
        .list(mc_repos::workspace::WorkspaceFilter {
            member_user_id: Some(user_id),
            archived: q.archived,
            limit: Some(200),
            ..Default::default()
        })
        .await
        .map_err(|e| ApiError(Error::Internal(format!("list workspaces: {e}"))))?;
    Ok(Json(rows.iter().map(WorkspaceResponse::from_row).collect()))
}

/// `GET /api/workspaces/{id}`
pub async fn get(
    State(state): State<Arc<AppState>>,
    _headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<WorkspaceResponse>, ApiError> {
    let id = Id::parse(&id)
        .map_err(|e| ApiError(Error::Validation {
            message: e.to_string(),
            details: vec![],
        }))?;
    let row = state
        .repos
        .workspaces
        .get(id)
        .await
        .map_err(|e| match e {
            mc_repos::RepoError::NotFound => ApiError(Error::WorkspaceNotFound(id.as_string())),
            other => ApiError(Error::Internal(format!("get workspace: {other}"))),
        })?;
    Ok(Json(WorkspaceResponse::from_row(&row)))
}

/// `POST /api/workspaces`
pub async fn create(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<CreateWorkspaceRequest>,
) -> Result<Json<WorkspaceResponse>, ApiError> {
    let user_id = actor_user_id(&headers)?;
    if body.name.trim().is_empty() {
        return Err(ApiError(Error::Validation {
            message: "name is required".into(),
            details: vec![],
        }));
    }
    let slug = Slug::parse(&body.slug).map_err(|e| ApiError(Error::Validation {
        message: e.to_string(),
        details: vec![],
    }))?;
    let row = state
        .repos
        .workspaces
        .create(
            user_id,
            NewWorkspace {
                id: None,
                name: body.name,
                slug,
                description: body.description,
                avatar_url: body.avatar_url,
                settings: body.settings,
            },
        )
        .await
        .map_err(|e| match e {
            mc_repos::RepoError::Conflict(msg) => ApiError(Error::Conflict { message: msg }),
            other => ApiError(Error::Internal(format!("create workspace: {other}"))),
        })?;
    Ok(Json(WorkspaceResponse::from_row(&row)))
}

/// `PUT /api/workspaces/{id}` 与 `PATCH /api/workspaces/{id}` 二选一即可。
pub async fn update(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<UpdateWorkspaceRequest>,
) -> Result<Json<WorkspaceResponse>, ApiError> {
    let _ = actor_user_id(&headers)?;
    let id = Id::parse(&id).map_err(|e| ApiError(Error::Validation {
        message: e.to_string(),
        details: vec![],
    }))?;

    // archive / restore flow: we pass the user's flag straight through; M2+
    // will require admin role for `archived=true` and owner-only for restore.
    let row = state
        .repos
        .workspaces
        .update(
            id,
            WorkspaceUpdate {
                name: body.name,
                description: body.description,
                avatar_url: body.avatar_url,
                settings: body.settings,
                archived: body.archived,
            },
        )
        .await
        .map_err(|e| match e {
            mc_repos::RepoError::NotFound => ApiError(Error::WorkspaceNotFound(id.as_string())),
            other => ApiError(Error::Internal(format!("update workspace: {other}"))),
        })?;
    Ok(Json(WorkspaceResponse::from_row(&row)))
}

/// `POST /api/workspaces/{id}/leave` — 当前 user 离开 workspace。
pub async fn leave(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user_id = actor_user_id(&headers)?;
    let workspace_id = Id::parse(&id).map_err(|e| ApiError(Error::Validation {
        message: e.to_string(),
        details: vec![],
    }))?;
    let member = state
        .repos
        .members
        .find(workspace_id, user_id)
        .await
        .map_err(|e| ApiError(Error::Internal(format!("find member: {e}"))))?
        .ok_or_else(|| ApiError(Error::NotFound { resource: "member".into() }))?;
    state
        .repos
        .members
        .delete(member.id)
        .await
        .map_err(|e| match e {
            mc_repos::RepoError::Invalid(msg) => ApiError(Error::Validation {
                message: msg,
                details: vec![],
            }),
            mc_repos::RepoError::NotFound => ApiError(Error::NotFound { resource: "member".into() }),
            other => ApiError(Error::Internal(format!("leave workspace: {other}"))),
        })?;
    Ok(Json(serde_json::json!({ "ok": true })))
}
