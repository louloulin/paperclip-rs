//! `/api/projects` 集合与单体端点：list / get / create / update / delete。
//!
//! 上游对照：`handler/project.go` 的 `ListProjects` / `GetProject` / `CreateProject` /
//! `UpdateProject` / `DeleteProject`（962 行文件里的 5 个 handler）。
//!
//! 关键语义（逐条对齐上游，别"顺手优化"）：
//! - **update 的三态**：`title` / `status` / `priority` 是 `*string` —— 缺失**或** `null`
//!   都是"不动"（SQL 里走 `COALESCE`）；`description` / `icon` / `lead_type` / `lead_id` /
//!   `start_date` / `due_date` 按 `rawFields` 的 key 存在性分三态：缺失 = 不动、显式
//!   `null` = 清空、`""`（日期）= 清空。
//! - **create 的 `title`**：只判 `== ""`（上游不 trim，别自作主张 trim）。
//! - **create 带 `resources[]`**：先逐条预校验（含 bundled 内同 daemon 去重 + worktree
//!   能力门），再在**一个事务**里落 project + 全部 resource（`create_with_resources`）。
//! - **delete**：`loadProject` → 角色门（owner/admin，非成员 404 `project not found`）→
//!   事务级联（锁行 → 清 `chat_session.project_id` → 删 project 作用域的 issue view →
//!   删 project）。
//!
//! 本片**不发** realtime 事件（上游 publish 6 处；跨片缺口见 `docs/39` §4.8）。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use mc_core::Id;
use mc_errors::Error;
use mc_repos::project::{NewProject, ProjectUpdate, WriteError};
use mc_repos::project_resource::NewProjectResource;
use mc_repos::RepoError;
use serde::Deserialize;
use serde_json::Value as JsonValue;
use uuid::Uuid;

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::routes::invitations::require_workspace_member;
use crate::routes::issues::{resolve_workspace, WorkspaceQuery};
use crate::state::AppState;

use super::dto::{
    CreateProjectEcho, CreateProjectRequest, LocalDirectoryRef, ProjectListResponse,
    ProjectResourceResponse, ProjectResponse, UpdateProjectRequest,
};
use super::helpers::{
    conflict, issue_stats_map, not_found, parse_body, parse_calendar_date, parse_uuid,
    project_repo, project_write_err, repo_err, require_project_admin, resource_count_map,
    validate_enum, validation,
};
use super::resource_ref::{require_worktree_capable_daemon, validate_and_normalize_resource_ref};

/// 上游 `validProjectStatuses`（= `project.status` 的 CHECK，迁移 `034`）。
const VALID_STATUSES: &[&str] = &["planned", "in_progress", "paused", "completed", "cancelled"];

/// 上游 `validProjectPriorities`（= `project.priority` 的 CHECK，迁移 `035`）。
const VALID_PRIORITIES: &[&str] = &["urgent", "high", "medium", "low", "none"];

const DEFAULT_STATUS: &str = "planned";
const DEFAULT_PRIORITY: &str = "none";

/// `GET /api/projects` 的查询串：workspace 选择器 + `status` / `priority` 过滤。
///
/// 不用 `#[serde(flatten)]` 复用 [`WorkspaceQuery`]：`serde_urlencoded` 不支持 flatten。
#[derive(Debug, Default, Deserialize)]
pub(crate) struct ListProjectsQuery {
    pub workspace_id: Option<String>,
    pub workspace_slug: Option<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
}

impl ListProjectsQuery {
    fn workspace(&self) -> WorkspaceQuery {
        WorkspaceQuery {
            workspace_id: self.workspace_id.clone(),
            workspace_slug: self.workspace_slug.clone(),
        }
    }

    /// 上游 `r.URL.Query().Get(...) != ""`：空串等于不传。
    fn filter(raw: &Option<String>) -> Option<&str> {
        raw.as_deref().filter(|value| !value.is_empty())
    }
}

// ---------------------------------------------------------------------------
// 集合
// ---------------------------------------------------------------------------

