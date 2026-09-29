//! 校验 / 规格构造：`normalize_*` / `parse_*` 与 `build_scope` / `build_filters` /
//! `build_order` / `build_group_spec` / `build_facets`。模块文档见父模块（`mod.rs`）。

use chrono::{DateTime, Utc};
use serde_json::Value as JsonValue;
use uuid::Uuid;

use mc_core::Id;
use mc_errors::Error;
use mc_repos::issue_table::{
    TableActor, TableDateField, TableDateFilter, TableFacetKind, TableFilter, TableGroupKind,
    TableGroupSpec, TableMyRelation, TableOrder, TableScope, TableSortDirection, TableSortField,
    ACTOR_TYPES, TABLE_DEFAULT_PAGE_SIZE, TABLE_MAX_FACETS, TABLE_MAX_PAGE_SIZE,
};

use crate::routes::issues::validation;

use super::super::TableError;
use super::dto::{
    unsupported_filter_err, unsupported_group_err, ActorDto, FacetSpecDto, FiltersDto, GroupDto,
    PageDto, ScopeDto, SortDto, STATUS_KEY_MAX_LEN,
};

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
