//! `CommentRepo` 的列表查询。
use super::input::{CommentFilter, CommentList};
use super::row::{CommentRow, COLUMNS, LIVE_DESCENDANT_EXISTS};
use super::CommentRepo;
use crate::workspace::map_sqlx_err;
use crate::Result;
use mc_core::id::Id;
use uuid::Uuid;

impl CommentRepo {
    /// 根评论窗口分页 + 线程拼装。
    ///
    /// 两步：先取最新（或 `before` 游标之前）的 `limit` 条根评论，
    /// 再按 `parent_id` 递归回补这些根的整棵子树。
    ///
    /// 可见性规则（`VISIBLE_PREDICATE`，窗口与回补两处共用）：
    /// 活评论 + **仍挂着活回复的 tombstone**。后者是必需的 —— `keep_replies`
    /// 只软删自身，若把 tombstone 一并藏起来，它的活回复就成了读不到的孤儿。
    /// 真正的死线程（自身和后代全软删）不占窗口名额。
    /// 遍历本身**不**受软删影响，否则 tombstone 下面的回复会断链。
    pub async fn list_for_issue(&self, filter: CommentFilter) -> Result<CommentList> {
        let limit = filter.effective_limit();
        // `limit` 已夹在 [1, COMMENT_MAX_LIMIT]，usize 转换不会丢符号。
        let limit_usize = usize::try_from(limit).unwrap_or(usize::MAX);
        // 多取一条判 has_more。
        let probe = limit + 1;
        let before_created = filter.before.map(|c| c.created_at);
        let before_id = filter.before.map_or_else(Uuid::nil, |c| c.id.as_uuid());

        let mut roots = sqlx::query_as::<_, CommentRow>(&format!(
            "SELECT {COLUMNS} FROM comment c \
             WHERE c.issue_id = $1 \
               AND c.parent_id IS NULL \
               AND ($2::boolean OR c.deleted_at IS NULL OR {LIVE_DESCENDANT_EXISTS}) \
               AND ($3::timestamptz IS NULL OR c.created_at >= $3) \
               AND ($4::timestamptz IS NULL OR (c.created_at, c.id) < ($4::timestamptz, $5::uuid)) \
               AND ($6::uuid IS NULL OR c.id = $6) \
             ORDER BY c.created_at DESC, c.id DESC \
             LIMIT $7"
        ))
        .bind(filter.issue_id.as_uuid())
        .bind(filter.include_deleted)
        .bind(filter.since)
        .bind(before_created)
        .bind(before_id)
        .bind(filter.thread.map(Id::as_uuid))
        .bind(probe)
        .fetch_all(self.pool())
        .await
        .map_err(map_sqlx_err)?;

        let has_more = roots.len() > limit_usize;
        roots.truncate(limit_usize);
        // 窗口是倒序取的，回正为时间升序。
        roots.reverse();

        if filter.roots_only || roots.is_empty() {
            return Ok(CommentList {
                comments: roots,
                has_more,
            });
        }

        let root_ids: Vec<Uuid> = roots.iter().map(|r| r.id).collect();
        let comments = sqlx::query_as::<_, CommentRow>(&format!(
            "WITH RECURSIVE subtree(id) AS ( \
                 SELECT id FROM comment WHERE id = ANY($1::uuid[]) \
                 UNION \
                 SELECT c.id FROM comment c JOIN subtree s ON c.parent_id = s.id \
                 WHERE c.issue_id = $2 \
             ) \
             SELECT {COLUMNS} FROM comment c \
             WHERE c.id IN (SELECT id FROM subtree) \
               AND ($3::boolean OR c.deleted_at IS NULL OR {LIVE_DESCENDANT_EXISTS}) \
             ORDER BY c.created_at ASC, c.id ASC"
        ))
        .bind(&root_ids)
        .bind(filter.issue_id.as_uuid())
        .bind(filter.include_deleted)
        .fetch_all(self.pool())
        .await
        .map_err(map_sqlx_err)?;

        Ok(CommentList { comments, has_more })
    }
}
