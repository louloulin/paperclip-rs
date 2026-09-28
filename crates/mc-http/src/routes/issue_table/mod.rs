//! `/api/issues/table/*` + `/api/issues/limit-usage`（M2-D / LUM-1355）。
//!
//! 对应上游 multica：
//! - `server/internal/handler/issue_table_query.go`（`decodeIssueTableJSON` /
//!   `normalizeIssueTablePage` / `issueTableCursorMatches` / `canonicalIssueTableFingerprint`）
//! - `server/internal/handler/issue_table_group.go:892` `ListIssueTableGroups`
//! - `server/internal/handler/issue_table_rows.go:253` `ListIssueTableRows`
//! - `server/internal/handler/issue_table_facets.go` `ListIssueTableFacets`
//! - `server/internal/handler/issue_limit.go:17` `GetIssueLimitUsage`
//! - `server/internal/handler/router.go:1956-1959`（四个路由的注册点）
//!
//! 职责边界：**SQL 全在 `mc_repos::issue_table`**（已校验的规格 → 行）。本文件只做
//! HTTP 概念：JSON 解码与 400/409/422 判定、DTO 字段名、cursor 编解码、`query_fingerprint`。
//!
//! 与上游的**有意偏离**（逐条论证见 `docs/14-M2-TABLE.md` §4/§5）：
//! 1. cursor 用 **hex 编码的 JSON**（上游 `base64.RawURLEncoding`）——沿用 M2-C
//!    `routes/inbox.rs` 的先例，避免给 mc-http 增依赖；cursor 对客户端不透明。
//! 2. `group.kind` 在 `label` / `property` / `parent` / `status_category` / `compound`
//!    与 `facets[].kind` 在 `label` / `working_agents` / `property` 时返回 **422**
//!    `{"error":"unsupported_group","code":…,"message":…}`（上游形状），绝不编造计数。
//! 3. `filters` 里本仓无法诚实实现的维度（`project_statuses` 非空、`label_ids` 非空、
//!    `properties` 非空、`working_only=true`、`working_issue_ids` 非 null）返回 422
//!    `{"error":"unsupported_filter",…}`；**空数组/空对象等价于“未提供”**（上游也不加谓词），
//!    因此常规客户端全字段请求不会被误拒。
//! 4. `scope.kind=my` 的 actor 取**当前登录用户**（上游从 session 取），请求里带的
//!    `actor` 被忽略；本仓 `scope.relation=involved` 只覆盖 agent 归属 + squad leader。
//! 5. `group.kind=priority` 是本仓新增维度（上游无），响应里多一个 `value.priority` 字段。
//! 6. `GET /api/issues/limit-usage` 的**策略源**在 M9-9 之后是已安装的 entitlement 平面
//!    （`mc_entitlement::client::provider()` 的 `issue_count` gate，判据与上游
//!    `ResolveIssueCountPolicy` 逐字相同）：未装平面 / 未强制限额 ⇒ 恒 **204**；
//!    `enforce` 且 `limit > 0` ⇒ `200 {"used":…,"limit":…}`。
//!    ⚠️ 计数只**采样**到 `limit`（上游 `CountIssueUsage` 同款），本仓复用
//!    `IssueRepo::count_in_workspace`（其文档逐字点名本路由）。
//! 7. 400 的错误体沿用本仓 `{"error":{"code","message"}}`；409/422 沿用上游的扁平
//!    形状（`{"error":"cursor_query_mismatch"…}` / `{"error":"unsupported_group"…}`）。
//!
//! 未接进 `scripts/gates.sh` 的 DB 回放测试见 `crates/mc-http/tests/issues.rs`
//! （`#[ignore]` + `MULTICA_TEST_DATABASE_URL`）。
//! 文件布局（R7：单文件 800 行硬上限，`scripts/file_size_check.py` + 门 ⑩ 执行）：
//! - `mod.rs`（本文件）：模块文档 + `pub fn router()` + 4 个 handler + `authorize` /
//!   `table_repo` + 响应 DTO + `TableError`（错误 → HTTP 形状）
//! - `spec.rs`（`mod spec`）：请求 DTO、`decode_body`、`build_*` 校验与规格构造、`query_fingerprint`、
//!   `group_identity` / `group_value` / `parse_group_key`
//! - `cursor.rs`（`mod cursor`）：`CursorWire` 与 `decode_cursor`
//! - `tests.rs`：现有单元测试（`#[cfg(test)] mod tests;`）
//!
//! 明细见 `docs/14-M2-TABLE.md` §4（有意偏离）/ §5（支持面）/ §6（文件布局）。
//! 子模块条目一律 `pub(crate)`：`issue_table` 对外仍然只暴露 `router()`。

