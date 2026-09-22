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
//! **所有实现都是运行时 sqlx builder + 参数绑定**（不用 compile-time 宏），因此构建期
//! 不需要数据库。workspace 由 header / query 解析（见 `resolve_workspace`），成员校验复用
//! `invitations::require_workspace_member`（非成员 → 404，与上游一致）。
//!
//! 上游存在但本仓尚未实现的端点统一返回 **501**（`not_implemented`），清单与原因见
//! `docs/11-M2-ISSUE.md`。
//!
//! 注意（M1-D 实测踩过的坑）：axum 0.7（matchit 0.7）路径参数必须写 `:id`，
//! `{id}` 会被当字面量段——编译通过但恒 404。
//!
//! `Option<Option<T>>`：JSON 补丁需要三态（缺失 / `null` / 有值），因此显式允许。
#![allow(clippy::option_option)]

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use chrono::NaiveDate;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value as JsonValue;
use uuid::Uuid;

use mc_core::priority::Priority;
use mc_core::status::{is_valid_transition, IssueStatus, StatusCategory, CANONICAL_KEYS};
use mc_core::Id;
use mc_errors::Error;
use mc_repos::issue::{
    parse_assignee_type, parse_issue_origin, split_comma_param, IssueFilter, IssueGroupField,
    IssueOrderBy, IssueReactionRow, IssueRepo, IssueRow, IssueStatusRow, IssueUpdate, NewIssue,
    CHILDREN_PARENTS_MAX, LIST_MAX_LIMIT, SEARCH_DEFAULT_LIMIT, SEARCH_MAX_LIMIT,
};
use mc_repos::issue_status::{
    category_str, parse_category, validate_key, IssueStatusRepo, IssueStatusUpdate, NewIssueStatus,
    DEFAULT_STATUSES, KEY_MAX_LEN,
};
use mc_repos::RepoError;

use crate::error::ApiResult;
use crate::routes::auth_user::AuthUser;
use crate::routes::invitations::{require_workspace_admin, require_workspace_member};
use crate::state::AppState;

/// workspace 解析 header（M0 未定义 header 解析，`/api/issues` 自带 `?workspace_id=` 回退）。
const WORKSPACE_ID_HEADER: &str = "x-workspace-id";
/// workspace slug header。
const WORKSPACE_SLUG_HEADER: &str = "x-workspace-slug";
/// metadata key 长度上限（上游同名常量）。
const METADATA_KEY_MAX_LEN: usize = 64;
/// metadata 条数上限（上游同名常量）。
const METADATA_KEYS_MAX: usize = 50;
/// 日期字段格式。
const DATE_FORMAT: &str = "%Y-%m-%d";

/// `/api/issues*` + `/api/issue-statuses*` 路由。
#[allow(clippy::too_many_lines)]
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        // ---- 集合 ----------------------------------------------------------
        .route("/api/issues", get(list_issues).post(create_issue))
        // 尾斜杠别名（M2-C `inbox` 同款处理：客户端常带斜杠）
        .route("/api/issues/", get(list_issues))
        .route("/api/issues/query", post(query_issues))
        .route("/api/issues/search", get(search_issues))
        .route("/api/issues/grouped", get(list_grouped))
        .route("/api/issues/children", get(list_children_by_parents))
        .route("/api/issues/child-progress", get(child_progress))
        .route("/api/issues/batch-update", post(batch_update))
        .route("/api/issues/batch-delete", post(batch_delete))
        // ---- 尚未实现（501；依赖 agent/squad/task/attachment/table 等 M3 能力）----
        .route("/api/issues/limit-usage", get(not_implemented))
        .route("/api/issues/quick-create", post(quick_create_issue))
        .route("/api/issues/preview-trigger", post(not_implemented))
        .route("/api/issues/table/groups", post(not_implemented))
        .route("/api/issues/table/rows", post(not_implemented))
        .route("/api/issues/table/facets", post(not_implemented))
        // ---- 单体 ----------------------------------------------------------
        .route(
            "/api/issues/:id",
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
        .route("/api/issues/:id/active-task", get(not_implemented))
        .route("/api/issues/:id/rerun", post(not_implemented))
        .route("/api/issues/:id/task-runs", get(not_implemented))
        .route("/api/issues/:id/usage", get(not_implemented))
        .route("/api/issues/:id/attachments", get(not_implemented))
        .route("/api/issues/:id/pull-requests", get(not_implemented))
        .route("/api/issues/:id/labels", get(not_implemented))
        .route("/api/issues/:id/labels/:labelId", delete(not_implemented))
        .route("/api/issues/:id/quick-actions", get(not_implemented))
        .route(
            "/api/issues/:id/tasks/:taskId/cancel",
            post(not_implemented),
        )
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
        .route("/api/issue-statuses/", get(list_statuses))
        .route("/api/issue-statuses/reorder", patch(reorder_statuses))
        .route(
            "/api/issue-statuses/:id",
            patch(update_status).delete(delete_status),
        )
}

// ---------------------------------------------------------------------------
// 错误 / 参数小工具
// ---------------------------------------------------------------------------

fn validation(message: impl Into<String>) -> Error {
    Error::Validation {
        message: message.into(),
        details: Vec::new(),
    }
}

fn repo_err(e: RepoError) -> Error {
    match e {
        RepoError::NotFound => Error::NotFound {
            resource: "issue".into(),
        },
        RepoError::Conflict => Error::Conflict {
            message: "issue was modified concurrently; refetch and retry".into(),
        },
        RepoError::Db(message) => Error::Database(message),
    }
}

