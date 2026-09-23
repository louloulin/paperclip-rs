//! `/api/projects/:id/resources*`：list / create / update / delete。
//!
//! 上游对照：`handler/project_resource.go` 的 `ListProjectResources` /
//! `CreateProjectResource` / `UpdateProjectResource` / `DeleteProjectResource` /
//! `findLocalDirectoryConflict`。
//!
//! 四条路由都是**单形态**（上游 `r.Route("/{id}/resources")` 下的 plain 注册），
//! 不要加尾斜杠别名（`EXTRA_ALIAS` 会被门 ⑦ 的第二条命令告警）。
//!
//! 更新路径的语义最密，逐条对齐上游：
//! - `resource_type` **不可改**（改类型 = 删了重加）。
//! - `resource_ref` 缺失 = 不动；出现即校验；`local_directory` 的「仅 label 不同」算
//!   重命名，那种请求**不**走 worktree 能力门（子客户端用重发 ref 完成改名）。
//! - `label`：缺失 = 保持；`null` / `""` = 清空。
//! - `position`：缺失 = 保持；`null` = 保持（上游只认 `*int32` 非空）；整数 = 设置。
//! - `local_directory` 的 label 有两个家（顶层列 + ref 内嵌副本），写入时收敛两者。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use mc_repos::project_resource::{NewProjectResource, ProjectResourceRow};
use serde_json::{Map, Value as JsonValue};

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::routes::issues::WorkspaceQuery;
use crate::state::AppState;

use super::dto::{
    CreateProjectResourceRequest, LocalDirectoryRef, ProjectResourceListResponse,
    ProjectResourceResponse,
};
use super::helpers::{
    conflict, load_project_scoped, not_found, parse_body, parse_uuid, project_repo, resource_repo,
    resource_write_err, validation,
};
use super::resource_ref::{
    local_directory_ref_differs_only_by_label, local_directory_ref_label,
    require_worktree_capable_daemon, validate_and_normalize_resource_ref,
    with_local_directory_ref_label,
};

/// `RESOURCE_TYPE_LOCAL_DIRECTORY`（上游字面量 `"local_directory"`）。
const RESOURCE_TYPE_LOCAL_DIRECTORY: &str = "local_directory";

const RESOURCE_UNIQUE_MESSAGE: &str = "this resource is already attached to the project";
const LOCAL_DIRECTORY_CREATE_CONFLICT: &str = "this daemon already has a local_directory attached to the project; remove it before adding another";
const LOCAL_DIRECTORY_UPDATE_CONFLICT: &str =
    "another local_directory on this daemon is already attached to the project";

// ---------------------------------------------------------------------------
// 集合
// ---------------------------------------------------------------------------

/// `GET /api/projects/:id/resources`。
pub(crate) async fn list_resources(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    Path(raw_project_id): Path<String>,
    user: AuthUser,
) -> ApiResult<Json<ProjectResourceListResponse>> {
    let (_, project) =
        load_project_scoped(&state, &headers, &query, &user, &raw_project_id).await?;
    let rows = resource_repo(&state)
        .list(project.id)
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "failed to list project resources");
            not_found("project")
        })?;
    let resources: Vec<ProjectResourceResponse> =
        rows.iter().map(ProjectResourceResponse::from_row).collect();
    let total = resources.len();
    Ok(Json(ProjectResourceListResponse { resources, total }))
}

/// `POST /api/projects/:id/resources`。
pub(crate) async fn create_resource(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    Path(raw_project_id): Path<String>,
    user: AuthUser,
    body: Bytes,
) -> ApiResult<Response> {
    let (_, project) =
        load_project_scoped(&state, &headers, &query, &user, &raw_project_id).await?;
    let req: CreateProjectResourceRequest = parse_body(&body)?;

    let resource_type = req.resource_type.trim().to_string();
    if resource_type.is_empty() {
        return Err(validation("resource_type is required").into());
    }
    let normalized_ref =
        validate_and_normalize_resource_ref(&resource_type, req.resource_ref.as_ref())?;

    if find_local_directory_conflict(&state, project.id, &resource_type, &normalized_ref, None)
        .await?
    {
        return Err(conflict(LOCAL_DIRECTORY_CREATE_CONFLICT).into());
    }

    if let Err(rejected) = require_worktree_capable_daemon(
        &state,
        project.workspace_id(),
        &resource_type,
        &normalized_ref,
    )
    .await
    {
        return Ok(*rejected);
    }

    let position = match req.position {
        Some(position) => position,
        None => i32::try_from(resource_repo(&state).count(project.id).await.unwrap_or(0))
            .unwrap_or(i32::MAX),
    };

    let new = NewProjectResource {
        project_id: project.id,
        workspace_id: project.workspace_id,
        resource_type,
        resource_ref: normalized_ref,
        label: trimmed_label(req.label.as_deref()),
        position,
        created_by: Some(user.id().0),
    };
    let row = match resource_repo(&state).create(&new).await {
        Ok(row) => row,
        Err(err) => {
            return Err(
                resource_write_err(err, "create project resource", RESOURCE_UNIQUE_MESSAGE).into(),
            )
        }
    };
    Ok((
        StatusCode::CREATED,
        Json(ProjectResourceResponse::from_row(&row)),
    )
        .into_response())
}