/// `GET /api/projects`（+ `/api/projects/`）。
pub(crate) async fn list_projects(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ListProjectsQuery>,
    user: AuthUser,
) -> ApiResult<Json<ProjectListResponse>> {
    let workspace_id = resolve_workspace(&state, &headers, &query.workspace()).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let rows = project_repo(&state)
        .list(
            workspace_id,
            ListProjectsQuery::filter(&query.status),
            ListProjectsQuery::filter(&query.priority),
        )
        .await
        .map_err(repo_err)?;

    let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
    let stats = issue_stats_map(&state, workspace_id, &ids).await;
    let counts = resource_count_map(&state, &ids).await;

    let projects: Vec<ProjectResponse> = rows
        .iter()
        .map(|row| {
            let mut resp = ProjectResponse::from_row(row);
            if let Some((total, done)) = stats.get(&row.id) {
                resp.issue_count = *total;
                resp.done_count = *done;
            }
            resp.resource_count = counts.get(&row.id).copied().unwrap_or(0);
            resp
        })
        .collect();

    let total = projects.len();
    Ok(Json(ProjectListResponse { projects, total }))
}

/// `POST /api/projects`（+ `/api/projects/`）。
///
/// 返回 `Response` 而不是 `Json<_>`：bundled 创建里的 worktree 能力门要走 422 扁平体。
#[allow(clippy::too_many_lines)]
pub(crate) async fn create_project(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    body: Bytes,
) -> ApiResult<Response> {
    let req: CreateProjectRequest = parse_body(&body)?;
    // 上游只判 `req.Title == ""`（不 trim）。
    if req.title.is_empty() {
        return Err(validation("title is required").into());
    }
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let status = if req.status.is_empty() {
        DEFAULT_STATUS.to_string()
    } else {
        req.status.clone()
    };
    validate_enum("status", &status, VALID_STATUSES)?;
    let priority = if req.priority.is_empty() {
        DEFAULT_PRIORITY.to_string()
    } else {
        req.priority.clone()
    };
    validate_enum("priority", &priority, VALID_PRIORITIES)?;

    let lead_id = match req.lead_id.as_deref() {
        Some(raw) => Some(parse_uuid("lead_id", raw)?),
        None => None,
    };
    let start_date = parse_create_date("start_date", req.start_date.as_deref())?;
    let due_date = parse_create_date("due_date", req.due_date.as_deref())?;

    // 逐条预校验：任何非法 ref 都在开事务之前变成干净的 400；`local_directory` 额外做
    // 批内同 daemon 去重（daemon 侧按 daemon_id 取第一条匹配，同一 daemon 两行会让 agent
    // 静默写进"先回来的那条"）与 worktree 能力门。
    let mut normalized_refs: Vec<JsonValue> = Vec::with_capacity(req.resources.len());
    let mut local_dir_seen: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for (index, resource) in req.resources.iter().enumerate() {
        let resource_type = resource.resource_type.trim().to_string();
        if resource_type.is_empty() {
            return Err(validation("resources[].resource_type is required").into());
        }
        let normalized =
            validate_and_normalize_resource_ref(&resource_type, resource.resource_ref.as_ref())
                .map_err(|err| prefixed_resource_error(index, err))?;
        normalized_refs.push(normalized);
        if resource_type == "local_directory" {
            let local_dir: LocalDirectoryRef =
                serde_json::from_value(normalized_refs[index].clone())
                    .map_err(|err| prefixed_resource_error(index, validation(err.to_string())))?;
            if let Some(previous) = local_dir_seen.get(&local_dir.daemon_id) {
                return Err(validation(format!(
                    "resources[{index}]: duplicate local_directory for daemon (already at index {previous}); each daemon may attach at most one local_directory per project"
                ))
                .into());
            }
            local_dir_seen.insert(local_dir.daemon_id.clone(), index);
            if let Err(rejected) = require_worktree_capable_daemon(
                &state,
                workspace_id,
                &resource_type,
                &normalized_refs[index],
            )
            .await
            {
                return Ok(*rejected);
            }
        }
    }

    let new = NewProject {
        workspace_id,
        title: req.title.clone(),
        description: req.description.clone(),
        icon: req.icon.clone(),
        status,
        priority,
        lead_type: req.lead_type.clone(),
        lead_id,
        start_date,
        due_date,
    };
    let repo = project_repo(&state);

    // 没有 resources：保持无需事务的简单路径。
    if req.resources.is_empty() {
        let row = repo
            .create(&new)
            .await
            .map_err(|err| project_write_err(err, "create"))?;
        return Ok((StatusCode::CREATED, Json(ProjectResponse::from_row(&row))).into_response());
    }

    // 有 resources：project + 全部 resource 原子写入。
    let new_resources: Vec<NewProjectResource> = req
        .resources
        .iter()
        .enumerate()
        .map(|(index, resource)| NewProjectResource {
            // project_id / workspace_id 由 `create_with_resources` 用新落的 project 行回填。
            project_id: Uuid::nil(),
            workspace_id: Uuid::nil(),
            resource_type: resource.resource_type.trim().to_string(),
            resource_ref: normalized_refs[index].clone(),
            label: resource
                .label
                .as_deref()
                .map(str::trim)
                .filter(|label| !label.is_empty())
                .map(str::to_string),
            position: resource
                .position
                .unwrap_or_else(|| i32::try_from(index).unwrap_or(i32::MAX)),
            created_by: Some(user.id().0),
        })
        .collect();

    let (project, rows) = match repo.create_with_resources(&new, &new_resources).await {
        Ok(created) => created,
        Err(WriteError::ResourceConflict { index }) => {
            return Err(conflict(format!(
                "resources[{index}]: this resource is already attached"
            ))
            .into())
        }
        Err(err) => return Err(project_write_err(err, "create").into()),
    };

    let resources: Vec<ProjectResourceResponse> = rows
        .iter()
        .map(ProjectResourceResponse::from_row)
        .collect();
    let mut resp = ProjectResponse::from_row(&project);
    resp.resource_count = i64::try_from(resources.len()).unwrap_or(i64::MAX);
    Ok((
        StatusCode::CREATED,
        Json(CreateProjectEcho {
            project: resp,
            resources,
        }),
    )
        .into_response())
}

