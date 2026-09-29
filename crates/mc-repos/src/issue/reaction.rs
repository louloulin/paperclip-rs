use mc_core::Id;

use super::{map_sqlx_err, IssueReactionRow, IssueRepo, RepoError, Result};

impl IssueRepo {
    /// issue 的 reactions。
    pub async fn list_reactions(&self, issue_id: Id) -> Result<Vec<IssueReactionRow>> {
        sqlx::query_as::<_, IssueReactionRow>(
            "SELECT id, issue_id, workspace_id, actor_type, actor_id::text AS actor_id, emoji, created_at FROM issue_reaction \
             WHERE issue_id = $1 ORDER BY created_at",
        )
        .bind(issue_id.0)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 幂等加 reaction（同一 actor + emoji 重复 POST 返回同一条）。
    pub async fn add_reaction(
        &self,
        workspace_id: Id,
        issue_id: Id,
        actor_type: &str,
        actor_id: &str,
        emoji: &str,
    ) -> Result<IssueReactionRow> {
        sqlx::query_as::<_, IssueReactionRow>(
            "INSERT INTO issue_reaction (issue_id, workspace_id, actor_type, actor_id, emoji) \
             VALUES ($1, $2, $3, $4::uuid, $5) \
             ON CONFLICT (issue_id, actor_type, actor_id, emoji) DO UPDATE SET emoji = EXCLUDED.emoji \
             RETURNING id, issue_id, workspace_id, actor_type, actor_id::text AS actor_id, emoji, created_at",
        )
        .bind(issue_id.0)
        .bind(workspace_id.0)
        .bind(actor_type)
        .bind(actor_id)
        .bind(emoji)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 幂等删 reaction；不存在 → `NotFound`。
    pub async fn remove_reaction(
        &self,
        issue_id: Id,
        actor_type: &str,
        actor_id: &str,
        emoji: &str,
    ) -> Result<IssueReactionRow> {
        sqlx::query_as::<_, IssueReactionRow>(
            "DELETE FROM issue_reaction WHERE issue_id = $1 AND actor_type = $2 AND actor_id = $3::uuid AND emoji = $4 \
             RETURNING id, issue_id, workspace_id, actor_type, actor_id::text AS actor_id, emoji, created_at",
        )
        .bind(issue_id.0)
        .bind(actor_type)
        .bind(actor_id)
        .bind(emoji)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)
    }
}
