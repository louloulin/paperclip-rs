//! issue table 查询面的 SQL 编译（`$n` 参数绑定 / WHERE / 分组 / 排序）。
//!
//! 拆自 `issue_table.rs`（R7：单文件 800 行上限）；规格与结果类型在
//! `super`（`issue_table/mod.rs`），执行入口在 `super::repo`。

use chrono::{DateTime, Utc};
use sqlx::postgres::{PgArguments, PgRow};
use sqlx::query::{QueryAs, QueryScalar};
use sqlx::{FromRow, Postgres};
use uuid::Uuid;

use mc_core::Id;

use crate::issue::ISSUE_COLUMNS;

use super::{
    TableActor, TableCursor, TableFilter, TableGroupCursor, TableGroupKey, TableGroupKind,
    TableGroupSpec, TableMyRelation, TableOrder, TableScope, TableSortDirection, TableSortField,
    BUILTIN_PRIORITIES, GROUP_VALUE_NO_PROJECT, GROUP_VALUE_UNASSIGNED,
};

// ---------------------------------------------------------------------------
// 参数绑定
// ---------------------------------------------------------------------------

/// 运行时绑定参数。用 enum 而不是 sqlx 的 `Any` 泛型，是为了让 `$n` 的 SQL 片段与
/// `.bind()` 调用一一对应、类型由编译器检查。
#[derive(Debug, Clone)]
pub(crate) enum Param {
    Text(String),
    TextArray(Vec<String>),
    Uuid(Uuid),
    UuidArray(Vec<Uuid>),
    Timestamp(DateTime<Utc>),
    Int(i64),
}

impl Param {
    pub(crate) fn bind<O>(
        self,
        query: QueryAs<'_, Postgres, O, PgArguments>,
    ) -> QueryAs<'_, Postgres, O, PgArguments> {
        match self {
            Param::Text(value) => query.bind(value),
            Param::TextArray(value) => query.bind(value),
            Param::Uuid(value) => query.bind(value),
            Param::UuidArray(value) => query.bind(value),
            Param::Timestamp(value) => query.bind(value),
            Param::Int(value) => query.bind(value),
        }
    }

    pub(crate) fn bind_scalar(
        self,
        query: QueryScalar<'_, Postgres, i64, PgArguments>,
    ) -> QueryScalar<'_, Postgres, i64, PgArguments> {
        match self {
            Param::Text(value) => query.bind(value),
            Param::TextArray(value) => query.bind(value),
            Param::Uuid(value) => query.bind(value),
            Param::UuidArray(value) => query.bind(value),
            Param::Timestamp(value) => query.bind(value),
            Param::Int(value) => query.bind(value),
        }
    }
}

/// `$n` 占位符分配器 + 参数列表。
#[derive(Debug, Default)]
pub(crate) struct QueryBuilder {
    pub(crate) args: Vec<Param>,
}

impl QueryBuilder {
    pub(crate) fn push(&mut self, param: Param) -> String {
        self.args.push(param);
        format!("${}", self.args.len())
    }

    pub(crate) fn text(&mut self, value: impl Into<String>) -> String {
        self.push(Param::Text(value.into()))
    }

    pub(crate) fn uuid(&mut self, value: Uuid) -> String {
        self.push(Param::Uuid(value))
    }

    pub(crate) fn text_array(&mut self, value: Vec<String>) -> String {
        self.push(Param::TextArray(value))
    }

    pub(crate) fn uuid_array(&mut self, value: Vec<Uuid>) -> String {
        self.push(Param::UuidArray(value))
    }

    pub(crate) fn timestamp(&mut self, value: DateTime<Utc>) -> String {
        self.push(Param::Timestamp(value))
    }
}

pub(crate) fn query_as_with<'q, O>(
    sql: &'q str,
    args: &[Param],
) -> QueryAs<'q, Postgres, O, PgArguments>
where
    O: for<'r> FromRow<'r, PgRow> + Send + Unpin,
{
    let mut query = sqlx::query_as::<Postgres, O>(sql);
    for param in args {
        query = param.clone().bind(query);
    }
    query
}

