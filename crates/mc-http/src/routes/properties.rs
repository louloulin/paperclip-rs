//! `/api/properties*`（M2-E / LUM-1370）：property 定义目录的读面 + 管理面。
//!
//! 覆盖上游 `server/internal/handler/property.go` 的**定义侧** handler：
//!
//! - `GET /api/properties`（`?include_archived=true`）
//! - `POST /api/properties`
//! - `GET /api/properties/:id`
//! - `PATCH /api/properties/:id`
//!
//! issue 侧的值读写（`PUT|DELETE /api/issues/:id/properties/:propertyId`）在
//! `routes/issues/extras.rs`，本片把它改成「先过定义面校验再写 JSONB」。
//!
//! **尾斜杠双形态（LUM-1458 规则）**：上游 `/api/properties` 与 `/api/properties/{id}`
//! 都是 `router.go:2031-2036` 的 `r.Route(...) + Get/Post("/")` / `Get/Patch("/")` 形态
//! （chi `Mount` 两种形态都服务）⇒ 这里每种方法注册两个形态，方法集合逐字相同。
//!
//! 语义要点（照上游）：
//! - **写面只有 owner/admin**（上游 `requirePropertyAdmin`）；读面无门。本仓补
//!   `require_workspace_member` / `require_workspace_admin`（非成员 → 404、
//!   角色不足 → 403 `workspace admin role required`）。
//! - **定义只归档不删除**（`archived: true`）；活跃定义上限 20，到顶 → 400
//!   `a workspace cannot have more than 20 active properties; archive unused ones first`。
//! - **`type` 不可变**（改类型 = 归档旧的、建新的）。
//! - 删掉仍被引用的 select 选项 → 409（普查在 `PropertyRepo::update` 的 advisory lock 内）。
//! - `archived_at` **恒**出现在响应里（未归档为 `null`），与上游 `ArchivedAt *string`
//!   不带 `omitempty` 一致。
//!
//! 已知偏差（详见 `docs/59-M2-E-LABEL-PROPERTY.md` §6）：
//! - 上游对 `actor == "agent"` 的管理请求回 403 `agents cannot manage property definitions`；
//!   本仓 mc-http 没有 agent 身份的请求上下文（只有 `X-Multica-User-Id`）⇒ 不可实现。
//! - 上游 `ListProperties` / `GetProperty` 不校验成员（workspace 来自 session）；本仓
//!   显式解析 workspace ⇒ 补成员门。
//! - 404 体是 `not found: property`（本仓 `Error::NotFound` 渲染），不是上游自由文本。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mc_errors::Error;
use mc_repos::property::{
    validate_config, validate_description, validate_icon, validate_name, validate_type,
    NewProperty, PropertyConfig, PropertyError, PropertyListRow, PropertyRepo, PropertyRow,
    PropertyUpdate,
};
use mc_repos::RepoError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::routes::invitations::{not_found, require_workspace_admin, require_workspace_member};
use crate::routes::issues::{parse_target_id, resolve_workspace, validation, WorkspaceQuery};
use crate::state::AppState;

// ---------------------------------------------------------------------------
// 请求 / 响应类型
// ---------------------------------------------------------------------------

/// `GET /api/properties` 的查询：workspace 选择器 + `?include_archived`。
#[derive(Debug, Default, Deserialize)]
pub struct PropertiesQuery {
    #[serde(default)]
    pub include_archived: Option<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub workspace_slug: Option<String>,
}

impl PropertiesQuery {
    fn selector(&self) -> WorkspaceQuery {
        WorkspaceQuery {
            workspace_id: self.workspace_id.clone(),
            workspace_slug: self.workspace_slug.clone(),
        }
    }
}

/// `POST /api/properties`（上游 `CreatePropertyRequest`）。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct CreatePropertyRequest {
    pub name: String,
    #[serde(rename = "type")]
    pub property_type: String,
    pub description: String,
    pub icon: String,
    pub config: Option<PropertyConfig>,
}

/// `PATCH /api/properties/:id`（上游 `UpdatePropertyRequest`；`*T` 三态 ⇒ `null` = 缺失）。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct UpdatePropertyRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub config: Option<PropertyConfig>,
    pub archived: Option<bool>,
}

