//! `/api/issue-views/*`（5）+ `/api/assignee-frequency`（1）—— M2-A 收尾面（LUM-1691）。
//!
//! 上游 `server/cmd/server/router.go:1952` 与 `2139-2145`（handler 在 `issue_view.go` /
//! `activity.go`）：
//!
//! ```text
//! r.Get("/api/assignee-frequency", h.GetAssigneeFrequency)   // 1952
//! r.Route("/api/issue-views", func(r chi.Router) {
//!     r.Get("/",  h.ListIssueViews)                               // 2139
//!     r.Post("/", h.CreateIssueView)                              // 2140
//!     r.Route("/{id}", func(r chi.Router) {
//!         r.Get("/",    h.GetIssueViewByID)                       // 2142
//!         r.Patch("/",  h.UpdateIssueView)                        // 2143
//!         r.Delete("/", h.DeleteIssueView)                        // 2144
//!     })
//! })
//! ```
//!
//! `/api/issue-view-preferences` 那 2 条在**另一个文件**
//! `routes/issue_view_preferences.rs`（上游 `issue_view_preference.go` 自成一面，且
//! R7 单文件 800 行上限要求拆）；`/api/pins*` 那 4 条在 `routes/pins.rs`。
//!
//! **尾斜杠形态**（`slash_alias_audit.py` 的硬判据，本波 allowlist 为 0 行 ⇒ 无豁免退路）：
//! fixture 把 `/api/issue-views/` 与 `/api/issue-views/{id}/` 记成**带斜杠**
//! （chi `Mount` 式子路由根 ⇒ 两种形态都服务）⇒ 这两组必须**两个形态都注册**；
//! `/api/assignee-frequency` 是 plain 注册 ⇒ **只注册无斜杠形态**。
//!
//! 语义要点（照上游，完整偏差表见 `docs/63-M2A-TAIL-ISSUE-VIEW-PIN.md`）：
//! - **读**：所有者或 `visibility='workspace'`；不可读与不存在都 404（私有视图的存在性不泄漏）。
//! - **写**：所有者，或共享视图的 workspace `owner`/`admin`（上游 `canManageIssueView`）；
//!   无写权限 → 403（**这是本面唯一会返回 403 的地方**，读路径一律 404）。
//! - **`scope_type` 不可改**，`scope_variant` 可以在同一 scope 内切。
//! - **乐观并发**：`expected_revision` 必填且必须命中当前 `revision`，否则 409。
//! - **`query` / `display` 必须 JSON 对象**（`null` 也拒），且更新侧
//!   「缺失 = 不动、`null` = 显式非法」—— 与 Go 的 `json.RawMessage` 长度判定等价
//!   （靠下面的 [`double_option`] 补回，serde 默认会把两者都折成 `None`）。
//! - **配额**：每成员每 workspace 100 个视图，超出 → 400 `view limit reached for this workspace`。
//! - **名字**：按 **rune** 计数（`len([]rune(name))`），1..=80。
//! - **`assignee-frequency`** 的口径与合并规则在 `mc_repos::stats`（逐字抄上游两条 SQL）。
//!
//! **时间戳格式**：上游 `timestampToString` 用 `time.RFC3339`，UTC 会渲染成 `…Z`；
//! 本仓既有端点（`labels.rs` / `inbox.rs` / `invitations.rs` 等）统一用
//! `chrono::DateTime::to_rfc3339()`，UTC 渲染成 `…+00:00`。本片沿用**本仓**口径
//! （偏差已登记，`docs/63` §5）。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mc_errors::Error;
use mc_repos::agent::role_is_admin;
use mc_repos::issue_view::{
    is_json_object, validate_variant, InvalidVariant, IssueViewPatch, IssueViewRepo, IssueViewRow,
    NewIssueView, BODY_MAX_BYTES, MAX_NAME_LEN, PER_OWNER_MAX, SCOPE_TYPES, VISIBILITIES,
};
use mc_repos::project::ProjectRepo;
use mc_repos::stats::{AssigneeFrequencyEntry, StatsRepo};
use mc_repos::RepoError;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value as JsonValue};

