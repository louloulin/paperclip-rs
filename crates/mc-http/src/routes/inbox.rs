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

use std::sync::Arc;

use axum::http::HeaderName;
use axum::routing::{get, post};
use axum::Router;

use mc_repos::inbox::BUILTIN_TERMINAL_STATUS_KEYS;

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
// 子模块（门 ⑩ 第 8 批拆分；0 路由、0 行为变更）
// ---------------------------------------------------------------------------

mod context;
mod dto;
mod handlers;
mod query;

#[cfg(test)]
mod tests;

// `resolve_workspace_id` 被 13 处 sibling 模块按 `crate::routes::inbox::resolve_workspace_id`
// 引用（agents / subscribers / autopilots/* / skills / notification_preferences …），
// 这里的重导出让那些路径**逐字不变**。
pub(crate) use context::resolve_workspace_id;
use handlers::{
    archive_all, archive_all_read, archive_completed, archive_item, count_unread,
    get_archived_facets, list_archived, list_archived_page, list_inbox, mark_all_read, mark_read,
    mark_unread, unarchive_item, unread_summary,
};

/// 内置终结状态 key 的只读视图（供文档/测试引用，避免常量漂移）。
pub fn builtin_terminal_status_keys() -> &'static [&'static str] {
    BUILTIN_TERMINAL_STATUS_KEYS
}
