//! `/api/issues/table/*` 的请求 DTO 与「已校验规格」构造（从 `issue_table.rs` 拆出，R7 单文件
//! 800 行上限；`scripts/file_size_check.py` + 门 ⑩ 执行）。
//!
//! 上游对应：`issue_table_query.go` 的 `decodeIssueTableJSON` / `normalizeIssueTablePage` /
//! `canonicalIssueTableFingerprint`，以及 `issue_table_group.go` 的 `resolveIssueTableGroup`。
//!
//! 职责边界：**JSON 请求形状（上游字段名逐字一致）→ `mc_repos::issue_table` 的规格类型**，
//! 外加指纹（`query_fingerprint`）与分组 key / value 的解析。路由与 handler 在 `super`，
//! cursor 编解码在 `super::cursor`。与上游的有意偏离逐条见 `docs/14-M2-TABLE.md` §4，
//! 支持面见 §5。

use axum::body::Bytes;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use mc_core::Id;
use mc_errors::Error;
use mc_repos::issue_table::{
    TableActor, TableDateField, TableDateFilter, TableFacetKind, TableFilter, TableGroupKey,
    TableGroupKind, TableGroupSpec, TableMyRelation, TableOrder, TableScope, TableSortDirection,
    TableSortField, ACTOR_TYPES, TABLE_DEFAULT_PAGE_SIZE, TABLE_MAX_FACETS, TABLE_MAX_PAGE_SIZE,
};

use crate::routes::issues::validation;

use super::TableError;

/// 422：分组 / facet 维度不支持的返回（保留上游字段名，见 `super` 的模块注释 2）。
pub(crate) fn unsupported_group_err(code: &str, message: &str) -> TableError {
    TableError::Unsupported {
        code: code.to_string(),
        message: message.to_string(),
    }
}

pub(crate) fn unsupported_filter_err(code: &str, message: &str) -> TableError {
    TableError::UnsupportedFilter {
        code: code.to_string(),
        message: message.to_string(),
    }
}

/// 请求体上限（上游 `http.MaxBytesReader(w, r.Body, 1<<20)`）。
pub(crate) const MAX_BODY_BYTES: usize = 1 << 20;

/// `filters.statuses` 单个 key 的长度上限（上游 `len(status) > 64`）。
pub(crate) const STATUS_KEY_MAX_LEN: usize = 64;

// ---------------------------------------------------------------------------
// 请求 DTO（字段名与上游一致；未知字段 → 400，镜像上游 DisallowUnknownFields）
// ---------------------------------------------------------------------------

/// `{"type": "...", "id": "..."}`。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActorDto {
    #[serde(rename = "type", default)]
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) id: String,
}

/// `query.scope`。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScopeDto {
    #[serde(default)]
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) assignee_types: Vec<String>,
    #[serde(default)]
    pub(crate) project_id: Option<String>,
    #[serde(default)]
    pub(crate) actor: Option<ActorDto>,
    #[serde(default)]
    pub(crate) relation: String,
}

/// `query.filters.date`。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DateFilterDto {
    #[serde(default)]
    pub(crate) field: String,
    #[serde(default)]
    pub(crate) start: String,
    #[serde(default)]
    pub(crate) end: String,
}

/// `query.filters`。上面是已实现字段，下面是**已声明但本仓不支持**的字段：
/// 空值等价于“未提供”，非空一律 422（见模块注释 3）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FiltersDto {
    #[serde(default)]
    pub(crate) statuses: Vec<String>,
    #[serde(default)]
    pub(crate) priorities: Vec<String>,
    #[serde(default)]
    pub(crate) assignees: Option<Vec<ActorDto>>,
    #[serde(default)]
    pub(crate) include_no_assignee: bool,
    #[serde(default)]
    pub(crate) creators: Vec<ActorDto>,
    #[serde(default)]
    pub(crate) project_ids: Vec<String>,
    #[serde(default)]
    pub(crate) include_no_project: bool,
    #[serde(default)]
    pub(crate) date: Option<DateFilterDto>,
    #[serde(default)]
    pub(crate) include_sub_issues: Option<bool>,
    // ---- 已声明但不支持（422）----
    #[serde(default)]
    pub(crate) project_statuses: Vec<String>,
    #[serde(default)]
    pub(crate) label_ids: Vec<String>,
    #[serde(default)]
    pub(crate) properties: Option<JsonValue>,
    #[serde(default)]
    pub(crate) working_only: bool,
    #[serde(default)]
    pub(crate) working_issue_ids: Option<Vec<String>>,
}