fn status_repo_err(e: RepoError) -> Error {
    match e {
        RepoError::NotFound => Error::NotFound {
            resource: "issue_status".into(),
        },
        RepoError::Conflict => Error::Conflict {
            message: "issue status key is reserved or still in use".into(),
        },
        RepoError::Db(message) => Error::Database(message),
    }
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// 日期补丁的三态转换：`None` 不动、`null`/`""` 清空、`YYYY-MM-DD` 写入。
fn parse_date_patch(
    field: &str,
    value: Option<Option<String>>,
) -> Result<Option<Option<NaiveDate>>, Error> {
    match value {
        None => Ok(None),
        Some(None) => Ok(Some(None)),
        Some(Some(raw)) if raw.trim().is_empty() => Ok(Some(None)),
        Some(Some(raw)) => NaiveDate::parse_from_str(raw.trim(), DATE_FORMAT)
            .map(|d| Some(Some(d)))
            .map_err(|_| validation(format!("{field} must be formatted as YYYY-MM-DD"))),
    }
}

/// `Option<Option<T>>` 的 serde helper：字段缺失 → `None`，`null` → `Some(None)`，
/// 有值 → `Some(Some(v))`。
fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// 上游把 assignee 类型写作 `member`（历史命名），本仓统一用 `user`。
fn normalize_assignee_type(raw: &str) -> &str {
    match raw.trim() {
        "member" => "user",
        other => other,
    }
}

// ---------------------------------------------------------------------------
// workspace 解析
// ---------------------------------------------------------------------------

/// workspace 选择器：`?workspace_id=` / `?workspace_slug=`（header 优先）。
#[derive(Debug, Default, Deserialize)]
pub struct WorkspaceQuery {
    pub workspace_id: Option<String>,
    pub workspace_slug: Option<String>,
}

/// 解析目标 workspace：header `x-workspace-id` → header `x-workspace-slug` →
/// `?workspace_id` → `?workspace_slug`（slug 要求 `archived_at IS NULL`）。
///
/// 上游是从 session 的 "current workspace" / task token 里取；本仓 M1 的 auth 只有
/// `X-Multica-User-Id` dev-mode 提取器，没有 workspace 上下文，因此显式传参（详见
/// `docs/11-M2-ISSUE.md`）。四个来源都缺 → 400。
async fn resolve_workspace(
    state: &AppState,
    headers: &HeaderMap,
    query: &WorkspaceQuery,
) -> Result<Id, Error> {
    if let Some(raw) = header_str(headers, WORKSPACE_ID_HEADER).or(query.workspace_id.as_deref()) {
        return Id::parse(raw).map_err(|_| validation("workspace_id must be a uuid"));
    }
    if let Some(slug) =
        header_str(headers, WORKSPACE_SLUG_HEADER).or(query.workspace_slug.as_deref())
    {
        let row: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM workspace WHERE slug = $1 AND archived_at IS NULL")
                .bind(slug)
                .fetch_optional(state.db.pool())
                .await
                .map_err(|e| Error::Database(e.to_string()))?;
        return row.map(|(id,)| Id::from(id)).ok_or(Error::NotFound {
            resource: "workspace".into(),
        });
    }
    Err(validation(
        "workspace_id (or workspace_slug) is required: pass the x-workspace-id header or ?workspace_id=",
    ))
}

fn issue_repo(state: &AppState) -> IssueRepo {
    IssueRepo::new(state.db.clone())
}

fn status_repo(state: &AppState) -> IssueStatusRepo {
    IssueStatusRepo::new(state.db.clone())
}

/// `:id` 既接受 UUID 也接受 identifier（`LUM-1348`）。
async fn load_issue(repo: &IssueRepo, workspace_id: Id, raw: &str) -> Result<IssueRow, Error> {
    let needle = raw.trim();
    match Id::parse(needle) {
        Ok(id) => repo.get(workspace_id, id).await.map_err(repo_err),
        Err(_) => repo
            .get_by_identifier(workspace_id, &needle.to_uppercase())
            .await
            .map_err(repo_err),
    }
}

fn parse_target_id(field: &str, raw: &str) -> Result<Id, Error> {
    Id::parse(raw.trim()).map_err(|_| validation(format!("{field} must be a uuid")))
}

// ---------------------------------------------------------------------------
// status 目录（内置 7 个 + workspace 自定义）
// ---------------------------------------------------------------------------

/// status key → (展示名, 生命周期分类)。内置目录打底，DB 里的自定义 status 覆盖。
#[derive(Debug, Clone, Default)]
struct StatusCatalog {
    names: HashMap<String, String>,
    categories: HashMap<String, StatusCategory>,
}

impl StatusCatalog {
    fn with_builtins() -> Self {
        let mut catalog = Self::default();
        for (key, name, category, _position) in DEFAULT_STATUSES {
            catalog.names.insert(key.to_string(), name.to_string());
            if let Some(parsed) = parse_category(category) {
                catalog.categories.insert(key.to_string(), parsed);
            }
        }
        catalog
    }

    fn insert(&mut self, key: &str, name: &str, category: Option<StatusCategory>) {
        self.names.insert(key.to_string(), name.to_string());
        if let Some(category) = category {
            self.categories.insert(key.to_string(), category);
        }
    }

    fn contains_key(&self, key: &str) -> bool {
        self.names.contains_key(key)
    }

    fn category_of(&self, key: &str) -> Option<StatusCategory> {
        self.categories.get(key).copied()
    }

    /// 某个分类下的全部 key（内置 + 自定义）——用于 `status_category` 过滤展开。
    fn keys_in_category(&self, category: StatusCategory) -> Vec<String> {
        self.categories
            .iter()
            .filter(|(_, c)| **c == category)
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// 自定义 status 的展示名（内置 key 返回 `None`，上游用空 `status_name` 表示内置）。
    fn custom_name(&self, key: &str) -> Option<String> {
        if CANONICAL_KEYS.contains(&key) {
            return None;
        }
        self.names.get(key).cloned()
    }

    /// 某个 issue 的展示名（自定义优先，其次行上的 `status_name`，内置回退空串）。
    fn display_name(&self, row: &IssueRow) -> String {
        if let Some(name) = row.status_name.as_deref().filter(|n| !n.is_empty()) {
            return name.to_string();
        }
        if CANONICAL_KEYS.contains(&row.status.as_str()) {
            return String::new();
        }
        self.names.get(&row.status).cloned().unwrap_or_default()
    }
}

async fn load_catalog(state: &AppState, workspace_id: Id) -> Result<StatusCatalog, Error> {
    let mut catalog = StatusCatalog::with_builtins();
    let rows = status_repo(state)
        .list(workspace_id)
        .await
        .map_err(status_repo_err)?;
    for row in rows {
        catalog.insert(&row.key, &row.name, parse_category(&row.category));
    }
    Ok(catalog)
}

// ---------------------------------------------------------------------------
// 列表查询参数
// ---------------------------------------------------------------------------

/// `/api/issues` 系列共用的过滤参数（`query` 接口用同一套 key，值是字符串）。
#[derive(Debug, Default, Deserialize)]
pub struct ListIssuesQuery {
    pub workspace_id: Option<String>,
    pub workspace_slug: Option<String>,
    /// 全文（title / description / identifier）
    pub q: Option<String>,
    /// 单个 status key
    pub status: Option<String>,
    /// status key CSV
    pub statuses: Option<String>,
    /// `open` / `closed`
    pub status_category: Option<String>,
    /// 分类 CSV
    pub status_categories: Option<String>,
    pub priority: Option<String>,
    pub priorities: Option<String>,
    pub assignee_id: Option<String>,
    pub assignee_ids: Option<String>,
    pub assignee_type: Option<String>,
    pub assignee_types: Option<String>,
    pub creator_id: Option<String>,
    pub parent_issue_id: Option<String>,
    pub project_id: Option<String>,
    pub stage: Option<i32>,
    /// `true` = 不过滤终态
    pub include_closed: Option<bool>,
    /// 只看未关闭（上游 `open_only`）
    pub open_only: Option<bool>,
    /// 只看顶层 issue
    pub only_parentless: Option<bool>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    /// `updated_at`（默认）/ `created_at` / `position` / `number`
    pub sort: Option<String>,
    /// 接受但忽略（白名单排序已固定方向，见 docs/11 §5）
    pub direction: Option<String>,
    /// `/api/issues/grouped` 专用
    pub group_by: Option<String>,
}

impl ListIssuesQuery {
    /// `POST /api/issues/query`：body 是「与 query string 同 key 的扁平对象」，
    /// 值接受字符串（上游约定）也接受 number / bool。
    fn from_pairs(pairs: &HashMap<String, JsonValue>) -> Result<Self, Error> {
        let mut query = Self::default();
        for (key, value) in pairs {
            let Some(text) = scalar_to_string(value) else {
                return Err(validation(format!("query param {key} must be a scalar")));
            };
            match key.as_str() {
                "q" => query.q = Some(text),
                "status" => query.status = Some(text),
                "statuses" => query.statuses = Some(text),
                "status_category" => query.status_category = Some(text),
                "status_categories" => query.status_categories = Some(text),
                "priority" => query.priority = Some(text),
                "priorities" => query.priorities = Some(text),
                "assignee_id" => query.assignee_id = Some(text),
                "assignee_ids" => query.assignee_ids = Some(text),
                "assignee_type" => query.assignee_type = Some(text),
                "assignee_types" => query.assignee_types = Some(text),
                "creator_id" => query.creator_id = Some(text),
                "parent_issue_id" => query.parent_issue_id = Some(text),
                "project_id" => query.project_id = Some(text),
                "workspace_id" => query.workspace_id = Some(text),
                "workspace_slug" => query.workspace_slug = Some(text),
                "stage" => query.stage = Some(parse_number("stage", &text)?),
                "limit" => query.limit = Some(parse_number("limit", &text)?),
                "offset" => query.offset = Some(parse_number("offset", &text)?),
                "sort" => query.sort = Some(text),
                "direction" => query.direction = Some(text),
                "group_by" => query.group_by = Some(text),
                "include_closed" => {
                    query.include_closed = Some(parse_bool("include_closed", &text)?);
                }
                "open_only" => query.open_only = Some(parse_bool("open_only", &text)?),
                "only_parentless" => {
                    query.only_parentless = Some(parse_bool("only_parentless", &text)?);
                }
                // 上游 `QueryIssues` 直接透传给 `ListIssues`，未知 key 被忽略
                _ => {}
            }
        }
        Ok(query)
    }

    fn workspace_selector(&self) -> WorkspaceQuery {
        WorkspaceQuery {
            workspace_id: self.workspace_id.clone(),
            workspace_slug: self.workspace_slug.clone(),
        }
    }
}

fn scalar_to_string(value: &JsonValue) -> Option<String> {
    match value {
        JsonValue::String(s) => Some(s.clone()),
        JsonValue::Number(n) => Some(n.to_string()),
        JsonValue::Bool(b) => Some(b.to_string()),
        JsonValue::Null | JsonValue::Array(_) | JsonValue::Object(_) => None,
    }
}

fn parse_number<T: std::str::FromStr>(field: &str, raw: &str) -> Result<T, Error> {
    raw.trim()
        .parse()
        .map_err(|_| validation(format!("{field} has an invalid value: {raw}")))
}

fn parse_bool(field: &str, raw: &str) -> Result<bool, Error> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        _ => Err(validation(format!("{field} must be true or false"))),
    }
}

