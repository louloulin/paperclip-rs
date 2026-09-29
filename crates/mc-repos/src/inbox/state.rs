//! `InboxRepo` 的状态迁移面（全部幂等）：已读/未读、归档/取消归档、批量归档。

use super::row::InboxItemRow;
use super::InboxRepo;
use crate::workspace::map_sqlx_err;
use crate::{RepoError, Result};
use mc_core::Id;
use uuid::Uuid;

/// 内置终结状态 key（上游 `issuestatus` 的 `done` / `cancelled`）。
pub const BUILTIN_TERMINAL_STATUS_KEYS: &[&str] = &["done", "cancelled"];

impl InboxRepo {
    // -----------------------------------------------------------------------
    // 状态迁移（全部幂等）
    // -----------------------------------------------------------------------

    /// 标记已读（item 级，幂等）：已读行保持不变，`read_at` 不会被刷新。
    pub async fn mark_read(&self, id: Id) -> Result<InboxItemRow> {
        let set_clause = "read_at = COALESCE(read_at, now()), read = TRUE";
        self.retouch(set_clause, id.as_uuid()).await
    }

    /// 标记未读（item 级，幂等）。
    ///
    /// 刻意只翻**这一行**：UI 渲染的是组内最新一条，组状态就是这行的状态。
    /// 翻整组会把用户已经处理完的旧兄弟节点变回未读。
    pub async fn mark_unread(&self, id: Id) -> Result<InboxItemRow> {
        self.retouch("read_at = NULL, read = FALSE", id.as_uuid())
            .await
    }

    /// 归档（**issue 级**，幂等）：同 issue 的全部兄弟行一起归档。
    ///
    /// 无 issue 的通知只归档自己。返回目标行归档后的最新状态。
    pub async fn archive(&self, id: Id) -> Result<InboxItemRow> {
        self.archive_scope(id, true).await
    }

    /// 取消归档（issue 级，幂等）。刻意不碰 `read_at`：还原时保持归档前的读写状态。
    pub async fn unarchive(&self, id: Id) -> Result<InboxItemRow> {
        self.archive_scope(id, false).await
    }

    async fn retouch(&self, set_clause: &str, id: Uuid) -> Result<InboxItemRow> {
        // 同 `create`：`RETURNING` 不能带 JOIN 投影，故写完再读回完整行。
        let sql = format!("UPDATE inbox_item SET {set_clause} WHERE id = $1");
        let res = sqlx::query(&sql)
            .bind(id)
            .execute(self.pool())
            .await
            .map_err(map_sqlx_err)?;
        if res.rows_affected() == 0 {
            return Err(RepoError::NotFound);
        }
        self.get(Id::from(id)).await
    }

    async fn archive_scope(&self, id: Id, archive: bool) -> Result<InboxItemRow> {
        let item = self.get(id).await?;
        let set_clause = if archive {
            "archived_at = COALESCE(archived_at, now()), archived = TRUE"
        } else {
            "archived_at = NULL, archived = FALSE"
        };
        // 幂等条件：归档只动未归档行，取消归档只动已归档行。
        let guard = if archive {
            "archived_at IS NULL"
        } else {
            "archived_at IS NOT NULL"
        };
        if let Some(issue_id) = item.issue_id {
            let sql = format!(
                "UPDATE inbox_item SET {set_clause} \
                 WHERE workspace_id = $1 AND recipient_id = $2 AND issue_id = $3 AND {guard}"
            );
            sqlx::query(&sql)
                .bind(item.workspace_id)
                .bind(item.user_id)
                .bind(issue_id)
                .execute(self.pool())
                .await
                .map_err(map_sqlx_err)?;
        } else {
            let sql = format!("UPDATE inbox_item SET {set_clause} WHERE id = $1 AND {guard}");
            sqlx::query(&sql)
                .bind(id.as_uuid())
                .execute(self.pool())
                .await
                .map_err(map_sqlx_err)?;
        }
        // 目标行必然已被上一句覆盖（同组 / 自己），重读拿归档后的状态。
        self.get(id).await
    }

