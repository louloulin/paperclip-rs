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
//! 6. `GET /api/issues/limit-usage` 恒 **204**：本仓没有 entitlement/Cloud 订阅源，
//!    等价于上游 `policy.Action != ActionEnforce` 的分支（“未强制限额，无 usage 可报”），
//!    而不是伪造一个 `{"used":…,"limit":…}`。
//! 7. 400 的错误体沿用本仓 `{"error":{"code","message"}}`；409/422 沿用上游的扁平
//!    形状（`{"error":"cursor_query_mismatch"…}` / `{"error":"unsupported_group"…}`）。
//!
//! 未接进 `scripts/gates.sh` 的 DB 回放测试见 `crates/mc-http/tests/issues.rs`
//! （`#[ignore]` + `MULTICA_TEST_DATABASE_URL`）。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use mc_core::Id;
use mc_errors::Error;
use mc_repos::issue_table::{
    IssueTableRepo, TableActor, TableCursor, TableDateField, TableDateFilter, TableFacetKind,
    TableFacetsQuery, TableFilter, TableGroupCursor, TableGroupKey, TableGroupKind,
    TableGroupSpec, TableGroupsQuery, TableMyRelation, TableOrder, TableRowsQuery, TableScope,
    TableSortDirection, TableSortField, ACTOR_TYPES, TABLE_DEFAULT_PAGE_SIZE, TABLE_MAX_FACETS,
    TABLE_MAX_PAGE_SIZE,
};
use mc_repos::RepoError;

use crate::routes::auth_user::AuthUser;
use crate::routes::invitations::require_workspace_member;
use crate::routes::issues::{
    load_catalog, repo_err, resolve_workspace, validation, IssueDto, WorkspaceQuery,
};
use crate::state::AppState;

/// 请求体上限（上游 `http.MaxBytesReader(w, r.Body, 1<<20)`）。
const MAX_BODY_BYTES: usize = 1 << 20;
/// cursor 版本号（上游 `issueTableCursor.Version`）。
const CURSOR_VERSION: u8 = 1;
/// `filters.statuses` 单个 key 的长度上限（上游 `len(status) > 64`）。
const STATUS_KEY_MAX_LEN: usize = 64;

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
    Unsupported { code: String, message: String },
    /// 422 `unsupported_filter`（过滤维度不支持）。
    UnsupportedFilter { code: String, message: String },
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

/// 422：分组 / facet 维度不支持的返回（保留上游字段名，见模块注释 2）。
fn unsupported_group_err(code: &str, message: &str) -> TableError {
    TableError::Unsupported {
        code: code.to_string(),
        message: message.to_string(),
    }
}

fn unsupported_filter_err(code: &str, message: &str) -> TableError {
    TableError::UnsupportedFilter {
        code: code.to_string(),
        message: message.to_string(),
    }
}

// ---------------------------------------------------------------------------
// 请求 DTO（字段名与上游一致；未知字段 → 400，镜像上游 DisallowUnknownFields）
// ---------------------------------------------------------------------------

/// `{"type": "...", "id": "..."}`。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActorDto {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    id: String,
}

/// `query.scope`。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeDto {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    assignee_types: Vec<String>,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    actor: Option<ActorDto>,
    #[serde(default)]
    relation: String,
}

/// `query.filters.date`。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DateFilterDto {
    #[serde(default)]
    field: String,
    #[serde(default)]
    start: String,
    #[serde(default)]
    end: String,
}

/// `query.filters`。上面是已实现字段，下面是**已声明但本仓不支持**的字段：
/// 空值等价于“未提供”，非空一律 422（见模块注释 3）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FiltersDto {
    #[serde(default)]
    statuses: Vec<String>,
    #[serde(default)]
    priorities: Vec<String>,
    #[serde(default)]
    assignees: Option<Vec<ActorDto>>,
    #[serde(default)]
    include_no_assignee: bool,
    #[serde(default)]
    creators: Vec<ActorDto>,
    #[serde(default)]
    project_ids: Vec<String>,
    #[serde(default)]
    include_no_project: bool,
    #[serde(default)]
    date: Option<DateFilterDto>,
    #[serde(default)]
    include_sub_issues: Option<bool>,
    // ---- 已声明但不支持（422）----
    #[serde(default)]
    project_statuses: Vec<String>,
    #[serde(default)]
    label_ids: Vec<String>,
    #[serde(default)]
    properties: Option<JsonValue>,
    #[serde(default)]
    working_only: bool,
    #[serde(default)]
    working_issue_ids: Option<Vec<String>>,
}

/// `query.sort`。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SortDto {
    #[serde(default)]
    field: String,
    #[serde(default)]
    direction: String,
}

/// `query`（`scope` / `filters` / `sort` 缺失时按空结构处理，与上游零值一致）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TableQueryDto {
    #[serde(default)]
    scope: ScopeDto,
    #[serde(default)]
    filters: FiltersDto,
    #[serde(default)]
    search: Option<String>,
    #[serde(default)]
    sort: SortDto,
}

/// `group`（`primary` / `secondary` 是**字符串**，与上游一致）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct GroupDto {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    property_id: Option<String>,
    /// 上游字段：只在 `status_category` 分组下消费，本仓该维度不支持（接受但忽略）。
    #[serde(default)]
    #[allow(dead_code)] // 上游已安装客户端会带上它，拒收会误伤整个请求
    category_format: Option<String>,
    #[serde(default)]
    include_empty: bool,
    #[serde(default)]
    primary: Option<String>,
    #[serde(default)]
    secondary: Option<String>,
    #[serde(default)]
    secondary_values: Option<Vec<String>>,
}