fn parse_order(sort: Option<&str>) -> Result<IssueOrderBy, Error> {
    let raw = sort.unwrap_or("").trim();
    match raw {
        "" | "updated_at" | "last_activity" | "last_activity_at" => Ok(IssueOrderBy::UpdatedDesc),
        "position" => Ok(IssueOrderBy::PositionAsc),
        "created_at" => Ok(IssueOrderBy::CreatedDesc),
        "number" => Ok(IssueOrderBy::NumberAsc),
        _ => Err(validation(format!("unsupported sort: {raw}"))),
    }
}

/// 过滤参数 → `IssueFilter`（`status_categories` 在这里展开成具体 key）。
fn build_filter(
    workspace_id: Id,
    query: &ListIssuesQuery,
    terminal_statuses: &[String],
) -> Result<IssueFilter, Error> {
    let mut filter = IssueFilter::new(workspace_id);

    let mut statuses = split_comma_param(query.statuses.as_deref().unwrap_or(""))
        .or_else(|| split_comma_param(query.status.as_deref().unwrap_or("")));

    // 分类过滤：展开成具体 key 后与显式 status 取交集（上游是 AND）
    let categories = split_comma_param(query.status_categories.as_deref().unwrap_or(""))
        .or_else(|| split_comma_param(query.status_category.as_deref().unwrap_or("")));
    if let Some(categories) = categories {
        // 需要内置 + 自定义目录；这里只展开内置（自定义 key 由 handler 追加）
        let mut expanded: Vec<String> = Vec::new();
        for raw in categories {
            let category = parse_category(&raw)
                .ok_or_else(|| validation(format!("unsupported status_category: {raw}")))?;
            for key in CANONICAL_KEYS {
                if IssueStatus::from_key(key).map(IssueStatus::category) == Some(category) {
                    expanded.push((*key).to_string());
                }
            }
        }
        statuses = Some(match statuses {
            Some(explicit) => explicit
                .into_iter()
                .filter(|s| expanded.contains(s))
                .collect(),
            None => expanded,
        });
    }
    filter.statuses = statuses;

    filter.priorities = split_comma_param(query.priorities.as_deref().unwrap_or(""))
        .or_else(|| split_comma_param(query.priority.as_deref().unwrap_or("")));

    let assignee_types = split_comma_param(query.assignee_types.as_deref().unwrap_or(""))
        .or_else(|| split_comma_param(query.assignee_type.as_deref().unwrap_or("")));
    if let Some(types) = assignee_types {
        if types.len() > 1 {
            return Err(validation("assignee_types accepts a single value"));
        }
        if let Some(raw) = types.first() {
            let normalized = normalize_assignee_type(raw);
            if parse_assignee_type(normalized).is_none() {
                return Err(validation(format!("invalid assignee_type: {raw}")));
            }
            filter.assignee_type = Some(normalized.to_string());
        }
    }

    filter.assignee_ids = split_comma_param(query.assignee_ids.as_deref().unwrap_or(""))
        .or_else(|| split_comma_param(query.assignee_id.as_deref().unwrap_or("")));
    filter.creator_id = split_comma_param(query.creator_id.as_deref().unwrap_or(""))
        .and_then(|v| v.into_iter().next());

    if let Some(raw) = query.parent_issue_id.as_deref() {
        filter.parent_issue_id = Some(parse_target_id("parent_issue_id", raw)?);
    }
    if let Some(raw) = query.project_id.as_deref() {
        filter.project_id = Some(parse_target_id("project_id", raw)?);
    }
    filter.stage = query.stage;
    filter.q = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(str::to_string);

    // 终态过滤：`open_only` / `include_closed=false` 都排除终态 key
    let include_closed = match (query.include_closed, query.open_only) {
        (_, Some(true)) => false,
        (Some(value), _) => value,
        (None, _) => true,
    };
    filter.include_closed = include_closed;
    filter.terminal_statuses = terminal_statuses.to_vec();
    filter.only_parentless = query.only_parentless.unwrap_or(false);
    filter.limit = query.limit.map(|l| l.clamp(1, LIST_MAX_LIMIT));
    filter.offset = query.offset.map(|o| o.max(0));
    filter.order = parse_order(query.sort.as_deref())?;
    Ok(filter)
}