/// property 定义响应（上游 `PropertyResponse`；`archived_at` 不省略 ⇒ 未归档为 `null`）。
#[derive(Debug, Clone, Serialize)]
pub struct PropertyResponse {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub property_type: String,
    pub description: String,
    pub icon: String,
    pub config: JsonValue,
    pub position: f64,
    pub archived: bool,
    pub archived_at: Option<String>,
    pub usage_count: i64,
    pub created_at: String,
    pub updated_at: String,
}

impl PropertyResponse {
    /// `issue_property` 行 + 显式 `usage_count`（上游 `propertyToResponse`）。
    fn from_row(row: &PropertyRow, usage_count: i64) -> Self {
        Self {
            id: row.id.to_string(),
            workspace_id: row.workspace_id.to_string(),
            name: row.name.clone(),
            property_type: row.property_type.clone(),
            description: row.description.clone(),
            icon: row.icon.clone(),
            config: row.config.clone(),
            position: row.position,
            archived: row.is_archived(),
            archived_at: row.archived_at.map(|at| at.to_rfc3339()),
            usage_count,
            created_at: row.created_at.to_rfc3339(),
            updated_at: row.updated_at.to_rfc3339(),
        }
    }

    /// 列表行（自带 `usage_count`；上游 `propertyListRowToResponse`）。
    fn from_list_row(row: &PropertyListRow) -> Self {
        Self {
            id: row.id.to_string(),
            workspace_id: row.workspace_id.to_string(),
            name: row.name.clone(),
            property_type: row.property_type.clone(),
            description: row.description.clone(),
            icon: row.icon.clone(),
            config: row.config.clone(),
            position: row.position,
            archived: row.archived_at.is_some(),
            archived_at: row.archived_at.map(|at| at.to_rfc3339()),
            usage_count: row.usage_count,
            created_at: row.created_at.to_rfc3339(),
            updated_at: row.updated_at.to_rfc3339(),
        }
    }
}

// ---------------------------------------------------------------------------
// 路由
// ---------------------------------------------------------------------------

/// `/api/properties*` 的注册表。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/properties",
            get(list_properties).post(create_property),
        )
        .route(
            "/api/properties/",
            get(list_properties).post(create_property),
        )
        .route(
            "/api/properties/:id",
            get(get_property).patch(update_property),
        )
        .route(
            "/api/properties/:id/",
            get(get_property).patch(update_property),
        )
}

// ---------------------------------------------------------------------------
// 端点
// ---------------------------------------------------------------------------

/// `GET /api/properties`（上游 `ListProperties`；无 admin 门）。
async fn list_properties(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<PropertiesQuery>,
    user: AuthUser,
) -> ApiResult<Json<JsonValue>> {
    let workspace_id = resolve_workspace(&state, &headers, &query.selector()).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    // 上游是字面量比较：只有 `"true"` 算真（`TRUE` / `1` 都算假）。
    let include_archived = query.include_archived.as_deref() == Some("true");
    let rows = property_repo(&state)
        .list(workspace_id, include_archived)
        .await
        .map_err(property_repo_err)?;
    let properties: Vec<PropertyResponse> =
        rows.iter().map(PropertyResponse::from_list_row).collect();
    Ok(Json(
        json!({ "properties": properties, "total": properties.len() }),
    ))
}

/// `POST /api/properties`（上游 `CreateProperty`；owner/admin，成功 201）。
async fn create_property(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    body: Bytes,
) -> ApiResult<Response> {
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_admin(&state, workspace_id, user.id()).await?;
    let req: CreatePropertyRequest = parse_body(&body)?;

    // 校验顺序照上游：name → type → description → icon → config。
    let name = validate_name(&req.name).map_err(validation)?;
    validate_type(&req.property_type).map_err(validation)?;
    validate_description(&req.description).map_err(validation)?;
    let icon = validate_icon(&req.icon).map_err(validation)?;
    let config = validate_config(&req.property_type, req.config.as_ref()).map_err(validation)?;

    let row = property_repo(&state)
        .create(
            workspace_id,
            &NewProperty {
                name,
                property_type: req.property_type,
                description: clean_description(&req.description),
                icon,
                config,
            },
        )
        .await
        .map_err(property_err)?;
    Ok((
        StatusCode::CREATED,
        Json(PropertyResponse::from_row(&row, 0)),
    )
        .into_response())
}