/// `page`。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PageDto {
    #[serde(default)]
    limit: i64,
    #[serde(default)]
    cursor: Option<String>,
}

/// `hierarchy`。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct HierarchyDto {
    #[serde(default)]
    enabled: bool,
}

/// `POST /api/issues/table/groups` 请求体。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct GroupsRequest {
    #[serde(default)]
    query: TableQueryDto,
    #[serde(default)]
    group: GroupDto,
    #[serde(default)]
    page: PageDto,
}

/// `POST /api/issues/table/rows` 请求体。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RowsRequest {
    #[serde(default)]
    query: TableQueryDto,
    #[serde(default)]
    group: GroupDto,
    #[serde(default)]
    group_key: Option<String>,
    #[serde(default)]
    hierarchy: HierarchyDto,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    page: PageDto,
}

/// `facets[]`。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FacetSpecDto {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    property_id: Option<String>,
}

/// `POST /api/issues/table/facets` 请求体。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FacetsRequest {
    #[serde(default)]
    query: TableQueryDto,
    #[serde(default)]
    facets: Vec<FacetSpecDto>,
    #[serde(default)]
    include_total: Option<bool>,
}

/// 解码请求体：上限 1 MiB、未知字段 400（上游 `decodeIssueTableJSON`）。
fn decode_body<T: serde::de::DeserializeOwned>(body: &Bytes) -> Result<T, Error> {
    if body.len() > MAX_BODY_BYTES {
        return Err(validation("invalid issue table query: request body too large"));
    }
    serde_json::from_slice(body)
        .map_err(|e| validation(format!("invalid issue table query: {e}")))
}

// ---------------------------------------------------------------------------
// 校验 / 规格构造
// ---------------------------------------------------------------------------

/// `normalizeIssueTablePage`：默认 50，范围 [1, 100]。
fn normalize_page(page: &PageDto) -> Result<i64, Error> {
    let limit = if page.limit == 0 {
        TABLE_DEFAULT_PAGE_SIZE
    } else {
        page.limit
    };
    if !(1..=TABLE_MAX_PAGE_SIZE).contains(&limit) {
        return Err(validation(format!(
            "page.limit must be between 1 and {TABLE_MAX_PAGE_SIZE}"
        )));
    }
    Ok(limit)
}

/// 角色类型归一化：上游写作 `member`，本仓 CHECK 是 `user`（与 M2-A 同款处理）。
fn normalize_actor_type(raw: &str) -> &str {
    match raw.trim() {
        "member" => "user",
        other => other,
    }
}

/// `{"type","id"}` → `TableActor`（类型不在 `ACTOR_TYPES`、id 非 uuid → 400）。
fn parse_actor(field: &str, dto: &ActorDto) -> Result<TableActor, Error> {
    let kind = normalize_actor_type(&dto.kind).to_string();
    if !ACTOR_TYPES.contains(&kind.as_str()) {
        return Err(validation(format!(
            "invalid {field}.type: expected one of {}",
            ACTOR_TYPES.join(", ")
        )));
    }
    let id = Uuid::parse_str(dto.id.trim())
        .map_err(|_| validation(format!("invalid {field}.id: expected a uuid")))?;
    Ok(TableActor { kind, id })
}

fn parse_actors(field: &str, dtos: &[ActorDto]) -> Result<Vec<TableActor>, Error> {
    dtos.iter().map(|dto| parse_actor(field, dto)).collect()
}

fn parse_project_id(field: &str, raw: &str) -> Result<Id, Error> {
    Id::parse(raw.trim()).map_err(|_| validation(format!("invalid {field}: expected a uuid")))
}

fn parse_rfc3339(field: &str, raw: &str) -> Result<DateTime<Utc>, Error> {
    DateTime::parse_from_rfc3339(raw.trim())
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| validation(format!("invalid {field}: expected an RFC3339 timestamp")))
}

/// `query.scope` → `TableScope`。
///
/// 空 `kind` 等价于 `workspace`（上游零值行为）；`my` 的 actor 取登录用户（偏离 4）。
fn build_scope(scope: &ScopeDto, user_id: Id) -> Result<TableScope, Error> {
    let assignee_types = scope
        .assignee_types
        .iter()
        .map(|raw| normalize_actor_type(raw).to_string())
        .collect::<Vec<_>>();
    for kind in &assignee_types {
        if !ACTOR_TYPES.contains(&kind.as_str()) {
            return Err(validation(format!(
                "invalid query.scope.assignee_types: expected one of {}",
                ACTOR_TYPES.join(", ")
            )));
        }
    }
    match scope.kind.trim() {
        "" | "workspace" => Ok(TableScope::Workspace { assignee_types }),
        "project" => {
            let raw = scope.project_id.as_deref().unwrap_or_default();
            if raw.trim().is_empty() {
                return Err(validation("query.scope.project_id is required"));
            }
            Ok(TableScope::Project {
                project_id: parse_project_id("query.scope.project_id", raw)?,
                assignee_types,
            })
        }
        "assignee" | "creator" => {
            let dto = scope
                .actor
                .as_ref()
                .ok_or_else(|| validation("query.scope.actor is required"))?;
            let actor = parse_actor("query.scope.actor", dto)?;
            if scope.kind.trim() == "assignee" {
                Ok(TableScope::Assignee(actor))
            } else {
                Ok(TableScope::Creator(actor))
            }
        }
        "my" => {
            let relation = TableMyRelation::parse(&scope.relation)
                .ok_or_else(|| validation("invalid query.scope.relation"))?;
            Ok(TableScope::My {
                actor: TableActor {
                    kind: "user".to_string(),
                    id: user_id.0,
                },
                relation,
            })
        }
        other => Err(validation(format!("invalid query.scope.kind: {other}"))),
    }
}

