//! `CommentRepo` 的 resolve / unresolve。
use super::row::CommentRow;
use super::CommentRepo;
use crate::workspace::map_sqlx_err;
use crate::{RepoError, Result};
use mc_core::id::Id;

impl CommentRepo {
    /// 解析（幂等：重复 resolve 不推进 `resolved_at`，也不 bump revision）。
    ///
    /// 已软删 → `NotFound`（tombstone 不能作为线程结论）。
    pub async fn resolve(&self, id: Id) -> Result<CommentRow> {
        self.set_resolved(id, true).await
    }

    /// 取消解析（幂等）。
    pub async fn unresolve(&self, id: Id) -> Result<CommentRow> {
        self.set_resolved(id, false).await
    }

    async fn set_resolved(&self, id: Id, resolved: bool) -> Result<CommentRow> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_err)?;
        // 幂等：只有状态真正翻转时才 bump revision / updated_at。
        let sql = if resolved {
            "UPDATE comment SET \
                 resolved_at = COALESCE(resolved_at, now()), \
                 revision = revision + CASE WHEN resolved_at IS NULL THEN 1 ELSE 0 END, \
                 updated_at = CASE WHEN resolved_at IS NULL THEN now() ELSE updated_at END \
             WHERE id = $1 AND deleted_at IS NULL \
             RETURNING id, workspace_id, issue_id, parent_id, author_type, author_id::text AS author_id, content AS body, source_task_id, routing_escalation, revision, \
                       resolved_at, deleted_at, created_at, updated_at"
        } else {
            "UPDATE comment SET \
                 resolved_at = NULL, \
                 revision = revision + CASE WHEN resolved_at IS NULL THEN 0 ELSE 1 END, \
                 updated_at = CASE WHEN resolved_at IS NULL THEN updated_at ELSE now() END \
             WHERE id = $1 AND deleted_at IS NULL \
             RETURNING id, workspace_id, issue_id, parent_id, author_type, author_id::text AS author_id, content AS body, source_task_id, routing_escalation, revision, \
                       resolved_at, deleted_at, created_at, updated_at"
        };
        let row = sqlx::query_as::<_, CommentRow>(sql)
            .bind(id.as_uuid())
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_sqlx_err)?
            .ok_or(RepoError::NotFound)?;
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(row)
    }
}
