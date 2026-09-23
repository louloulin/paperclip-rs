//! `/api/labels*`（M2-E / LUM-1370）+ `/api/issues/:id/labels*` 的 3 个 handler。
//!
//! 覆盖上游 `server/internal/handler/label.go` 的标签目录读写面 + issue 侧挂/摘：
//!
//! - 目录：`GET/POST /api/labels`、`GET/PUT/DELETE /api/labels/:id`
//! - issue 侧：`GET/POST /api/issues/:id/labels`、`DELETE /api/issues/:id/labels/:labelId`
//!
//! **尾斜杠双形态（LUM-1458 规则）**：上游 `router.go` 的 `/api/labels` 与
//! `/api/labels/{id}` 都是 `r.Route(...) + Get/Post("/")` 形态（chi `Mount` 同时服务
//! `<P>` 与 `<P>/`），因此本文件对**目录面**注册两个形态且方法集合逐字相同；
//! 而 `/api/issues/{id}/labels` 是 `router.go:181-185` 的 `r.Get("/labels")` 这类
//! **plain 子路由**，上游只有**一个**形态 ⇒ 不要加尾斜杠别名（`EXTRA_ALIAS` 会被
//! 门 ⑦ 的第二条命令 `slash_alias_audit.py` 告警）。
//!
//! 语义要点（照上游）：
//! - **没有任何 admin 门**：任何 workspace 成员都能建/改/删标签（上游 `CreateLabel` /
//!   `UpdateLabel` / `DeleteLabel` 只做 `requireUserID`）。本仓补 `require_workspace_member`
//!   （非成员 → 404），因为 workspace 不是从 session 里来的。
//! - `resource_type` 默认 `issue`，非法值 → 400 `resource_type must be issue, agent, or skill`。
//! - 颜色**必须**是 6 位十六进制（`LabelChip` 直接当 `backgroundColor` 用，见
//!   `mc_repos::label::normalize_color` 的注释），非法 → 400。
//! - 重名（同 workspace + 同 `resource_type` + 大小写不敏感）→ 409
//!   `a label with that name already exists`。
//! - `attach` / `detach` **幂等**；`changed == false` 时不返回 `issue_revision` 字段。
//! - 跨 workspace 的 `:labelId` → 404（不泄漏「存在但不在你的 workspace」）。
//!
//! **错误体差异（登记为偏差）**：上游写 `label not found` 这类自由文本，本仓统一走
//! `mc_errors::Error::NotFound { resource }`（渲染成 `not found: label`）。全仓既有端点
//! 同款处理，状态码是对外契约，见 `docs/59-M2-E-LABEL-PROPERTY.md` §6。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mc_errors::Error;
use mc_repos::label::{
    normalize_color, parse_resource_type, validate_name, LabelListRow, LabelRepo, LabelRow,
    LabelUpdate, NewLabel,
};
use mc_repos::RepoError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::routes::invitations::{not_found, require_workspace_member};
use crate::routes::issues::{
    issue_repo, load_issue, parse_target_id, resolve_workspace, validation, WorkspaceQuery,
};
use crate::state::AppState;

// ---------------------------------------------------------------------------
// 请求 / 响应类型
// ---------------------------------------------------------------------------

/// `GET /api/labels` 的查询：workspace 选择器 + `?resource_type`（默认 `issue`）。
#[derive(Debug, Default, Deserialize)]
pub struct LabelsQuery {
    #[serde(default)]
    pub resource_type: Option<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub workspace_slug: Option<String>,
}

impl LabelsQuery {
    fn selector(&self) -> WorkspaceQuery {
        WorkspaceQuery {
            workspace_id: self.workspace_id.clone(),
            workspace_slug: self.workspace_slug.clone(),
        }
    }
}

/// `POST /api/labels`（上游 `CreateLabelRequest`；`description` 缺失 = `""`）。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct CreateLabelRequest {
    pub resource_type: String,
    pub name: String,
    pub description: String,
    pub color: String,
}

/// `PUT /api/labels/:id`（上游 `UpdateLabelRequest`；`*string` 三态 ⇒ `null` 与缺失同义）。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct UpdateLabelRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub color: Option<String>,
}

/// `POST /api/issues/:id/labels`（上游 `AttachLabelRequest`）。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct AttachLabelRequest {
    pub label_id: String,
}

/// 标签响应（上游 `LabelResponse`；字段与顺序逐字对齐，`usage_count` 恒在）。
#[derive(Debug, Clone, Serialize)]
pub struct LabelResponse {
    pub id: String,
    pub workspace_id: String,
    pub resource_type: String,
    pub name: String,
    pub description: String,
    pub color: String,
    pub usage_count: i64,
    pub created_at: String,
    pub updated_at: String,
}

