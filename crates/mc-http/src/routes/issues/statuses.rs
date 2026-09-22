//! `/api/issue-statuses*` 目录端点（从 `issues.rs` 拆出，R7 单文件 800 行上限）。

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::routes::invitations::{require_workspace_admin, require_workspace_member};
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use mc_repos::issue::IssueStatusRow;
use mc_repos::issue_status::{
    parse_category, validate_key, IssueStatusUpdate, NewIssueStatus, KEY_MAX_LEN,
};
use std::sync::Arc;

use super::context::{parse_target_id, resolve_workspace, status_repo, WorkspaceQuery};
use super::dto::{
    CreateIssueStatusRequest, IssueStatusDto, ReorderStatusesRequest, StatusListResponse,
    UpdateIssueStatusRequest,
};
use super::helpers::{status_repo_err, validation};

// ---------------------------------------------------------------------------
// status 目录端点
// ---------------------------------------------------------------------------

pub(crate) fn status_list(rows: &[IssueStatusRow]) -> StatusListResponse {
    let statuses: Vec<IssueStatusDto> = rows.iter().map(IssueStatusDto::from_row).collect();
    StatusListResponse {
        total: statuses.len(),
        statuses,
        categories: vec!["open", "closed"],
    }
}

/// `GET /api/issue-statuses`（读路径 self-heal：先 `ensure_defaults`，与上游一致）
pub(crate) async fn list_statuses(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Json<StatusListResponse>> {
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let repo = status_repo(&state);
    repo.ensure_defaults(workspace_id)
        .await
        .map_err(status_repo_err)?;
    let rows = repo.list(workspace_id).await.map_err(status_repo_err)?;
    Ok(Json(status_list(&rows)))
}

/// `POST /api/issue-statuses`
pub(crate) async fn create_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    Json(req): Json<CreateIssueStatusRequest>,
) -> ApiResult<Response> {
    let name = req.name.trim().to_string();
    if name.is_empty() || name.len() > KEY_MAX_LEN {
        return Err(validation(format!("name must be 1..={KEY_MAX_LEN} characters")).into());
    }
    let category = parse_category(req.category.trim())
        .ok_or_else(|| validation("category must be open or closed"))?;
    let key = match req.key.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
        Some(raw) => Some(
            validate_key(raw).ok_or_else(|| validation(format!("invalid status key: {raw}")))?,
        ),
        None => None,
    };

    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    // 上游（`issue_status.go` / router.go L2051 注释）：写路径限 owner/admin
    require_workspace_admin(&state, workspace_id, user.id()).await?;

    // `description` / `color` 本仓无处可存（无新迁移），接受但忽略
    let input = NewIssueStatus {
        name,
        key,
        category,
        icon: req.icon,
        position: req.position,
    };
    let row = status_repo(&state)
        .create(workspace_id, &input)
        .await
        .map_err(status_repo_err)?;
    Ok((StatusCode::CREATED, Json(IssueStatusDto::from_row(&row))).into_response())
}

/// `PATCH /api/issue-statuses/:id`
pub(crate) async fn update_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    Json(req): Json<UpdateIssueStatusRequest>,
) -> ApiResult<Json<IssueStatusDto>> {
    let status_id = parse_target_id("status id", &raw_id)?;
    let name = req
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_string);
    if req.name.is_some() && name.is_none() {
        return Err(validation("name must not be empty").into());
    }
    let category = match req
        .category
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        Some(raw) => {
            Some(parse_category(raw).ok_or_else(|| validation("category must be open or closed"))?)
        }
        None => None,
    };

    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_admin(&state, workspace_id, user.id()).await?;

    let patch = IssueStatusUpdate {
        name,
        category,
        icon: req.icon,
        position: req.position,
    };
    let row = status_repo(&state)
        .update(workspace_id, status_id, &patch)
        .await
        .map_err(status_repo_err)?;
    Ok(Json(IssueStatusDto::from_row(&row)))
}

/// `DELETE /api/issue-statuses/:id`
///
/// 本仓没有 `archived_at` 列 → 硬删（上游是归档）。内置 key 与仍在使用的 key 都会被
/// 仓储层拒绝（409，docs/11 §5）。
pub(crate) async fn delete_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<StatusCode> {
    let status_id = parse_target_id("status id", &raw_id)?;
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_admin(&state, workspace_id, user.id()).await?;
    status_repo(&state)
        .delete(workspace_id, status_id)
        .await
        .map_err(status_repo_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `PATCH /api/issue-statuses/reorder`
#[allow(clippy::cast_precision_loss)]
pub(crate) async fn reorder_statuses(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    Json(req): Json<ReorderStatusesRequest>,
) -> ApiResult<Json<StatusListResponse>> {
    if req.ids.is_empty() {
        return Err(validation("ids must not be empty").into());
    }
    // `category` / `include_system` 接受但忽略：本仓 position 全局唯一，不分分类
    let _ = (&req.category, req.include_system);

    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_admin(&state, workspace_id, user.id()).await?;

    let mut order = Vec::with_capacity(req.ids.len());
    for (index, raw) in req.ids.iter().enumerate() {
        order.push((parse_target_id("ids", raw)?, index as f64));
    }
    let repo = status_repo(&state);
    repo.reorder(workspace_id, &order)
        .await
        .map_err(status_repo_err)?;
    let rows = repo.list(workspace_id).await.map_err(status_repo_err)?;
    Ok(Json(status_list(&rows)))
}
