//! `CommentRepo` 的 reactions。
use super::row::{CommentReactionRow, REACTION_COLUMNS};
use super::util::bump_comment_revision;
use super::CommentRepo;
use crate::workspace::map_sqlx_err;
use crate::{RepoError, Result};
use mc_core::id::Id;
use uuid::Uuid;

impl CommentRepo {
    /// 加 reaction（幂等：同 `(actor_type, actor_id, emoji)` 重复 POST 返回同一行）。
    ///
    /// 只有**首次**插入才 bump 评论 `revision`（对齐上游 `AddReaction`）。
    /// 评论不存在或已软删 → `NotFound`。
    pub async fn add_reaction(
        &self,
        comment_id: Id,
        actor_type: &str,
        actor_id: &str,
        emoji: &str,
    ) -> Result<CommentReactionRow> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_err)?;
        let inserted = sqlx::query_as::<_, CommentReactionRow>(&format!(
            "INSERT INTO comment_reaction (comment_id, workspace_id, actor_type, actor_id, emoji) \
             SELECT c.id, c.workspace_id, $2, $3::uuid, $4 FROM comment c \
             WHERE c.id = $1 AND c.deleted_at IS NULL \
             ON CONFLICT (comment_id, actor_type, actor_id, emoji) DO NOTHING \
             RETURNING {REACTION_COLUMNS}"
        ))
        .bind(comment_id.as_uuid())
        .bind(actor_type)
        .bind(actor_id)
        .bind(emoji)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;

        let (row, newly_added) = if let Some(row) = inserted {
            (row, true)
        } else {
            let existing = sqlx::query_as::<_, CommentReactionRow>(&format!(
                "SELECT {REACTION_COLUMNS} FROM comment_reaction \
                 WHERE comment_id = $1 AND actor_type = $2 AND actor_id = $3::uuid AND emoji = $4"
            ))
            .bind(comment_id.as_uuid())
            .bind(actor_type)
            .bind(actor_id)
            .bind(emoji)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
            match existing {
                // 插入被 ON CONFLICT 吞掉 → 已存在，幂等返回。
                Some(row) => (row, false),
                // 冲突都没有、行也不在 → 评论不存在或已软删（INSERT ... SELECT 没产出行）。
                None => return Err(RepoError::NotFound),
            }
        };

        if newly_added {
            bump_comment_revision(&mut tx, comment_id).await?;
        }
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(row)
    }

    /// 移除 reaction（幂等：不存在时返回 `false`，不报错）。
    ///
    /// 只有**真正删掉**才 bump 评论 `revision`。评论不存在或已软删 → `NotFound`。
    pub async fn remove_reaction(
        &self,
        comment_id: Id,
        actor_type: &str,
        actor_id: &str,
        emoji: &str,
    ) -> Result<bool> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_err)?;
        let live: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM comment WHERE id = $1 AND deleted_at IS NULL")
                .bind(comment_id.as_uuid())
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_err)?;
        if live.is_none() {
            return Err(RepoError::NotFound);
        }

        let res = sqlx::query(
            "DELETE FROM comment_reaction \
             WHERE comment_id = $1 AND actor_type = $2 AND actor_id = $3::uuid AND emoji = $4",
        )
        .bind(comment_id.as_uuid())
        .bind(actor_type)
        .bind(actor_id)
        .bind(emoji)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        let changed = res.rows_affected() > 0;
        if changed {
            bump_comment_revision(&mut tx, comment_id).await?;
        }
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(changed)
    }

    /// 批量取 reaction（列表接口水合用）。
    pub async fn list_reactions(&self, comment_ids: &[Id]) -> Result<Vec<CommentReactionRow>> {
        if comment_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<Uuid> = comment_ids.iter().map(|id| id.as_uuid()).collect();
        let rows = sqlx::query_as::<_, CommentReactionRow>(&format!(
            "SELECT {REACTION_COLUMNS} FROM comment_reaction \
             WHERE comment_id = ANY($1::uuid[]) \
             ORDER BY created_at ASC, id ASC"
        ))
        .bind(&ids)
        .fetch_all(self.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(rows)
    }
}