/// 上游 `findLocalDirectoryConflict`：`(project, daemon)` 至多一条 `local_directory`。
///
/// DB 的 `UNIQUE (project_id, resource_type, resource_ref)` 只挡「ref JSON 逐字相等」，
/// 同一 daemon 换个 `local_path`（甚至只是 label 打错）就漏过去了 ⇒ 应用层再按
/// `daemon_id` 查一遍。`exclude` 让更新路径忽略自己那一行。
async fn find_local_directory_conflict(
    state: &AppState,
    project_id: uuid::Uuid,
    resource_type: &str,
    normalized_ref: &JsonValue,
    exclude: Option<uuid::Uuid>,
) -> Result<bool, crate::error::ApiError> {
    if resource_type != RESOURCE_TYPE_LOCAL_DIRECTORY {
        return Ok(false);
    }
    let incoming: LocalDirectoryRef = serde_json::from_value(normalized_ref.clone())
        .map_err(|err| crate::error::ApiError(validation(err.to_string())))?;
    let rows = resource_repo(state).list(project_id).await.map_err(|err| {
        tracing::error!(error = %err, "failed to check existing resources");
        crate::error::ApiError(mc_errors::Error::Internal(
            "failed to check existing resources".into(),
        ))
    })?;
    for row in rows {
        if row.resource_type != RESOURCE_TYPE_LOCAL_DIRECTORY {
            continue;
        }
        if exclude == Some(row.id) {
            continue;
        }
        let Ok(existing) = serde_json::from_value::<LocalDirectoryRef>(row.resource_ref.clone())
        else {
            // 解不开的旧行当不存在（上游 `continue`）。
            continue;
        };
        if existing.daemon_id == incoming.daemon_id {
            return Ok(true);
        }
    }
    Ok(false)
}

// ---------------------------------------------------------------------------
// 单体
// ---------------------------------------------------------------------------