/// `GET /api/properties/:id`（上游 `GetProperty`；无 admin 门）。
async fn get_property(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Json<PropertyResponse>> {
    let property_id = parse_target_id("property id", &raw_id)?;
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    let row = property_repo(&state)
        .get(workspace_id, property_id)
        .await
        .map_err(property_repo_err)?;
    Ok(Json(PropertyResponse::from_row(&row, 0)))
}

/// `PATCH /api/properties/:id`（上游 `UpdateProperty`；owner/admin）。
///
/// 顺序刻意照上游：先解析 id → 解码 body → **读一次定义**（未命中 = 404，先于任何
/// 字段校验）→ 逐字段校验 → 写。真正的写仍走 `PropertyRepo::update`，它在
/// `props:<ws>` + `prop:<id>` 双锁内**重新**读取定义并做「选项使用普查 / 取消归档
/// 上限」判定（上游 F1/F5）；因此这里这次读只用于「校验顺序 + 拿不可变的 type」。
async fn update_property(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    body: Bytes,
) -> ApiResult<Json<PropertyResponse>> {
    let property_id = parse_target_id("property id", &raw_id)?;
    let req: UpdatePropertyRequest = parse_body(&body)?;
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_admin(&state, workspace_id, user.id()).await?;

    let repo = property_repo(&state);
    let existing = repo
        .get(workspace_id, property_id)
        .await
        .map_err(property_repo_err)?;

    let patch = PropertyUpdate {
        name: req
            .name
            .as_deref()
            .map(validate_name)
            .transpose()
            .map_err(validation)?,
        description: match req.description.as_deref() {
            Some(value) => {
                validate_description(value).map_err(validation)?;
                Some(clean_description(value))
            }
            None => None,
        },
        icon: req
            .icon
            .as_deref()
            .map(validate_icon)
            .transpose()
            .map_err(validation)?,
        // `type` 不可变 ⇒ 用现存定义的 type 校验 config（上游同款）。
        config: match req.config.as_ref() {
            Some(config) => {
                Some(validate_config(&existing.property_type, Some(config)).map_err(validation)?)
            }
            None => None,
        },
        archived: req.archived,
    };
    let row = repo
        .update(workspace_id, property_id, &patch)
        .await
        .map_err(property_err)?;
    Ok(Json(PropertyResponse::from_row(&row, 0)))
}

// ---------------------------------------------------------------------------
// 内部 helper
// ---------------------------------------------------------------------------

/// 本模块的 repo 构造（`issues/extras.rs` 的值写入路径也用它）。
pub(crate) fn property_repo(state: &AppState) -> PropertyRepo {
    PropertyRepo::new(state.db.clone())
}

/// `RepoError` → HTTP（property 面：未命中 404 `property`，重名 409）。
pub(crate) fn property_repo_err(err: RepoError) -> Error {
    match err {
        RepoError::NotFound => not_found("property"),
        // `issue_property` 上唯一的唯一索引是 `(workspace_id, LOWER(name))`。
        RepoError::Conflict => Error::Conflict {
            message: "a property with that name already exists".into(),
        },
        RepoError::Db(message) => Error::Database(message),
    }
}

/// `PropertyError` → HTTP（上游同位置的状态码：404 / 409 / 400 / 500）。
pub(crate) fn property_err(err: PropertyError) -> Error {
    let message = err.to_string();
    match err {
        PropertyError::Repo(inner) => property_repo_err(inner),
        PropertyError::OptionsInUse(message) => Error::Conflict { message },
        // `ActiveCap` / `Archived` / `Invalid` 都是 400，消息逐字对齐上游。
        PropertyError::ActiveCap(_) | PropertyError::Archived(_) | PropertyError::Invalid(_) => {
            validation(message)
        }
    }
}

/// `description` 清洗（上游 `sanitizeNullBytes(strings.TrimSpace(...))`）。
fn clean_description(raw: &str) -> String {
    raw.trim().replace('\0', "")
}

/// body 解码（上游 `json.Decoder` 失败 ⇒ 400 `invalid request body`）。
fn parse_body<T: serde::de::DeserializeOwned>(body: &Bytes) -> Result<T, Error> {
    serde_json::from_slice::<T>(body).map_err(|_| validation("invalid request body"))
}
