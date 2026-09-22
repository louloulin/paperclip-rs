//! `/api/issues/:id` 单体端点（从 `issues.rs` 拆出，R7 单文件 800 行上限）。

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::routes::invitations::require_workspace_member;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use mc_core::priority::Priority;
use mc_core::status::{is_valid_transition, IssueStatus};
use mc_core::Id;
use mc_errors::Error;
use mc_repos::issue::{
    parse_assignee_type, parse_issue_origin, IssueRepo, IssueRow, IssueUpdate, NewIssue,
};
use mc_repos::RepoError;
use serde_json::Value as JsonValue;
use std::sync::Arc;
use uuid::Uuid;

use super::context::{
    issue_repo, load_catalog, load_issue, parse_target_id, resolve_workspace, StatusCatalog,
    WorkspaceQuery,
};
use super::dto::{
    BatchDeleteRequest, BatchUpdateRequest, CreateIssueRequest, DeletedResponse, IssueDto,
    UpdateIssueRequest, UpdatedResponse,
};
use super::helpers::{
    normalize_assignee_type, parse_attachment_ids, parse_date_patch, repo_err,
    validate_assignee_target, validation,
};

// ---------------------------------------------------------------------------
// 单体端点
// ---------------------------------------------------------------------------

/// `POST /api/issues`
#[allow(clippy::too_many_lines)]
pub(crate) async fn create_issue(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    Json(req): Json<CreateIssueRequest>,
) -> ApiResult<Response> {
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let title = req.title.trim().to_string();
    if title.is_empty() {
        return Err(validation("title is required").into());
    }
    if let Some(stage) = req.stage {
        if stage < 1 {
            return Err(validation("stage must be >= 1").into());
        }
    }

    let catalog = load_catalog(&state, workspace_id).await?;
    let repo = issue_repo(&state);

    let status = req
        .status
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map_or_else(|| "todo".to_string(), str::to_lowercase);
    if !catalog.contains_key(&status) {
        return Err(validation(format!("unknown status: {status}")).into());
    }

    let priority = match req.priority.as_deref().map(str::trim) {
        None | Some("") => Priority::None,
        Some(raw) => Priority::from_str_opt(raw)
            .ok_or_else(|| validation(format!("invalid priority: {raw}")))?,
    };

    let mut assignee_type = None;
    if let Some(raw) = req.assignee_type.as_deref().map(str::trim) {
        if !raw.is_empty() {
            let normalized = normalize_assignee_type(raw);
            assignee_type = Some(
                parse_assignee_type(normalized)
                    .ok_or_else(|| validation(format!("invalid assignee_type: {raw}")))?,
            );
        }
    }
    let assignee_id = req
        .assignee_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if assignee_type.is_some() != assignee_id.is_some() {
        return Err(validation("assignee_type and assignee_id must be provided together").into());
    }
    // 上游 `validateAssigneePair`（L3138）：两者都给时必须指向本 workspace 内真实存在的实体。
    if let (Some(kind), Some(target)) = (assignee_type, assignee_id.as_deref()) {
        validate_assignee_target(&state, workspace_id, kind, target).await?;
    }
    // 上游 `parseUUIDSliceOrBadRequest`：形态非法 → 400，且在写库之前（LUM-1410）。
    let _attachment_ids = parse_attachment_ids(&req.attachment_ids)?;

    let parent_issue_id = match req.parent_issue_id.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => {
            let parent_id = parse_target_id("parent_issue_id", raw)?;
            repo.get(workspace_id, parent_id)
                .await
                .map_err(|e| match e {
                    RepoError::NotFound => validation("parent issue not found in this workspace"),
                    other => repo_err(other),
                })?;
            Some(parent_id)
        }
    };

    let project_id = match req.project_id.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => Some(parse_target_id("project_id", raw)?),
    };

    let start_date = parse_date_patch("start_date", req.start_date.map(Some))?;
    let due_date = parse_date_patch("due_date", req.due_date.map(Some))?;

    let metadata = req
        .metadata
        .unwrap_or_else(|| JsonValue::Object(serde_json::Map::new()));
    if !metadata.is_object() {
        return Err(validation("metadata must be a JSON object").into());
    }

    // origin：两者同时出现或同时缺失；目前只接受 `quick_create`
    let origin = match (req.origin_type.as_deref(), req.origin_id.as_deref()) {
        (None, None) => None,
        (Some(kind), Some(_origin_id)) => {
            if kind != "quick_create" {
                return Err(validation(format!("unsupported origin_type: {kind}")).into());
            }
            parse_issue_origin(kind)
                .ok_or_else(|| validation(format!("invalid origin: {kind}")))?
                .into()
        }
        _ => return Err(validation("origin_type and origin_id must be provided together").into()),
    };

    let mut input = NewIssue::new(workspace_id, title, user.id().to_string());
    input.description = req.description.clone();
    input.status = status;
    input.status_name = catalog.custom_name(&input.status);
    input.priority = priority;
    input.assignee_type = assignee_type;
    input.assignee_id = assignee_id;
    input.parent_issue_id = parent_issue_id;
    input.project_id = project_id;
    input.stage = req.stage;
    input.start_date = start_date.flatten();
    input.due_date = due_date.flatten();
    input.metadata = metadata;
    input.origin = origin;

    let row = repo.create(input).await.map_err(repo_err)?;
    let dto = IssueDto::from_row(&row, &catalog);
    Ok((StatusCode::CREATED, Json(dto)).into_response())
}

