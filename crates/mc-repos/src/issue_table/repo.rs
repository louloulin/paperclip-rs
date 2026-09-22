//! `IssueTableRepo`：issue table 三个查询入口的执行层。
//!
//! 拆自 `issue_table.rs`（R7：单文件 800 行上限）；规格在 `super`，SQL 编译在 `super::sql`。
//! 三个入口与上游一一对应：
//! - `table_groups` ← `ListIssueTableGroups`（`issue_table_group.go:892`）
//! - `table_rows` ← `ListIssueTableRows`（`issue_table_rows.go:253`）
//! - `table_facets` ← `ListIssueTableFacets`（`issue_table_facets.go`）

use sqlx::postgres::PgRow;
use sqlx::{FromRow, Row};

use mc_core::Id;
use mc_db::Db;

use crate::issue::IssueRow;
use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

use super::sql::{
    compile_where, count_with, encode_group_cursor, group_key_of, group_predicate,
    prefixed_issue_columns, query_as_with, resolve_group, resolve_sort, Param, QueryBuilder,
};
use super::{
    TableCursor, TableFacetCount, TableFacetKind, TableFacetValue, TableFacetsPage,
    TableFacetsQuery, TableGroupCount, TableGroupKind, TableGroupsPage, TableGroupsQuery, TableRow,
    TableRowsPage, TableRowsQuery, TableSortField, BUILTIN_STATUSES, FACET_VALUE_NONE,
};

// ---------------------------------------------------------------------------
// 内部行结构
// ---------------------------------------------------------------------------

impl<'r> FromRow<'r, PgRow> for TableRow {
    fn from_row(row: &'r PgRow) -> std::result::Result<Self, sqlx::Error> {
        Ok(Self {
            issue: IssueRow::from_row(row)?,
            direct_child_count: row.try_get("direct_child_count")?,
            sort_key: row.try_get("table_sort_key")?,
        })
    }
}

/// `issue_status` 目录行（`status_order`）。
#[derive(FromRow)]
struct StatusRow {
    key: String,
    category: String,
    position: f64,
}

/// `/groups` 内层查询行。
#[derive(FromRow)]
struct GroupRow {
    group_value: String,
    issue_count: i64,
    group_sort: String,
    group_order: i32,
    total: i64,
}

/// `/facets` 每个 facet 的查询行。
#[derive(FromRow)]
struct FacetRow {
    facet_value: String,
    facet_count: i64,
}

// ---------------------------------------------------------------------------
// Repo
// ---------------------------------------------------------------------------

/// issue table 查询仓储。
#[derive(Clone)]
pub struct IssueTableRepo {
    db: Db,
}

