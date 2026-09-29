//! `comment` 的共享自由函数。

use crate::workspace::map_sqlx_err;
use crate::Result;
use mc_core::comment::CommentAuthorType;
use mc_core::id::Id;
use uuid::Uuid;

/// 评论即 issue 活动：bump `revision` + `last_activity_at`（对齐上游 `CreateComment`）。
pub(super) async fn touch_issue(
    conn: &mut sqlx::PgConnection,
    issue_id: Uuid,
    workspace_id: Uuid,
) -> Result<()> {
    sqlx::query(
        "UPDATE issue SET updated_at = now(), revision = revision + 1, \
                last_activity_at = GREATEST(COALESCE(last_activity_at, updated_at), now()) \
         WHERE id = $1 AND workspace_id = $2",
    )
    .bind(issue_id)
    .bind(workspace_id)
    .execute(conn)
    .await
    .map_err(map_sqlx_err)?;
    Ok(())
}

/// reaction 变更顺带 bump 所属评论的 `revision` / `updated_at`。
pub(super) async fn bump_comment_revision(
    conn: &mut sqlx::PgConnection,
    comment_id: Id,
) -> Result<()> {
    sqlx::query(
        "UPDATE comment SET revision = revision + 1, updated_at = now() \
         WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(comment_id.as_uuid())
    .execute(conn)
    .await
    .map_err(map_sqlx_err)?;
    Ok(())
}

/// `comment.author_type` TEXT → 领域枚举（未知值回落 `User`）。
pub(super) fn parse_author_type(raw: &str) -> CommentAuthorType {
    match raw {
        "agent" => CommentAuthorType::Agent,
        "system" => CommentAuthorType::System,
        "plugin" => CommentAuthorType::Plugin,
        "squad" => CommentAuthorType::Squad,
        "autopilot" => CommentAuthorType::Autopilot,
        _ => CommentAuthorType::User,
    }
}