/// `POST /api/issues/quick-create`（上游 `QuickCreateIssue` 的降级实现）。
///
/// 上游把快速创建交给常驻 daemon 异步落库并派发 agent 任务；本仓还没有 M3 的任务队列 /
/// daemon，因此走上游「无 daemon」的那条分支：同步建 issue（`origin = quick_create`），
/// 返回与 `POST /api/issues` 相同的 `201` + `IssueDto`。请求体沿用 `CreateIssueRequest`
/// （`title` 必填，可带 `description` / `project_id` / `priority` / `stage` 等）。
/// 偏离说明见 `docs/11-M2-ISSUE.md` §5。
pub(crate) async fn quick_create_issue(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    Json(mut req): Json<CreateIssueRequest>,
) -> ApiResult<Response> {
    // 未显式给 origin 时标记为 quick_create（`origin_type` / `origin_id` 必须同时出现）。
    if req.origin_type.is_none() && req.origin_id.is_none() {
        req.origin_type = Some("quick_create".to_string());
        req.origin_id = Some(Uuid::new_v4().to_string());
    }
    create_issue(State(state), headers, Query(query), user, Json(req)).await
}

/// `GET /api/issues/:id`（`:id` 接受 UUID 或 `LUM-1348`）
pub(crate) async fn get_issue(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Json<IssueDto>> {
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    let catalog = load_catalog(&state, workspace_id).await?;
    let row = load_issue(&issue_repo(&state), workspace_id, &raw_id).await?;
    Ok(Json(IssueDto::from_row(&row, &catalog)))
}

/// `PUT /api/issues/:id`
#[allow(clippy::too_many_lines)]
pub(crate) async fn update_issue(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    Json(req): Json<UpdateIssueRequest>,
) -> ApiResult<Json<IssueDto>> {
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let repo = issue_repo(&state);
    let current = load_issue(&repo, workspace_id, &raw_id).await?;
    let catalog = load_catalog(&state, workspace_id).await?;
    let patch = apply_update_request(&state, &repo, workspace_id, &current, &catalog, req).await?;

    if patch.is_empty() {
        // no-op：不产生 revision 自增（上游对空更新也会走一次 UPDATE，这里更保守）
        return Ok(Json(IssueDto::from_row(&current, &catalog)));
    }
    let row = repo
        .update(workspace_id, current.id(), &patch)
        .await
        .map_err(repo_err)?;
    Ok(Json(IssueDto::from_row(&row, &catalog)))
}

/// 把 `UpdateIssueRequest` 翻译成仓储层补丁（含 status 迁移、owner 配对、父子成环校验）。
#[allow(clippy::too_many_lines)]
pub(crate) async fn apply_update_request(
    state: &AppState,
    repo: &IssueRepo,
    workspace_id: Id,
    current: &IssueRow,
    catalog: &StatusCatalog,
    req: UpdateIssueRequest,
) -> Result<IssueUpdate, Error> {
    let mut patch = IssueUpdate {
        expected_revision: req.expected_revision,
        ..IssueUpdate::default()
    };

    if let Some(title) = req.title {
        if title.trim().is_empty() {
            return Err(validation("title must not be empty"));
        }
        patch.title = Some(title);
    }

    patch.description = req.description;

    if let Some(raw_status) = req.status {
        let status = raw_status.trim().to_lowercase();
        if !catalog.contains_key(&status) {
            return Err(validation(format!("unknown status: {status}")));
        }
        if let (Some(from), Some(to)) = (
            IssueStatus::from_key(&current.status),
            IssueStatus::from_key(&status),
        ) {
            if from != to && !is_valid_transition(from, to) {
                return Err(Error::IssueTransitionInvalid {
                    from: from.key().to_string(),
                    to: to.key().to_string(),
                });
            }
        }
        patch.status_name = Some(catalog.custom_name(&status));
        patch.status = Some(status);
    }
    if req.status_name.is_some() {
        patch.status_name = req.status_name;
    }

    if let Some(raw_priority) = req.priority.as_deref().map(str::trim) {
        patch.priority = Some(
            Priority::from_str_opt(raw_priority)
                .ok_or_else(|| validation(format!("invalid priority: {raw_priority}")))?,
        );
    }

    if let Some(raw_type) = req.assignee_type {
        patch.assignee_type = Some(match raw_type {
            Some(raw) => {
                let normalized = normalize_assignee_type(&raw);
                Some(
                    parse_assignee_type(normalized)
                        .ok_or_else(|| validation(format!("invalid assignee_type: {raw}")))?,
                )
            }
            None => None,
        });
    }
    if let Some(raw_id) = req.assignee_id {
        patch.assignee_id = Some(
            raw_id
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        );
    }
    // 配对 + 存在性校验：补丁后的最终状态必须「同时有值」或「同时为空」，且目标必须存在。
    // 上游 `UpdateIssue`（L3814）/`BatchUpdateIssues`（L4562）在补丁碰到 assignee 任一半段时
    // 都调 `validateAssigneePair`；`move_issue` 复用本函数，因此三条路径口径一致。
    if patch.assignee_type.is_some() || patch.assignee_id.is_some() {
        let type_present = match patch.assignee_type {
            Some(value) => value.is_some(),
            None => current.assignee_type.is_some(),
        };
        let id_present = match &patch.assignee_id {
            Some(value) => value.is_some(),
            None => current.assignee_id.is_some(),
        };
        if type_present != id_present {
            return Err(validation(
                "assignee_type and assignee_id must be set together",
            ));
        }
        let kind = match patch.assignee_type {
            Some(value) => value,
            None => current.assignee_type(),
        };
        let target = match &patch.assignee_id {
            Some(value) => value.as_deref(),
            None => current.assignee_id_str(),
        };
        if let (Some(kind), Some(target)) = (kind, target) {
            validate_assignee_target(state, workspace_id, kind, target).await?;
        }
    }

    if let Some(raw_parent) = req.parent_issue_id {
        patch.parent_issue_id = match raw_parent {
            None => None,
            Some(raw) if raw.trim().is_empty() => None,
            Some(raw) => {
                let parent_id = parse_target_id("parent_issue_id", &raw)?;
                if parent_id == current.id() {
                    return Err(validation("issue cannot be its own parent"));
                }
                repo.get(workspace_id, parent_id)
                    .await
                    .map_err(|e| match e {
                        RepoError::NotFound => {
                            validation("parent issue not found in this workspace")
                        }
                        other => repo_err(other),
                    })?;
                if repo
                    .has_ancestor(workspace_id, parent_id, current.id())
                    .await
                    .map_err(repo_err)?
                {
                    return Err(validation("parent_issue_id would create a cycle"));
                }
                Some(Some(parent_id))
            }
        };
    }

    if let Some(raw_project) = req.project_id {
        patch.project_id = match raw_project {
            None => None,
            Some(raw) if raw.trim().is_empty() => None,
            Some(raw) => Some(Some(parse_target_id("project_id", &raw)?)),
        };
    }

    if let Some(raw_stage) = req.stage {
        // 内部 binding 不能再叫 `stage`：与新增的 `state: &AppState` 参数触发
        // `clippy::similar_names`（pedantic 门 `-D warnings`）。
        patch.stage = match raw_stage {
            Some(value) if value < 1 => return Err(validation("stage must be >= 1")),
            Some(value) => Some(Some(value)),
            None => None,
        };
    }

    patch.start_date = parse_date_patch("start_date", req.start_date)?;
    patch.due_date = parse_date_patch("due_date", req.due_date)?;
    // 上游字段，本仓没有 triage_state 语义 → 显式忽略（docs/11 §5）
    let _ = req.triage_state;
    Ok(patch)
}

/// `DELETE /api/issues/:id`
pub(crate) async fn delete_issue(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<StatusCode> {
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    let repo = issue_repo(&state);
    let row = load_issue(&repo, workspace_id, &raw_id).await?;
    repo.delete(workspace_id, row.id())
        .await
        .map_err(repo_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/issues/:id/move`（拖拽排序：`before_id` / `after_id` 必须显式出现）
#[allow(clippy::too_many_lines)]
pub(crate) async fn move_issue(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    Json(body): Json<JsonValue>,
) -> ApiResult<Json<IssueDto>> {
    // 白名单：`position` 由 `before_id`/`after_id` 推导，不允许直接传
    const ALLOWED: [&str; 8] = [
        "status",
        "assignee_type",
        "assignee_id",
        "parent_issue_id",
        "project_id",
        "before_id",
        "after_id",
        "expected_revision",
    ];
    let object = body
        .as_object()
        .ok_or_else(|| validation("move body must be a JSON object"))?
        .clone();
    for key in object.keys() {
        if !ALLOWED.contains(&key.as_str()) {
            return Err(validation(format!("unsupported move field: {key}")).into());
        }
    }
    if !object.contains_key("before_id") || !object.contains_key("after_id") {
        return Err(validation(
            "before_id and after_id are required (use null when there is no anchor)",
        )
        .into());
    }

    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let repo = issue_repo(&state);
    let current = load_issue(&repo, workspace_id, &raw_id).await?;
    let catalog = load_catalog(&state, workspace_id).await?;

    let before_id = parse_anchor(object.get("before_id"))?;
    let after_id = parse_anchor(object.get("after_id"))?;

    // 其余字段与 `PUT /api/issues/:id` 同一套语义
    let rest = JsonValue::Object(
        object
            .iter()
            .filter(|(key, _)| !matches!(key.as_str(), "before_id" | "after_id"))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    );
    let req: UpdateIssueRequest =
        serde_json::from_value(rest).map_err(|e| validation(format!("invalid move body: {e}")))?;
    let patch = apply_update_request(&state, &repo, workspace_id, &current, &catalog, req).await?;

    let row = repo
        .move_issue_with_update(workspace_id, current.id(), before_id, after_id, &patch)
        .await
        .map_err(repo_err)?;
    Ok(Json(IssueDto::from_row(&row, &catalog)))
}

pub(crate) fn parse_anchor(raw: Option<&JsonValue>) -> Result<Option<Id>, Error> {
    match raw {
        None | Some(JsonValue::Null) => Ok(None),
        Some(JsonValue::String(value)) => {
            if value.trim().is_empty() {
                Ok(None)
            } else {
                parse_target_id("move anchor", value).map(Some)
            }
        }
        Some(_) => Err(validation("move anchors must be a uuid string or null")),
    }
}

/// `POST /api/issues/batch-update`
pub(crate) async fn batch_update(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    Json(req): Json<BatchUpdateRequest>,
) -> ApiResult<Json<UpdatedResponse>> {
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let repo = issue_repo(&state);
    let mut ids = Vec::with_capacity(req.issue_ids.len());
    for raw in &req.issue_ids {
        ids.push(parse_target_id("issue_ids", raw)?);
    }
    if ids.is_empty() {
        return Err(validation("issue_ids must not be empty").into());
    }

    let mut patch = IssueUpdate::default();
    if !ids.is_empty() {
        // 用第一条 issue 作为 status/assignee 校验的基准；逐行 update 各自做 revision 校验
        let first = repo.get(workspace_id, ids[0]).await.map_err(|e| match e {
            RepoError::NotFound => validation("issue_ids contains an unknown issue"),
            other => repo_err(other),
        })?;
        let catalog = load_catalog(&state, workspace_id).await?;
        patch = apply_update_request(&state, &repo, workspace_id, &first, &catalog, req.updates)
            .await?;
    }

    let updated = repo
        .batch_update(workspace_id, &ids, &patch)
        .await
        .map_err(repo_err)?;
    Ok(Json(UpdatedResponse { updated }))
}

/// `POST /api/issues/batch-delete`
pub(crate) async fn batch_delete(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    Json(req): Json<BatchDeleteRequest>,
) -> ApiResult<Json<DeletedResponse>> {
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let mut ids = Vec::with_capacity(req.issue_ids.len());
    for raw in &req.issue_ids {
        ids.push(parse_target_id("issue_ids", raw)?);
    }
    if ids.is_empty() {
        return Err(validation("issue_ids must not be empty").into());
    }
    let deleted = issue_repo(&state)
        .batch_delete(workspace_id, &ids)
        .await
        .map_err(repo_err)?;
    Ok(Json(DeletedResponse { deleted }))
}
