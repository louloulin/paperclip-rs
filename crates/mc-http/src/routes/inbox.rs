//! `/api/inbox*` 系列路由（14 条，与上游 `server/cmd/server/router.go` L2377-2397 逐条对应）。
//!
//! 上游 handler 在 `server/internal/handler/inbox.go` + `inbox_archive.go`，
//! SQL 在 `server/pkg/db/queries/inbox.sql` + `inbox_archive.sql`。
//!
//! 鉴权（M2 阶段，沿用 M1 的 dev-mode 约定）：
//! - 当前用户来自 `X-Multica-User-Id` header（`auth_user::AuthUser`）；
//! - workspace 来自 `X-Workspace-ID` header，其次 `?workspace_id=`（上游
//!   `middleware.ResolveWorkspaceIDFromRequest` 的第 5/6 优先级）；
//! - 所有 14 条路由都在上游的 `RequireWorkspaceMember` 组内，故一律要求调用者是
//!   该 workspace 的成员，否则 404（`errWorkspaceNotFound`，复用
//!   `invitations::require_workspace_member`）。
//!
//! 与上游的已知偏离（完整清单见 `docs/13-M2-INBOX.md`）：
//! - **不解析 `X-Workspace-Slug` / `?workspace_slug`**（上游优先级 3/4，本切片延后）；
//! - 主列表支持 `limit` / `offset`（上游一次返回全部活跃行，不分页）；
//! - 游标用 **hex 编码的 JSON**（上游是 base64），避免给 mc-http 引入 `base64` 依赖；
//! - `severity` 恒为 `"info"`、`details` 恒为 `{}`（本仓 `inbox_item` 无这两列）；
//! - 错误体是 `{"error":{"code","message"}}`（M1 既有约定），上游是 `{"error":"msg"}`；
//! - 不发 realtime 事件（M1 各路由同样未接 `RealtimeHandle`）。

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderName};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use mc_core::Id;
use mc_errors::Error;
use mc_repos::inbox::{
    ArchivedCursor, ArchivedInboxFilter, InboxItemRow, InboxRepo, BUILTIN_TERMINAL_STATUS_KEYS,
};

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::routes::invitations::require_workspace_member;
use crate::state::AppState;

pub fn router() -> Router<Arc<AppState>> {
    // 注意：axum 0.7（matchit 0.7）路径参数语法是 `:id`，不是 `{id}`（那是 axum 0.8）；
    // 写成 `{id}` 会把整段当字面量，路由恒 404。
    //
    // `/api/inbox` 与 `/api/inbox/` 都注册：上游是 chi 的
    // `Route("/api/inbox") + Get("/")`，两种写法都能命中同一 handler。
    Router::new()
        .route("/api/inbox", get(list_inbox))
        .route("/api/inbox/", get(list_inbox))
        .route("/api/inbox/archived", get(list_archived))
        .route("/api/inbox/archived/page", get(list_archived_page))
        .route("/api/inbox/archived/facets", get(get_archived_facets))
        .route("/api/inbox/unread-count", get(count_unread))
        .route("/api/inbox/unread-summary", get(unread_summary))
        .route("/api/inbox/mark-all-read", post(mark_all_read))
        .route("/api/inbox/archive-all", post(archive_all))
        .route("/api/inbox/archive-all-read", post(archive_all_read))
        .route("/api/inbox/archive-completed", post(archive_completed))
        .route("/api/inbox/:id/read", post(mark_read))
        .route("/api/inbox/:id/unread", post(mark_unread))
        .route("/api/inbox/:id/archive", post(archive_item))
        .route("/api/inbox/:id/unarchive", post(unarchive_item))
}

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// 主列表默认 `limit`。上游不分页（返回全部活跃行），本仓按 sub-issue 要求加分页。
const LIST_DEFAULT_LIMIT: i64 = 200;
/// 主列表最大 `limit`。
const LIST_MAX_LIMIT: i64 = 500;
/// `archived`（不分页）最多返回的 issue 组数，与上游 SQL 的 `LIMIT 200` 对齐。
const ARCHIVED_GROUP_LIMIT: i64 = 200;
/// `archived/page` 默认 `limit`（与上游逐字一致）。
const ARCHIVED_PAGE_DEFAULT_LIMIT: i64 = 50;
/// `archived/page` 最大 `limit`（与上游逐字一致）。
const ARCHIVED_PAGE_MAX_LIMIT: i64 = 100;
/// 列表响应里 `new_comment` body 的预览上限（**含**省略号），与上游一致。
const LIST_BODY_PREVIEW_LIMIT: usize = 200;
/// 游标串长度上限（上游 2048）。
const CURSOR_MAX_LEN: usize = 2048;
/// 单个过滤器原始串的长度上限（上游 8192）。
const FILTER_MAX_RAW_LEN: usize = 8192;
/// 单个过滤器的取值个数上限（上游 100）。
const FILTER_MAX_VALUES: usize = 100;
/// 上游 `InboxItemResponse.recipient_type` 的取值。
const RECIPIENT_TYPE_USER: &str = "user";
/// 上游 `InboxItemResponse.severity` 的取值（本仓 `inbox_item` 无该列）。
const SEVERITY_INFO: &str = "info";
/// workspace 上下文 header（上游 `ResolveWorkspaceIDFromRequest` 优先级 5）。
const WORKSPACE_ID_HEADER: HeaderName = HeaderName::from_static("x-workspace-id");

