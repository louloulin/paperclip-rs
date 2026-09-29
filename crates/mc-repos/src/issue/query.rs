use mc_core::Id;
use uuid::Uuid;

use super::{
    map_sqlx_err, ChildProgressRow, GroupedCountRow, IssueFilter, IssueGroupField, IssueOrderBy,
    IssueRepo, IssueRow, Result, ISSUE_COLUMNS, LIST_DEFAULT_LIMIT, LIST_MAX_LIMIT, LIST_WHERE,
};

impl IssueRepo {
    /// 过滤列表。
    pub async fn list(&self, filter: &IssueFilter) -> Result<Vec<IssueRow>> {
        let limit = filter
            .limit
            .unwrap_or(LIST_DEFAULT_LIMIT)
            .clamp(1, LIST_MAX_LIMIT);
        let offset = filter.offset.unwrap_or(0).max(0);
        let sql = format!(
            "SELECT {ISSUE_COLUMNS} FROM issue WHERE {LIST_WHERE} ORDER BY {} LIMIT $14 OFFSET $15",
            filter.order.as_sql()
        );
        sqlx::query_as::<_, IssueRow>(&sql)
            .bind(filter.workspace_id.0)
            .bind(filter.statuses.clone())
            .bind(filter.priorities.clone())
            .bind(filter.assignee_type.clone())
            .bind(filter.assignee_ids.clone())
            .bind(filter.creator_id.clone())
            .bind(filter.parent_issue_id.map(Id::as_uuid))
            .bind(filter.project_id.map(Id::as_uuid))
            .bind(filter.stage)
            .bind(filter.q.clone())
            .bind(filter.include_closed)
            .bind(filter.terminal_statuses.clone())
            .bind(filter.only_parentless)
            .bind(limit)
            .bind(offset)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 过滤列表 + 总数（同一 WHERE，两条查询）。
    pub async fn list_with_total(&self, filter: &IssueFilter) -> Result<(Vec<IssueRow>, i64)> {
        let rows = self.list(filter).await?;
        let total: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*)::bigint FROM issue WHERE {LIST_WHERE}"
        ))
        .bind(filter.workspace_id.0)
        .bind(filter.statuses.clone())
        .bind(filter.priorities.clone())
        .bind(filter.assignee_type.clone())
        .bind(filter.assignee_ids.clone())
        .bind(filter.creator_id.clone())
        .bind(filter.parent_issue_id.map(Id::as_uuid))
        .bind(filter.project_id.map(Id::as_uuid))
        .bind(filter.stage)
        .bind(filter.q.clone())
        .bind(filter.include_closed)
        .bind(filter.terminal_statuses.clone())
        .bind(filter.only_parentless)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok((rows, total))
    }

    /// 某个父 issue 的直接子 issue。
    pub async fn children_of(&self, workspace_id: Id, parent_id: Id) -> Result<Vec<IssueRow>> {
        let mut filter = IssueFilter::new(workspace_id);
        filter.parent_issue_id = Some(parent_id);
        filter.order = IssueOrderBy::PositionAsc;
        filter.include_closed = true;
        self.list(&filter).await
    }

    /// 多个父 issue 的子 issue（`GET /api/issues/children?parent_ids=`）。
    ///
    /// workspace 隔离在 SQL 层完成：不属于本 workspace 的 `parent_id` 自然返回 0 行。
    pub async fn children_of_parents(
        &self,
        workspace_id: Id,
        parent_ids: &[Uuid],
    ) -> Result<Vec<IssueRow>> {
        if parent_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, IssueRow>(&format!(
            "SELECT {ISSUE_COLUMNS} FROM issue \
             WHERE workspace_id = $1 AND parent_issue_id = ANY($2::uuid[]) \
             ORDER BY parent_issue_id, position ASC, number ASC"
        ))
        .bind(workspace_id.0)
        .bind(parent_ids)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 每个父 issue 的子 issue 进度（total / done）。`terminal_statuses` 决定什么算 done。
    pub async fn child_progress(
        &self,
        workspace_id: Id,
        terminal_statuses: &[String],
    ) -> Result<Vec<ChildProgressRow>> {
        sqlx::query_as::<_, ChildProgressRow>(
            "SELECT parent_issue_id, \
                    COUNT(*)::bigint AS total, \
                    COUNT(*) FILTER (WHERE status = ANY($2::text[]))::bigint AS done \
             FROM issue \
             WHERE workspace_id = $1 AND parent_issue_id IS NOT NULL \
             GROUP BY parent_issue_id \
             ORDER BY parent_issue_id",
        )
        .bind(workspace_id.0)
        .bind(terminal_statuses)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 分组计数（`/api/issues/grouped`、`table/facets`）。
    ///
    /// 只返回计数（key/total/done）；上游还会回传每组 issue 列表，M2-A 不做——见 docs/11 §5。
    pub async fn grouped_counts(
        &self,
        filter: &IssueFilter,
        group_by: IssueGroupField,
    ) -> Result<Vec<GroupedCountRow>> {
        let sql = format!(
            "SELECT {}::text AS key, COUNT(*)::bigint AS total, \
                    COUNT(*) FILTER (WHERE status = ANY($12::text[]))::bigint AS done \
             FROM issue WHERE {LIST_WHERE} \
             GROUP BY {} ORDER BY total DESC",
            group_by.group_expr(),
            group_by.group_expr()
        );
        sqlx::query_as::<_, GroupedCountRow>(&sql)
            .bind(filter.workspace_id.0)
            .bind(filter.statuses.clone())
            .bind(filter.priorities.clone())
            .bind(filter.assignee_type.clone())
            .bind(filter.assignee_ids.clone())
            .bind(filter.creator_id.clone())
            .bind(filter.parent_issue_id.map(Id::as_uuid))
            .bind(filter.project_id.map(Id::as_uuid))
            .bind(filter.stage)
            .bind(filter.q.clone())
            .bind(filter.include_closed)
            .bind(filter.terminal_statuses.clone())
            .bind(filter.only_parentless)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 分组字段名（用于响应回显）。
    pub fn group_field_name(field: IssueGroupField) -> &'static str {
        field.as_str()
    }
}