mod cursor;
mod spec;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{json, Value as JsonValue};

use mc_core::Id;
use mc_entitlement::GateName;
use mc_errors::Error;
use mc_repos::issue_table::{
    IssueTableRepo, TableFacetsQuery, TableFilter, TableGroupKey, TableGroupKind, TableGroupSpec,
    TableGroupsQuery, TableOrder, TableRowsQuery,
};
use mc_repos::RepoError;

use crate::routes::auth_user::AuthUser;
use crate::routes::invitations::require_workspace_member;
use crate::routes::issues::{
    issue_repo, load_catalog, repo_err, resolve_workspace, validation, IssueDto, WorkspaceQuery,
};
use crate::state::AppState;

use self::cursor::{decode_cursor, CursorWire};
use self::spec::{
    build_facets, build_filters, build_group_spec, build_order, build_scope, build_search,
    decode_body, group_identity, group_value, normalize_page, parse_group_key, parse_project_id,
    query_fingerprint, FacetsRequest, GroupsRequest, RowsRequest,
};

/// `/api/issues/table/*` + `/api/issues/limit-usage` 路由。
///
/// 由 `routes/issues.rs` 的 `router()` 通过 `.merge()` 挂载（`mount.rs` 不动）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/issues/table/groups", post(table_groups))
        .route("/api/issues/table/rows", post(table_rows))
        .route("/api/issues/table/facets", post(table_facets))
        // 静态段优先于 `issues.rs` 的 `/api/issues/:id`（matchit 的优先级规则），
        // 否则 "limit-usage" 会被当成 identifier 走进单体路由。
        .route("/api/issues/limit-usage", get(limit_usage))
}

// ---------------------------------------------------------------------------
// 错误：400 用本仓 Error，409 / 422 用上游的扁平形状
// ---------------------------------------------------------------------------

/// 409 / 422 需要上游自己的错误体形状，`mc_errors::Error` 覆盖不到（见模块注释 7）。
#[derive(Debug)]
enum TableError {
    Api(Error),
    /// 409 `cursor_query_mismatch`。
    CursorMismatch,
    /// 422 `unsupported_group`（分组维度 / facet 维度不支持）。
    Unsupported {
        code: String,
        message: String,
    },
    /// 422 `unsupported_filter`（过滤维度不支持）。
    UnsupportedFilter {
        code: String,
        message: String,
    },
}

impl From<Error> for TableError {
    fn from(value: Error) -> Self {
        TableError::Api(value)
    }
}

impl From<RepoError> for TableError {
    fn from(value: RepoError) -> Self {
        TableError::Api(repo_err(value))
    }
}

impl IntoResponse for TableError {
    fn into_response(self) -> Response {
        match self {
            TableError::Api(err) => crate::error::ApiError(err).into_response(),
            TableError::CursorMismatch => (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": "cursor_query_mismatch",
                    "message": "cursor does not belong to this table query",
                })),
            )
                .into_response(),
            TableError::Unsupported { code, message } => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "error": "unsupported_group",
                    "code": code,
                    "message": message,
                })),
            )
                .into_response(),
            TableError::UnsupportedFilter { code, message } => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "error": "unsupported_filter",
                    "code": code,
                    "message": message,
                })),
            )
                .into_response(),
        }
    }
}

// ---------------------------------------------------------------------------
// 响应 DTO（字段名 / 空值行为与上游一致）
// ---------------------------------------------------------------------------

/// `/groups` 单个分组。
#[derive(Debug, Serialize)]
struct GroupDescriptor {
    key: String,
    value: JsonValue,
    count: i64,
    /// `compound` 分组才有值；本仓不支持（模块注释 2），因此永远是空数组（上游 omitempty）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    secondary_groups: Vec<GroupDescriptor>,
}

/// `/groups` 响应。
#[derive(Debug, Serialize)]
struct GroupsResponse {
    query_fingerprint: String,
    total: i64,
    groups: Vec<GroupDescriptor>,
    next_cursor: Option<String>,
}

/// `/rows` 一行。
#[derive(Debug, Serialize)]
struct RowResponse {
    issue: IssueDto,
    direct_child_count: i64,
}

/// `/rows` 响应。
#[derive(Debug, Serialize)]
struct RowsResponse {
    query_fingerprint: String,
    group_key: Option<String>,
    parent_id: Option<String>,
    total: i64,
    rows: Vec<RowResponse>,
    /// 上游注释：当前页大小，为响应兼容保留。
    branch_total: i64,
    next_cursor: Option<String>,
}

