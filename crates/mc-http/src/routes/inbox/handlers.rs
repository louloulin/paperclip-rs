//! inbox 的 14 条 handler 实体（`router()` 的注册目标）。
//!
//! 从 `routes/inbox.rs` 拆出（门 ⑩ 第 8 批）。**0 路由、0 行为变更**：
//! 父模块用 `pub(crate) use` / `pub use` 把符号原样重导出，外部路径逐字不变。

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;

use mc_repos::inbox::ArchivedInboxFilter;

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::routes::inbox::{ARCHIVED_GROUP_LIMIT};
use crate::routes::inbox::context::InboxScope;
use crate::routes::inbox::dto::{
    ArchivedFacetsDto, ArchivedPageDto, CountDto, InboxItemDto, WorkspaceUnreadDto,
};
use crate::routes::inbox::query::*;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// handler：读
// ---------------------------------------------------------------------------

/// `GET /api/inbox`（上游 `ListInbox`）。
pub(super) async fn list_inbox(
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
pub(super) async fn list_archived(
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
pub(super) async fn list_archived_page(
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
pub(super) async fn get_archived_facets(
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
pub(super) async fn count_unread(
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
pub(super) async fn unread_summary(
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
pub(super) async fn mark_all_read(
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
pub(super) async fn archive_all(
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
pub(super) async fn archive_all_read(
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
pub(super) async fn archive_completed(
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
pub(super) async fn mark_read(
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
pub(super) async fn mark_unread(
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
pub(super) async fn archive_item(
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
pub(super) async fn unarchive_item(
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

