//! `query_fingerprint`（上游 `canonicalIssueTableFingerprint`）与分组 spec 的稳定身份 /
//! `group_key` 解析。模块文档见父模块（`mod.rs`）。

use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use mc_core::Id;
use mc_errors::Error;
use mc_repos::issue_table::{
    TableActor, TableDateField, TableFilter, TableGroupKey, TableGroupKind, TableGroupSpec,
    TableMyRelation, TableOrder, TableScope, TableSortDirection,
};

use crate::routes::issues::validation;

use super::build::{normalize_actor_type, parse_project_id};

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