/// `query.filters` → `TableFilter`（含“不支持维度 → 422”的判定）。
fn build_filters(
    filters: &FiltersDto,
    scope: TableScope,
    search: &str,
) -> Result<(TableFilter, bool), TableError> {
    for status in &filters.statuses {
        if status.is_empty() || status.len() > STATUS_KEY_MAX_LEN {
            return Err(validation("invalid filters.statuses").into());
        }
    }
    for priority in &filters.priorities {
        if mc_core::priority::Priority::from_str_opt(priority).is_none() {
            return Err(validation("invalid filters.priorities").into());
        }
    }
    // ---- 不支持维度：非空 → 422（空数组/空对象等价于未提供，见模块注释 3）----
    if !filters.project_statuses.is_empty() {
        return Err(unsupported_filter_err(
            "project_status_filter_unsupported",
            "filters.project_statuses is not supported in multica-rs yet",
        ));
    }
    if !filters.label_ids.is_empty() {
        return Err(unsupported_filter_err(
            "label_filter_unsupported",
            "filters.label_ids is not supported in multica-rs yet (no label tables; see LUM-1370)",
        ));
    }
    if filters.properties.as_ref().is_some_and(has_any_property) {
        return Err(unsupported_filter_err(
            "property_filter_unsupported",
            "filters.properties is not supported in multica-rs yet (no issue_properties table)",
        ));
    }
    if filters.working_only {
        return Err(unsupported_filter_err(
            "working_filter_unsupported",
            "filters.working_only is not supported in multica-rs yet (agent task queue lands in M3)",
        ));
    }
    if filters.working_issue_ids.is_some() {
        return Err(unsupported_filter_err(
            "working_filter_unsupported",
            "filters.working_issue_ids is not supported in multica-rs yet (agent task queue lands in M3)",
        ));
    }

    let assignees = match &filters.assignees {
        None => None,
        Some(dtos) => Some(parse_actors("filters.assignees", dtos)?),
    };
    let creators = parse_actors("filters.creators", &filters.creators)?;
    let project_ids = filters
        .project_ids
        .iter()
        .map(|raw| parse_project_id("filters.project_ids", raw))
        .collect::<Result<Vec<_>, _>>()?;
    let date = match &filters.date {
        None => None,
        Some(dto) => {
            let field = TableDateField::parse(&dto.field)
                .ok_or_else(|| validation("invalid filters.date.field"))?;
            if dto.start.trim().is_empty() || dto.end.trim().is_empty() {
                return Err(validation("filters.date.start and filters.date.end are required").into());
            }
            Some(TableDateFilter {
                field,
                start: parse_rfc3339("filters.date.start", &dto.start)?,
                end: parse_rfc3339("filters.date.end", &dto.end)?,
            })
        }
    };
    let explicit_empty_assignees = filters.assignees.as_ref().is_some_and(Vec::is_empty);
    Ok((
        TableFilter {
            scope,
            assignees,
            include_no_assignee: filters.include_no_assignee,
            statuses: filters.statuses.clone(),
            priorities: filters.priorities.clone(),
            creators,
            project_ids,
            include_no_project: filters.include_no_project,
            date,
            include_sub_issues: filters.include_sub_issues,
            search: search.trim().to_string(),
        },
        explicit_empty_assignees,
    ))
}

/// `properties` 是否真的携带了过滤值（`null` / `{}` / 全空数组都不算）。
fn has_any_property(value: &JsonValue) -> bool {
    match value {
        JsonValue::Null => false,
        JsonValue::Object(map) => map.values().any(has_any_property),
        JsonValue::Array(items) => !items.is_empty(),
        _ => true,
    }
}

fn build_search(search: Option<&str>) -> String {
    search.unwrap_or_default().trim().to_string()
}

/// `query.sort` → `TableOrder`。
fn build_order(sort: &SortDto) -> Result<TableOrder, Error> {
    let field = TableSortField::parse(&sort.field)
        .ok_or_else(|| validation(format!("invalid query.sort.field: {}", sort.field)))?;
    let direction = TableSortDirection::parse(&sort.direction)
        .ok_or_else(|| validation(format!("invalid query.sort.direction: {}", sort.direction)))?;
    // 上游把空 direction 当“字段默认方向”（不是 asc），这里用 None 表达。
    let direction = if sort.direction.trim().is_empty() {
        None
    } else {
        Some(direction)
    };
    Ok(TableOrder { field, direction })
}

/// 分组 kind 的解析。`allow_none=false` 时 `none` 直接 400（`/groups` 不允许分组头）；
/// 未知 / 不支持的 kind → 422（`label` / `parent` / `property` / `compound` / `status_category`）。
fn build_group_spec(group: &GroupDto, allow_none: bool) -> Result<TableGroupSpec, TableError> {
    if group.property_id.as_deref().is_some_and(|v| !v.trim().is_empty())
        || group.primary.as_deref().is_some_and(|v| !v.trim().is_empty())
        || group.secondary.as_deref().is_some_and(|v| !v.trim().is_empty())
        || group
            .secondary_values
            .as_ref()
            .is_some_and(|values| !values.is_empty())
    {
        return Err(unsupported_group_err(
            "compound_group_unsupported",
            "property and compound grouping are not supported in multica-rs yet",
        ));
    }
    let raw = group.kind.trim();
    if raw == "none" && !allow_none {
        return Err(validation("group.kind must not be none for /api/issues/table/groups").into());
    }
    match TableGroupKind::parse(raw) {
        Some(TableGroupKind::None) => Ok(TableGroupSpec {
            kind: TableGroupKind::None,
            include_empty: false,
        }),
        Some(kind) => Ok(TableGroupSpec {
            kind,
            include_empty: group.include_empty,
        }),
        None => Err(unsupported_group_err(
            "group_kind_unsupported",
            &format!(
                "group.kind={raw} is not supported in multica-rs yet (supported: none, status, \
                 priority, assignee, project)"
            ),
        )),
    }
}

