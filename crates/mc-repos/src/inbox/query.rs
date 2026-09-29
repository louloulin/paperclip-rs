//! `InboxRepo` 的读取面：单条、主列表、归档列表/分页/facets、未读计数与汇总。

use super::input::{
    ArchivedCursor, ArchivedInboxFacets, ArchivedInboxFilter, ArchivedInboxPage, WorkspaceUnread,
};
use super::row::{InboxItemRow, ITEM_COLUMNS, ITEM_FROM, NEWEST_ARCHIVED_CTE};
use super::InboxRepo;
use crate::workspace::map_sqlx_err;
use crate::{RepoError, Result};
use mc_core::Id;
use uuid::Uuid;

impl InboxRepo {
    // -----------------------------------------------------------------------
    // 读
    // -----------------------------------------------------------------------

    /// 单条 item（不做归属校验；路由层用 `get_for_user`）。
    pub async fn get(&self, id: Id) -> Result<InboxItemRow> {
        let sql = format!("SELECT {ITEM_COLUMNS} {ITEM_FROM} WHERE i.id = $1");
        sqlx::query_as::<_, InboxItemRow>(&sql)
            .bind(id.as_uuid())
            .fetch_optional(self.pool())
            .await
            .map_err(map_sqlx_err)?
            .ok_or(RepoError::NotFound)
    }

    /// 单条 item，且必须属于 `(workspace_id, user_id)`；否则 `NotFound`。
    ///
    /// 上游 `loadInboxItemForUser` 的等价物：把"别人的通知"与"不存在的通知"都收敛
    /// 成 404，不泄漏资源存在性。
    pub async fn get_for_user(
        &self,
        id: Id,
        workspace_id: Id,
        user_id: Id,
    ) -> Result<InboxItemRow> {
        let sql = format!(
            "SELECT {ITEM_COLUMNS} {ITEM_FROM} \
             WHERE i.id = $1 AND i.workspace_id = $2 AND i.recipient_id = $3"
        );
        sqlx::query_as::<_, InboxItemRow>(&sql)
            .bind(id.as_uuid())
            .bind(workspace_id.as_uuid())
            .bind(user_id.as_uuid())
            .fetch_optional(self.pool())
            .await
            .map_err(map_sqlx_err)?
            .ok_or(RepoError::NotFound)
    }