/// `resources[i]: <message>`：上游逐条报错都带下标。
fn prefixed_resource_error(index: usize, err: Error) -> Error {
    match err {
        Error::Validation { message, .. } => validation(format!("resources[{index}]: {message}")),
        other => other,
    }
}

/// 创建时的日期：`None` 或空串都留 NULL，形态非法 → 400。
fn parse_create_date(
    field: &str,
    raw: Option<&str>,
) -> Result<Option<chrono::NaiveDate>, Error> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(value) => parse_calendar_date(field, value).map(Some),
    }
}

// ---------------------------------------------------------------------------
// 单体
// ---------------------------------------------------------------------------

/// `GET /api/projects/:id`（+ `/:id/`）。
pub(crate) async fn get_project(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    Path(raw_id): Path<String>,
    user: AuthUser,
) -> ApiResult<Json<ProjectResponse>> {
    let project_id = parse_uuid("project id", &raw_id)?;
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    let row = project_repo(&state)
        .get_in_workspace(project_id, workspace_id)
        .await
        .map_err(repo_err)?
        .ok_or_else(|| not_found("project"))?;

    let mut resp = ProjectResponse::from_row(&row);
    fill_counts(&state, workspace_id, row.id, &mut resp).await;
    Ok(Json(resp))
}

/// `PUT /api/projects/:id`（+ `/:id/`）。
#[allow(clippy::too_many_lines)]
pub(crate) async fn update_project(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    Path(raw_id): Path<String>,
    user: AuthUser,
    body: Bytes,
) -> ApiResult<Json<ProjectResponse>> {
    let project_id = parse_uuid("project id", &raw_id)?;
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    let repo = project_repo(&state);
    let previous = repo
        .get_in_workspace(project_id, workspace_id)
        .await
        .map_err(repo_err)?
        .ok_or_else(|| not_found("project"))?;

    let req: UpdateProjectRequest = parse_body(&body)?;

    // 基线 = 现有行；未出现的键保持基线值 ⇒ SQL 里写回原值，语义等价于"不动"。
    let mut params = ProjectUpdate {
        id: project_id.0,
        workspace_id,
        title: None,
        status: None,
        priority: None,
        description: previous.description.clone(),
        icon: previous.icon.clone(),
        lead_type: previous.lead_type.clone(),
        lead_id: previous.lead_id,
        start_date: previous.start_date,
        due_date: previous.due_date,
    };

    // `*string`：缺失或 null 都是"不动"。
    if let Some(title) = &req.title {
        params.title = Some(title.clone());
    }
    if let Some(status) = &req.status {
        validate_enum("status", status, VALID_STATUSES)?;
        params.status = Some(status.clone());
    }
    if let Some(priority) = &req.priority {
        validate_enum("priority", priority, VALID_PRIORITIES)?;
        params.priority = Some(priority.clone());
    }
    // `rawFields` 三态：`Some(None)` = 显式 null = 清空。
    if let Some(description) = &req.description {
        params.description = description.clone();
    }
    if let Some(icon) = &req.icon {
        params.icon = icon.clone();
    }
    if let Some(lead_type) = &req.lead_type {
        params.lead_type = lead_type.clone();
    }
    if let Some(lead_id) = &req.lead_id {
        params.lead_id = match lead_id {
            Some(raw) => Some(parse_uuid("lead_id", raw)?),
            None => None,
        };
    }
    if let Some(start_date) = &req.start_date {
        params.start_date = parse_update_date("start_date", start_date.as_deref())?;
    }
    if let Some(due_date) = &req.due_date {
        params.due_date = parse_update_date("due_date", due_date.as_deref())?;
    }

    let row = repo
        .update(&params)
        .await
        .map_err(|err| project_write_err(err, "update"))?;
    let mut resp = ProjectResponse::from_row(&row);
    fill_counts(&state, workspace_id, row.id, &mut resp).await;
    Ok(Json(resp))
}