/// 把目录里的自定义 closed status 也塞进 `status_categories` 展开结果。
///
/// `IssueRepo::terminal_status_keys` 已覆盖「内置终态 + 自定义 closed」，这里复用同一
/// 语义去补全分类过滤（内置部分由 `build_filter` 负责）。
fn expand_custom_categories(
    filter: &mut IssueFilter,
    query: &ListIssuesQuery,
    catalog: &StatusCatalog,
) -> Result<(), Error> {
    let categories = split_comma_param(query.status_categories.as_deref().unwrap_or(""))
        .or_else(|| split_comma_param(query.status_category.as_deref().unwrap_or("")));
    let Some(categories) = categories else {
        return Ok(());
    };
    let mut custom: Vec<String> = Vec::new();
    for raw in categories {
        let category = parse_category(&raw)
            .ok_or_else(|| validation(format!("unsupported status_category: {raw}")))?;
        custom.extend(
            catalog
                .keys_in_category(category)
                .into_iter()
                .filter(|key| !CANONICAL_KEYS.contains(&key.as_str())),
        );
    }
    if custom.is_empty() {
        return Ok(());
    }
    let mut keys = filter.statuses.clone().unwrap_or_default();
    keys.extend(custom);
    keys.sort();
    keys.dedup();
    filter.statuses = Some(keys);
    let _ = category_str;
    Ok(())
}

// ---------------------------------------------------------------------------
// DTO
// ---------------------------------------------------------------------------

/// issue 响应（对齐上游 `IssueResponse` 字段名）。
#[derive(Debug, Clone, Serialize)]
pub struct IssueDto {
    pub id: String,
    pub workspace_id: String,
    pub number: i32,
    pub identifier: String,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub status_category: String,
    pub status_name: String,
    pub priority: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee_id: Option<String>,
    pub creator_type: String,
    pub creator_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_issue_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub position: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
    pub revision: i64,
    pub metadata: JsonValue,
    pub properties: JsonValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<String>,
}

impl IssueDto {
    fn from_row(row: &IssueRow, catalog: &StatusCatalog) -> Self {
        let category = catalog
            .category_of(&row.status)
            .or_else(|| row.status_category());
        Self {
            id: row.id.to_string(),
            workspace_id: row.workspace_id.to_string(),
            number: row.number,
            identifier: row.identifier.clone(),
            title: row.title.clone(),
            description: row.description.clone(),
            status: row.status.clone(),
            status_category: category.map_or(String::new(), |c| category_str(c).to_string()),
            status_name: catalog.display_name(row),
            priority: row.priority.clone(),
            assignee_type: row.assignee_type.clone(),
            assignee_id: row.assignee_id.clone(),
            creator_type: row.creator_type.clone(),
            creator_id: row.creator_id.clone(),
            parent_issue_id: row.parent_issue_id.map(|id| id.to_string()),
            project_id: row.project_id.map(|id| id.to_string()),
            position: row.position,
            stage: row.stage,
            start_date: row.start_date.map(|d| d.format(DATE_FORMAT).to_string()),
            due_date: row.due_date.map(|d| d.format(DATE_FORMAT).to_string()),
            revision: row.revision,
            metadata: row.metadata.clone(),
            properties: row.properties.clone(),
            origin: row.origin.clone(),
            created_at: row.created_at.to_rfc3339(),
            updated_at: row.updated_at.to_rfc3339(),
            last_activity_at: row.last_activity_at.map(|t| t.to_rfc3339()),
        }
    }
}

#[derive(Debug, Serialize)]
struct IssueListResponse {
    issues: Vec<IssueDto>,
    total: i64,
}

#[derive(Debug, Serialize)]
struct IssueChildrenResponse {
    issues: Vec<IssueDto>,
}

#[derive(Debug, Serialize)]
struct SearchHitDto {
    #[serde(flatten)]
    issue: IssueDto,
    match_source: String,
}

#[derive(Debug, Serialize)]
struct SearchResponse {
    issues: Vec<SearchHitDto>,
}

#[derive(Debug, Serialize)]
struct GroupedGroupDto {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assignee_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assignee_id: Option<String>,
    total: i64,
    done: i64,
    issues: Vec<IssueDto>,
}

#[derive(Debug, Serialize)]
struct GroupedResponse {
    group_by: String,
    groups: Vec<GroupedGroupDto>,
}

/// 子 issue 进度。
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ChildProgressDto {
    pub parent_issue_id: String,
    pub total: i64,
    pub done: i64,
    pub percent: f64,
}

#[derive(Debug, Serialize)]
struct ChildProgressResponse {
    progress: Vec<ChildProgressDto>,
}

#[derive(Debug, Serialize)]
struct MetadataResponse {
    metadata: JsonValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    issue_revision: Option<i64>,
}

#[derive(Debug, Serialize)]
struct PropertiesResponse {
    properties: JsonValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    issue_revision: Option<i64>,
}

#[derive(Debug, Serialize)]
struct UpdatedResponse {
    updated: u64,
}