    /// 主列表：该 workspace 下该用户**未归档**的通知，按 `created_at` 倒序分页。
    ///
    /// 上游 `GET /api/inbox` 不分页（返回全部活跃行）；本仓按 sub-issue 要求加
    /// `limit` / `offset`（路由默认 `limit=200`，见 `docs/13-M2-INBOX.md`）。
    pub async fn list(
        &self,
        workspace_id: Id,
        user_id: Id,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<InboxItemRow>> {
        let sql = format!(
            "SELECT {ITEM_COLUMNS} {ITEM_FROM} \
             WHERE i.workspace_id = $1 AND i.recipient_id = $2 AND i.archived_at IS NULL \
             ORDER BY i.created_at DESC, i.id DESC \
             LIMIT $3 OFFSET $4"
        );
        sqlx::query_as::<_, InboxItemRow>(&sql)
            .bind(workspace_id.as_uuid())
            .bind(user_id.as_uuid())
            .bind(limit)
            .bind(offset)
            .fetch_all(self.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 归档列表（不分页）：最多 `group_limit` 个 issue 组，每组只返回最新一行。
    ///
    /// 上游固定 200 组 + 额外补 comment anchor 行；本仓 schema 无 `details` 列，
    /// 故只有每组最新一行。
    pub async fn list_archived(
        &self,
        workspace_id: Id,
        user_id: Id,
        group_limit: i64,
    ) -> Result<Vec<InboxItemRow>> {
        let filter = ArchivedInboxFilter::default();
        let page = self
            .list_archived_page(workspace_id, user_id, &filter, None, group_limit)
            .await?;
        Ok(page.items)
    }

    /// 归档列表分页（`archived/page`）：先按组选出代表行，再套过滤 / 游标 / limit。
    ///
    /// 游标是 `(created_at, id)` 的行比较，`limit` 计的是**组**而不是原始行——
    /// 与上游一致（否则一个噪音 issue 能吃光整页）。
    pub async fn list_archived_page(
        &self,
        workspace_id: Id,
        user_id: Id,
        filter: &ArchivedInboxFilter,
        cursor: Option<&ArchivedCursor>,
        limit: i64,
    ) -> Result<ArchivedInboxPage> {
        let sql = format!(
            "{NEWEST_ARCHIVED_CTE}, selected AS ( \
                SELECT * FROM matched \
                WHERE status_match AND priority_match AND actor_match AND read_match \
                  AND ($9::uuid IS NULL OR issue_id = $9::uuid \
                       OR (issue_id IS NULL AND id = $9::uuid)) \
                  AND ($7::timestamptz IS NULL \
                       OR (created_at, id) < ($7::timestamptz, $8::uuid)) \
                ORDER BY created_at DESC, id DESC \
                LIMIT $10 \
             ) \
             SELECT {ITEM_COLUMNS} \
             FROM selected \
             JOIN inbox_item i ON i.id = selected.id \
             LEFT JOIN issue iss ON iss.id = i.issue_id AND iss.workspace_id = $1 \
             ORDER BY i.created_at DESC, i.id DESC"
        );
        let rows = sqlx::query_as::<_, InboxItemRow>(&sql)
            .bind(workspace_id.as_uuid())
            .bind(user_id.as_uuid())
            .bind(&filter.statuses)
            .bind(&filter.priorities)
            .bind(&filter.actors)
            .bind(filter.unread_only)
            .bind(cursor.map(|c| c.created_at))
            .bind(cursor.map(|c| c.id.as_uuid()))
            .bind(filter.group_id.map(Id::as_uuid))
            // 多取一行判断 has_more，返回前截断。
            .bind(limit.saturating_add(1))
            .fetch_all(self.pool())
            .await
            .map_err(map_sqlx_err)?;
        let has_more = rows.len() > usize::try_from(limit).unwrap_or(usize::MAX);
        let mut items = rows;
        if has_more {
            items.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        }
        Ok(ArchivedInboxPage { items, has_more })
    }

    /// 归档视图的 facet 计数（`archived/facets`）：一次查询返回
    /// `(dimension, key, count)` 三元组，由调用方分派到四个桶。
    ///
    /// 每个维度的计数都按**其他维度**过滤（facet 互斥计数的常规语义），
    /// 与上游 `ArchivedInboxFacets` 逐字对应。
    pub async fn archived_facets(
        &self,
        workspace_id: Id,
        user_id: Id,
        filter: &ArchivedInboxFilter,
    ) -> Result<ArchivedInboxFacets> {
        let sql = format!(
            "{NEWEST_ARCHIVED_CTE} \
             SELECT 'statuses'::text AS dimension, issue_status::text AS key, \
                    count(*) FILTER (WHERE priority_match AND actor_match AND read_match) AS count \
             FROM matched WHERE issue_status IS NOT NULL GROUP BY issue_status \
             UNION ALL \
             SELECT 'priorities'::text, issue_priority::text, \
                    count(*) FILTER (WHERE status_match AND actor_match AND read_match) \
             FROM matched WHERE issue_priority IS NOT NULL GROUP BY issue_priority \
             UNION ALL \
             SELECT 'actors'::text, actor::text, \
                    count(*) FILTER (WHERE status_match AND priority_match AND read_match) \
             FROM matched WHERE actor IS NOT NULL GROUP BY actor \
             UNION ALL \
             SELECT 'unread'::text, 'unread'::text, \
                    count(*) FILTER (WHERE status_match AND priority_match AND actor_match AND NOT is_read) \
             FROM matched"
        );
        let rows = sqlx::query_as::<_, (String, String, i64)>(&sql)
            .bind(workspace_id.as_uuid())
            .bind(user_id.as_uuid())
            .bind(&filter.statuses)
            .bind(&filter.priorities)
            .bind(&filter.actors)
            .bind(filter.unread_only)
            .fetch_all(self.pool())
            .await
            .map_err(map_sqlx_err)?;
        let mut facets = ArchivedInboxFacets::default();
        for (dimension, key, count) in rows {
            match dimension.as_str() {
                "statuses" => {
                    facets.statuses.insert(key, count);
                }
                "priorities" => {
                    facets.priorities.insert(key, count);
                }
                "actors" => {
                    facets.actors.insert(key, count);
                }
                // `unread` 维度的 key 恒为字面量 "unread"。
                _ => {
                    facets.unread_count = count;
                }
            }
        }
        Ok(facets)
    }

    /// 该 workspace 的**原始行**未读数（上游 `CountUnreadInbox`）。
    pub async fn unread_count(&self, workspace_id: Id, user_id: Id) -> Result<i64> {
        let row = sqlx::query_as::<_, (i64,)>(
            "SELECT count(*) FROM inbox_item \
             WHERE workspace_id = $1 AND recipient_id = $2 AND read_at IS NULL AND archived_at IS NULL",
        )
        .bind(workspace_id.as_uuid())
        .bind(user_id.as_uuid())
        .fetch_one(self.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(row.0)
    }

    /// 跨 workspace 的未读**组**数汇总（账户级；`unread-summary`）。
    ///
    /// - 按 issue 分组，组内最新一条未读则该组计 1（`DISTINCT ON`）；
    /// - `JOIN member` 把范围限定在用户当前仍加入的 workspace（已退出的 workspace
    ///   里残留的通知不能点亮侧边栏小圆点）。
    pub async fn unread_summary(&self, user_id: Id) -> Result<Vec<WorkspaceUnread>> {
        let rows = sqlx::query_as::<_, (Uuid, i64)>(
            "SELECT newest.workspace_id, count(*) \
             FROM ( \
                 SELECT DISTINCT ON (i.workspace_id, COALESCE(i.issue_id, i.id)) \
                        i.workspace_id, i.read_at \
                 FROM inbox_item i \
                 JOIN member m ON m.workspace_id = i.workspace_id AND m.user_id = i.recipient_id \
                 WHERE i.recipient_id = $1 AND i.archived_at IS NULL \
                 ORDER BY i.workspace_id, COALESCE(i.issue_id, i.id), i.created_at DESC, i.id DESC \
             ) newest \
             WHERE newest.read_at IS NULL \
             GROUP BY newest.workspace_id \
             ORDER BY newest.workspace_id",
        )
        .bind(user_id.as_uuid())
        .fetch_all(self.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows
            .into_iter()
            .map(|(workspace_id, count)| WorkspaceUnread {
                workspace_id: Id::from(workspace_id),
                count,
            })
            .collect())
    }
}