impl LabelResponse {
    /// 目录行（`usage_count` 来自关联表计数）。
    fn from_list_row(row: &LabelListRow) -> Self {
        Self {
            id: row.id.to_string(),
            workspace_id: row.workspace_id.to_string(),
            resource_type: row.resource_type.clone(),
            name: row.name.clone(),
            description: row.description.clone(),
            color: row.color.clone(),
            usage_count: row.usage_count,
            created_at: row.created_at.to_rfc3339(),
            updated_at: row.updated_at.to_rfc3339(),
        }
    }

    /// `issue_label` 行（上游 `labelToResponse`：不带计数 ⇒ `usage_count = 0`）。
    fn from_row(row: &LabelRow) -> Self {
        Self {
            id: row.id.to_string(),
            workspace_id: row.workspace_id.to_string(),
            resource_type: row.resource_type.clone(),
            name: row.name.clone(),
            description: row.description.clone(),
            color: row.color.clone(),
            usage_count: 0,
            created_at: row.created_at.to_rfc3339(),
            updated_at: row.updated_at.to_rfc3339(),
        }
    }
}

/// `GET /api/issues/:id/labels` / `POST` / `DELETE` 的响应体。
///
/// `issue_revision` 只在 `> 0` 时出现（上游 `AttachLabel` / `DetachLabel` 的
/// `if attached.IssueRevision > 0`）；`ListLabelsForIssue` 则**恒**带该字段。
#[derive(Debug, Serialize)]
pub struct IssueLabelsResponse {
    pub labels: Vec<LabelResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_revision: Option<i64>,
}

// ---------------------------------------------------------------------------
// 路由
// ---------------------------------------------------------------------------

/// `/api/labels*` 的注册表（issue 侧那 3 条注册在 `issues::router()` 里，见模块文档）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        // 目录集合：上游 `Route("/api/labels") + Get/Post("/")` ⇒ 两个形态。
        .route("/api/labels", get(list_labels).post(create_label))
        .route("/api/labels/", get(list_labels).post(create_label))
        // 目录单体：上游 `Route("/api/labels/{id}") + Get/Put/Delete("/")` ⇒ 两个形态。
        .route(
            "/api/labels/:id",
            get(get_label).put(update_label).delete(delete_label),
        )
        .route(
            "/api/labels/:id/",
            get(get_label).put(update_label).delete(delete_label),
        )
}

// ---------------------------------------------------------------------------
// 目录端点
// ---------------------------------------------------------------------------

/// `GET /api/labels`（上游 `ListLabels`）。
async fn list_labels(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<LabelsQuery>,
    user: AuthUser,
) -> ApiResult<Json<JsonValue>> {
    let workspace_id = resolve_workspace(&state, &headers, &query.selector()).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    let resource_type = parse_resource_type(query.resource_type.as_deref().unwrap_or(""))
        .ok_or_else(|| validation("resource_type must be issue, agent, or skill"))?;
    let rows = label_repo(&state)
        .list(workspace_id, &resource_type)
        .await
        .map_err(label_err)?;
    let labels: Vec<LabelResponse> = rows.iter().map(LabelResponse::from_list_row).collect();
    Ok(Json(json!({ "labels": labels, "total": labels.len() })))
}

/// `POST /api/labels`（上游 `CreateLabel`；成功 201）。
async fn create_label(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    body: Bytes,
) -> ApiResult<Response> {
    let req: CreateLabelRequest = parse_body(&body)?;
    // 校验顺序照上游：name → color → resource_type。
    let name = validate_name(&req.name).map_err(validation)?;
    let color = normalize_color(&req.color).map_err(validation)?;
    let resource_type = parse_resource_type(&req.resource_type)
        .ok_or_else(|| validation("resource_type must be issue, agent, or skill"))?;
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let row = label_repo(&state)
        .create(
            workspace_id,
            &NewLabel {
                resource_type,
                name,
                description: clean_description(&req.description),
                color,
            },
        )
        .await
        .map_err(label_err)?;
    Ok((StatusCode::CREATED, Json(LabelResponse::from_row(&row))).into_response())
}