    /// 全部标记已读（组语义上等价于"全部行已读"，因为读状态的粒度是行）。
    pub async fn mark_all_read(&self, workspace_id: Id, user_id: Id) -> Result<u64> {
        let res = sqlx::query(
            "UPDATE inbox_item SET read_at = COALESCE(read_at, now()), read = TRUE \
             WHERE workspace_id = $1 AND recipient_id = $2 \
               AND archived_at IS NULL AND read_at IS NULL",
        )
        .bind(workspace_id.as_uuid())
        .bind(user_id.as_uuid())
        .execute(self.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(res.rows_affected())
    }

    /// 归档全部未归档通知（不分读写状态）。
    pub async fn archive_all(&self, workspace_id: Id, user_id: Id) -> Result<u64> {
        let res = sqlx::query(
            "UPDATE inbox_item SET archived_at = COALESCE(archived_at, now()), archived = TRUE \
             WHERE workspace_id = $1 AND recipient_id = $2 AND archived_at IS NULL",
        )
        .bind(workspace_id.as_uuid())
        .bind(user_id.as_uuid())
        .execute(self.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(res.rows_affected())
    }

    /// 归档"已读"的组：组内最新一条已读 → 整组归档；未读组一行不动。
    ///
    /// 直接更新"读过的行"是错的：最新行归档后，旧兄弟节点会重新冒出来，
    /// 也可能把未读组里的旧已读行归档掉。
    pub async fn archive_all_read(&self, workspace_id: Id, user_id: Id) -> Result<u64> {
        let res = sqlx::query(
            "WITH newest_groups AS ( \
                 SELECT DISTINCT ON (COALESCE(i.issue_id, i.id)) \
                        COALESCE(i.issue_id, i.id) AS group_id, (i.read_at IS NOT NULL) AS is_read \
                 FROM inbox_item i \
                 WHERE i.workspace_id = $1 AND i.recipient_id = $2 AND i.archived_at IS NULL \
                 ORDER BY COALESCE(i.issue_id, i.id), i.created_at DESC, i.id DESC \
             ), read_groups AS ( \
                 SELECT group_id FROM newest_groups WHERE is_read \
             ) \
             UPDATE inbox_item i SET archived_at = COALESCE(i.archived_at, now()), archived = TRUE \
             FROM read_groups selected \
             WHERE i.workspace_id = $1 AND i.recipient_id = $2 AND i.archived_at IS NULL \
               AND COALESCE(i.issue_id, i.id) = selected.group_id",
        )
        .bind(workspace_id.as_uuid())
        .bind(user_id.as_uuid())
        .execute(self.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(res.rows_affected())
    }

    /// 归档"已完成"issue 的通知：issue 状态属于终结态（`done` / `cancelled`）。
    ///
    /// 终结态取值 = 本 workspace `issue_status.category = 'closed'` 的 key，并集
    /// 内置终结 key（本仓 `issue_status.category` 的 CHECK 只有 `open`/`closed`，
    /// 没有上游的 `done` 分类，故显式并上内置 key，见 `docs/13-M2-INBOX.md`）。
    pub async fn archive_completed(&self, workspace_id: Id, user_id: Id) -> Result<u64> {
        let res = sqlx::query(
            "WITH terminal AS ( \
                 SELECT key FROM issue_status \
                 WHERE workspace_id = $1 AND category = 'closed' \
                 UNION SELECT unnest($3::text[]) \
             ) \
             UPDATE inbox_item i SET archived_at = COALESCE(i.archived_at, now()), archived = TRUE \
             WHERE i.workspace_id = $1 AND i.recipient_id = $2 AND i.archived_at IS NULL \
               AND i.issue_id IN ( \
                   SELECT id FROM issue \
                   WHERE workspace_id = $1 AND status IN (SELECT key FROM terminal))",
        )
        .bind(workspace_id.as_uuid())
        .bind(user_id.as_uuid())
        .bind(BUILTIN_TERMINAL_STATUS_KEYS.to_vec())
        .execute(self.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(res.rows_affected())
    }
}
