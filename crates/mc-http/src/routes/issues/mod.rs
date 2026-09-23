//! `/api/issues*` + `/api/issue-statuses*`（M2-A / LUM-1348）。
//!
//! 覆盖上游 `server/internal/handler/issue.go` / `issue_status.go` 的核心读写面：
//!
//! - 集合：`GET/POST /api/issues`、`POST /api/issues/query`、`GET /api/issues/search`、
//!   `GET /api/issues/grouped`、`GET /api/issues/children`、`GET /api/issues/child-progress`、
//!   `POST /api/issues/batch-update`、`POST /api/issues/batch-delete`、
//!   `POST /api/issues/quick-create`（降级：同步落库，无 daemon 派单）
//! - 单体：`GET/PUT/DELETE /api/issues/:id`、`POST /api/issues/:id/move`、
//!   `GET /api/issues/:id/children`、`GET/POST/DELETE /api/issues/:id/reactions`、
//!   `GET /api/issues/:id/metadata` + `PUT/DELETE /api/issues/:id/metadata/:key`、
//!   `PUT/DELETE /api/issues/:id/properties/:propertyId`
//! - 目录：`GET/POST /api/issue-statuses`、`PATCH/DELETE /api/issue-statuses/:id`、
//!   `PATCH /api/issue-statuses/reorder`
//!
//! **尾斜杠双形态（LUM-1458）**：上游 `router.go` 的 `Route("<P>") + Get/Put/Delete("/")`
//! 走 chi 的 `Mount`，同时服务 `<P>` 与 `<P>/`；axum 0.7 不做归一化（少注册一个就是 404，
//! 不是 307）。因此集合与 item root 一律注册两个形态，且两个形态的方法集合逐字相同：
//!
//! - `GET|POST /api/issues` + `GET|POST /api/issues/`
//! - `GET|PUT|DELETE /api/issues/:id` + `GET|PUT|DELETE /api/issues/:id/`
//! - `GET|POST /api/issue-statuses` + `GET|POST /api/issue-statuses/`
//! - `PATCH|DELETE /api/issue-statuses/:id` + `PATCH|DELETE /api/issue-statuses/:id/`
//!
//! 而 `move` / `children` / `reactions` / `reorder` 这些是 `r.Get("/move")` 之类的 plain
//! 子路由，上游只有**一个**形态 ⇒ 不要加别名（`EXTRA_ALIAS` 会由 gate ⑦ 的第二条命令告警）。
//! 规则与全仓对账见 `docs/37-M3-W3C-PREFLIGHT.md` §15.1/§15.3，本片落地记录见其 §21。
//!
//! **所有实现都是运行时 sqlx builder + 参数绑定**（不用 compile-time 宏），因此构建期
//! 不需要数据库。workspace 由 header / query 解析（见 `resolve_workspace`），成员校验复用
//! `invitations::require_workspace_member`（非成员 → 404，与上游一致）。
//!
//! 上游存在但本仓尚未实现的端点统一返回 **501**（`not_implemented`），清单与原因见
//! `docs/11-M2-ISSUE.md`。
//!
//! **M3-6（LUM-1429）移交**：原先在本文件以 501 stub 注册的 6 条
//! （`POST /api/issues/preview-trigger`、`GET /api/issues/:id/active-task`、
//! `POST /api/issues/:id/rerun`、`GET /api/issues/:id/task-runs`、
//! `GET /api/issues/:id/usage`、`POST /api/issues/:id/tasks/:taskId/cancel`）
//! 已由 `super::tasks::router()` 真实实现，本文件**删除**这些注册（同 path+method
//! 重复注册会让 axum 在 `Router::route` 处 panic）。`not_implemented` 仍为其余
//! 未实现端点服务。
//!
//! 注意（M1-D 实测踩过的坑）：axum 0.7（matchit 0.7）路径参数必须写 `:id`，
//! `{id}` 会被当字面量段——编译通过但恒 404。
//!
//! `Option<Option<T>>`：JSON 补丁需要三态（缺失 / `null` / 有值），因此显式允许。
//!
//! 文件布局（R7：单文件 800 行硬上限，`scripts/file_size_check.py` + 门 ⑩ 执行）：
//! - `mod.rs`（本文件）：模块文档 + 常量 + `pub fn router()` + 501 占位 handler（`not_implemented`）
//! - `helpers.rs`：错误构造 / 参数小工具（三态日期、三态 `Option<Option<T>>`、assignee 存在性校验、`attachment_ids` 形态）
//! - `context.rs`：workspace 解析 + status key 目录（`StatusCatalog`，含 `load_catalog` / `load_issue`）
//! - `query.rs`：列表查询参数 `ListIssuesQuery` → `mc_repos::issue::IssueFilter`
//! - `dto.rs`：响应 DTO + 请求体（`CreateIssueRequest` / `UpdateIssueRequest` / …）
//! - `list.rs`：集合端点（list / query / search / grouped / children / child-progress / batch-*）
//! - `crud.rs`：单体端点（create / quick-create / get / update / delete / move）
//! - `extras.rs`：reactions / metadata / properties
//! - `statuses.rs`：`/api/issue-statuses*` 目录端点
//!
//! 子模块条目一律 `pub(crate)`；对外面（`routes/mod.rs` 的 `pub mod issues;`、`mount.rs` 的
//! `super::issues::router()`、`issue_table` 的 `crate::routes::issues::{validation, repo_err,
//! resolve_workspace, load_catalog, IssueDto, WorkspaceQuery}`）与拆分前逐字一致（后者由本文件的
//! `pub(crate) use` 维持）。
#![allow(clippy::option_option)]