/// `PUT /api/projects/:id/resources/:resourceId`。
#[allow(clippy::too_many_lines)]
pub(crate) async fn update_resource(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    Path((raw_project_id, raw_resource_id)): Path<(String, String)>,
    user: AuthUser,
    body: Bytes,
) -> ApiResult<Response> {
    let (_, project) =
        load_project_scoped(&state, &headers, &query, &user, &raw_project_id).await?;
    let resource_id = parse_uuid("resource id", &raw_resource_id)?;
    let existing = load_resource(&state, &project, resource_id).await?;

    let raw: Map<String, JsonValue> = serde_json::from_slice::<JsonValue>(&body)
        .ok()
        .and_then(|value| match value {
            JsonValue::Object(map) => Some(map),
            _ => None,
        })
        .ok_or_else(|| validation("invalid request body"))?;
    // `resource_type` 不可改：上游根本不读这个键；传了就静默忽略（不是 400）。
    let _ = &raw;

    let mut next_ref = existing.resource_ref.clone();
    let raw_ref = raw.get("resource_ref");
    if let Some(raw_ref) = raw_ref {
        next_ref = validate_and_normalize_resource_ref(&existing.resource_type, Some(raw_ref))?;
    }
    let ref_provided = raw_ref.is_some();

    if find_local_directory_conflict(
        &state,
        project.id,
        &existing.resource_type,
        &next_ref,
        Some(existing.id),
    )
    .await?
    {
        return Err(conflict(LOCAL_DIRECTORY_UPDATE_CONFLICT).into());
    }

    // ≤ v0.4.28 的客户端改名 = 重发整个 ref，所以"ref 出现了"不等于"execution_mode 被动过"。
    let ref_rename_only = ref_provided
        && existing.resource_type == RESOURCE_TYPE_LOCAL_DIRECTORY
        && local_directory_ref_differs_only_by_label(&next_ref, &existing.resource_ref);

    if ref_provided && !ref_rename_only {
        if let Err(rejected) = require_worktree_capable_daemon(
            &state,
            project.workspace_id(),
            &existing.resource_type,
            &next_ref,
        )
        .await
        {
            return Ok(*rejected);
        }
    }

    let mut next_label = existing.label.clone();
    let mut label_cleared = false;
    if let Some(raw_label) = raw.get("label") {
        let label: Option<String> = serde_json::from_value(raw_label.clone())
            .map_err(|_| validation("label must be a string or null"))?;
        match label {
            Some(label) if !label.trim().is_empty() => {
                next_label = Some(label.trim().to_string());
            }
            _ => {
                next_label = None;
                label_cleared = true;
            }
        }
    } else if ref_rename_only {
        // 没带 label 字段、而 ref 只差内嵌 label ⇒ 那就是改名，把它跟进顶层列，
        // 否则读列的客户端会一直显示改名前的旧名字。
        let next_ref_label = local_directory_ref_label(&next_ref);
        if next_ref_label != local_directory_ref_label(&existing.resource_ref) {
            if next_ref_label.is_empty() {
                next_label = None;
                label_cleared = true;
            } else {
                next_label = Some(next_ref_label);
            }
        }
    }

    let mut next_position = existing.position;
    if let Some(raw_position) = raw.get("position") {
        let position: Option<i32> = serde_json::from_value(raw_position.clone())
            .map_err(|_| validation("position must be an integer"))?;
        if let Some(position) = position {
            next_position = position;
        }
    }

    // 把最终 label 镜像回 ref 的内嵌副本：显式清空也要同步删除，否则界面回退到
    // 用户刚删掉的名字。从没设过名字的行（列 NULL、ref 里也没有）保持原样。
    if existing.resource_type == RESOURCE_TYPE_LOCAL_DIRECTORY {
        let name = match &next_label {
            Some(label) => Some(label.clone()),
            None if label_cleared => None,
            None => Some(local_directory_ref_label(&existing.resource_ref))
                .filter(|stored| !stored.is_empty()),
        };
        if ref_provided || next_label.is_some() || label_cleared {
            next_ref = with_local_directory_ref_label(&next_ref, name.as_deref())?;
        }
    }

    let row = match resource_repo(&state)
        .update(
            existing.id,
            project.workspace_id(),
            &next_ref,
            next_label.as_deref(),
            next_position,
        )
        .await
    {
        Ok(row) => row,
        Err(err) => {
            return Err(
                resource_write_err(err, "update project resource", RESOURCE_UNIQUE_MESSAGE).into(),
            )
        }
    };
    Ok(Json(ProjectResourceResponse::from_row(&row)).into_response())
}

/// `DELETE /api/projects/:id/resources/:resourceId`。
pub(crate) async fn delete_resource(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    Path((raw_project_id, raw_resource_id)): Path<(String, String)>,
    user: AuthUser,
) -> ApiResult<StatusCode> {
    let (_, project) =
        load_project_scoped(&state, &headers, &query, &user, &raw_project_id).await?;
    let resource_id = parse_uuid("resource id", &raw_resource_id)?;
    let existing = load_resource(&state, &project, resource_id).await?;
    match resource_repo(&state)
        .delete(existing.id, project.workspace_id())
        .await
    {
        Ok(_) => Ok(StatusCode::NO_CONTENT),
        Err(err) => {
            tracing::error!(error = %err, "failed to delete project resource");
            Err(mc_errors::Error::Internal("failed to delete project resource".into()).into())
        }
    }
}

// ---------------------------------------------------------------------------
// 内部
// ---------------------------------------------------------------------------

/// 上游：`GetProjectResourceInWorkspace` 查不到、或 `project_id` 不匹配本项目 → 404
/// `project resource not found`。
async fn load_resource(
    state: &AppState,
    project: &mc_repos::project::ProjectRow,
    resource_id: mc_core::Id,
) -> Result<ProjectResourceRow, crate::error::ApiError> {
    let row = resource_repo(state)
        .get_in_workspace(resource_id.0, project.workspace_id())
        .await
        .map_err(|err| crate::error::ApiError(mc_errors::Error::Database(err.to_string())))?
        .filter(|row| row.project_id == project.id)
        .ok_or_else(|| crate::error::ApiError(not_found("project resource")))?;
    Ok(row)
}

/// 上游 label 归一化：`nil` 或 trim 后为空 → NULL，否则 trim 后落库。
fn trimmed_label(label: Option<&str>) -> Option<String> {
    label
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_string)
}

/// 引用 `project_repo`，避免"未使用导入"（其余 handler 都走 `project_repo`）。
#[allow(dead_code)]
fn _keep_project_repo(state: &AppState) -> mc_repos::project::ProjectRepo {
    project_repo(state)
}