/// `query.sort`。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SortDto {
    #[serde(default)]
    pub(crate) field: String,
    #[serde(default)]
    pub(crate) direction: String,
}

/// `query`（`scope` / `filters` / `sort` 缺失时按空结构处理，与上游零值一致）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TableQueryDto {
    #[serde(default)]
    pub(crate) scope: ScopeDto,
    #[serde(default)]
    pub(crate) filters: FiltersDto,
    #[serde(default)]
    pub(crate) search: Option<String>,
    #[serde(default)]
    pub(crate) sort: SortDto,
}

/// `group`（`primary` / `secondary` 是**字符串**，与上游一致）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GroupDto {
    #[serde(default)]
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) property_id: Option<String>,
    /// 上游字段：只在 `status_category` 分组下消费，本仓该维度不支持（接受但忽略）。
    #[serde(default)]
    #[allow(dead_code)] // 上游已安装客户端会带上它，拒收会误伤整个请求
    pub(crate) category_format: Option<String>,
    #[serde(default)]
    pub(crate) include_empty: bool,
    #[serde(default)]
    pub(crate) primary: Option<String>,
    #[serde(default)]
    pub(crate) secondary: Option<String>,
    #[serde(default)]
    pub(crate) secondary_values: Option<Vec<String>>,
}

/// `page`。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PageDto {
    #[serde(default)]
    pub(crate) limit: i64,
    #[serde(default)]
    pub(crate) cursor: Option<String>,
}

/// `hierarchy`。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HierarchyDto {
    #[serde(default)]
    pub(crate) enabled: bool,
}

/// `POST /api/issues/table/groups` 请求体。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GroupsRequest {
    #[serde(default)]
    pub(crate) query: TableQueryDto,
    #[serde(default)]
    pub(crate) group: GroupDto,
    #[serde(default)]
    pub(crate) page: PageDto,
}

/// `POST /api/issues/table/rows` 请求体。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RowsRequest {
    #[serde(default)]
    pub(crate) query: TableQueryDto,
    #[serde(default)]
    pub(crate) group: GroupDto,
    #[serde(default)]
    pub(crate) group_key: Option<String>,
    #[serde(default)]
    pub(crate) hierarchy: HierarchyDto,
    #[serde(default)]
    pub(crate) parent_id: Option<String>,
    #[serde(default)]
    pub(crate) page: PageDto,
}

/// `facets[]`。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FacetSpecDto {
    #[serde(default)]
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) property_id: Option<String>,
}

/// `POST /api/issues/table/facets` 请求体。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FacetsRequest {
    #[serde(default)]
    pub(crate) query: TableQueryDto,
    #[serde(default)]
    pub(crate) facets: Vec<FacetSpecDto>,
    #[serde(default)]
    pub(crate) include_total: Option<bool>,
}

/// 解码请求体：上限 1 MiB、未知字段 400（上游 `decodeIssueTableJSON`）。
pub(crate) fn decode_body<T: serde::de::DeserializeOwned>(body: &Bytes) -> Result<T, Error> {
    if body.len() > MAX_BODY_BYTES {
        return Err(validation(
            "invalid issue table query: request body too large",
        ));
    }
    serde_json::from_slice(body).map_err(|e| validation(format!("invalid issue table query: {e}")))
}

// ---------------------------------------------------------------------------
// 校验 / 规格构造
// ---------------------------------------------------------------------------