/// 更新时的日期：显式 `null` 或空串都是清空，形态非法 → 400。
fn parse_update_date(
    field: &str,
    raw: Option<&str>,
) -> Result<Option<chrono::NaiveDate>, Error> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(value) => parse_calendar_date(field, value).map(Some),
    }
}

/// `DELETE /api/projects/:id`（+ `/:id/`）：owner/admin 才能删。
pub(crate) async fn delete_project(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    Path(raw_id): Path<String>,
    user: AuthUser,
) -> ApiResult<StatusCode> {
    let project_id = parse_uuid("project id", &raw_id)?;
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    let repo = project_repo(&state);
    let project = repo
        .get_in_workspace(project_id, workspace_id)
        .await
        .map_err(repo_err)?
        .ok_or_else(|| not_found("project"))?;
    require_project_admin(&state, workspace_id, user.id()).await?;

    match repo.delete_cascade(project.id, workspace_id).await {
        Ok(_) => Ok(StatusCode::NO_CONTENT),
        Err(RepoError::NotFound) => Err(not_found("project").into()),
        Err(other) => {
            tracing::error!(error = %other, "failed to delete project");
            Err(Error::Internal("failed to delete project".into()).into())
        }
    }
}

/// 单体读写的计数装饰（上游 `loadProjectIssueStats` + `loadProjectResourceCount`）。
async fn fill_counts(state: &AppState, workspace_id: Id, project_id: Uuid, resp: &mut ProjectResponse) {
    let ids = [project_id];
    if let Some((total, done)) = issue_stats_map(state, workspace_id, &ids).await.get(&project_id) {
        resp.issue_count = *total;
        resp.done_count = *done;
    }
    resp.resource_count = resource_count_map(state, &ids)
        .await
        .get(&project_id)
        .copied()
        .unwrap_or(0);
}