use crate::error::ApiResult;
use crate::routes::agents::workspace_role;
use crate::routes::auth_user::AuthUser;
use crate::routes::invitations::{not_found, require_workspace_member};
use crate::routes::issues::{parse_target_id, resolve_workspace, validation, WorkspaceQuery};
use crate::state::AppState;

// ---------------------------------------------------------------------------
// 请求 / 响应类型
// ---------------------------------------------------------------------------

/// `GET /api/issue-views` 的查询：workspace 选择器 + `scope_type` + 可选 `scope_id`。
#[derive(Debug, Default, Deserialize)]
pub struct IssueViewsQuery {
    #[serde(default)]
    pub scope_type: Option<String>,
    #[serde(default)]
    pub scope_id: Option<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub workspace_slug: Option<String>,
}

impl IssueViewsQuery {
    fn selector(&self) -> WorkspaceQuery {
        WorkspaceQuery {
            workspace_id: self.workspace_id.clone(),
            workspace_slug: self.workspace_slug.clone(),
        }
    }
}

/// `POST /api/issue-views`（上游 `CreateIssueViewRequest`）。
///
/// `query` / `display` 用 [`double_option`]：Go 的 `json.RawMessage` 能区分「键缺失」
/// （len 0）与「显式 `null`」（len 4），serde 的 `Option<JsonValue>` 会把两者都变成
/// `None` ⇒ 那个差别会直接漏掉上游的一条 400。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
#[allow(clippy::option_option)] // 见 `double_option`：「键缺失」与「显式 null」必须分得开
pub struct CreateIssueViewRequest {
    pub name: String,
    pub scope_type: String,
    pub scope_id: Option<String>,
    pub scope_variant: Option<String>,
    pub visibility: Option<String>,
    pub definition_version: i32,
    #[serde(deserialize_with = "double_option")]
    pub query: Option<Option<JsonValue>>,
    #[serde(deserialize_with = "double_option")]
    pub display: Option<Option<JsonValue>>,
}

/// `PATCH /api/issue-views/:id`（上游 `UpdateIssueViewRequest`；指针字段 = 缺失即不动）。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
#[allow(clippy::option_option)] // 同上：三层语义「缺失 / null / 有值」不是冗余嵌套
pub struct UpdateIssueViewRequest {
    pub name: Option<String>,
    pub visibility: Option<String>,
    pub scope_variant: Option<String>,
    #[serde(deserialize_with = "double_option")]
    pub query: Option<Option<JsonValue>>,
    #[serde(deserialize_with = "double_option")]
    pub display: Option<Option<JsonValue>>,
    pub expected_revision: i32,
}

/// 把 JSON `null` 与「键缺失」区分开（serde 默认会把两者都折成 `None`）：
///
/// | 请求体里 | 解出来 |
/// | --- | --- |
/// | 没有这个键 | `None` |
/// | `"f": null` | `Some(None)` |
/// | `"f": <值>` | `Some(Some(值))` |
///
/// 上游对 `json.RawMessage` 用 `len(raw) == 0` 判「缺失」、用 `len(raw) > 0` 判「给过了」
/// ⇒ `null` 会被当**给了**一个非对象值而返回 400，不能当缺失处理。
/// `issue_view_preferences.rs` 的 `prefs` 字段复用本函数。
#[allow(clippy::option_option)] // 三层语义本来就是「缺失 / null / 有值」，不是冗余嵌套
pub(crate) fn double_option<'de, D>(deserializer: D) -> Result<Option<Option<JsonValue>>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Some(Option::<JsonValue>::deserialize(deserializer)?))
}

/// 视图响应（上游 `IssueViewResponse`；字段与顺序逐字对齐）。
#[derive(Debug, Clone, Serialize)]
pub struct IssueViewResponse {
    pub id: String,
    pub workspace_id: String,
    pub owner_id: String,
    pub name: String,
    pub scope_type: String,
    pub scope_id: Option<String>,
    pub scope_variant: Option<String>,
    pub visibility: String,
    pub definition_version: i32,
    pub query: JsonValue,
    pub display: JsonValue,
    pub revision: i32,
    pub created_at: String,
    pub updated_at: String,
}