/// `normalizeIssueTablePage`：默认 50，范围 [1, 100]。
pub(crate) fn normalize_page(page: &PageDto) -> Result<i64, Error> {
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
pub(crate) fn normalize_actor_type(raw: &str) -> &str {
    match raw.trim() {
        "member" => "user",
        other => other,
    }
}

/// `{"type","id"}` → `TableActor`（类型不在 `ACTOR_TYPES`、id 非 uuid → 400）。
pub(crate) fn parse_actor(field: &str, dto: &ActorDto) -> Result<TableActor, Error> {
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

pub(crate) fn parse_actors(field: &str, dtos: &[ActorDto]) -> Result<Vec<TableActor>, Error> {
    dtos.iter().map(|dto| parse_actor(field, dto)).collect()
}

pub(crate) fn parse_project_id(field: &str, raw: &str) -> Result<Id, Error> {
    Id::parse(raw.trim()).map_err(|_| validation(format!("invalid {field}: expected a uuid")))
}

pub(crate) fn parse_rfc3339(field: &str, raw: &str) -> Result<DateTime<Utc>, Error> {
    DateTime::parse_from_rfc3339(raw.trim())
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| validation(format!("invalid {field}: expected an RFC3339 timestamp")))
}

/// `query.scope` → `TableScope`。
///
/// 空 `kind` 等价于 `workspace`（上游零值行为）；`my` 的 actor 取登录用户（偏离 4）。
pub(crate) fn build_scope(scope: &ScopeDto, user_id: Id) -> Result<TableScope, Error> {
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
pub(crate) fn build_filters(
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
                return Err(
                    validation("filters.date.start and filters.date.end are required").into(),
                );
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
pub(crate) fn has_any_property(value: &JsonValue) -> bool {
    match value {
        JsonValue::Null => false,
        JsonValue::Object(map) => map.values().any(has_any_property),
        JsonValue::Array(items) => !items.is_empty(),
        _ => true,
    }
}

pub(crate) fn build_search(search: Option<&str>) -> String {
    search.unwrap_or_default().trim().to_string()
}

/// `query.sort` → `TableOrder`。
pub(crate) fn build_order(sort: &SortDto) -> Result<TableOrder, Error> {
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
pub(crate) fn build_group_spec(
    group: &GroupDto,
    allow_none: bool,
) -> Result<TableGroupSpec, TableError> {
    if group
        .property_id
        .as_deref()
        .is_some_and(|v| !v.trim().is_empty())
        || group
            .primary
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty())
        || group
            .secondary
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty())
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
pub(crate) fn build_facets(facets: &[FacetSpecDto]) -> Result<Vec<TableFacetKind>, TableError> {
    if facets.len() > TABLE_MAX_FACETS {
        return Err(validation(format!(
            "facets must contain at most {TABLE_MAX_FACETS} entries"
        ))
        .into());
    }
    if facets.iter().any(|facet| {
        facet
            .property_id
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty())
    }) {
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

pub(crate) fn sorted_unique_strings(values: &[String]) -> Vec<String> {
    let mut out: Vec<String> = values.iter().map(|v| v.trim().to_string()).collect();
    out.sort();
    out.dedup();
    out
}

pub(crate) fn sorted_unique_actors(actors: &[TableActor]) -> Vec<JsonValue> {
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
pub(crate) fn query_fingerprint(
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

/// 分组 spec 的稳定身份（cursor 里用来发现“翻页途中换了分组维度”）。
pub(crate) fn group_identity(group: TableGroupSpec) -> String {
    format!("{}:{}", group.kind.key(), group.include_empty)
}

/// 从 `/groups` 返回的 key 反推上游的 `value` 对象（键格式由 repo 的 `group_key_of` 决定）。
pub(crate) fn group_value(key: &str, kind: TableGroupKind) -> JsonValue {
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
pub(crate) fn parse_group_key(
    kind: TableGroupKind,
    raw: Option<&str>,
) -> Result<TableGroupKey, Error> {
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
        (TableGroupKind::Project, Some(value)) => {
            Ok(TableGroupKey::Project(Some(parse_project_id(
                "group_key",
                value.strip_prefix("project:").unwrap_or_default(),
            )?)))
        }
    }
}