mod context;
mod crud;
mod dto;
mod extras;
mod helpers;
mod list;
mod query;
mod statuses;

use crate::state::AppState;
use axum::extract::OriginalUri;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use std::sync::Arc;

pub(crate) use self::crud::{
    batch_delete, batch_update, create_issue, delete_issue, get_issue, move_issue,
    quick_create_issue, update_issue,
};
pub(crate) use self::extras::{
    add_reaction, delete_metadata_key, delete_property, get_metadata, list_reactions,
    remove_reaction, set_metadata_key, set_property,
};
pub(crate) use self::list::{
    child_progress, list_children_by_parents, list_grouped, list_issue_children, list_issues,
    query_issues, search_issues,
};
pub(crate) use self::statuses::{
    create_status, delete_status, list_statuses, reorder_statuses, update_status,
};

pub(crate) use self::context::{load_catalog, resolve_workspace, WorkspaceQuery};
pub(crate) use self::dto::IssueDto;
pub(crate) use self::helpers::{repo_err, validation};

/// workspace 解析 header（M0 未定义 header 解析，`/api/issues` 自带 `?workspace_id=` 回退）。
pub(crate) const WORKSPACE_ID_HEADER: &str = "x-workspace-id";
/// workspace slug header。
pub(crate) const WORKSPACE_SLUG_HEADER: &str = "x-workspace-slug";
/// metadata key 长度上限（上游同名常量）。
pub(crate) const METADATA_KEY_MAX_LEN: usize = 64;
/// metadata 条数上限（上游同名常量）。
pub(crate) const METADATA_KEYS_MAX: usize = 50;
/// 日期字段格式。
pub(crate) const DATE_FORMAT: &str = "%Y-%m-%d";
/// `/api/issues*` + `/api/issue-statuses*` 路由。
#[allow(clippy::too_many_lines)]
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        // ---- 集合 ----------------------------------------------------------
        .route("/api/issues", get(list_issues).post(create_issue))
        // 尾斜杠别名（M2-C `inbox` 同款处理：客户端常带斜杠）。上游是
        // `Route("/api/issues") + Get("/")/Post("/")` 走 chi `Mount`，两种形态都服务
        // （docs/37 §15.1）⇒ 这里两个形态都必须注册，且**方法集合逐字相同**。
        .route("/api/issues/", get(list_issues).post(create_issue))
        .route("/api/issues/query", post(query_issues))
        .route("/api/issues/search", get(search_issues))
        .route("/api/issues/grouped", get(list_grouped))
        .route("/api/issues/children", get(list_children_by_parents))
        .route("/api/issues/child-progress", get(child_progress))
        .route("/api/issues/batch-update", post(batch_update))
        .route("/api/issues/batch-delete", post(batch_delete))
        // ---- 尚未实现（501；依赖 agent/squad/task/attachment 等 M3 能力）----
        .route("/api/issues/quick-create", post(quick_create_issue))
        // ---- 单体 ----------------------------------------------------------
        .route(
            "/api/issues/:id",
            get(get_issue).put(update_issue).delete(delete_issue),
        )
        // 同上：上游 `Route("/{id}") + Get/Put/Delete("/")` ⇒ 带尾斜杠的形态也要服务。
        // 下面的普通子路由（`move`/`children`/`reactions`/…）是 `r.Get("/move")` 这类
        // plain 注册，只有**一个**形态，不要跟着加别名。
        .route(
            "/api/issues/:id/",
            get(get_issue).put(update_issue).delete(delete_issue),
        )
        .route("/api/issues/:id/move", post(move_issue))
        .route("/api/issues/:id/children", get(list_issue_children))
        .route(
            "/api/issues/:id/reactions",
            get(list_reactions)
                .post(add_reaction)
                .delete(remove_reaction),
        )
        .route("/api/issues/:id/metadata", get(get_metadata))
        .route(
            "/api/issues/:id/metadata/:key",
            put(set_metadata_key).delete(delete_metadata_key),
        )
        .route(
            "/api/issues/:id/properties/:propertyId",
            put(set_property).delete(delete_property),
        )
        // ---- 尚未实现（501）：comments / subscribers 归 M2-B / M2-C ----------
        .route(
            "/api/issues/:id/comments/trigger-preview",
            post(not_implemented),
        )
        .route("/api/issues/:id/timeline", get(not_implemented))
        .route("/api/issues/:id/attachments", get(not_implemented))
        .route("/api/issues/:id/pull-requests", get(not_implemented))
        .route("/api/issues/:id/labels", get(not_implemented))
        .route("/api/issues/:id/labels/:labelId", delete(not_implemented))
        .route("/api/issues/:id/quick-actions", get(not_implemented))
        .route(
            "/api/issues/:id/wakeups",
            get(not_implemented).post(not_implemented),
        )
        .route("/api/issues/:id/wakeups/:wakeupId", put(not_implemented))
        .route(
            "/api/issues/:id/wakeups/:wakeupId/disable",
            post(not_implemented),
        )
        .route(
            "/api/issues/:id/wakeups/:wakeupId/enable",
            post(not_implemented),
        )
        .route(
            "/api/issues/:id/wakeups/:wakeupId/instruction",
            patch(not_implemented),
        )
        .route("/api/issue-wakeups", get(not_implemented))
        // ---- status 目录 ---------------------------------------------------
        .route(
            "/api/issue-statuses",
            get(list_statuses).post(create_status),
        )
        // 双形态注册，理由同上；`reorder` 是 plain 子路由 ⇒ 只注册不带斜杠的形态。
        .route(
            "/api/issue-statuses/",
            get(list_statuses).post(create_status),
        )
        .route("/api/issue-statuses/reorder", patch(reorder_statuses))
        .route(
            "/api/issue-statuses/:id",
            patch(update_status).delete(delete_status),
        )
        .route(
            "/api/issue-statuses/:id/",
            patch(update_status).delete(delete_status),
        )
        // M2-D（LUM-1355）：`/api/issues/table/{groups,rows,facets}` 与
        // `/api/issues/limit-usage` 在独立文件里实现，这里**委托合并**（`mount.rs`
        // 与 `routes/mod.rs` 现有注册点都不动）。清单与理由见 `docs/14-M2-TABLE.md` §6。
        .merge(super::issue_table::router())
}

// ---------------------------------------------------------------------------
// 501 占位
// ---------------------------------------------------------------------------

/// 上游存在但本仓尚未实现的端点：统一 501，body 与 `ApiError` 同形。
///
/// axum 的 handler 必须是 `async fn`；这里没有真的 await，因此显式 `allow(unused_async)`。
#[allow(clippy::unused_async)]
pub(crate) async fn not_implemented(OriginalUri(uri): OriginalUri) -> Response {
    let body = Json(serde_json::json!({
        "error": {
            "code": "not_implemented",
            "message": format!(
                "{} is not implemented in multica-rs yet (see docs/11-M2-ISSUE.md)",
                uri.path()
            ),
        }
    }));
    (StatusCode::NOT_IMPLEMENTED, body).into_response()
}