#[derive(Debug, Serialize)]
struct DeletedResponse {
    deleted: u64,
}

/// `issue_reaction` 响应。
#[derive(Debug, Clone, Serialize)]
pub struct IssueReactionDto {
    pub id: String,
    pub issue_id: String,
    pub actor_type: String,
    pub actor_id: String,
    pub emoji: String,
    pub created_at: String,
}

/// `issue_status` 响应（对齐上游 `IssueStatusResponse`；本仓没有的列用固定值补齐）。
#[derive(Debug, Clone, Serialize)]
pub struct IssueStatusDto {
    pub id: String,
    pub workspace_id: String,
    pub key: String,
    pub name: String,
    pub description: String,
    pub color: String,
    pub category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub is_system: bool,
    pub position: f64,
    pub archived_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl IssueStatusDto {
    fn from_row(row: &IssueStatusRow) -> Self {
        Self {
            id: row.id.to_string(),
            workspace_id: row.workspace_id.to_string(),
            key: row.key.clone(),
            name: row.name.clone(),
            // 本仓 `issue_status` 没有 description / color / archived_at 列（无新迁移可用）
            description: String::new(),
            color: String::new(),
            category: row.category.clone(),
            icon: row.icon.clone(),
            is_system: row.is_builtin(),
            position: row.position,
            archived_at: None,
            created_at: row.created_at.to_rfc3339(),
            updated_at: row.updated_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Serialize)]
struct StatusListResponse {
    statuses: Vec<IssueStatusDto>,
    categories: Vec<&'static str>,
    total: usize,
}

// ---------------------------------------------------------------------------
// 请求体
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CreateIssueRequest {
    title: String,
    description: Option<String>,
    status: Option<String>,
    priority: Option<String>,
    assignee_type: Option<String>,
    assignee_id: Option<String>,
    parent_issue_id: Option<String>,
    project_id: Option<String>,
    stage: Option<i32>,
    start_date: Option<String>,
    due_date: Option<String>,
    metadata: Option<JsonValue>,
    /// `quick_create`（目前只接受这一个来源）
    origin_type: Option<String>,
    origin_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct UpdateIssueRequest {
    expected_revision: Option<i64>,
    title: Option<String>,
    #[serde(deserialize_with = "double_option")]
    description: Option<Option<String>>,
    status: Option<String>,
    #[serde(deserialize_with = "double_option")]
    status_name: Option<Option<String>>,
    priority: Option<String>,
    #[serde(deserialize_with = "double_option")]
    assignee_type: Option<Option<String>>,
    #[serde(deserialize_with = "double_option")]
    assignee_id: Option<Option<String>>,
    #[serde(deserialize_with = "double_option")]
    parent_issue_id: Option<Option<String>>,
    #[serde(deserialize_with = "double_option")]
    project_id: Option<Option<String>>,
    #[serde(deserialize_with = "double_option")]
    stage: Option<Option<i32>>,
    #[serde(deserialize_with = "double_option")]
    start_date: Option<Option<String>>,
    #[serde(deserialize_with = "double_option")]
    due_date: Option<Option<String>>,
    /// 上游字段，本仓忽略（见 docs/11 §5）
    #[serde(deserialize_with = "double_option")]
    triage_state: Option<Option<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct BatchUpdateRequest {
    issue_ids: Vec<String>,
    updates: UpdateIssueRequest,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct BatchDeleteRequest {
    issue_ids: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ChildrenQuery {
    workspace_id: Option<String>,
    workspace_slug: Option<String>,
    /// 父 issue id CSV
    parent_ids: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ReactionRequest {
    #[serde(default)]
    emoji: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ValueRequest {
    value: JsonValue,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CreateIssueStatusRequest {
    name: String,
    key: Option<String>,
    category: String,
    icon: Option<String>,
    position: Option<f64>,
    /// 上游字段，本仓无对应列 → 接受但忽略
    description: Option<String>,
    color: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct UpdateIssueStatusRequest {
    name: Option<String>,
    category: Option<String>,
    #[serde(deserialize_with = "double_option")]
    icon: Option<Option<String>>,
    position: Option<f64>,
    /// 上游字段，本仓无对应列 → 接受但忽略
    description: Option<String>,
    color: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ReorderStatusesRequest {
    category: Option<String>,
    ids: Vec<String>,
    include_system: Option<bool>,
}

// ---------------------------------------------------------------------------
// 集合端点
// ---------------------------------------------------------------------------

/// `GET /api/issues`
async fn list_issues(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ListIssuesQuery>,
    user: AuthUser,
) -> ApiResult<Json<IssueListResponse>> {
    let workspace_id = resolve_workspace(&state, &headers, &query.workspace_selector()).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let catalog = load_catalog(&state, workspace_id).await?;
    let repo = issue_repo(&state);
    let terminal = repo
        .terminal_status_keys(workspace_id)
        .await
        .map_err(repo_err)?;
    let mut filter = build_filter(workspace_id, &query, &terminal)?;
    expand_custom_categories(&mut filter, &query, &catalog)?;

    let (rows, total) = repo.list_with_total(&filter).await.map_err(repo_err)?;
    Ok(Json(IssueListResponse {
        issues: rows
            .iter()
            .map(|row| IssueDto::from_row(row, &catalog))
            .collect(),
        total,
    }))
}

/// `POST /api/issues/query`（body 与 `GET /api/issues` 同 key）
async fn query_issues(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    user: AuthUser,
    Json(body): Json<HashMap<String, JsonValue>>,
) -> ApiResult<Json<IssueListResponse>> {
    let query = ListIssuesQuery::from_pairs(&body)?;
    list_issues(State(state), headers, Query(query), user).await
}

/// `GET /api/issues/search`（`q` 必填，上限 50 条）
async fn search_issues(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ListIssuesQuery>,
    user: AuthUser,
) -> ApiResult<Json<SearchResponse>> {
    let needle = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .ok_or_else(|| validation("q parameter is required"))?
        .to_string();

    let workspace_id = resolve_workspace(&state, &headers, &query.workspace_selector()).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let catalog = load_catalog(&state, workspace_id).await?;
    let repo = issue_repo(&state);
    let terminal = repo
        .terminal_status_keys(workspace_id)
        .await
        .map_err(repo_err)?;
    let mut filter = build_filter(workspace_id, &query, &terminal)?;
    expand_custom_categories(&mut filter, &query, &catalog)?;
    filter.limit = Some(
        query
            .limit
            .unwrap_or(SEARCH_DEFAULT_LIMIT)
            .clamp(1, SEARCH_MAX_LIMIT),
    );

    let rows = repo.list(&filter).await.map_err(repo_err)?;
    Ok(Json(SearchResponse {
        issues: rows
            .iter()
            .map(|row| SearchHitDto {
                issue: IssueDto::from_row(row, &catalog),
                match_source: match_source(row, &needle),
            })
            .collect(),
    }))
}

fn match_source(row: &IssueRow, needle: &str) -> String {
    let needle = needle.to_lowercase();
    if row.title.to_lowercase().contains(&needle) {
        "title".to_string()
    } else if row.identifier.to_lowercase().contains(&needle) {
        "identifier".to_string()
    } else {
        "description".to_string()
    }
}

/// `GET /api/issues/grouped`（默认 `group_by=assignee`）
#[allow(clippy::cast_precision_loss)]
async fn list_grouped(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ListIssuesQuery>,
    user: AuthUser,
) -> ApiResult<Json<GroupedResponse>> {
    let raw_group = query.group_by.clone().unwrap_or_default();
    let raw_group = raw_group.trim().to_string();
    let raw_group = if raw_group.is_empty() {
        "assignee".to_string()
    } else {
        raw_group
    };
    let field = IssueGroupField::parse(&raw_group)
        .ok_or_else(|| validation(format!("unsupported group_by: {raw_group}")))?;

    let workspace_id = resolve_workspace(&state, &headers, &query.workspace_selector()).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let catalog = load_catalog(&state, workspace_id).await?;
    let repo = issue_repo(&state);
    let terminal = repo
        .terminal_status_keys(workspace_id)
        .await
        .map_err(repo_err)?;
    let mut filter = build_filter(workspace_id, &query, &terminal)?;
    expand_custom_categories(&mut filter, &query, &catalog)?;

    let rows = repo.list(&filter).await.map_err(repo_err)?;
    let counts = repo
        .grouped_counts(&filter, field)
        .await
        .map_err(repo_err)?;

    let mut groups = Vec::with_capacity(counts.len());
    for count in counts {
        let key = count.key.clone();
        let matching: Vec<&IssueRow> = rows
            .iter()
            .filter(|row| group_key(row, field) == key)
            .collect();
        let (assignee_type, assignee_id) = if field == IssueGroupField::Assignee {
            (
                matching.first().and_then(|r| r.assignee_type.clone()),
                key.clone(),
            )
        } else {
            (None, None)
        };
        groups.push(GroupedGroupDto {
            id: group_id(field, key.as_deref()),
            key,
            assignee_type,
            assignee_id,
            total: count.total,
            done: count.done,
            issues: matching
                .into_iter()
                .map(|row| IssueDto::from_row(row, &catalog))
                .collect(),
        });
    }

    Ok(Json(GroupedResponse {
        group_by: IssueRepo::group_field_name(field).to_string(),
        groups,
    }))
}

fn group_key(row: &IssueRow, field: IssueGroupField) -> Option<String> {
    match field {
        IssueGroupField::Status => Some(row.status.clone()),
        IssueGroupField::Priority => Some(row.priority.clone()),
        IssueGroupField::Assignee => row.assignee_id.clone(),
        IssueGroupField::Project => row.project_id.map(|id| id.to_string()),
    }
}

fn group_id(field: IssueGroupField, key: Option<&str>) -> String {
    match (field, key) {
        (IssueGroupField::Assignee, Some(key)) => format!("assignee:{key}"),
        (IssueGroupField::Assignee, None) => "assignee:unassigned".to_string(),
        (field, Some(key)) => format!("{}:{key}", IssueRepo::group_field_name(field)),
        (field, None) => format!("{}:unset", IssueRepo::group_field_name(field)),
    }
}

/// `GET /api/issues/children?parent_ids=...`（批量取子 issue）
async fn list_children_by_parents(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ChildrenQuery>,
    user: AuthUser,
) -> ApiResult<Json<IssueChildrenResponse>> {
    let selector = WorkspaceQuery {
        workspace_id: query.workspace_id.clone(),
        workspace_slug: query.workspace_slug.clone(),
    };
    let workspace_id = resolve_workspace(&state, &headers, &selector).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let raw = split_comma_param(query.parent_ids.as_deref().unwrap_or("")).unwrap_or_default();
    if raw.len() > CHILDREN_PARENTS_MAX {
        return Err(validation(format!(
            "parent_ids accepts at most {CHILDREN_PARENTS_MAX} ids"
        ))
        .into());
    }
    let mut parents = Vec::with_capacity(raw.len());
    for id in raw {
        parents.push(parse_target_id("parent_ids", &id)?.0);
    }

    let catalog = load_catalog(&state, workspace_id).await?;
    let rows = issue_repo(&state)
        .children_of_parents(workspace_id, &parents)
        .await
        .map_err(repo_err)?;
    Ok(Json(IssueChildrenResponse {
        issues: rows
            .iter()
            .map(|row| IssueDto::from_row(row, &catalog))
            .collect(),
    }))
}

/// `GET /api/issues/:id/children`
async fn list_issue_children(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Json<IssueChildrenResponse>> {
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let repo = issue_repo(&state);
    let parent = load_issue(&repo, workspace_id, &raw_id).await?;
    let catalog = load_catalog(&state, workspace_id).await?;
    let rows = repo
        .children_of(workspace_id, parent.id())
        .await
        .map_err(repo_err)?;
    Ok(Json(IssueChildrenResponse {
        issues: rows
            .iter()
            .map(|row| IssueDto::from_row(row, &catalog))
            .collect(),
    }))
}

/// `GET /api/issues/child-progress`
#[allow(clippy::cast_precision_loss)]
async fn child_progress(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Json<ChildProgressResponse>> {
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let repo = issue_repo(&state);
    let terminal = repo
        .terminal_status_keys(workspace_id)
        .await
        .map_err(repo_err)?;
    let rows = repo
        .child_progress(workspace_id, &terminal)
        .await
        .map_err(repo_err)?;
    let progress = rows
        .into_iter()
        .map(|row| {
            let percent = if row.total > 0 {
                row.done as f64 / row.total as f64 * 100.0
            } else {
                0.0
            };
            ChildProgressDto {
                parent_issue_id: row.parent_issue_id.to_string(),
                total: row.total,
                done: row.done,
                percent,
            }
        })
        .collect();
    Ok(Json(ChildProgressResponse { progress }))
}

// ---------------------------------------------------------------------------
// 单体端点
// ---------------------------------------------------------------------------

/// `POST /api/issues`
#[allow(clippy::too_many_lines)]
async fn create_issue(
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
async fn quick_create_issue(
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
async fn get_issue(
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
async fn update_issue(
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
    let patch = apply_update_request(&repo, workspace_id, &current, &catalog, req).await?;

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
async fn apply_update_request(
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
    // 配对校验：补丁后的最终状态必须「同时有值」或「同时为空」
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
        patch.stage = match raw_stage {
            Some(stage) if stage < 1 => return Err(validation("stage must be >= 1")),
            Some(stage) => Some(Some(stage)),
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
async fn delete_issue(
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
async fn move_issue(
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
    let patch = apply_update_request(&repo, workspace_id, &current, &catalog, req).await?;

    let row = repo
        .move_issue_with_update(workspace_id, current.id(), before_id, after_id, &patch)
        .await
        .map_err(repo_err)?;
    Ok(Json(IssueDto::from_row(&row, &catalog)))
}

fn parse_anchor(raw: Option<&JsonValue>) -> Result<Option<Id>, Error> {
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
async fn batch_update(
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
        patch = apply_update_request(&repo, workspace_id, &first, &catalog, req.updates).await?;
    }

    let updated = repo
        .batch_update(workspace_id, &ids, &patch)
        .await
        .map_err(repo_err)?;
    Ok(Json(UpdatedResponse { updated }))
}

/// `POST /api/issues/batch-delete`
async fn batch_delete(
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

// ---------------------------------------------------------------------------
// reactions / metadata / properties
fn reaction_dto(row: &IssueReactionRow) -> IssueReactionDto {
    IssueReactionDto {
        id: row.id.to_string(),
        issue_id: row.issue_id.to_string(),
        actor_type: row.actor_type.clone(),
        actor_id: row.actor_id.clone(),
        emoji: row.emoji.clone(),
        created_at: row.created_at.to_rfc3339(),
    }
}

/// `GET /api/issues/:id/reactions`
async fn list_reactions(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Json<Vec<IssueReactionDto>>> {
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    let repo = issue_repo(&state);
    let issue = load_issue(&repo, workspace_id, &raw_id).await?;
    let rows = repo.list_reactions(issue.id()).await.map_err(repo_err)?;
    Ok(Json(rows.iter().map(reaction_dto).collect()))
}

/// `POST /api/issues/:id/reactions`
async fn add_reaction(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    Json(req): Json<ReactionRequest>,
) -> ApiResult<Response> {
    let emoji = req.emoji.trim().to_string();
    if emoji.is_empty() {
        return Err(validation("emoji is required").into());
    }
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let repo = issue_repo(&state);
    let issue = load_issue(&repo, workspace_id, &raw_id).await?;
    // 0004 的 CHECK 只允许 user/agent/system（上游写 `member`）；X-Agent-ID 归 M3
    let row = repo
        .add_reaction(
            workspace_id,
            issue.id(),
            "user",
            &user.id().to_string(),
            &emoji,
        )
        .await
        .map_err(repo_err)?;
    Ok((StatusCode::CREATED, Json(reaction_dto(&row))).into_response())
}

/// `DELETE /api/issues/:id/reactions`（body `{"emoji": "👍"}`）
async fn remove_reaction(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    Json(req): Json<ReactionRequest>,
) -> ApiResult<StatusCode> {
    let emoji = req.emoji.trim().to_string();
    if emoji.is_empty() {
        return Err(validation("emoji is required").into());
    }
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let repo = issue_repo(&state);
    let issue = load_issue(&repo, workspace_id, &raw_id).await?;
    repo.remove_reaction(issue.id(), "user", &user.id().to_string(), &emoji)
        .await
        .map_err(repo_err)?;
    Ok(StatusCode::NO_CONTENT)
}

fn validate_metadata_key(key: &str) -> Result<(), Error> {
    let trimmed = key.trim();
    if trimmed.is_empty() || trimmed.len() > METADATA_KEY_MAX_LEN {
        return Err(validation(format!(
            "metadata key must be 1..={METADATA_KEY_MAX_LEN} characters"
        )));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(validation(
            "metadata key may only contain letters, digits, '_', '-' and '.'",
        ));
    }
    Ok(())
}

fn validate_metadata_value(value: &JsonValue) -> Result<(), Error> {
    if value.is_array() || value.is_object() {
        return Err(validation(
            "metadata values must be primitives (string, number, boolean, null)",
        ));
    }
    Ok(())
}

/// `GET /api/issues/:id/metadata`
async fn get_metadata(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Json<MetadataResponse>> {
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;
    let repo = issue_repo(&state);
    let issue = load_issue(&repo, workspace_id, &raw_id).await?;
    let metadata = repo
        .get_metadata(workspace_id, issue.id())
        .await
        .map_err(repo_err)?;
    Ok(Json(MetadataResponse {
        metadata,
        issue_revision: None,
    }))
}

/// `PUT /api/issues/:id/metadata/:key`
async fn set_metadata_key(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((raw_id, key)): Path<(String, String)>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    Json(req): Json<ValueRequest>,
) -> ApiResult<Json<MetadataResponse>> {
    validate_metadata_key(&key)?;
    validate_metadata_value(&req.value)?;
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let repo = issue_repo(&state);
    let current = load_issue(&repo, workspace_id, &raw_id).await?;
    let existing = current.metadata.as_object().map_or(0, serde_json::Map::len);
    let known = current
        .metadata
        .as_object()
        .is_some_and(|map| map.contains_key(&key));
    if !known && existing >= METADATA_KEYS_MAX {
        return Err(validation(format!("metadata cannot exceed {METADATA_KEYS_MAX} keys")).into());
    }

    let metadata = repo
        .set_metadata_key(workspace_id, current.id(), &key, &req.value)
        .await
        .map_err(repo_err)?;
    let row = repo
        .get(workspace_id, current.id())
        .await
        .map_err(repo_err)?;
    Ok(Json(MetadataResponse {
        metadata,
        issue_revision: Some(row.revision),
    }))
}

/// `DELETE /api/issues/:id/metadata/:key`
async fn delete_metadata_key(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((raw_id, key)): Path<(String, String)>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Json<MetadataResponse>> {
    validate_metadata_key(&key)?;
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let repo = issue_repo(&state);
    let current = load_issue(&repo, workspace_id, &raw_id).await?;
    let metadata = repo
        .delete_metadata_key(workspace_id, current.id(), &key)
        .await
        .map_err(repo_err)?;
    Ok(Json(MetadataResponse {
        metadata,
        issue_revision: None,
    }))
}

/// `PUT /api/issues/:id/properties/:propertyId`
///
/// 本仓把 property 值存在 `issue.properties` JSONB 里（以 `:propertyId` 作为 key），
/// 没有 `property` 定义表，因此不做「定义存在 / 类型匹配 / 归档」校验（docs/11 §5）。
async fn set_property(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((raw_id, property_id)): Path<(String, String)>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    Json(req): Json<ValueRequest>,
) -> ApiResult<Json<PropertiesResponse>> {
    if property_id.trim().is_empty() {
        return Err(validation("property id is required").into());
    }
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let repo = issue_repo(&state);
    let current = load_issue(&repo, workspace_id, &raw_id).await?;
    let properties = repo
        .set_property(workspace_id, current.id(), &property_id, &req.value)
        .await
        .map_err(repo_err)?;
    let row = repo
        .get(workspace_id, current.id())
        .await
        .map_err(repo_err)?;
    Ok(Json(PropertiesResponse {
        properties,
        issue_revision: Some(row.revision),
    }))
}

/// `DELETE /api/issues/:id/properties/:propertyId`
async fn delete_property(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((raw_id, property_id)): Path<(String, String)>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Json<PropertiesResponse>> {
    if property_id.trim().is_empty() {
        return Err(validation("property id is required").into());
    }
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let repo = issue_repo(&state);
    let current = load_issue(&repo, workspace_id, &raw_id).await?;
    let properties = repo
        .delete_property(workspace_id, current.id(), &property_id)
        .await
        .map_err(repo_err)?;
    Ok(Json(PropertiesResponse {
        properties,
        issue_revision: None,
    }))
}

// ---------------------------------------------------------------------------
// status 目录端点
// ---------------------------------------------------------------------------

fn status_list(rows: &[IssueStatusRow]) -> StatusListResponse {
    let statuses: Vec<IssueStatusDto> = rows.iter().map(IssueStatusDto::from_row).collect();
    StatusListResponse {
        total: statuses.len(),
        statuses,
        categories: vec!["open", "closed"],
    }
}

/// `GET /api/issue-statuses`（读路径 self-heal：先 `ensure_defaults`，与上游一致）
async fn list_statuses(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<Json<StatusListResponse>> {
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_member(&state, workspace_id, user.id()).await?;

    let repo = status_repo(&state);
    repo.ensure_defaults(workspace_id)
        .await
        .map_err(status_repo_err)?;
    let rows = repo.list(workspace_id).await.map_err(status_repo_err)?;
    Ok(Json(status_list(&rows)))
}

/// `POST /api/issue-statuses`
async fn create_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    Json(req): Json<CreateIssueStatusRequest>,
) -> ApiResult<Response> {
    let name = req.name.trim().to_string();
    if name.is_empty() || name.len() > KEY_MAX_LEN {
        return Err(validation(format!("name must be 1..={KEY_MAX_LEN} characters")).into());
    }
    let category = parse_category(req.category.trim())
        .ok_or_else(|| validation("category must be open or closed"))?;
    let key = match req.key.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
        Some(raw) => Some(
            validate_key(raw).ok_or_else(|| validation(format!("invalid status key: {raw}")))?,
        ),
        None => None,
    };

    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    // 上游（`issue_status.go` / router.go L2051 注释）：写路径限 owner/admin
    require_workspace_admin(&state, workspace_id, user.id()).await?;

    // `description` / `color` 本仓无处可存（无新迁移），接受但忽略
    let input = NewIssueStatus {
        name,
        key,
        category,
        icon: req.icon,
        position: req.position,
    };
    let row = status_repo(&state)
        .create(workspace_id, &input)
        .await
        .map_err(status_repo_err)?;
    Ok((StatusCode::CREATED, Json(IssueStatusDto::from_row(&row))).into_response())
}

/// `PATCH /api/issue-statuses/:id`
async fn update_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    Json(req): Json<UpdateIssueStatusRequest>,
) -> ApiResult<Json<IssueStatusDto>> {
    let status_id = parse_target_id("status id", &raw_id)?;
    let name = req
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_string);
    if req.name.is_some() && name.is_none() {
        return Err(validation("name must not be empty").into());
    }
    let category = match req
        .category
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        Some(raw) => {
            Some(parse_category(raw).ok_or_else(|| validation("category must be open or closed"))?)
        }
        None => None,
    };

    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_admin(&state, workspace_id, user.id()).await?;

    let patch = IssueStatusUpdate {
        name,
        category,
        icon: req.icon,
        position: req.position,
    };
    let row = status_repo(&state)
        .update(workspace_id, status_id, &patch)
        .await
        .map_err(status_repo_err)?;
    Ok(Json(IssueStatusDto::from_row(&row)))
}

/// `DELETE /api/issue-statuses/:id`
///
/// 本仓没有 `archived_at` 列 → 硬删（上游是归档）。内置 key 与仍在使用的 key 都会被
/// 仓储层拒绝（409，docs/11 §5）。
async fn delete_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> ApiResult<StatusCode> {
    let status_id = parse_target_id("status id", &raw_id)?;
    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_admin(&state, workspace_id, user.id()).await?;
    status_repo(&state)
        .delete(workspace_id, status_id)
        .await
        .map_err(status_repo_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `PATCH /api/issue-statuses/reorder`
#[allow(clippy::cast_precision_loss)]
async fn reorder_statuses(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    Json(req): Json<ReorderStatusesRequest>,
) -> ApiResult<Json<StatusListResponse>> {
    if req.ids.is_empty() {
        return Err(validation("ids must not be empty").into());
    }
    // `category` / `include_system` 接受但忽略：本仓 position 全局唯一，不分分类
    let _ = (&req.category, req.include_system);

    let workspace_id = resolve_workspace(&state, &headers, &query).await?;
    require_workspace_admin(&state, workspace_id, user.id()).await?;

    let mut order = Vec::with_capacity(req.ids.len());
    for (index, raw) in req.ids.iter().enumerate() {
        order.push((parse_target_id("ids", raw)?, index as f64));
    }
    let repo = status_repo(&state);
    repo.reorder(workspace_id, &order)
        .await
        .map_err(status_repo_err)?;
    let rows = repo.list(workspace_id).await.map_err(status_repo_err)?;
    Ok(Json(status_list(&rows)))
}

// ---------------------------------------------------------------------------
// 501 占位
// ---------------------------------------------------------------------------

/// 上游存在但本仓尚未实现的端点：统一 501，body 与 `ApiError` 同形。
///
/// axum 的 handler 必须是 `async fn`；这里没有真的 await，因此显式 `allow(unused_async)`。
#[allow(clippy::unused_async)]
async fn not_implemented(OriginalUri(uri): OriginalUri) -> Response {
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