impl IssueTableRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// workspace 的 status 顺序（上游 `issueTableStatusOrder`）：目录按
    /// (分类, position, 内置优先, 内置序, key) 排序，内建缺失时补齐。
    pub async fn status_order(&self, workspace_id: Id) -> Result<Vec<String>> {
        // 本仓 0001 的 issue_status 没有 archived / is_system 列，无法过滤归档目录项；
        // 分类 CHECK 只有 open/closed（上游是 unstarted/started/done/closed 四档）。
        let rows: Vec<StatusRow> = sqlx::query_as(
            "SELECT key, category, position FROM issue_status WHERE workspace_id = $1",
        )
        .bind(workspace_id.0)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(merge_status_order(
            rows.into_iter()
                .map(|row| (row.key, row.category, row.position))
                .collect(),
        ))
    }

    /// `/api/issues/table/groups`：分组计数 + 总数 + 下一页 cursor。
    pub async fn table_groups(&self, query: &TableGroupsQuery) -> Result<TableGroupsPage> {
        let status_order = if query.group.kind == TableGroupKind::Status {
            self.status_order(query.workspace_id).await?
        } else {
            Vec::new()
        };
        let mut builder = QueryBuilder::default();
        let where_sql = compile_where(&mut builder, query.workspace_id, &query.filter);
        let group = resolve_group(query.group, &status_order, &mut builder);
        let limit_ref = builder.push(Param::Int(query.limit + 1));
        let cursor_predicate = match &query.cursor {
            None => "TRUE".to_string(),
            Some(cursor) => {
                let order_ref = builder.push(Param::Int(cursor.order));
                let sort_ref = builder.text(cursor.sort_key.clone());
                let key_ref = builder.text(cursor.value.clone());
                format!(
                    "(group_order > {order_ref}::int OR (group_order = {order_ref}::int \
                     AND (group_sort > {sort_ref}::text \
                     OR (group_sort = {sort_ref}::text AND group_value > {key_ref}::text))))"
                )
            }
        };
        let grouped_cte = if group.include_empty {
            let expected_ref = builder.text_array(group.order_values.clone());
            format!(
                "actual AS (SELECT {expr} AS group_value, COUNT(*)::bigint AS issue_count \
                   FROM issue i WHERE {where_sql} GROUP BY 1), \
                 expected AS (SELECT unnest({expected_ref}::text[]) AS group_value), \
                 grouped AS (SELECT e.group_value, COALESCE(a.issue_count, 0)::bigint AS issue_count \
                   FROM expected e LEFT JOIN actual a USING (group_value) \
                   UNION ALL \
                   SELECT a.group_value, a.issue_count FROM actual a \
                   WHERE NOT (a.group_value = ANY({expected_ref}::text[])))",
                expr = group.expression
            )
        } else {
            format!(
                "grouped AS (SELECT {expr} AS group_value, COUNT(*)::bigint AS issue_count \
                   FROM issue i WHERE {where_sql} GROUP BY 1)",
                expr = group.expression
            )
        };
        let sql = format!(
            "WITH {grouped_cte}, sorted AS (SELECT group_value, issue_count, \
               ({sort_expr})::text AS group_sort FROM grouped), \
             ranked AS (SELECT group_value, issue_count, group_sort, ({order_expr})::int AS group_order, \
               SUM(issue_count) OVER ()::bigint AS total FROM sorted) \
             SELECT group_value, issue_count, group_sort, group_order, total FROM ranked \
             WHERE {cursor_predicate} \
             ORDER BY group_order ASC, group_sort ASC, group_value ASC LIMIT {limit_ref}",
            sort_expr = group.sort_expression,
            order_expr = group.order_expression,
        );

        let rows: Vec<GroupRow> = query_as_with(&sql, &builder.args)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;

        let total = rows.first().map_or(0, |row| row.total);
        let limit = usize::try_from(query.limit).unwrap_or(0);
        let mut groups: Vec<TableGroupCount> = Vec::with_capacity(rows.len().min(limit + 1));
        let mut next = None;
        for (index, row) in rows.iter().enumerate() {
            if index == limit {
                let last = &rows[limit - 1];
                next = Some(encode_group_cursor(
                    &last.group_value,
                    i64::from(last.group_order),
                    &last.group_sort,
                ));
                break;
            }
            groups.push(TableGroupCount {
                key: group_key_of(group.kind, &row.group_value),
                count: row.issue_count,
            });
        }
        Ok(TableGroupsPage {
            total,
            groups,
            next,
        })
    }

    /// `/api/issues/table/rows`：一行 issue + 直接子 issue 数 + 下一页 cursor。
    #[allow(clippy::too_many_lines)] // 单个 SQL 的 CTE 拼装按上游分支平铺，拆函数更难看
    pub async fn table_rows(&self, query: &TableRowsQuery) -> Result<TableRowsPage> {
        let status_order = if query.order.field == TableSortField::Status {
            self.status_order(query.workspace_id).await?
        } else {
            Vec::new()
        };
        let mut builder = QueryBuilder::default();
        let where_sql = compile_where(&mut builder, query.workspace_id, &query.filter);
        // 注意：**不**调用 `resolve_group`——`/rows` 只用分组的谓词，分组表达式/顺序数组
        // 不进 SQL；多推一个未被引用的 `$n` 会让 Postgres 在 bind 阶段报参数数量不符。
        let group_predicate = group_predicate(&query.group_key, &mut builder);
        let order = resolve_sort(query.order, &mut builder, &status_order);
        let cursor_predicate = match &query.cursor {
            None => "TRUE".to_string(),
            Some(cursor) => order.cursor_predicate(&mut builder, cursor),
        };

        // 有层级请求时先物化“当前分支的成员集合”，子计数与父子判定都基于它；
        // 没有层级请求时 page 直接扫 issue，不做多余聚合（上游同）。
        let (cte_prefix, page_source, page_predicate) = if query.hierarchy {
            (
                format!(
                    "WITH membership AS NOT MATERIALIZED (SELECT i.* FROM issue i \
                     WHERE {where_sql} AND ({group_predicate})), "
                ),
                "membership",
                "TRUE".to_string(),
            )
        } else {
            (
                "WITH ".to_string(),
                "issue",
                format!("({where_sql}) AND ({group_predicate})"),
            )
        };
        // 层级分支谓词：有 parent_id 时看它的直接子；无 parent_id 时看“父不在当前分支里”的根。
        // 注意 `parent_id` 只有 `hierarchy.enabled=true` 才有意义（HTTP 层否则 400）。
        let page_clause = if query.hierarchy {
            let branch_predicate = match &query.parent_id {
                Some(parent_id) => {
                    let parent_ref = builder.uuid(parent_id.0);
                    format!(
                        "i.parent_issue_id = {parent_ref}::uuid \
                         AND EXISTS (SELECT 1 FROM membership parent WHERE parent.id = {parent_ref}::uuid)"
                    )
                }
                None => "i.parent_issue_id IS NULL OR \
                     (SELECT parent.id FROM membership parent WHERE parent.id = i.parent_issue_id) IS NULL"
                    .to_string(),
            };
            format!("({page_predicate}) AND ({branch_predicate})")
        } else {
            page_predicate
        };
        let limit_ref = builder.push(Param::Int(query.limit + 1));
        let child_count_expr = if query.hierarchy {
            "(SELECT COUNT(*)::bigint FROM membership child WHERE child.parent_issue_id = i.id)"
        } else {
            "0::bigint"
        };
        let order_by = order.order_by();
        let sql = format!(
            "{cte_prefix}page AS MATERIALIZED (SELECT i.*, ({sort_expr})::text AS table_sort_key \
               FROM {page_source} i WHERE ({page_clause}) AND {cursor_predicate} \
               ORDER BY {order_by} LIMIT {limit_ref}) \
             SELECT {columns}, {child_count_expr} AS direct_child_count, i.table_sort_key \
               FROM page i ORDER BY {order_by}",
            sort_expr = order.expression,
            columns = prefixed_issue_columns(),
        );
        let rows: Vec<TableRow> = query_as_with(&sql, &builder.args)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;

        // 只有“无 cursor + 未分组 + 无 parent”的页头才付全量 COUNT 的代价（上游同）。
        let total = if query.cursor.is_none()
            && query.group.kind == TableGroupKind::None
            && query.parent_id.is_none()
        {
            let mut counter = QueryBuilder::default();
            let count_where = compile_where(&mut counter, query.workspace_id, &query.filter);
            let count_sql = format!("SELECT COUNT(*)::bigint FROM issue i WHERE {count_where}");
            Some(
                count_with(&count_sql, &counter.args)
                    .fetch_one(self.db.pool())
                    .await
                    .map_err(map_sqlx_err)?,
            )
        } else {
            None
        };

        let limit = usize::try_from(query.limit).unwrap_or(0);
        let mut next = None;
        let mut page = rows;
        if page.len() > limit {
            page.truncate(limit);
            let last = page.last().expect("page has at least one row");
            next = Some(TableCursor {
                sort_value: last.sort_key.clone(),
                sort_is_null: last.sort_key.is_none(),
                row_created_at: last.issue.created_at,
                row_id: last.issue.id(),
            });
        }
        Ok(TableRowsPage {
            total,
            rows: page,
            next,
        })
    }

    /// `/api/issues/table/facets`：每个 facet 的取值计数（disjunctive）+ 可选总数。
    pub async fn table_facets(&self, query: &TableFacetsQuery) -> Result<TableFacetsPage> {
        let mut facets = Vec::with_capacity(query.facets.len());
        for facet in &query.facets {
            let mut builder = QueryBuilder::default();
            let filter = query.filter.without_facet(*facet);
            let where_sql = compile_where(&mut builder, query.workspace_id, &filter);
            let expression = match facet {
                TableFacetKind::Status => "i.status".to_string(),
                TableFacetKind::Priority => "i.priority".to_string(),
                TableFacetKind::Assignee => format!(
                    "CASE WHEN i.assignee_type IS NULL OR i.assignee_id IS NULL \
                     THEN '{FACET_VALUE_NONE}' ELSE i.assignee_type || ':' || i.assignee_id END"
                ),
                TableFacetKind::Creator => "i.creator_type || ':' || i.creator_id".to_string(),
                TableFacetKind::Project => {
                    format!("COALESCE(i.project_id::text, '{FACET_VALUE_NONE}')")
                }
            };
            let sql = format!(
                "SELECT {expression} AS facet_value, COUNT(*)::bigint AS facet_count \
                 FROM issue i WHERE {where_sql} GROUP BY 1 ORDER BY 1"
            );
            let rows: Vec<FacetRow> = query_as_with(&sql, &builder.args)
                .fetch_all(self.db.pool())
                .await
                .map_err(map_sqlx_err)?;
            facets.push(TableFacetCount {
                values: rows
                    .into_iter()
                    .map(|row| TableFacetValue {
                        key: row.facet_value,
                        count: row.facet_count,
                    })
                    .collect(),
            });
        }
        let total = if query.include_total {
            let mut builder = QueryBuilder::default();
            let where_sql = compile_where(&mut builder, query.workspace_id, &query.filter);
            let sql = format!("SELECT COUNT(*)::bigint FROM issue i WHERE {where_sql}");
            count_with(&sql, &builder.args)
                .fetch_one(self.db.pool())
                .await
                .map_err(map_sqlx_err)?
        } else {
            0
        };
        Ok(TableFacetsPage { total, facets })
    }
}