/// `facets[]` → `TableFacetKind`（`label` / `working_agents` / `property` → 422）。
fn build_facets(facets: &[FacetSpecDto]) -> Result<Vec<TableFacetKind>, TableError> {
    if facets.len() > TABLE_MAX_FACETS {
        return Err(validation(format!(
            "facets must contain at most {TABLE_MAX_FACETS} entries"
        ))
        .into());
    }
    if facets
        .iter()
        .any(|facet| facet.property_id.as_deref().is_some_and(|v| !v.trim().is_empty()))
    {
        return Err(unsupported_group_err(
            "facet_kind_unsupported",
            "facets[].kind=property is not supported in multica-rs yet (no issue_properties table)",
        ));
    }
    facets
        .iter()
        .map(|facet| match TableFacetKind::parse(&facet.kind) {
            Some(kind) => Ok(kind),
            None => Err(unsupported_group_err(
                "facet_kind_unsupported",
                &format!(
                    "facets[].kind={} is not supported in multica-rs yet (supported: status, \
                     priority, assignee, creator, project)",
                    facet.kind.trim()
                ),
            )),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// query_fingerprint（上游 `canonicalIssueTableFingerprint`）
// ---------------------------------------------------------------------------

fn sorted_unique_strings(values: &[String]) -> Vec<String> {
    let mut out: Vec<String> = values.iter().map(|v| v.trim().to_string()).collect();
    out.sort();
    out.dedup();
    out
}

fn sorted_unique_actors(actors: &[TableActor]) -> Vec<JsonValue> {
    let mut out: Vec<String> = actors
        .iter()
        .map(|actor| format!("{}:{}", actor.kind, actor.id))
        .collect();
    out.sort();
    out.dedup();
    out.into_iter()
        .map(|raw| {
            let (kind, id) = raw.split_once(':').expect("actor key always contains ':'");
            json!({ "type": kind, "id": id })
        })
        .collect()
}

/// 规范化后的查询指纹：`sha256:<hex>`，与上游同前缀，但只覆盖本仓支持的维度。
///
/// 数组一律排序去重、`search` 去空白，保证“同一查询不同书写顺序”得到同一指纹。
fn query_fingerprint(
    workspace_id: Id,
    filter: &TableFilter,
    order: TableOrder,
    explicit_empty_assignees: bool,
) -> String {
    let scope = match &filter.scope {
        TableScope::Workspace { assignee_types } => json!({
            "kind": "workspace",
            "assignee_types": sorted_unique_strings(assignee_types),
        }),
        TableScope::Project {
            project_id,
            assignee_types,
        } => json!({
            "kind": "project",
            "project_id": project_id.0.to_string(),
            "assignee_types": sorted_unique_strings(assignee_types),
        }),
        TableScope::Assignee(actor) => json!({
            "kind": "assignee",
            "actor": { "type": actor.kind, "id": actor.id.to_string() },
        }),
        TableScope::Creator(actor) => json!({
            "kind": "creator",
            "actor": { "type": actor.kind, "id": actor.id.to_string() },
        }),
        TableScope::My { actor, relation } => json!({
            "kind": "my",
            "actor": { "type": actor.kind, "id": actor.id.to_string() },
            "relation": match relation {
                TableMyRelation::Assigned => "assigned",
                TableMyRelation::Created => "created",
                TableMyRelation::Involved => "involved",
                TableMyRelation::Any => "any",
            },
        }),
    };
    let assigns = filter
        .assignees
        .as_ref()
        .map(|actors| sorted_unique_actors(actors));
    let date = filter.date.as_ref().map(|date| {
        json!({
            "field": match date.field {
                TableDateField::CreatedAt => "created_at",
                TableDateField::UpdatedAt => "updated_at",
            },
            "start": date.start.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
            "end": date.end.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        })
    });
    let canonical = json!({
        "workspace_id": workspace_id.0.to_string(),
        "query": {
            "scope": scope,
            "filters": {
                "statuses": sorted_unique_strings(&filter.statuses),
                "priorities": sorted_unique_strings(&filter.priorities),
                "assignees": assigns,
                "include_no_assignee": filter.include_no_assignee,
                "creators": sorted_unique_actors(&filter.creators),
                "project_ids": sorted_unique_strings(
                    &filter
                        .project_ids
                        .iter()
                        .map(|id| id.0.to_string())
                        .collect::<Vec<_>>(),
                ),
                "include_no_project": filter.include_no_project,
                "date": date,
                "include_sub_issues": filter.include_sub_issues,
            },
            "search": filter.search,
            "sort": {
                "field": order.field.key(),
                "direction": match order.direction.unwrap_or_else(|| order.field.default_direction()) {
                    TableSortDirection::Asc => "asc",
                    TableSortDirection::Desc => "desc",
                },
            },
        },
        "explicit_empty_assignees": explicit_empty_assignees,
    });
    let encoded = serde_json::to_vec(&canonical).expect("canonical query is always serializable");
    let digest = Sha256::digest(encoded);
    format!("sha256:{}", hex::encode(digest))
}

// ---------------------------------------------------------------------------
// cursor（上游 `issueTableCursor`；本仓 hex 编码，见模块注释 1）
// ---------------------------------------------------------------------------

/// cursor 的 JSON 形状，字段名与上游一致（`v` / `query` / `group_key` / …）。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CursorWire {
    v: u8,
    #[serde(rename = "query")]
    query_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    group_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    group_order: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    group_sort_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    group_cursor_key: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    branch_identity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sort_value: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    sort_is_null: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    row_created_at: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    row_id: String,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde 的 skip_serializing_if 只接受 `&T`
fn is_false(value: &bool) -> bool {
    !*value
}

impl CursorWire {
    fn new(query_fingerprint: &str) -> Self {
        Self {
            v: CURSOR_VERSION,
            query_fingerprint: query_fingerprint.to_string(),
            group_key: None,
            parent_id: None,
            group_order: None,
            group_sort_key: None,
            group_cursor_key: None,
            branch_identity: String::new(),
            sort_value: None,
            sort_is_null: false,
            row_created_at: String::new(),
            row_id: String::new(),
        }
    }

    fn encode(&self) -> String {
        let json = serde_json::to_vec(self).expect("cursor is always serializable");
        hex::encode(json)
    }

    fn decode(raw: &str) -> Result<Self, Error> {
        if raw.len() > 16 * 1024 {
            return Err(validation("invalid cursor"));
        }
        let bytes = hex::decode(raw.trim()).map_err(|_| validation("invalid cursor"))?;
        let wire: Self = serde_json::from_slice(&bytes).map_err(|_| validation("invalid cursor"))?;
        if wire.v != CURSOR_VERSION {
            return Err(validation("invalid cursor"));
        }
        Ok(wire)
    }

    /// 上游 `issueTableCursorMatches`：指纹 / `group_key` / `parent_id` 任一不符 → 409。
    fn matches(
        &self,
        fingerprint: &str,
        group_key: Option<&str>,
        parent_id: Option<&str>,
    ) -> Result<(), TableError> {
        if self.query_fingerprint != fingerprint
            || self.group_key.as_deref() != group_key
            || self.parent_id.as_deref() != parent_id
        {
            return Err(TableError::CursorMismatch);
        }
        Ok(())
    }

    /// 解析 `/rows` 的 keyset cursor（缺字段 → 400，镜像上游）。
    fn into_row_cursor(self) -> Result<TableCursor, Error> {
        if self.row_id.is_empty() || self.row_created_at.is_empty() {
            return Err(validation("invalid cursor"));
        }
        Ok(TableCursor {
            sort_value: self.sort_value,
            sort_is_null: self.sort_is_null,
            row_created_at: parse_rfc3339("cursor.row_created_at", &self.row_created_at)?,
            row_id: Id::parse(&self.row_id).map_err(|_| validation("invalid cursor"))?,
        })
    }

    /// 解析 `/groups` 的 keyset cursor（缺字段 → 400，镜像上游）。
    fn into_group_cursor(self) -> Result<TableGroupCursor, Error> {
        let (Some(order), Some(sort_key), Some(value)) = (
            self.group_order,
            self.group_sort_key,
            self.group_cursor_key,
        ) else {
            return Err(validation("invalid cursor"));
        };
        Ok(TableGroupCursor {
            order,
            sort_key,
            value,
        })
    }
}

/// 页面 cursor 解码：`None` = 首页；非空串解码失败 → 400（上游 `normalizeIssueTablePage`）。
fn decode_cursor(raw: Option<&str>) -> Result<Option<CursorWire>, Error> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(value) => CursorWire::decode(value).map(Some),
    }
}

/// 分组 spec 的稳定身份（cursor 里用来发现“翻页途中换了分组维度”）。
fn group_identity(group: TableGroupSpec) -> String {
    format!("{}:{}", group.kind.key(), group.include_empty)
}

/// 从 `/groups` 返回的 key 反推上游的 `value` 对象（键格式由 repo 的 `group_key_of` 决定）。
fn group_value(key: &str, kind: TableGroupKind) -> JsonValue {
    match kind {
        TableGroupKind::None => json!({ "kind": "none", "actor": JsonValue::Null }),
        TableGroupKind::Status => json!({
            "kind": "status",
            "status": key.strip_prefix("status:").unwrap_or_default(),
            "actor": JsonValue::Null,
        }),
        // 本仓新增维度（模块注释 5）。
        TableGroupKind::Priority => json!({
            "kind": "priority",
            "priority": key.strip_prefix("priority:").unwrap_or_default(),
            "actor": JsonValue::Null,
        }),
        TableGroupKind::Assignee => {
            let rest = key.strip_prefix("assignee:").unwrap_or_default();
            let actor = if rest == "unassigned" || rest.is_empty() {
                JsonValue::Null
            } else {
                match rest.split_once(':') {
                    Some((kind, id)) => json!({ "type": kind, "id": id }),
                    None => JsonValue::Null,
                }
            };
            json!({ "kind": "assignee", "actor": actor })
        }
        TableGroupKind::Project => {
            let rest = key.strip_prefix("project:").unwrap_or_default();
            if rest == "none" || rest.is_empty() {
                json!({ "kind": "project", "actor": JsonValue::Null })
            } else {
                json!({
                    "kind": "project",
                    "project_id": rest,
                    "actor": JsonValue::Null,
                })
            }
        }
    }
}

/// `/rows` 请求里的 `group_key` → `TableGroupKey`；与 `group.kind` 不符 → 400
/// （上游 `normalizeIssueTableGroupKey` + `resolveIssueTableGroupKey`）。
fn parse_group_key(kind: TableGroupKind, raw: Option<&str>) -> Result<TableGroupKey, Error> {
    let raw = raw.map(str::trim).filter(|v| !v.is_empty());
    match (kind, raw) {
        (TableGroupKind::None, None) => Ok(TableGroupKey::None),
        (TableGroupKind::None, Some(_)) => {
            Err(validation("group_key must be empty when group.kind=none"))
        }
        (_, None) => Err(validation("group_key is required")),
        (TableGroupKind::Status, Some(value)) => Ok(TableGroupKey::Status(
            value
                .strip_prefix("status:")
                .ok_or_else(|| validation("invalid group_key for group.kind=status"))?
                .to_string(),
        )),
        (TableGroupKind::Priority, Some(value)) => Ok(TableGroupKey::Priority(
            value
                .strip_prefix("priority:")
                .ok_or_else(|| validation("invalid group_key for group.kind=priority"))?
                .to_string(),
        )),
        (TableGroupKind::Assignee, Some("assignee:unassigned")) => {
            Ok(TableGroupKey::Assignee(None))
        }
        (TableGroupKind::Assignee, Some(value)) => {
            let rest = value
                .strip_prefix("assignee:")
                .ok_or_else(|| validation("invalid group_key for group.kind=assignee"))?;
            let (actor_kind, actor_id) = rest
                .split_once(':')
                .ok_or_else(|| validation("invalid group_key for group.kind=assignee"))?;
            Ok(TableGroupKey::Assignee(Some(TableActor {
                kind: normalize_actor_type(actor_kind).to_string(),
                id: Uuid::parse_str(actor_id)
                    .map_err(|_| validation("invalid group_key for group.kind=assignee"))?,
            })))
        }
        (TableGroupKind::Project, Some("project:none")) => Ok(TableGroupKey::Project(None)),
        (TableGroupKind::Project, Some(value)) => Ok(TableGroupKey::Project(Some(
            parse_project_id("group_key", value.strip_prefix("project:").unwrap_or_default())?,
        ))),
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
    let (filter, explicit_empty_assignees) =
        build_filters(&request.query.filters, scope, &search)?;
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
    let (filter, explicit_empty_assignees) =
        build_filters(&request.query.filters, scope, &search)?;
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
    let cursor = input.cursor_raw.map(CursorWire::into_row_cursor).transpose()?;

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
        wire.row_created_at =
            next.row_created_at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
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
    let (filter, explicit_empty_assignees) =
        build_filters(&request.query.filters, scope, &search)?;
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

/// 本仓没有 Cloud 订阅源 ⇒ 等价于上游“未强制限额”的分支：204 No Content（模块注释 6）。
async fn limit_usage(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(selector): Query<WorkspaceQuery>,
    user: AuthUser,
) -> Result<StatusCode, TableError> {
    let _ = authorize(&state, &headers, &selector, &user).await?;
    // 本仓没有 entitlement 源，无 usage 可报（模块注释 6）：204。
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn actor(kind: &str, id: u128) -> TableActor {
        TableActor {
            kind: kind.to_string(),
            id: uuid(id),
        }
    }

    fn workspace_scope() -> TableScope {
        TableScope::Workspace {
            assignee_types: Vec::new(),
        }
    }

    fn base_input(limit: i64) -> RowsRequest {
        RowsRequest {
            query: TableQueryDto::default(),
            group: GroupDto {
                kind: "status".to_string(),
                ..GroupDto::default()
            },
            group_key: Some("status:todo".to_string()),
            hierarchy: HierarchyDto::default(),
            parent_id: None,
            page: PageDto {
                limit,
                cursor: None,
            },
        }
    }

    #[test]
    fn page_limit_bounds() {
        assert_eq!(normalize_page(&PageDto::default()).unwrap(), TABLE_DEFAULT_PAGE_SIZE);
        assert!(normalize_page(&PageDto {
            limit: 0,
            cursor: None
        })
        .is_ok());
        assert!(normalize_page(&PageDto {
            limit: TABLE_MAX_PAGE_SIZE,
            cursor: None
        })
        .is_ok());
        assert!(normalize_page(&PageDto {
            limit: TABLE_MAX_PAGE_SIZE + 1,
            cursor: None
        })
        .is_err());
        assert!(normalize_page(&PageDto {
            limit: -1,
            cursor: None
        })
        .is_err());
    }

    #[test]
    fn group_key_round_trip() {
        assert_eq!(
            parse_group_key(TableGroupKind::Status, Some("status:todo")).unwrap(),
            TableGroupKey::Status("todo".into())
        );
        assert_eq!(
            parse_group_key(TableGroupKind::Assignee, Some("assignee:unassigned")).unwrap(),
            TableGroupKey::Assignee(None)
        );
        assert_eq!(
            parse_group_key(
                TableGroupKind::Assignee,
                Some(&format!("assignee:user:{}", uuid(7)))
            )
                .unwrap(),
            TableGroupKey::Assignee(Some(actor("user", 7)))
        );
        assert_eq!(
            parse_group_key(TableGroupKind::Project, Some("project:none")).unwrap(),
            TableGroupKey::Project(None)
        );
        assert_eq!(
            parse_group_key(TableGroupKind::None, None).unwrap(),
            TableGroupKey::None
        );
        assert!(parse_group_key(TableGroupKind::None, Some("status:x")).is_err());
        assert!(parse_group_key(TableGroupKind::Status, None).is_err());
        assert!(parse_group_key(TableGroupKind::Status, Some("assignee:user:x")).is_err());
        assert!(parse_group_key(TableGroupKind::Priority, Some("priority:urgent")).is_ok());
    }

    #[test]
    fn group_value_shape_matches_upstream() {
        let value = group_value("status:todo", TableGroupKind::Status);
        assert_eq!(value["kind"], "status");
        assert_eq!(value["status"], "todo");
        assert!(value["actor"].is_null());

        let value = group_value("assignee:unassigned", TableGroupKind::Assignee);
        assert_eq!(value["kind"], "assignee");
        assert!(value["actor"].is_null());

        let id = uuid(3);
        let value = group_value(&format!("assignee:agent:{id}"), TableGroupKind::Assignee);
        assert_eq!(value["actor"]["type"], "agent");
        assert_eq!(value["actor"]["id"], id.to_string());

        let value = group_value("project:none", TableGroupKind::Project);
        assert_eq!(value["kind"], "project");
        assert!(value.get("project_id").is_none());

        let value = group_value("priority:urgent", TableGroupKind::Priority);
        assert_eq!(value["priority"], "urgent");
    }

    #[test]
    fn cursor_round_trip_and_mismatch() {
        let mut wire = CursorWire::new("sha256:abc");
        wire.group_key = Some("status".into());
        wire.row_created_at = "2026-01-02T03:04:05.000006Z".into();
        wire.row_id = uuid(9).to_string();
        wire.sort_is_null = true;
        let encoded = wire.encode();
        let decoded = CursorWire::decode(&encoded).unwrap();
        assert_eq!(decoded.encode(), encoded);
        assert!(decoded.matches("sha256:abc", Some("status"), None).is_ok());
        assert!(matches!(
            decoded.matches("sha256:other", Some("status"), None),
            Err(TableError::CursorMismatch)
        ));
        assert!(matches!(
            decoded.matches("sha256:abc", Some("project"), None),
            Err(TableError::CursorMismatch)
        ));
        let cursor = decoded.into_row_cursor().unwrap();
        assert!(cursor.sort_is_null);
        assert!(cursor.sort_value.is_none());
        assert_eq!(cursor.row_id, Id::from(uuid(9)));
        assert!(CursorWire::decode("not-hex").is_err());
        assert!(CursorWire::decode(&hex::encode(b"{\"v\":9}")).is_err());
    }

    #[test]
    fn group_cursor_requires_three_fields() {
        let mut wire = CursorWire::new("sha256:abc");
        wire.group_order = Some(3);
        assert!(wire.clone().into_group_cursor().is_err());
        wire.group_sort_key = Some("10".into());
        wire.group_cursor_key = Some("todo".into());
        let cursor = wire.into_group_cursor().unwrap();
        assert_eq!(cursor.order, 3);
        assert_eq!(cursor.value, "todo");
    }

    #[test]
    fn fingerprint_is_order_insensitive_and_dimension_sensitive() {
        let workspace = Id::from(uuid(1));
        let order = TableOrder::default();
        let mut filter = TableFilter {
            scope: workspace_scope(),
            statuses: vec!["todo".into(), "in_progress".into()],
            ..TableFilter::default()
        };
        let first = query_fingerprint(workspace, &filter, order, false);
        filter.statuses = vec!["in_progress".into(), "todo".into(), "todo".into()];
        assert_eq!(query_fingerprint(workspace, &filter, order, false), first);
        assert!(first.starts_with("sha256:"));
        filter.statuses = vec!["todo".into()];
        assert_ne!(query_fingerprint(workspace, &filter, order, false), first);
        let sorted = TableFilter {
            scope: workspace_scope(),
            statuses: vec!["todo".into(), "in_progress".into()],
            ..TableFilter::default()
        };
        assert_ne!(
            query_fingerprint(Id::from(uuid(2)), &sorted, order, false),
            first
        );
        let desc = TableOrder {
            field: TableSortField::Position,
            direction: Some(TableSortDirection::Desc),
        };
        assert_ne!(query_fingerprint(workspace, &sorted, desc, false), first);
    }

    #[test]
    fn scope_and_filter_validation() {
        let user = Id::from(uuid(42));
        assert!(matches!(
            build_scope(&ScopeDto::default(), user).unwrap(),
            TableScope::Workspace { .. }
        ));
        assert!(matches!(
            build_scope(
                &ScopeDto {
                    kind: "my".into(),
                    relation: "involved".into(),
                    ..ScopeDto::default()
                },
                user
            )
            .unwrap(),
            TableScope::My {
                relation: TableMyRelation::Involved,
                ..
            }
        ));
        assert!(build_scope(
            &ScopeDto {
                kind: "project".into(),
                ..ScopeDto::default()
            },
            user
        )
        .is_err());
        assert!(build_scope(
            &ScopeDto {
                kind: "nope".into(),
                ..ScopeDto::default()
            },
            user
        )
        .is_err());
        assert!(build_scope(
            &ScopeDto {
                kind: "workspace".into(),
                assignee_types: vec!["robot".into()],
                ..ScopeDto::default()
            },
            user
        )
        .is_err());

        // 空值等价于未提供；非空 → 422。
        let empty = FiltersDto::default();
        assert!(build_filters(&empty, workspace_scope(), "").is_ok());
        let empty_labels = FiltersDto {
            label_ids: Vec::new(),
            properties: Some(JsonValue::Object(serde_json::Map::default())),
            ..FiltersDto::default()
        };
        assert!(build_filters(&empty_labels, workspace_scope(), "").is_ok());
        let labels = FiltersDto {
            label_ids: vec![uuid(5).to_string()],
            ..FiltersDto::default()
        };
        assert!(matches!(
            build_filters(&labels, workspace_scope(), ""),
            Err(TableError::UnsupportedFilter { .. })
        ));
        let working = FiltersDto {
            working_issue_ids: Some(Vec::new()),
            ..FiltersDto::default()
        };
        assert!(matches!(
            build_filters(&working, workspace_scope(), ""),
            Err(TableError::UnsupportedFilter { .. })
        ));
        let bad_status = FiltersDto {
            statuses: vec!["x".repeat(65)],
            ..FiltersDto::default()
        };
        assert!(build_filters(&bad_status, workspace_scope(), "").is_err());
        let bad_priority = FiltersDto {
            priorities: vec!["nope".into()],
            ..FiltersDto::default()
        };
        assert!(build_filters(&bad_priority, workspace_scope(), "").is_err());
        let explicit_empty = FiltersDto {
            assignees: Some(Vec::new()),
            ..FiltersDto::default()
        };
        let (filter, flag) = build_filters(&explicit_empty, workspace_scope(), "").unwrap();
        assert!(flag);
        assert_eq!(filter.assignees, Some(Vec::new()));
    }

    #[test]
    fn group_spec_rejects_unsupported_dimensions() {
        assert!(matches!(
            build_group_spec(
                &GroupDto {
                    kind: "none".into(),
                    ..GroupDto::default()
                },
                false
            ),
            Err(TableError::Api(_))
        ));
        assert!(build_group_spec(
            &GroupDto {
                kind: "none".into(),
                ..GroupDto::default()
            },
            true
        )
        .is_ok());
        for kind in ["label", "parent", "property", "compound", "status_category", ""] {
            assert!(
                matches!(
                    build_group_spec(
                        &GroupDto {
                            kind: kind.into(),
                            ..GroupDto::default()
                        },
                        true
                    ),
                    Err(TableError::Unsupported { .. })
                ),
                "kind {kind} should be unsupported"
            );
        }
        assert!(matches!(
            build_group_spec(
                &GroupDto {
                    kind: "status".into(),
                    property_id: Some(uuid(4).to_string()),
                    ..GroupDto::default()
                },
                true
            ),
            Err(TableError::Unsupported { .. })
        ));
    }

    #[test]
    fn facet_kinds_and_limits() {
        let ok = build_facets(&[
            FacetSpecDto {
                kind: "status".into(),
                property_id: None,
            },
            FacetSpecDto {
                kind: "priority".into(),
                property_id: None,
            },
        ])
        .unwrap();
        assert_eq!(ok, vec![TableFacetKind::Status, TableFacetKind::Priority]);
        assert!(matches!(
            build_facets(&[FacetSpecDto {
                kind: "working_agents".into(),
                property_id: None,
            }]),
            Err(TableError::Unsupported { .. })
        ));
        let too_many: Vec<FacetSpecDto> = (0..=TABLE_MAX_FACETS)
            .map(|_| FacetSpecDto {
                kind: "status".into(),
                property_id: None,
            })
            .collect();
        assert!(matches!(build_facets(&too_many), Err(TableError::Api(_))));
    }

    #[test]
    fn body_decoding_rejects_unknown_fields() {
        let ok = decode_body::<GroupsRequest>(&Bytes::from_static(
            br#"{"query":{"scope":{"kind":"workspace"}},"group":{"kind":"status"},"page":{"limit":10}}"#,
        ));
        assert!(ok.is_ok());
        let unknown = decode_body::<GroupsRequest>(&Bytes::from_static(
            br#"{"query":{},"group":{"kind":"status"},"wat":1}"#,
        ));
        assert!(unknown.is_err());
        let trailing = decode_body::<GroupsRequest>(&Bytes::from_static(
            br#"{"query":{},"group":{"kind":"status"}} trailing"#,
        ));
        assert!(trailing.is_err());
        let oversized = decode_body::<GroupsRequest>(&Bytes::from(vec![b' '; MAX_BODY_BYTES + 1]));
        assert!(oversized.is_err());
    }

    #[test]
    fn rows_input_validates_parent_and_group_key() {
        let workspace = Id::from(uuid(1));
        let user = Id::from(uuid(2));
        let mut request = base_input(10);
        // 无 `hierarchy.enabled` 时 `parent_id` → 400（上游同）。
        request.parent_id = Some(uuid(3).to_string());
        assert!(matches!(
            build_rows_input(workspace, user, &request),
            Err(TableError::Api(_))
        ));
        request.hierarchy = HierarchyDto { enabled: true };
        assert!(build_rows_input(workspace, user, &request).is_ok());
        // `group.kind=none` 时带 `group_key` → 400。
        let mut request = base_input(10);
        request.group = GroupDto {
            kind: "none".into(),
            ..GroupDto::default()
        };
        assert!(matches!(
            build_rows_input(workspace, user, &request),
            Err(TableError::Api(_))
        ));
        request.group_key = None;
        let input = build_rows_input(workspace, user, &request).unwrap();
        assert_eq!(input.group_key, TableGroupKey::None);
        assert_eq!(input.group.kind, TableGroupKind::None);
    }
}