// ---------------------------------------------------------------------------
// DTO（字段名与上游 `InboxItemResponse` 逐字对齐）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct InboxItemDto {
    pub id: String,
    pub workspace_id: String,
    pub recipient_type: String,
    pub recipient_id: String,
    /// 上游字段名是 `type`（本仓列名 `category`）。
    pub r#type: String,
    pub severity: String,
    pub issue_id: Option<String>,
    pub title: String,
    pub body: Option<String>,
    pub read: bool,
    pub archived: bool,
    pub created_at: String,
    pub issue_status: Option<String>,
    pub issue_priority: Option<String>,
    pub actor_type: String,
    pub actor_id: String,
    pub details: serde_json::Value,
}

impl InboxItemDto {
    /// 单条响应（保留完整 body）。
    fn from_row(row: &InboxItemRow) -> Self {
        Self {
            id: row.id().as_string(),
            workspace_id: Id::from(row.workspace_id).as_string(),
            recipient_type: RECIPIENT_TYPE_USER.to_string(),
            recipient_id: Id::from(row.user_id).as_string(),
            r#type: row.category.clone(),
            severity: SEVERITY_INFO.to_string(),
            issue_id: row.issue_id.map(|id| Id::from(id).as_string()),
            title: row.title.clone(),
            body: row.body.clone(),
            read: row.is_read(),
            archived: row.is_archived(),
            created_at: row.created_at.to_rfc3339(),
            issue_status: row.issue_status.clone(),
            issue_priority: row.issue_priority.clone(),
            actor_type: row.actor_type.clone(),
            actor_id: row.actor_id.clone(),
            details: serde_json::json!({}),
        }
    }

    /// 列表响应：`new_comment` 且有 issue 的行按上游规则截断 body。
    fn from_list_row(row: &InboxItemRow) -> Self {
        let mut dto = Self::from_row(row);
        dto.body = list_body_preview(&row.category, row.issue_id.is_some(), row.body.as_deref());
        dto
    }
}

#[derive(Debug, Serialize)]
struct ArchivedPageDto {
    items: Vec<InboxItemDto>,
    next_cursor: Option<String>,
    has_more: bool,
}

#[derive(Debug, Serialize)]
struct CountDto {
    count: i64,
}

#[derive(Debug, Serialize)]
struct WorkspaceUnreadDto {
    workspace_id: String,
    count: i64,
}

#[derive(Debug, Serialize)]
struct ArchivedFacetsDto {
    statuses: BTreeMap<String, i64>,
    priorities: BTreeMap<String, i64>,
    actors: BTreeMap<String, i64>,
    unread_count: i64,
}

/// `archived/page` 游标载荷（上游同名结构的字段一致；外层编码换成 hex）。
#[derive(Debug, Serialize, Deserialize)]
struct ArchivedCursorWire {
    time: String,
    id: String,
    scope: String,
}

// ---------------------------------------------------------------------------
// 请求上下文
// ---------------------------------------------------------------------------

/// 一次 inbox 请求的 workspace/user 上下文 + 仓储。
struct InboxScope {
    workspace_id: Id,
    user_id: Id,
    repo: InboxRepo,
}