impl RepoWithDb for IssueTableRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

/// 上游 `issueTableStatusOrder`：目录（key, category, position）合并 7 个内建 key 后排序。
///
/// 排序键：分类（open < closed，上游是 unstarted < started < done < closed 四档，
/// 本仓 `issue_status.category` 的 CHECK 只有两档）→ position → 内置优先 → 内置序 → key。
fn merge_status_order(mut entries: Vec<(String, String, f64)>) -> Vec<String> {
    let builtin_rank = |key: &str| BUILTIN_STATUSES.iter().position(|(k, _)| *k == key);
    let seen: Vec<String> = entries.iter().map(|(key, _, _)| key.clone()).collect();
    for (key, category) in BUILTIN_STATUSES {
        if !seen.iter().any(|existing| existing == key) {
            entries.push((key.to_string(), category.to_string(), 0.0));
        }
    }
    entries.sort_by(|a, b| {
        let category_rank = |category: &str| match category {
            "closed" => 1,
            _ => 0,
        };
        category_rank(&a.1)
            .cmp(&category_rank(&b.1))
            .then(a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| {
                let a_builtin = builtin_rank(&a.0);
                let b_builtin = builtin_rank(&b.0);
                b_builtin.is_some().cmp(&a_builtin.is_some())
            })
            .then_with(|| {
                builtin_rank(&a.0)
                    .unwrap_or(usize::MAX)
                    .cmp(&builtin_rank(&b.0).unwrap_or(usize::MAX))
            })
            .then_with(|| a.0.cmp(&b.0))
    });
    entries.into_iter().map(|(key, _, _)| key).collect()
}