impl IssueViewResponse {
    fn from_row(row: &IssueViewRow) -> Self {
        Self {
            id: row.id.to_string(),
            workspace_id: row.workspace_id.to_string(),
            owner_id: row.owner_id.to_string(),
            name: row.name.clone(),
            scope_type: row.scope_type.clone(),
            scope_id: row.scope_id.map(|id| id.to_string()),
            scope_variant: row.scope_variant.clone(),
            visibility: row.visibility.clone(),
            definition_version: row.definition_version,
            query: row.query.clone(),
            display: row.display.clone(),
            revision: row.revision,
            created_at: row.created_at.to_rfc3339(),
            updated_at: row.updated_at.to_rfc3339(),
        }
    }
}

// ---------------------------------------------------------------------------
// 路由
// ---------------------------------------------------------------------------

/// 6 条上游键 / 10 个注册点（`/api/issue-views` 与 `/api/issue-views/:id` 各带尾斜杠别名）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        // 集合：上游 `Route("/api/issue-views") + Get/Post("/")` ⇒ 两个形态都要。
        .route(
            "/api/issue-views",
            get(list_issue_views).post(create_issue_view),
        )
        .route(
            "/api/issue-views/",
            get(list_issue_views).post(create_issue_view),
        )
        // 单体：上游 `Route("/{id}") + Get/Patch/Delete("/")` ⇒ 两个形态都要。
        .route(
            "/api/issue-views/:id",
            get(get_issue_view)
                .patch(update_issue_view)
                .delete(delete_issue_view),
        )
        .route(
            "/api/issue-views/:id/",
            get(get_issue_view)
                .patch(update_issue_view)
                .delete(delete_issue_view),
        )
        // 指派人频次：plain 注册 ⇒ 只有无斜杠这一形态。
        .route("/api/assignee-frequency", get(assignee_frequency))
}

// ---------------------------------------------------------------------------
// issue-views
// ---------------------------------------------------------------------------

/// `GET /api/issue-views`（上游 `ListIssueViews`）。
async fn list_issue_views(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<IssueViewsQuery>,
    user: AuthUser,
) -> ApiResult<Json<JsonValue>> {
    let workspace_id = resolve_workspace(&state, &headers, &query.selector()).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let scope_type = query.scope_type.clone().unwrap_or_default();
    if !SCOPE_TYPES.contains(&scope_type.as_str()) {
        return Err(validation("invalid scope_type").into());
    }
    let scope_id = match query.scope_id.as_deref() {
        Some(raw) if !raw.trim().is_empty() => Some(parse_target_id("scope_id", raw)?),
        _ => None,
    };

    let rows = issue_view_repo(&state)
        .list_for_user(workspace_id, &scope_type, user.id(), scope_id)
        .await
        .map_err(view_err)?;
    let views: Vec<IssueViewResponse> = rows.iter().map(IssueViewResponse::from_row).collect();
    Ok(Json(json!(views)))
}