/// `/facets` 一个取值。
#[derive(Debug, Serialize)]
struct FacetValueResponse {
    key: String,
    count: i64,
}

/// `/facets` 一个 facet。
#[derive(Debug, Serialize)]
struct FacetResponse {
    kind: &'static str,
    values: Vec<FacetValueResponse>,
}

/// `/facets` 响应。
#[derive(Debug, Serialize)]
struct FacetsResponse {
    query_fingerprint: String,
    total: i64,
    facets: Vec<FacetResponse>,
}

/// 解析 workspace + 成员校验（三个端点共用）。
async fn authorize(
    state: &AppState,
    headers: &HeaderMap,
    query: &WorkspaceQuery,
    user: &AuthUser,
) -> Result<Id, TableError> {
    let workspace_id = resolve_workspace(state, headers, query).await?;
    require_workspace_member(state, workspace_id, user.id())
        .await
        .map_err(TableError::Api)?;
    Ok(workspace_id)
}

fn table_repo(state: &AppState) -> IssueTableRepo {
    IssueTableRepo::new(state.db.clone())
}

// ---------------------------------------------------------------------------
// POST /api/issues/table/groups
// ---------------------------------------------------------------------------

async fn table_groups(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(selector): Query<WorkspaceQuery>,
    user: AuthUser,
    body: Bytes,
) -> Result<Json<GroupsResponse>, TableError> {
    let request: GroupsRequest = decode_body(&body)?;
    let workspace_id = authorize(&state, &headers, &selector, &user).await?;
    let limit = normalize_page(&request.page)?;
    let search = build_search(request.query.search.as_deref());
    let scope = build_scope(&request.query.scope, user.id())?;
    let (filter, explicit_empty_assignees) = build_filters(&request.query.filters, scope, &search)?;
    let order = build_order(&request.query.sort)?;
    let group = build_group_spec(&request.group, false)?;
    let fingerprint = query_fingerprint(workspace_id, &filter, order, explicit_empty_assignees);

    let cursor = decode_cursor(request.page.cursor.as_deref())?;
    if let Some(wire) = &cursor {
        let identity = group_identity(group);
        wire.matches(&fingerprint, Some(&identity), None)?;
    }
    let cursor = cursor.map(CursorWire::into_group_cursor).transpose()?;

    let page = table_repo(&state)
        .table_groups(&TableGroupsQuery {
            workspace_id,
            filter,
            group,
            limit,
            cursor,
        })
        .await?;
    let next_cursor = page.next.map(|next| {
        let mut wire = CursorWire::new(&fingerprint);
        wire.group_key = Some(group_identity(group));
        wire.group_order = Some(next.order);
        wire.group_sort_key = Some(next.sort_key);
        wire.group_cursor_key = Some(next.value);
        wire.encode()
    });
    Ok(Json(GroupsResponse {
        query_fingerprint: fingerprint,
        total: page.total,
        groups: page
            .groups
            .into_iter()
            .map(|group_count| GroupDescriptor {
                key: group_count.key.clone(),
                value: group_value(&group_count.key, group.kind),
                count: group_count.count,
                secondary_groups: Vec::new(),
            })
            .collect(),
        next_cursor,
    }))
}

// ---------------------------------------------------------------------------
// POST /api/issues/table/rows
// ---------------------------------------------------------------------------

/// `/rows` 的三个已校验输入（避免 handler 里堆参数）。
struct RowsInput {
    filter: TableFilter,
    group: TableGroupSpec,
    group_key: TableGroupKey,
    order: TableOrder,
    limit: i64,
    fingerprint: String,
    cursor_raw: Option<CursorWire>,
    parent_id: Option<Id>,
}

fn build_rows_input(
    workspace_id: Id,
    user_id: Id,
    request: &RowsRequest,
) -> Result<RowsInput, TableError> {
    let limit = normalize_page(&request.page)?;
    let search = build_search(request.query.search.as_deref());
    let scope = build_scope(&request.query.scope, user_id)?;
    let (filter, explicit_empty_assignees) = build_filters(&request.query.filters, scope, &search)?;
    let order = build_order(&request.query.sort)?;
    let group = build_group_spec(&request.group, true)?;
    let group_key = parse_group_key(group.kind, request.group_key.as_deref())?;
    let fingerprint = query_fingerprint(workspace_id, &filter, order, explicit_empty_assignees);
    // `parent_id` 只有 `hierarchy.enabled=true` 才有意义（上游 400）。
    let parent_id = match request.parent_id.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => {
            if !request.hierarchy.enabled {
                return Err(validation("parent_id requires hierarchy.enabled").into());
            }
            Some(parse_project_id("parent_id", raw)?)
        }
    };
    Ok(RowsInput {
        filter,
        group,
        group_key,
        order,
        limit,
        fingerprint,
        cursor_raw: decode_cursor(request.page.cursor.as_deref())?,
        parent_id,
    })
}