pub(crate) fn count_with<'q>(
    sql: &'q str,
    args: &[Param],
) -> QueryScalar<'q, Postgres, i64, PgArguments> {
    let mut query = sqlx::query_scalar::<Postgres, i64>(sql);
    for param in args {
        query = param.clone().bind_scalar(query);
    }
    query
}

/// `ISSUE_COLUMNS` 加 `i.` 前缀（`SELECT ... FROM page i` 需要限定列）。
pub(crate) fn prefixed_issue_columns() -> String {
    ISSUE_COLUMNS
        .split(',')
        .map(|column| format!("i.{}", column.trim()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// 已解析的排序（表达式 + 方向 + cursor 语义）。
#[derive(Debug, Clone)]
pub(crate) struct ResolvedSort {
    pub(crate) expression: String,
    pub(crate) direction: TableSortDirection,
    pub(crate) cast: &'static str,
    pub(crate) nulls_last: bool,
    pub(crate) id_only_tie: bool,
}

impl ResolvedSort {
    pub(crate) fn order_by(&self) -> String {
        let mut order = format!("{} {}", self.expression, self.direction.sql());
        if self.nulls_last {
            order.push_str(" NULLS LAST");
        }
        if self.id_only_tie {
            order.push_str(", i.id DESC");
        } else {
            order.push_str(", i.created_at DESC, i.id DESC");
        }
        order
    }

    /// keyset 分页谓词（上游 `cursorPredicate`，逐分支镜像）。
    pub(crate) fn cursor_predicate(
        &self,
        builder: &mut QueryBuilder,
        cursor: &TableCursor,
    ) -> String {
        let id_ref = builder.uuid(cursor.row_id.0);
        let mut tie = format!("i.id < {id_ref}::uuid");
        if !self.id_only_tie {
            let created_ref = builder.timestamp(cursor.row_created_at);
            tie = format!(
                "(i.created_at < {created_ref}::timestamptz \
                 OR (i.created_at = {created_ref}::timestamptz AND i.id < {id_ref}::uuid))"
            );
        }
        if cursor.sort_is_null {
            return format!("({} IS NULL AND {})", self.expression, tie);
        }
        let Some(sort_value) = cursor.sort_value.as_deref() else {
            // HTTP 层已保证 sort_is_null / sort_value 二者必居其一。
            return "FALSE".to_string();
        };
        let value_ref = builder.text(sort_value);
        let value_expr = format!("{value_ref}::{}", self.cast);
        let comparison = if self.direction == TableSortDirection::Desc {
            "<"
        } else {
            ">"
        };
        let mut predicate = format!(
            "({expr} {comparison} {value_expr} OR ({expr} = {value_expr} AND {tie}))",
            expr = self.expression
        );
        if self.expression == "i.position" {
            // 上游注释：混合方向的 keyset 谓词本身不可索引，这个冗余下界让
            // PostgreSQL 从 cursor 处开始扫 position 索引。
            predicate = format!("({} >= {} AND {})", self.expression, value_expr, predicate);
        }
        if self.nulls_last {
            predicate = format!("({} IS NULL OR {})", self.expression, predicate);
        }
        predicate
    }
}

// ---------------------------------------------------------------------------
// WHERE 编译
// ---------------------------------------------------------------------------

/// 作用域里的 `assignee_types` 收窄（workspace / project 两种 scope 共用）。
pub(crate) fn push_assignee_types(
    builder: &mut QueryBuilder,
    parts: &mut Vec<String>,
    assignee_types: &[String],
) {
    if !assignee_types.is_empty() {
        let mut types = assignee_types.to_vec();
        types.sort();
        types.dedup();
        parts.push(format!(
            "i.assignee_type = ANY({}::text[])",
            builder.text_array(types)
        ));
    }
}

/// 编译 `i.workspace_id = $1 AND ...`（$1 固定是 workspace，分组/排序的子查询硬编码引用 $1）。
#[allow(clippy::too_many_lines)]
pub(crate) fn compile_where(
    builder: &mut QueryBuilder,
    workspace_id: Id,
    filter: &TableFilter,
) -> String {
    let mut parts = vec![format!("i.workspace_id = {}", builder.uuid(workspace_id.0))];
    match &filter.scope {
        TableScope::Workspace { assignee_types } => {
            push_assignee_types(builder, &mut parts, assignee_types);
        }
        TableScope::Project {
            project_id,
            assignee_types,
        } => {
            parts.push(format!(
                "i.project_id = {}::uuid",
                builder.uuid(project_id.0)
            ));
            push_assignee_types(builder, &mut parts, assignee_types);
        }
        TableScope::Assignee(actor) => {
            let kind_ref = builder.text(actor.kind.clone());
            let id_ref = builder.text(actor.id.to_string());
            parts.push(format!(
                "(i.assignee_type = {kind_ref}::text AND i.assignee_id = {id_ref}::text)"
            ));
        }
        TableScope::Creator(actor) => {
            let kind_ref = builder.text(actor.kind.clone());
            let id_ref = builder.text(actor.id.to_string());
            parts.push(format!(
                "(i.creator_type = {kind_ref}::text AND i.creator_id = {id_ref}::text)"
            ));
        }
        TableScope::My { actor, relation } => {
            let assigned = my_assigned_predicate(builder, actor);
            match relation {
                TableMyRelation::Assigned => parts.push(assigned),
                TableMyRelation::Created => parts.push(my_created_predicate(builder, actor)),
                TableMyRelation::Involved => parts.push(my_involved_predicate(builder, actor)),
                TableMyRelation::Any => {
                    let created = my_created_predicate(builder, actor);
                    let involved = my_involved_predicate(builder, actor);
                    parts.push(format!("({assigned} OR {created} OR {involved})"));
                }
            }
        }
    }

    if !filter.statuses.is_empty() {
        let mut statuses = filter.statuses.clone();
        statuses.sort();
        statuses.dedup();
        parts.push(format!(
            "i.status = ANY({}::text[])",
            builder.text_array(statuses)
        ));
    }
    if !filter.priorities.is_empty() {
        let mut priorities = filter.priorities.clone();
        priorities.sort();
        priorities.dedup();
        parts.push(format!(
            "i.priority = ANY({}::text[])",
            builder.text_array(priorities)
        ));
    }

    if filter.assignees.is_some() || filter.include_no_assignee {
        let mut ors: Vec<String> = Vec::new();
        for actor in filter.assignees.iter().flatten() {
            let kind_ref = builder.text(actor.kind.clone());
            let id_ref = builder.text(actor.id.to_string());
            ors.push(format!(
                "(i.assignee_type = {kind_ref}::text AND i.assignee_id = {id_ref}::text)"
            ));
        }
        if filter.include_no_assignee {
            ors.push("(i.assignee_type IS NULL AND i.assignee_id IS NULL)".to_string());
        }
        // 显式空数组 = “匹配 0 行”（上游同语义，让客户端能表达“空选中集”）。
        parts.push(if ors.is_empty() {
            "FALSE".to_string()
        } else {
            format!("({})", ors.join(" OR "))
        });
    }

    if !filter.creators.is_empty() {
        let ors: Vec<String> = filter
            .creators
            .iter()
            .map(|actor| {
                let kind_ref = builder.text(actor.kind.clone());
                let id_ref = builder.text(actor.id.to_string());
                format!("(i.creator_type = {kind_ref}::text AND i.creator_id = {id_ref}::text)")
            })
            .collect();
        parts.push(format!("({})", ors.join(" OR ")));
    }

    if !filter.project_ids.is_empty() || filter.include_no_project {
        let mut ors: Vec<String> = Vec::new();
        if !filter.project_ids.is_empty() {
            let ids: Vec<Uuid> = filter.project_ids.iter().map(|id| id.0).collect();
            ors.push(format!(
                "i.project_id = ANY({}::uuid[])",
                builder.uuid_array(ids)
            ));
        }
        if filter.include_no_project {
            ors.push("i.project_id IS NULL".to_string());
        }
        parts.push(format!("({})", ors.join(" OR ")));
    }

    if let Some(date) = &filter.date {
        let column = date.field.column();
        let start_ref = builder.timestamp(date.start);
        let end_ref = builder.timestamp(date.end);
        parts.push(format!(
            "{column} >= {start_ref}::timestamptz AND {column} < {end_ref}::timestamptz"
        ));
    }

    if filter.include_sub_issues == Some(false) {
        parts.push("i.parent_issue_id IS NULL".to_string());
    }

    parts.extend(search_predicates(builder, &filter.search));
    parts.join(" AND ")
}

pub(crate) fn my_assigned_predicate(builder: &mut QueryBuilder, actor: &TableActor) -> String {
    let kind_ref = builder.text(actor.kind.clone());
    let id_ref = builder.text(actor.id.to_string());
    format!("(i.assignee_type = {kind_ref}::text AND i.assignee_id = {id_ref}::text)")
}

pub(crate) fn my_created_predicate(builder: &mut QueryBuilder, actor: &TableActor) -> String {
    let kind_ref = builder.text(actor.kind.clone());
    let id_ref = builder.text(actor.id.to_string());
    format!("(i.creator_type = {kind_ref}::text AND i.creator_id = {id_ref}::text)")
}

/// 上游 `appendIssueTableInvolvedPredicate`：把本人（`user`）拥有的 agent 及其带队的 squad
/// 也算作“我参与的”。本仓没有 `squad_member` 表，所以只保留 agent 归属与 squad leader 两条支路。
pub(crate) fn my_involved_predicate(builder: &mut QueryBuilder, actor: &TableActor) -> String {
    let owner_ref = builder.uuid(actor.id);
    format!(
        "((i.assignee_type = 'agent' AND i.assignee_id IN (\
           SELECT a.id::text FROM agent a \
            WHERE a.workspace_id = $1 AND a.owner_id = {owner_ref}::uuid)) \
         OR (i.assignee_type = 'squad' AND i.assignee_id IN (\
           SELECT s.id::text FROM squad s JOIN agent a ON a.id = s.leader_agent_id \
            WHERE s.workspace_id = $1 AND a.workspace_id = $1 AND a.owner_id = {owner_ref}::uuid)))"
    )
}

/// 上游 `appendIssueTableSearchFilter`：所有词都命中 title，或命中 issue number。
pub(crate) fn search_predicates(builder: &mut QueryBuilder, raw: &str) -> Vec<String> {
    let query = raw.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let mut ors: Vec<String> = Vec::new();
    let lower = query.to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().filter(|w| !w.is_empty()).collect();
    if !words.is_empty() {
        let matches: Vec<String> = words
            .iter()
            .map(|word| {
                let pattern = format!("%{}%", escape_like(word));
                format!("LOWER(i.title) LIKE {}", builder.text(pattern))
            })
            .collect();
        ors.push(format!("({})", matches.join(" AND ")));
    }
    if let Some(number) = parse_query_number(query) {
        ors.push(format!("i.number = {}", builder.push(Param::Int(number))));
    }
    if ors.is_empty() {
        return Vec::new();
    }
    vec![format!("({})", ors.join(" OR "))]
}

pub(crate) fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// 上游 `parseQueryNumber`：`ABC-45` 或裸数字。
pub(crate) fn parse_query_number(query: &str) -> Option<i64> {
    let trimmed = query.trim();
    if let Some((prefix, digits)) = trimmed.split_once('-') {
        if !prefix.is_empty()
            && prefix.chars().all(|c| c.is_ascii_alphabetic())
            && !digits.is_empty()
            && digits.chars().all(|c| c.is_ascii_digit())
        {
            return digits.parse::<i64>().ok().filter(|n| *n > 0);
        }
    }
    trimmed.parse::<i64>().ok().filter(|n| *n > 0)
}

// ---------------------------------------------------------------------------
// 分组表达式 / 排序解析
// ---------------------------------------------------------------------------

/// 分组的 `group_value` 表达式（上游 `resolvedIssueTableGroup.expression`）。
pub(crate) fn group_expression(group: TableGroupSpec) -> String {
    match group.kind {
        TableGroupKind::None => "''::text".to_string(),
        TableGroupKind::Status => "i.status".to_string(),
        TableGroupKind::Priority => "i.priority".to_string(),
        TableGroupKind::Assignee => format!(
            "CASE WHEN i.assignee_type IS NULL OR i.assignee_id IS NULL \
             THEN '{GROUP_VALUE_UNASSIGNED}' ELSE i.assignee_type || ':' || i.assignee_id END"
        ),
        TableGroupKind::Project => {
            format!("COALESCE(i.project_id::text, '{GROUP_VALUE_NO_PROJECT}')")
        }
    }
}

/// 分组的次要排序键（上游 `sortExpression`：默认就是 `group_value`）。
pub(crate) fn group_sort_expression(group: TableGroupSpec) -> String {
    match group.kind {
        // 上游用 `p.title`，本仓 `project` 表列名是 `name`。
        TableGroupKind::Project => format!(
            "CASE WHEN group_value = '{GROUP_VALUE_NO_PROJECT}' THEN '' ELSE LOWER(COALESCE(\
               (SELECT p.name FROM project p WHERE p.workspace_id = $1 AND p.id = group_value::uuid), '')) END"
        ),
        TableGroupKind::Assignee => "LOWER(COALESCE(CASE split_part(group_value, ':', 1) \
             WHEN 'user' THEN (SELECT u.name FROM \"user\" u WHERE u.id = split_part(group_value, ':', 2)::uuid) \
             WHEN 'agent' THEN (SELECT a.name FROM agent a WHERE a.workspace_id = $1 AND a.id = split_part(group_value, ':', 2)::uuid) \
             WHEN 'squad' THEN (SELECT s.name FROM squad s WHERE s.workspace_id = $1 AND s.id = split_part(group_value, ':', 2)::uuid) \
             WHEN 'autopilot' THEN (SELECT ap.name FROM autopilot ap WHERE ap.workspace_id = $1 AND ap.id = split_part(group_value, ':', 2)::uuid) \
             END, ''))"
            .to_string(),
        _ => "group_value".to_string(),
    }
}

/// 已解析分组（表达式 + 顺序数组）。
pub(crate) struct ResolvedGroup {
    pub(crate) kind: TableGroupKind,
    pub(crate) expression: String,
    pub(crate) sort_expression: String,
    pub(crate) order_expression: String,
    /// `status` / `priority` 的固定顺序（用于 `array_position` 与 `include_empty`）。
    pub(crate) order_values: Vec<String>,
    pub(crate) include_empty: bool,
}

pub(crate) fn resolve_group(
    group: TableGroupSpec,
    status_order: &[String],
    builder: &mut QueryBuilder,
) -> ResolvedGroup {
    let expression = group_expression(group);
    let sort_expression = group_sort_expression(group);
    let order_values: Vec<String> = match group.kind {
        TableGroupKind::Status => status_order.to_vec(),
        TableGroupKind::Priority => BUILTIN_PRIORITIES
            .iter()
            .map(|p| (*p).to_string())
            .collect(),
        _ => Vec::new(),
    };
    let order_expression = match group.kind {
        TableGroupKind::Status | TableGroupKind::Priority => {
            let order_ref = builder.text_array(order_values.clone());
            format!("COALESCE(array_position({order_ref}::text[], group_value), 100000)")
        }
        TableGroupKind::Assignee => "CASE split_part(group_value, ':', 1) WHEN 'user' THEN 0 \
             WHEN 'agent' THEN 1 WHEN 'squad' THEN 2 ELSE 3 END"
            .to_string(),
        TableGroupKind::Project => {
            format!("CASE WHEN group_value = '{GROUP_VALUE_NO_PROJECT}' THEN 0 ELSE 1 END")
        }
        TableGroupKind::None => "0".to_string(),
    };
    ResolvedGroup {
        kind: group.kind,
        expression,
        sort_expression,
        order_expression,
        order_values,
        include_empty: group.include_empty
            && matches!(
                group.kind,
                TableGroupKind::Status | TableGroupKind::Priority
            ),
    }
}

/// `group_key` → 谓词（上游 `predicate`）。
pub(crate) fn group_predicate(key: &TableGroupKey, builder: &mut QueryBuilder) -> String {
    match key {
        TableGroupKey::None => "TRUE".to_string(),
        TableGroupKey::Status(status) => {
            let status_ref = builder.text(status.clone());
            format!("i.status = {status_ref}::text")
        }
        TableGroupKey::Priority(priority) => {
            let priority_ref = builder.text(priority.clone());
            format!("i.priority = {priority_ref}::text")
        }
        TableGroupKey::Assignee(None) => {
            "i.assignee_type IS NULL AND i.assignee_id IS NULL".to_string()
        }
        TableGroupKey::Assignee(Some(actor)) => {
            let kind_ref = builder.text(actor.kind.clone());
            let id_ref = builder.text(actor.id.to_string());
            format!("i.assignee_type = {kind_ref}::text AND i.assignee_id = {id_ref}::text")
        }
        TableGroupKey::Project(None) => "i.project_id IS NULL".to_string(),
        TableGroupKey::Project(Some(project_id)) => {
            let project_ref = builder.uuid(project_id.0);
            format!("i.project_id = {project_ref}::uuid")
        }
    }
}

/// `group_value` → wire group key（上游 `descriptor` 的 key 部分）。
pub fn group_key_of(kind: TableGroupKind, group_value: &str) -> String {
    match kind {
        TableGroupKind::None => String::new(),
        TableGroupKind::Status => format!("status:{group_value}"),
        TableGroupKind::Priority => format!("priority:{group_value}"),
        TableGroupKind::Assignee if group_value == GROUP_VALUE_UNASSIGNED => {
            "assignee:unassigned".to_string()
        }
        TableGroupKind::Assignee => format!("assignee:{group_value}"),
        TableGroupKind::Project if group_value == GROUP_VALUE_NO_PROJECT => {
            "project:none".to_string()
        }
        TableGroupKind::Project => format!("project:{group_value}"),
    }
}

/// 解析排序（上游 `issueTableOrderBy`）。`position` 的方向被上游强制为 asc，这里一并镜像。
pub(crate) fn resolve_sort(
    order: TableOrder,
    builder: &mut QueryBuilder,
    status_order: &[String],
) -> ResolvedSort {
    let expression = match order.field.plain_column() {
        Some(column) => column.to_string(),
        None => match order.field {
            TableSortField::Status => {
                let order_ref = builder.text_array(status_order.to_vec());
                format!("COALESCE(array_position({order_ref}::text[], i.status), 100000)")
            }
            TableSortField::Priority => "CASE i.priority WHEN 'urgent' THEN 0 WHEN 'high' THEN 1 \
                 WHEN 'medium' THEN 2 WHEN 'low' THEN 3 ELSE 4 END"
                .to_string(),
            _ => unreachable!("status/priority are the only non-column sort fields"),
        },
    };
    let direction = if order.field == TableSortField::Position {
        TableSortDirection::Asc
    } else {
        order
            .direction
            .unwrap_or_else(|| order.field.default_direction())
    };
    ResolvedSort {
        expression,
        direction,
        cast: order.field.cast(),
        nulls_last: order.field.nulls_last(),
        id_only_tie: order.field.id_only_tie(),
    }
}

pub(crate) fn encode_group_cursor(
    group_value: &str,
    order: i64,
    sort_key: &str,
) -> TableGroupCursor {
    TableGroupCursor {
        order,
        sort_key: sort_key.to_string(),
        value: group_value.to_string(),
    }
}