impl InboxScope {
    /// 解析 workspace（400）→ 校验成员身份（404）→ 装配仓储。
    async fn resolve(
        state: &AppState,
        user: AuthUser,
        headers: &HeaderMap,
        query: &HashMap<String, String>,
    ) -> Result<Self, Error> {
        let workspace_id = resolve_workspace_id(headers, query)?;
        require_workspace_member(state, workspace_id, user.id()).await?;
        Ok(Self {
            workspace_id,
            user_id: user.id(),
            repo: InboxRepo::new(state.db.clone()),
        })
    }

    /// `loadInboxItemForUser` 的等价物：归属校验（别人的通知与不存在的通知都 404）。
    async fn load_item(&self, raw_id: &str) -> Result<InboxItemRow, Error> {
        let id = Id::parse(raw_id).map_err(|_| bad_request("invalid inbox item id"))?;
        self.repo
            .get_for_user(id, self.workspace_id, self.user_id)
            .await
            .map_err(|e| repo_err(e, "inbox item"))
    }
}

pub(crate) fn resolve_workspace_id(
    headers: &HeaderMap,
    query: &HashMap<String, String>,
) -> Result<Id, Error> {
    let raw = headers
        .get(WORKSPACE_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .or_else(|| query_value(query, "workspace_id"));
    match raw {
        Some(raw) => Id::parse(raw).map_err(|_| bad_request("invalid workspace id")),
        None => Err(bad_request("invalid workspace id")),
    }
}

// ---------------------------------------------------------------------------
// handler：读
// ---------------------------------------------------------------------------

/// `GET /api/inbox`（上游 `ListInbox`）。
async fn list_inbox(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Vec<InboxItemDto>>> {
    let scope = InboxScope::resolve(&state, user, &headers, &query).await?;
    let (limit, offset) = parse_list_window(&query)?;
    let rows = scope
        .repo
        .list(scope.workspace_id, scope.user_id, limit, offset)
        .await
        .map_err(|e| repo_err(e, "inbox item"))?;
    Ok(Json(rows.iter().map(InboxItemDto::from_list_row).collect()))
}

/// `GET /api/inbox/archived`（上游 `ListArchivedInbox`）：最多 200 个 issue 组。
async fn list_archived(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Vec<InboxItemDto>>> {
    let scope = InboxScope::resolve(&state, user, &headers, &query).await?;
    let rows = scope
        .repo
        .list_archived(scope.workspace_id, scope.user_id, ARCHIVED_GROUP_LIMIT)
        .await
        .map_err(|e| repo_err(e, "inbox item"))?;
    Ok(Json(rows.iter().map(InboxItemDto::from_list_row).collect()))
}

/// `GET /api/inbox/archived/page`（上游 `ListArchivedInboxPage`）。
async fn list_archived_page(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<ArchivedPageDto>> {
    let scope = InboxScope::resolve(&state, user, &headers, &query).await?;
    let filter = parse_filter(&query)?;
    let limit = parse_archived_limit(&query)?;
    let group_id = parse_group_id(&query)?;
    let filter = ArchivedInboxFilter { group_id, ..filter };
    let scope_tag = archive_scope_tag(scope.workspace_id, scope.user_id, &filter);
    let cursor = parse_cursor(&query, &scope_tag)?;

    let page = scope
        .repo
        .list_archived_page(
            scope.workspace_id,
            scope.user_id,
            &filter,
            cursor.as_ref(),
            limit,
        )
        .await
        .map_err(|e| repo_err(e, "inbox item"))?;

    let next_cursor = if page.has_more {
        page.items
            .last()
            .map(|row| encode_cursor(&scope_tag, row))
            .transpose()?
    } else {
        None
    };
    Ok(Json(ArchivedPageDto {
        items: page.items.iter().map(InboxItemDto::from_list_row).collect(),
        next_cursor,
        has_more: page.has_more,
    }))
}

/// `GET /api/inbox/archived/facets`（上游 `GetArchivedInboxFacets`）。
async fn get_archived_facets(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<ArchivedFacetsDto>> {
    let scope = InboxScope::resolve(&state, user, &headers, &query).await?;
    let filter = parse_filter(&query)?;
    let facets = scope
        .repo
        .archived_facets(scope.workspace_id, scope.user_id, &filter)
        .await
        .map_err(|e| repo_err(e, "inbox item"))?;
    Ok(Json(ArchivedFacetsDto {
        statuses: facets.statuses,
        priorities: facets.priorities,
        actors: facets.actors,
        unread_count: facets.unread_count,
    }))
}

/// `GET /api/inbox/unread-count`（上游 `CountUnreadInbox`）：**行**粒度。
async fn count_unread(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<CountDto>> {
    let scope = InboxScope::resolve(&state, user, &headers, &query).await?;
    let count = scope
        .repo
        .unread_count(scope.workspace_id, scope.user_id)
        .await
        .map_err(|e| repo_err(e, "inbox item"))?;
    Ok(Json(CountDto { count }))
}

/// `GET /api/inbox/unread-summary`（上游 `UnreadInboxSummary`）。
///
/// 查询本身是账户级的（跨 workspace，键是 user），但**路由仍要求 workspace 上下文**
/// ——上游把它放在 `RequireWorkspaceMember` 组内，`ctxWorkspaceID` 才有值。
async fn unread_summary(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Vec<WorkspaceUnreadDto>>> {
    let scope = InboxScope::resolve(&state, user, &headers, &query).await?;
    let rows = scope
        .repo
        .unread_summary(scope.user_id)
        .await
        .map_err(|e| repo_err(e, "inbox item"))?;
    Ok(Json(
        rows.into_iter()
            .map(|row| WorkspaceUnreadDto {
                workspace_id: row.workspace_id.as_string(),
                count: row.count,
            })
            .collect(),
    ))
}

// ---------------------------------------------------------------------------
// handler：写（全部幂等）
// ---------------------------------------------------------------------------

/// `POST /api/inbox/mark-all-read`（上游 `MarkAllInboxRead`）。
async fn mark_all_read(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<CountDto>> {
    let scope = InboxScope::resolve(&state, user, &headers, &query).await?;
    let count = scope
        .repo
        .mark_all_read(scope.workspace_id, scope.user_id)
        .await
        .map_err(|e| repo_err(e, "inbox item"))?;
    Ok(Json(CountDto {
        count: count_as_i64(count),
    }))
}

/// `POST /api/inbox/archive-all`（上游 `ArchiveAllInbox`）。
async fn archive_all(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<CountDto>> {
    let scope = InboxScope::resolve(&state, user, &headers, &query).await?;
    let count = scope
        .repo
        .archive_all(scope.workspace_id, scope.user_id)
        .await
        .map_err(|e| repo_err(e, "inbox item"))?;
    Ok(Json(CountDto {
        count: count_as_i64(count),
    }))
}

/// `POST /api/inbox/archive-all-read`（上游 `ArchiveAllReadInbox`）：只归档已读**组**。
async fn archive_all_read(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<CountDto>> {
    let scope = InboxScope::resolve(&state, user, &headers, &query).await?;
    let count = scope
        .repo
        .archive_all_read(scope.workspace_id, scope.user_id)
        .await
        .map_err(|e| repo_err(e, "inbox item"))?;
    Ok(Json(CountDto {
        count: count_as_i64(count),
    }))
}

/// `POST /api/inbox/archive-completed`（上游 `ArchiveCompletedInbox`）。
async fn archive_completed(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<CountDto>> {
    let scope = InboxScope::resolve(&state, user, &headers, &query).await?;
    let count = scope
        .repo
        .archive_completed(scope.workspace_id, scope.user_id)
        .await
        .map_err(|e| repo_err(e, "inbox item"))?;
    Ok(Json(CountDto {
        count: count_as_i64(count),
    }))
}

/// `POST /api/inbox/{id}/read`（上游 `MarkInboxRead`）：返回单条，**保留完整 body**。
async fn mark_read(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<InboxItemDto>> {
    let scope = InboxScope::resolve(&state, user, &headers, &query).await?;
    let item = scope.load_item(&item_id).await?;
    let row = scope
        .repo
        .mark_read(item.id())
        .await
        .map_err(|e| repo_err(e, "inbox item"))?;
    Ok(Json(InboxItemDto::from_row(&row)))
}

/// `POST /api/inbox/{id}/unread`（上游 `MarkInboxUnread`）。
async fn mark_unread(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<InboxItemDto>> {
    let scope = InboxScope::resolve(&state, user, &headers, &query).await?;
    let item = scope.load_item(&item_id).await?;
    let row = scope
        .repo
        .mark_unread(item.id())
        .await
        .map_err(|e| repo_err(e, "inbox item"))?;
    Ok(Json(InboxItemDto::from_row(&row)))
}

/// `POST /api/inbox/{id}/archive`（上游 `ArchiveInboxItem`）：issue 级归档。
async fn archive_item(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<InboxItemDto>> {
    let scope = InboxScope::resolve(&state, user, &headers, &query).await?;
    let item = scope.load_item(&item_id).await?;
    let row = scope
        .repo
        .archive(item.id())
        .await
        .map_err(|e| repo_err(e, "inbox item"))?;
    Ok(Json(InboxItemDto::from_row(&row)))
}

/// `POST /api/inbox/{id}/unarchive`（上游 `UnarchiveInboxItem`）：issue 级还原。
async fn unarchive_item(
    State(state): State<Arc<AppState>>,
    Path(item_id): Path<String>,
    user: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<InboxItemDto>> {
    let scope = InboxScope::resolve(&state, user, &headers, &query).await?;
    let item = scope.load_item(&item_id).await?;
    let row = scope
        .repo
        .unarchive(item.id())
        .await
        .map_err(|e| repo_err(e, "inbox item"))?;
    Ok(Json(InboxItemDto::from_row(&row)))
}

// ---------------------------------------------------------------------------
// 解析 / 编码 helper
// ---------------------------------------------------------------------------

fn bad_request(message: &str) -> Error {
    Error::Validation {
        message: message.to_string(),
        details: vec![],
    }
}

/// 上游 `q.Get(name) != ""` 的等价语义：空值等于没传。
fn query_value<'a>(query: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    query
        .get(name)
        .map(String::as_str)
        .filter(|v| !v.is_empty())
}

fn parse_list_window(query: &HashMap<String, String>) -> Result<(i64, i64), Error> {
    let limit = match query_value(query, "limit") {
        Some(raw) => raw
            .parse::<i64>()
            .ok()
            .filter(|v| (1..=LIST_MAX_LIMIT).contains(v))
            .ok_or_else(|| bad_request("limit must be between 1 and 500"))?,
        None => LIST_DEFAULT_LIMIT,
    };
    let offset = match query_value(query, "offset") {
        Some(raw) => raw
            .parse::<i64>()
            .ok()
            .filter(|v| *v >= 0)
            .ok_or_else(|| bad_request("offset must be >= 0"))?,
        None => 0,
    };
    Ok((limit, offset))
}

fn parse_archived_limit(query: &HashMap<String, String>) -> Result<i64, Error> {
    match query_value(query, "limit") {
        Some(raw) => raw
            .parse::<i64>()
            .ok()
            .filter(|v| (1..=ARCHIVED_PAGE_MAX_LIMIT).contains(v))
            .ok_or_else(|| bad_request("limit must be between 1 and 100")),
        None => Ok(ARCHIVED_PAGE_DEFAULT_LIMIT),
    }
}

/// 解析一个逗号分隔的过滤器（排序 + 去重，与上游 `slices.Sort`+`Compact` 一致）。
fn parse_filter_values(query: &HashMap<String, String>, name: &str) -> Result<Vec<String>, Error> {
    let Some(raw) = query_value(query, name) else {
        return Ok(Vec::new());
    };
    if raw.len() > FILTER_MAX_RAW_LEN {
        return Err(bad_request("filter is too long"));
    }
    let mut values: Vec<String> = raw.split(',').map(str::to_string).collect();
    if values.len() > FILTER_MAX_VALUES {
        return Err(bad_request("too many filter values"));
    }
    if values.iter().any(String::is_empty) {
        return Err(bad_request("empty filter value"));
    }
    values.sort();
    values.dedup();
    Ok(values)
}

/// 解析 `statuses` / `priorities` / `actors` / `unread_only`（不含 `group_id`）。
fn parse_filter(query: &HashMap<String, String>) -> Result<ArchivedInboxFilter, Error> {
    let unread_only = match query_value(query, "unread_only") {
        None | Some("false") => false,
        Some("true") => true,
        Some(_) => return Err(bad_request("invalid unread_only")),
    };
    Ok(ArchivedInboxFilter {
        statuses: parse_filter_values(query, "statuses")?,
        priorities: parse_filter_values(query, "priorities")?,
        actors: parse_filter_values(query, "actors")?,
        unread_only,
        group_id: None,
    })
}

fn parse_group_id(query: &HashMap<String, String>) -> Result<Option<Id>, Error> {
    match query_value(query, "group_id") {
        Some(raw) => Id::parse(raw)
            .map(Some)
            .map_err(|_| bad_request("invalid group_id")),
        None => Ok(None),
    }
}

/// 游标作用域：绑定「用户 + workspace + 过滤条件 + 分组」，防止跨查询续页。
fn archive_scope_tag(workspace_id: Id, user_id: Id, filter: &ArchivedInboxFilter) -> String {
    let canonical = format!(
        "{}|{}|{}|{}|{}|{}|{}",
        workspace_id.as_string(),
        user_id.as_string(),
        filter.statuses.join(","),
        filter.priorities.join(","),
        filter.actors.join(","),
        filter.unread_only,
        filter.group_id.map_or_else(String::new, Id::as_string),
    );
    hex::encode(Sha256::digest(canonical.as_bytes()))
}

fn parse_cursor(
    query: &HashMap<String, String>,
    scope_tag: &str,
) -> Result<Option<ArchivedCursor>, Error> {
    let Some(raw) = query_value(query, "cursor") else {
        return Ok(None);
    };
    if raw.len() > CURSOR_MAX_LEN {
        return Err(bad_request("invalid archive cursor"));
    }
    let bytes = hex::decode(raw).map_err(|_| bad_request("invalid archive cursor"))?;
    let wire: ArchivedCursorWire =
        serde_json::from_slice(&bytes).map_err(|_| bad_request("invalid archive cursor"))?;
    if wire.scope != scope_tag {
        return Err(bad_request("invalid archive cursor"));
    }
    let created_at: DateTime<Utc> = DateTime::parse_from_rfc3339(&wire.time)
        .map_err(|_| bad_request("invalid archive cursor time"))?
        .with_timezone(&Utc);
    let id = Id::parse(&wire.id).map_err(|_| bad_request("invalid cursor id"))?;
    Ok(Some(ArchivedCursor { created_at, id }))
}

fn encode_cursor(scope_tag: &str, row: &InboxItemRow) -> Result<String, Error> {
    let wire = ArchivedCursorWire {
        time: row.created_at.to_rfc3339_opts(SecondsFormat::Nanos, true),
        id: row.id().as_string(),
        scope: scope_tag.to_string(),
    };
    let bytes = serde_json::to_vec(&wire).map_err(|e| Error::Internal(e.to_string()))?;
    Ok(hex::encode(bytes))
}

/// 列表响应里的 body 预览：只有「有 issue 的 `new_comment`」才截断，
/// 且截断按**字符**（不是字节）计数，避免切坏多字节字符。
///
/// 上限**含**省略号：超过 200 字符时保留前 199 个字符 + `…`（与上游
/// `inboxListBody` 逐字对应）。
fn list_body_preview(category: &str, has_issue: bool, body: Option<&str>) -> Option<String> {
    let full = body?;
    if category != "new_comment" || !has_issue {
        return Some(full.to_string());
    }
    let mut cut = 0usize;
    let mut seen = 0usize;
    for (offset, _) in full.char_indices() {
        if seen == LIST_BODY_PREVIEW_LIMIT - 1 {
            cut = offset;
        }
        seen += 1;
        if seen > LIST_BODY_PREVIEW_LIMIT {
            return Some(format!("{}…", &full[..cut]));
        }
    }
    Some(full.to_string())
}

fn count_as_i64(count: u64) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX)
}

fn repo_err(e: mc_repos::RepoError, resource: &str) -> Error {
    match e {
        mc_repos::RepoError::NotFound => Error::NotFound {
            resource: resource.to_string(),
        },
        mc_repos::RepoError::Conflict => Error::Conflict {
            message: format!("{resource} state conflict"),
        },
        mc_repos::RepoError::Db(msg) => Error::Database(msg),
    }
}

/// 内置终结状态 key 的只读视图（供文档/测试引用，避免常量漂移）。
pub fn builtin_terminal_status_keys() -> &'static [&'static str] {
    BUILTIN_TERMINAL_STATUS_KEYS
}

// ---------------------------------------------------------------------------
// 单元测试（不依赖 DB）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn q(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // 装配完整 router + state，读起来比抽 helper 清楚。
    async fn mounted_router_builds_without_route_conflict() {
        // 完整 router 装配：任何 path+method 重复都会让 axum 在 `.merge` 时 panic。
        let db = mc_db::Db::connect_lazy("postgres://u:p@127.0.0.1:5432/none", 1, 0).unwrap();
        let realtime = mc_realtime::RealtimeHandle::start(8);
        let ws = Arc::new(mc_realtime::WsState::new(realtime.clone(), "test"));
        let state = Arc::new(AppState::new(
            db,
            crate::state::RuntimeHandles {
                actors: mc_core::actor::ActorRegistry::new(),
                adapters: Arc::new(crate::state::AdapterRegistryStub::default()),
            },
            crate::state::ConfigSnapshot {
                host: "127.0.0.1".into(),
                port: 0,
                session_cookie: "multica_session".into(),
                api_key_header: "X-Multica-Api-Key".into(),
                csrf_header: "X-Multica-Csrf".into(),
                ..Default::default()
            },
            realtime,
            ws,
        ));
        let _app: Router = crate::routes::router(state.clone()).with_state(state);
    }

    #[test]
    fn workspace_resolution_prefers_header_then_query() {
        let ws = Id::new();
        let mut headers = HeaderMap::new();
        headers.insert(WORKSPACE_ID_HEADER, ws.as_string().parse().unwrap());
        assert_eq!(
            resolve_workspace_id(&headers, &q(&[("workspace_id", &Id::new().as_string())]))
                .unwrap(),
            ws
        );
        let empty = HeaderMap::new();
        assert_eq!(
            resolve_workspace_id(&empty, &q(&[("workspace_id", &ws.as_string())])).unwrap(),
            ws
        );
        // 缺失 / 空 / 非法 → 400 "invalid workspace id"（上游 parseUUIDOrBadRequest）。
        for headers in [HeaderMap::new(), {
            let mut h = HeaderMap::new();
            h.insert(WORKSPACE_ID_HEADER, "not-a-uuid".parse().unwrap());
            h
        }] {
            let err = resolve_workspace_id(&headers, &q(&[])).unwrap_err();
            assert_eq!(err.http_status(), 400);
            assert_eq!(err.message(), "validation error: invalid workspace id");
        }
        let empty_header = {
            let mut h = HeaderMap::new();
            h.insert(WORKSPACE_ID_HEADER, "   ".parse().unwrap());
            h
        };
        assert!(resolve_workspace_id(&empty_header, &q(&[("workspace_id", "")])).is_err());
    }

    #[test]
    fn filters_are_sorted_deduped_and_validated() {
        let filter = parse_filter(&q(&[
            ("statuses", "todo,backlog,todo"),
            ("priorities", "high"),
            ("unread_only", "true"),
        ]))
        .unwrap();
        assert_eq!(filter.statuses, vec!["backlog", "todo"]);
        assert_eq!(filter.priorities, vec!["high"]);
        assert!(filter.unread_only);
        assert_eq!(filter.actors, Vec::<String>::new());

        // 空串等于未传（上游 `q.Get(name) != ""`）。
        assert!(parse_filter(&q(&[("statuses", "")]))
            .unwrap()
            .statuses
            .is_empty());

        for (query, message) in [
            (
                q(&[("unread_only", "1")]),
                "validation error: invalid unread_only",
            ),
            (
                q(&[("statuses", "a,,b")]),
                "validation error: empty filter value",
            ),
        ] {
            let err = parse_filter(&query).unwrap_err();
            assert_eq!(err.http_status(), 400);
            assert_eq!(err.message(), message);
        }

        let long = "x".repeat(FILTER_MAX_RAW_LEN + 1);
        assert_eq!(
            parse_filter(&q(&[("statuses", &long)]))
                .unwrap_err()
                .message(),
            "validation error: filter is too long"
        );
        let many = (0..=FILTER_MAX_VALUES)
            .map(|i| format!("s{i}"))
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(
            parse_filter(&q(&[("statuses", &many)]))
                .unwrap_err()
                .message(),
            "validation error: too many filter values"
        );
    }

    #[test]
    fn archive_limit_matches_upstream_bounds() {
        assert_eq!(parse_archived_limit(&q(&[])).unwrap(), 50);
        assert_eq!(parse_archived_limit(&q(&[("limit", "1")])).unwrap(), 1);
        assert_eq!(parse_archived_limit(&q(&[("limit", "100")])).unwrap(), 100);
        for bad in ["0", "101", "abc", "-1"] {
            let err = parse_archived_limit(&q(&[("limit", bad)])).unwrap_err();
            assert_eq!(err.http_status(), 400);
            assert_eq!(
                err.message(),
                "validation error: limit must be between 1 and 100"
            );
        }
    }

    #[test]
    fn list_window_defaults_and_bounds() {
        assert_eq!(parse_list_window(&q(&[])).unwrap(), (200, 0));
        assert_eq!(
            parse_list_window(&q(&[("limit", "5"), ("offset", "10")])).unwrap(),
            (5, 10)
        );
        assert_eq!(
            parse_list_window(&q(&[("limit", "501")]))
                .unwrap_err()
                .message(),
            "validation error: limit must be between 1 and 500"
        );
        assert_eq!(
            parse_list_window(&q(&[("offset", "-1")]))
                .unwrap_err()
                .message(),
            "validation error: offset must be >= 0"
        );
    }

    #[test]
    fn cursor_roundtrip_is_scope_bound() {
        let ws = Id::new();
        let user = Id::new();
        let filter = ArchivedInboxFilter {
            statuses: vec!["todo".into()],
            ..ArchivedInboxFilter::default()
        };
        let tag = archive_scope_tag(ws, user, &filter);
        let row = InboxItemRow {
            id: Id::new().as_uuid(),
            workspace_id: ws.as_uuid(),
            user_id: user.as_uuid(),
            issue_id: None,
            actor_type: "user".into(),
            actor_id: user.as_string(),
            category: "new_comment".into(),
            title: "t".into(),
            body: None,
            read_at: None,
            archived_at: None,
            created_at: Utc::now(),
            issue_status: None,
            issue_priority: None,
        };
        let encoded = encode_cursor(&tag, &row).unwrap();
        let parsed = parse_cursor(&q(&[("cursor", &encoded)]), &tag)
            .unwrap()
            .expect("cursor present");
        assert_eq!(parsed.id, row.id());
        assert_eq!(
            parsed.created_at.timestamp_micros(),
            row.created_at.timestamp_micros()
        );

        // 换了 scope（不同用户 / 不同过滤）就不能续页。
        let other = archive_scope_tag(ws, user, &ArchivedInboxFilter::default());
        assert_eq!(
            parse_cursor(&q(&[("cursor", &encoded)]), &other)
                .unwrap_err()
                .message(),
            "validation error: invalid archive cursor"
        );
        for bad in ["zzzz", "e30", "not json"] {
            assert!(parse_cursor(&q(&[("cursor", bad)]), &tag).is_err());
        }
        // 空串等于未传（上游 `q.Get("cursor") != ""`）→ 首页，不是错误。
        assert!(parse_cursor(&q(&[("cursor", "")]), &tag).unwrap().is_none());
        let too_long = "a".repeat(CURSOR_MAX_LEN + 1);
        assert_eq!(
            parse_cursor(&q(&[("cursor", &too_long)]), &tag)
                .unwrap_err()
                .message(),
            "validation error: invalid archive cursor"
        );
    }

    #[test]
    fn list_body_preview_matches_upstream_character_limit() {
        // 非 new_comment / 无 issue：原样返回。
        let long = "字".repeat(500);
        assert_eq!(
            list_body_preview("new_issue", true, Some(&long)),
            Some(long.clone())
        );
        assert_eq!(
            list_body_preview("new_comment", false, Some(&long)),
            Some(long.clone())
        );
        assert_eq!(list_body_preview("new_comment", true, None), None);

        // 恰好 200 字符：不截断。
        let exact = "a".repeat(200);
        assert_eq!(
            list_body_preview("new_comment", true, Some(&exact)),
            Some(exact.clone())
        );
        // 201 字符：前 199 + 省略号 = 200 字符。
        let over = "a".repeat(201);
        let preview = list_body_preview("new_comment", true, Some(&over)).unwrap();
        assert_eq!(preview.chars().count(), 200);
        assert!(preview.ends_with('…'));
        assert_eq!(preview, format!("{}…", "a".repeat(199)));

        // 多字节字符不会被切坏。
        let multibyte = "字".repeat(201);
        let preview = list_body_preview("new_comment", true, Some(&multibyte)).unwrap();
        assert_eq!(preview.chars().count(), 200);
        assert!(preview.starts_with('字'));
    }

    #[test]
    fn terminal_status_keys_are_the_builtin_ones() {
        assert_eq!(builtin_terminal_status_keys(), ["done", "cancelled"]);
    }
}