/// `GET /api/labels/:id`（上游 `GetLabel`）。
async fn get_label(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Json<LabelResponse>> {
    let label_id = parse_target_id("label id", &raw_id)?;
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    let row = label_repo(&state)
        .get(workspace_id, label_id)
        .await
        .map_err(label_err)?;
    Ok(Json(LabelResponse::from_row(&row)))
}

/// `PUT /api/labels/:id`（上游 `UpdateLabel`：直接 UPDATE，靠 `WHERE` 决定 404）。
async fn update_label(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    body: Bytes,
) -> ApiResult<Json<LabelResponse>> {
    let label_id = parse_target_id("label id", &raw_id)?;
    let req: UpdateLabelRequest = parse_body(&body)?;
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let patch = LabelUpdate {
        name: req
            .name
            .as_deref()
            .map(validate_name)
            .transpose()
            .map_err(validation)?,
        description: req.description.as_deref().map(clean_description),
        color: req
            .color
            .as_deref()
            .map(normalize_color)
            .transpose()
            .map_err(validation)?,
    };
    let row = label_repo(&state)
        .update(workspace_id, label_id, &patch)
        .await
        .map_err(label_err)?;
    Ok(Json(LabelResponse::from_row(&row)))
}

/// `DELETE /api/labels/:id`（上游 `DeleteLabel`：同事务清关联 → 删目录 → 204）。
async fn delete_label(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<StatusCode> {
    let label_id = parse_target_id("label id", &raw_id)?;
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    label_repo(&state)
        .delete(workspace_id, label_id)
        .await
        .map_err(label_err)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// issue 侧端点（注册在 `issues::router()`，见模块文档）
// ---------------------------------------------------------------------------

/// `GET /api/issues/:id/labels`（上游 `ListLabelsForIssue`）。
pub(crate) async fn list_issue_labels(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Json<IssueLabelsResponse>> {
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    let issue = load_issue(&issue_repo(&state), workspace_id, &raw_id).await?;
    let rows = label_repo(&state)
        .list_for_issue(workspace_id, issue.id())
        .await
        .map_err(label_err)?;
    Ok(Json(IssueLabelsResponse {
        labels: rows.iter().map(LabelResponse::from_row).collect(),
        // 上游这条**恒**返回 issue_revision（不区分是否变更）。
        issue_revision: Some(issue.revision),
    }))
}

/// `POST /api/issues/:id/labels`（上游 `AttachLabel`；幂等，成功 200）。
pub(crate) async fn attach_label(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    body: Bytes,
) -> ApiResult<Json<IssueLabelsResponse>> {
    let req: AttachLabelRequest = parse_body(&body)?;
    if req.label_id.trim().is_empty() {
        return Err(validation("label_id is required").into());
    }
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    let issue = load_issue(&issue_repo(&state), workspace_id, &raw_id).await?;

    // label 用 **issue 的** workspace 收窄（上游同款）：跨 workspace ⇒ 404。
    let label_id = parse_target_id("label_id", &req.label_id)?;
    let repo = label_repo(&state);
    let label = repo.get(workspace_id, label_id).await.map_err(label_err)?;
    if !label.is_issue_label() {
        // 上游 `issue label not found`（agent/skill 标签不能挂 issue）。
        return Err(not_found("issue label").into());
    }
    let outcome = repo
        .attach_to_issue(workspace_id, issue.id(), label_id)
        .await
        .map_err(label_err)?;
    issue_labels_response(&repo, workspace_id, issue.id(), outcome.issue_revision).await
}

/// `DELETE /api/issues/:id/labels/:labelId`（上游 `DetachLabel`；幂等，成功 200）。
pub(crate) async fn detach_label(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((raw_id, raw_label_id)): Path<(String, String)>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Json<IssueLabelsResponse>> {
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    let issue = load_issue(&issue_repo(&state), workspace_id, &raw_id).await?;

    let label_id = parse_target_id("label id", &raw_label_id)?;
    let repo = label_repo(&state);
    let label = repo.get(workspace_id, label_id).await.map_err(label_err)?;
    if !label.is_issue_label() {
        return Err(not_found("issue label").into());
    }
    let outcome = repo
        .detach_from_issue(workspace_id, issue.id(), label_id)
        .await
        .map_err(label_err)?;
    issue_labels_response(&repo, workspace_id, issue.id(), outcome.issue_revision).await
}

// ---------------------------------------------------------------------------
// 内部 helper
// ---------------------------------------------------------------------------

fn label_repo(state: &AppState) -> LabelRepo {
    LabelRepo::new(state.db.clone())
}

fn label_err(err: RepoError) -> Error {
    match err {
        RepoError::NotFound => not_found("label"),
        // `issue_label` 上唯一的唯一索引是 `(workspace_id, resource_type, LOWER(name))`。
        RepoError::Conflict => Error::Conflict {
            message: "a label with that name already exists".into(),
        },
        RepoError::Db(message) => Error::Database(message),
    }
}

/// `{labels, issue_revision}`：`issue_revision` 只在 `> 0` 时出现（上游语义）。
async fn issue_labels_response(
    repo: &LabelRepo,
    workspace_id: mc_core::Id,
    issue_id: mc_core::Id,
    issue_revision: i64,
) -> ApiResult<Json<IssueLabelsResponse>> {
    let rows = repo
        .list_for_issue(workspace_id, issue_id)
        .await
        .map_err(label_err)?;
    Ok(Json(IssueLabelsResponse {
        labels: rows.iter().map(LabelResponse::from_row).collect(),
        issue_revision: (issue_revision > 0).then_some(issue_revision),
    }))
}

/// `description` 清洗（上游 `sanitizeNullBytes(strings.TrimSpace(...))`）。
fn clean_description(raw: &str) -> String {
    raw.trim().replace('\0', "")
}

/// body 解码（上游 `json.Decoder` 失败 ⇒ 400 `invalid request body`）。
fn parse_body<T: serde::de::DeserializeOwned>(body: &Bytes) -> Result<T, Error> {
    serde_json::from_slice::<T>(body).map_err(|_| validation("invalid request body"))
}