/// `POST /api/issue-views`（上游 `CreateIssueView`；成功 201）。
async fn create_issue_view(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    body: Bytes,
) -> ApiResult<Response> {
    let mut req: CreateIssueViewRequest = parse_view_body(&body)?;
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let name = validate_name(&req.name)?;
    let repo = issue_view_repo(&state);
    // 配额先查（上游在 scope/visibility 校验之前就问 DB）。
    let owned = repo
        .count_by_owner(workspace_id, user.id())
        .await
        .map_err(view_err)?;
    if owned >= PER_OWNER_MAX {
        return Err(validation("view limit reached for this workspace").into());
    }

    if !SCOPE_TYPES.contains(&req.scope_type.as_str()) {
        return Err(validation("invalid scope_type").into());
    }
    let mut visibility = req.visibility.take().unwrap_or_default();
    if visibility.is_empty() {
        visibility = "private".into();
    }
    if !VISIBILITIES.contains(&visibility.as_str()) {
        return Err(validation("invalid visibility").into());
    }
    let definition_version = if req.definition_version <= 0 {
        1
    } else {
        req.definition_version
    };
    let query_blob = match req.query {
        // 缺失与显式 `null` 都判 400（上游 `len(req.Query) == 0 || !isJSONObject` 的两支）。
        Some(Some(value)) if is_json_object(&value) => value,
        _ => return Err(validation("query must be a JSON object").into()),
    };
    let display_blob = match req.display {
        // 缺失 ⇒ 默认 `{}`；显式 `null` 是**给了**一个非对象 ⇒ 400。
        None => json!({}),
        Some(Some(value)) if is_json_object(&value) => value,
        _ => return Err(validation("display must be a JSON object").into()),
    };
    let scope_variant = validate_variant(&req.scope_type, req.scope_variant.as_deref())
        .map_err(|InvalidVariant| validation("invalid scope_variant for this scope_type"))?;

    let mut scope_id = None;
    match req.scope_type.as_str() {
        "project" => {
            let raw = req
                .scope_id
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| validation("scope_id is required for project views"))?;
            let project_id = parse_target_id("scope_id", raw)?;
            // 项目必须在本 workspace：指向别处或已删项目的视图不可达，还会跨租户泄漏。
            match ProjectRepo::new(state.db.clone())
                .get_in_workspace(project_id.0, workspace_id)
                .await
            {
                Ok(Some(_)) => {}
                Err(RepoError::Db(message)) => return Err(Error::Database(message).into()),
                // `Ok(None)`（本 workspace 没有这个项目）与其余仓储错误都按 404 处理。
                _ => return Err(not_found("project").into()),
            }
            scope_id = Some(project_id);
        }
        "my" => {
            // My Issues 是每用户视角，共享没有意义（DB CHECK 也这么钉）。
            visibility = "private".into();
        }
        _ => {}
    }

    let row = repo
        .create(&NewIssueView {
            workspace_id,
            owner_id: user.id(),
            name,
            scope_type: req.scope_type.clone(),
            scope_id,
            scope_variant,
            visibility,
            definition_version,
            query: query_blob,
            display: display_blob,
        })
        .await
        .map_err(view_err)?;
    Ok((StatusCode::CREATED, Json(IssueViewResponse::from_row(&row))).into_response())
}