async fn table_rows(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(selector): Query<WorkspaceQuery>,
    user: AuthUser,
    body: Bytes,
) -> Result<Json<RowsResponse>, TableError> {
    let request: RowsRequest = decode_body(&body)?;
    let workspace_id = authorize(&state, &headers, &selector, &user).await?;
    let input = build_rows_input(workspace_id, user.id(), &request)?;

    let normalized_group_key = request
        .group_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty() && input.group.kind != TableGroupKind::None)
        .map(str::to_string);
    let normalized_parent_id = input.parent_id.map(|id| id.0.to_string());
    if let Some(wire) = &input.cursor_raw {
        wire.matches(
            &input.fingerprint,
            normalized_group_key.as_deref(),
            normalized_parent_id.as_deref(),
        )?;
    }
    // 只在真的带 cursor 时解析；缺 `row_id` / `row_created_at` → 400（上游同）。
    let cursor = input
        .cursor_raw
        .map(CursorWire::into_row_cursor)
        .transpose()?;

    let catalog = load_catalog(&state, workspace_id)
        .await
        .map_err(TableError::Api)?;
    let page = table_repo(&state)
        .table_rows(&TableRowsQuery {
            workspace_id,
            filter: input.filter,
            group: input.group,
            group_key: input.group_key,
            hierarchy: request.hierarchy.enabled,
            parent_id: input.parent_id,
            order: input.order,
            limit: input.limit,
            cursor,
        })
        .await?;

    let next_cursor = page.next.map(|next| {
        let mut wire = CursorWire::new(&input.fingerprint);
        wire.group_key.clone_from(&normalized_group_key);
        wire.parent_id.clone_from(&normalized_parent_id);
        wire.sort_value = next.sort_value;
        wire.sort_is_null = next.sort_is_null;
        wire.row_created_at = next
            .row_created_at
            .to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
        wire.row_id = next.row_id.0.to_string();
        wire.encode()
    });
    let rows: Vec<RowResponse> = page
        .rows
        .iter()
        .map(|row| RowResponse {
            issue: IssueDto::from_row(&row.issue, &catalog),
            direct_child_count: row.direct_child_count,
        })
        .collect();
    let branch_total = i64::try_from(rows.len()).unwrap_or(i64::MAX);
    Ok(Json(RowsResponse {
        query_fingerprint: input.fingerprint,
        group_key: normalized_group_key,
        parent_id: normalized_parent_id,
        total: page.total.unwrap_or(0),
        rows,
        branch_total,
        next_cursor,
    }))
}

// ---------------------------------------------------------------------------
// POST /api/issues/table/facets
// ---------------------------------------------------------------------------

async fn table_facets(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(selector): Query<WorkspaceQuery>,
    user: AuthUser,
    body: Bytes,
) -> Result<Json<FacetsResponse>, TableError> {
    let request: FacetsRequest = decode_body(&body)?;
    let workspace_id = authorize(&state, &headers, &selector, &user).await?;
    let search = build_search(request.query.search.as_deref());
    let scope = build_scope(&request.query.scope, user.id())?;
    let (filter, explicit_empty_assignees) = build_filters(&request.query.filters, scope, &search)?;
    let order = build_order(&request.query.sort)?;
    let facets = build_facets(&request.facets)?;
    let fingerprint = query_fingerprint(workspace_id, &filter, order, explicit_empty_assignees);

    let page = table_repo(&state)
        .table_facets(&TableFacetsQuery {
            workspace_id,
            filter,
            facets: facets.clone(),
            include_total: request.include_total.unwrap_or(false),
        })
        .await?;
    let facets = page
        .facets
        .into_iter()
        .zip(facets)
        .map(|(count, kind)| FacetResponse {
            kind: kind.key(),
            values: count
                .values
                .into_iter()
                .map(|value| FacetValueResponse {
                    key: value.key,
                    count: value.count,
                })
                .collect(),
        })
        .collect();
    Ok(Json(FacetsResponse {
        query_fingerprint: fingerprint,
        total: page.total,
        facets,
    }))
}

// ---------------------------------------------------------------------------
// GET /api/issues/limit-usage
// ---------------------------------------------------------------------------