/// `GET /api/issue-views/:id`（上游 `GetIssueViewByID`）。
async fn get_issue_view(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Json<IssueViewResponse>> {
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    let view = load_readable_view(&state, workspace_id, &raw_id, user.id()).await?;
    Ok(Json(IssueViewResponse::from_row(&view)))
}

/// `PATCH /api/issue-views/:id`（上游 `UpdateIssueView`）。
async fn update_issue_view(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    body: Bytes,
) -> ApiResult<Json<IssueViewResponse>> {
    let req: UpdateIssueViewRequest = parse_view_body(&body)?;
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    let view = load_readable_view(&state, workspace_id, &raw_id, user.id()).await?;
    if !can_manage_issue_view(&state, &view, user.id()).await? {
        return Err(Error::Forbidden {
            message: "insufficient permissions".into(),
        }
        .into());
    }

    // `expected_revision` 缺失（= 0）与负值都判 400（上游 `<= 0`）。
    if req.expected_revision <= 0 {
        return Err(validation("expected_revision is required").into());
    }

    let name = match req.name.as_deref() {
        Some(raw) => validate_name(raw)?,
        None => view.name.clone(),
    };
    let mut visibility = view.visibility.clone();
    if let Some(next) = req.visibility.as_deref() {
        if !VISIBILITIES.contains(&next) {
            return Err(validation("invalid visibility").into());
        }
        if view.scope_type == "my" && next != "private" {
            return Err(validation("my views are always private").into());
        }
        visibility = next.to_string();
    }
    let query_blob = match req.query {
        None => view.query.clone(),
        Some(Some(value)) if is_json_object(&value) => value,
        _ => return Err(validation("query must be a JSON object").into()),
    };
    let display_blob = match req.display {
        None => view.display.clone(),
        Some(Some(value)) if is_json_object(&value) => value,
        _ => return Err(validation("display must be a JSON object").into()),
    };
    // `scope_type` 本身不可变；同 scope 内的 variant 可切。
    let scope_variant = match req.scope_variant.as_deref() {
        None => view.scope_variant.clone(),
        Some(next) => validate_variant(&view.scope_type, Some(next))
            .map_err(|InvalidVariant| validation("invalid scope_variant for this scope_type"))?,
    };

    let updated = issue_view_repo(&state)
        .update(
            workspace_id,
            view.id(),
            &IssueViewPatch {
                name,
                visibility,
                scope_variant,
                query: query_blob,
                display: display_blob,
                expected_revision: req.expected_revision,
            },
        )
        .await
        .map_err(view_err)?;
    match updated {
        Some(row) => Ok(Json(IssueViewResponse::from_row(&row))),
        // 行是刚读到的 ⇒ 0 行只可能是版本被别人推进（上游 `pgx.ErrNoRows` 分支）。
        None => Err(Error::Conflict {
            message: "view was modified by someone else".into(),
        }
        .into()),
    }
}

/// `DELETE /api/issue-views/:id`（上游 `DeleteIssueView`；成功 204，顺手清 view pin）。
async fn delete_issue_view(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<StatusCode> {
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    let view = load_readable_view(&state, workspace_id, &raw_id, user.id()).await?;
    if !can_manage_issue_view(&state, &view, user.id()).await? {
        return Err(Error::Forbidden {
            message: "insufficient permissions".into(),
        }
        .into());
    }
    issue_view_repo(&state)
        .delete(workspace_id, view.id())
        .await
        .map_err(view_err)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// assignee-frequency
// ---------------------------------------------------------------------------

/// `GET /api/assignee-frequency`（上游 `GetAssigneeFrequency`）。
async fn assignee_frequency(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Json<JsonValue>> {
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    let entries: Vec<AssigneeFrequencyEntry> = StatsRepo::new(state.db.clone())
        .assignee_frequency(workspace_id, user.id())
        .await
        .map_err(view_err)?;
    // 上游 `make([]AssigneeFrequencyEntry, 0, …)` ⇒ 空库给 `[]`，不是 `null`。
    Ok(Json(json!(entries)))
}

// ---------------------------------------------------------------------------
// 内部 helper
// ---------------------------------------------------------------------------

/// 本面用的视图仓储（`pins.rs` 的 `view` 归属校验也走这里）。
pub(crate) fn issue_view_repo(state: &AppState) -> IssueViewRepo {
    IssueViewRepo::new(state.db.clone())
}

/// 仓储错误 → HTTP 错误（`issue_view_preferences.rs` 复用）。
pub(crate) fn view_err(err: RepoError) -> Error {
    match err {
        RepoError::NotFound => not_found("view"),
        RepoError::Conflict => Error::Conflict {
            message: "view was modified by someone else".into(),
        },
        RepoError::Db(message) => Error::Database(message),
    }
}

/// 解析 `:id` 并施加读权限（不可读与不存在都 404，不泄漏存在性）。
async fn load_readable_view(
    state: &AppState,
    workspace_id: mc_core::Id,
    raw_id: &str,
    user_id: mc_core::Id,
) -> Result<IssueViewRow, Error> {
    let view_id = parse_target_id("view id", raw_id)?;
    let view = issue_view_repo(state)
        .get(workspace_id, view_id)
        .await
        .map_err(|_| not_found("view"))?;
    if !view.is_readable_by(user_id) {
        return Err(not_found("view"));
    }
    Ok(view)
}

/// 写权限（上游 `canManageIssueView`）：所有者，或共享视图的 workspace `owner`/`admin`。
///
/// 别人的私有视图在 [`load_readable_view`] 就已经 404 ⇒ 管理员的权力碰不到它们。
async fn can_manage_issue_view(
    state: &AppState,
    view: &IssueViewRow,
    user_id: mc_core::Id,
) -> Result<bool, Error> {
    if view.owner_id == user_id.0 {
        return Ok(true);
    }
    if !view.is_shared() {
        return Ok(false);
    }
    match workspace_role(state, view.workspace_id(), user_id).await {
        Ok(role) => Ok(role_is_admin(&role)),
        // 读角色失败（不再是成员）不是 500：按「不授权」处理（上游 `err != nil → false`）。
        Err(Error::NotFound { .. }) => Ok(false),
        Err(other) => Err(other),
    }
}

/// 视图名校验：按 **rune** 计数 1..=80（上游 `len([]rune(name))`）。
fn validate_name(raw: &str) -> Result<String, Error> {
    let len = raw.chars().count();
    if len == 0 || len > MAX_NAME_LEN {
        return Err(validation(format!(
            "name must be between 1 and {MAX_NAME_LEN} characters"
        )));
    }
    Ok(raw.to_string())
}

/// 视图写请求体解码 + 上游的 128 KiB 上限（超限 ⇒ 400 `invalid request body`，
/// 与 Go 的 `http.MaxBytesReader` 让 `Decode` 失败同款；**不是** 413）。
/// `issue_view_preferences.rs` 复用。
pub(crate) fn parse_view_body<T: serde::de::DeserializeOwned>(body: &Bytes) -> Result<T, Error> {
    if body.len() > BODY_MAX_BYTES {
        return Err(validation("invalid request body"));
    }
    serde_json::from_slice::<T>(body).map_err(|_| validation("invalid request body"))
}

// ---------------------------------------------------------------------------
// 纯单测（无需 DB）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_builds_without_panicking() {
        // 6 条上游键 / 10 个注册点（`/api/issue-views` 与 `/:id` 各带尾斜杠别名）不得撞键。
        let _ = router();
    }

    #[test]
    fn name_length_is_counted_in_runes() {
        // 80 个 ASCII 字符合法；81 个非法。
        assert!(validate_name(&"a".repeat(MAX_NAME_LEN)).is_ok());
        assert!(validate_name(&"a".repeat(MAX_NAME_LEN + 1)).is_err());
        // 80 个多字节字符（每个 3 字节）也必须合法 —— 按字节数会误判成 240。
        assert!(validate_name(&"中".repeat(MAX_NAME_LEN)).is_ok());
        assert!(validate_name(&"中".repeat(MAX_NAME_LEN + 1)).is_err());
        assert!(validate_name("").is_err());
    }

    #[test]
    fn update_style_blobs_reject_null_but_accept_absence() {
        // 「键缺失」vs「显式 null」必须分得开 —— Go 的 `json.RawMessage` 靠长度区分，
        // serde 的 `Option<JsonValue>` 会把两者都折成 `None` ⇒ 用 `double_option` 补回。
        let absent: UpdateIssueViewRequest =
            serde_json::from_str(r#"{"expected_revision":1}"#).unwrap();
        assert_eq!(absent.query, None, "缺失 ⇒ None（保持原值）");
        let explicit: UpdateIssueViewRequest =
            serde_json::from_str(r#"{"query":null,"expected_revision":1}"#).unwrap();
        assert_eq!(explicit.query, Some(None), "显式 null ⇒ Some(None)（400）");
        let object: UpdateIssueViewRequest =
            serde_json::from_str(r#"{"query":{},"expected_revision":1}"#).unwrap();
        assert_eq!(object.query, Some(Some(json!({}))));
        // `scope_variant: null` 与缺失同义（Go 的 `*string` 三态）。
        let variant: UpdateIssueViewRequest =
            serde_json::from_str(r#"{"scope_variant":null,"expected_revision":1}"#).unwrap();
        assert!(variant.scope_variant.is_none());
        // 同一个 helper 也被 create 用。
        let create: CreateIssueViewRequest = serde_json::from_str(r#"{"display":null}"#).unwrap();
        assert_eq!(create.display, Some(None));
        assert_eq!(create.query, None);
    }

    #[test]
    fn oversized_bodies_are_400_not_413() {
        let big = Bytes::from(vec![b' '; BODY_MAX_BYTES + 1]);
        assert!(parse_view_body::<CreateIssueViewRequest>(&big).is_err());
        let small = Bytes::from_static(br#"{"name":"v","scope_type":"workspace"}"#);
        assert!(parse_view_body::<CreateIssueViewRequest>(&small).is_ok());
    }
}