/// `GET /api/issues/limit-usage`。
///
/// 上游 `handler/issue_limit.go`：`policy.Action != ActionEnforce` ⇒ **204**；
/// 否则报 `{"used":…,"limit":…}`。本仓的策略来自**已安装的 entitlement 平面**
/// （`mc_entitlement::client::provider()`）—— 与 `GET /api/autopilots/usage` 读的是
/// **同一个** `Arc`、同一份缓存（组合根 `apps/mc-server/src/entitlement.rs` 一次装两个槽）
/// ⇒ 同一格不可能出现两个不同结论（`docs/62` §9.8 判据 ③）。
///
/// 🔴 **不**用 `mc_autopilot::quota::policy_for`：那条路读的是 `autopilot_runs` 那个 gate，
/// 与本路由的 `issue_count` 是**两个不同的 enforcement point**。上游的
/// `ResolveIssueCountPolicy` 判的也是 `GateIssueCount`。
async fn limit_usage(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(selector): Query<WorkspaceQuery>,
    user: AuthUser,
) -> Result<Response, TableError> {
    let workspace_id = authorize(&state, &headers, &selector, &user).await?;
    let decision = mc_entitlement::client::provider().gate(workspace_id, GateName::IssueCount);
    // 逐字上游 `ResolveIssueCountPolicy`：`off` / `observe` / 缺 limit / `limit == 0`
    // 全部折成「没有强制限额」⇒ 204。**绝不用缓存或刷新的 reason 去推断「无限」**。
    let Some(limit) = enforced_issue_limit(&decision) else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    let counted = issue_repo(&state)
        .count_in_workspace(workspace_id)
        .await
        .map_err(repo_err)?
        // 逐字上游 `CountIssueUsage`：采样到 `limit + 1` 为止，够判「已达上限」即可。
        .min(limit);
    Ok(Json(json!({ "used": counted, "limit": limit })).into_response())
}

/// `Decision` → issue-count 面真正会拦的上限（`Some` ⇒ 有强制限额）。
///
/// 逐字 `service.ResolveIssueCountPolicy` 的后半段 + 替身禁令：`off` ⇒ `None`；
/// `enforce` 且 `limit > 0` ⇒ `Some(limit)`；其余（`observe`、缺 limit、`limit == 0`）
/// 一律 `None`。**替身判决（`Reason::Stub`）永远不得驱动生产路由** —— 那会让
/// `entitlementtest.Stub` 的返回值变成生产配额。
fn enforced_issue_limit(decision: &mc_entitlement::Decision) -> Option<i64> {
    if decision.reason == mc_entitlement::Reason::Stub
        || decision.gate.action != mc_entitlement::Action::Enforce
    {
        return None;
    }
    decision.gate.limit.filter(|limit| *limit > 0)
}

#[cfg(test)]
mod limit_usage_tests {
    use super::*;
    use mc_core::Timestamp;
    use mc_entitlement::{off_decision, Action, Decision, Gate, Reason};

    fn enforce(limit: Option<i64>, reason: Reason) -> Decision {
        Decision {
            gate: Gate {
                action: Action::Enforce,
                limit,
                ..Gate::off()
            },
            reason,
            policy_revision: 1,
            subscription_version: 1,
            cloud_valid_until: Timestamp::default(),
        }
    }

    /// 只有「`enforce` + `limit > 0`」才报 usage；其余**一律 204**（逐字上游）。
    #[test]
    fn only_a_positive_enforced_limit_produces_a_body() {
        assert_eq!(
            enforced_issue_limit(&enforce(Some(50), Reason::CacheFresh)),
            Some(50)
        );
        assert_eq!(
            enforced_issue_limit(&enforce(Some(1), Reason::Refreshed)),
            Some(1)
        );
        // limit 缺失 / 为 0 ⇒ 「没有强制限额」，不是「无限」。
        assert_eq!(
            enforced_issue_limit(&enforce(None, Reason::CacheFresh)),
            None
        );
        assert_eq!(
            enforced_issue_limit(&enforce(Some(0), Reason::CacheFresh)),
            None
        );
        // observe 永不作废一个 issue。
        let mut observed = enforce(Some(50), Reason::Stale);
        observed.gate.action = Action::Observe;
        assert_eq!(enforced_issue_limit(&observed), None);
        // off / 无策略 ⇒ 204。
        assert_eq!(enforced_issue_limit(&off_decision(Reason::Disabled)), None);
        assert_eq!(
            enforced_issue_limit(&off_decision(Reason::Unavailable)),
            None
        );
        // 🔴 替身判决不得变成生产配额。
        assert_eq!(enforced_issue_limit(&enforce(Some(50), Reason::Stub)), None);
    }
}
