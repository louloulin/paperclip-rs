//! `CommentRepo` 的构造与增删改。
use super::input::{CommentPatch, NewComment};
use super::row::{CommentRow, COLUMNS};
use super::util::touch_issue;
use super::CommentRepo;
use crate::workspace::map_sqlx_err;
use crate::{RepoError, Result};
use chrono::{DateTime, Utc};
use mc_core::id::Id;
use mc_db::Db;
use sqlx::PgPool;
use uuid::Uuid;

impl CommentRepo {
    /// 从应用共享 `Db` 句柄构造。
    pub fn new(db: &Db) -> Self {
        Self {
            pool: db.pool().clone(),
        }
    }

    /// 用自定义 pool 构造（集成测试用）。
    pub fn with_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// 创建评论（线程回复用 `parent_id`）。
    ///
    /// 事务内保证两件事（对齐上游 `CreateComment` 的 CTE）：
    /// 1. **租户完整性**：`(issue_id, workspace_id)` 必须真实配对存在，否则 `NotFound`
    /// 2. **父评论合法**：`parent_id` 必须属于同一 issue 且未软删，否则 `NotFound`
    ///
    /// 并在同事务里 bump 父 issue 的 `revision` / `last_activity_at`
    /// （上游语义：评论即 issue 活动）。
    pub async fn create(&self, input: NewComment) -> Result<CommentRow> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_err)?;

        let issue: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM issue WHERE id = $1 AND workspace_id = $2")
                .bind(input.issue_id.as_uuid())
                .bind(input.workspace_id.as_uuid())
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_err)?;
        if issue.is_none() {
            return Err(RepoError::NotFound);
        }

        if let Some(parent_id) = input.parent_id {
            let parent: Option<(Uuid,)> = sqlx::query_as(
                "SELECT id FROM comment \
                 WHERE id = $1 AND issue_id = $2 AND workspace_id = $3 AND deleted_at IS NULL",
            )
            .bind(parent_id.as_uuid())
            .bind(input.issue_id.as_uuid())
            .bind(input.workspace_id.as_uuid())
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
            if parent.is_none() {
                return Err(RepoError::NotFound);
            }
        }

        let row = sqlx::query_as::<_, CommentRow>(&format!(
            "INSERT INTO comment \
                (workspace_id, issue_id, parent_id, author_type, author_id, content, source_task_id) \
             VALUES ($1, $2, $3, $4, $5::uuid, $6, $7) \
             RETURNING {COLUMNS}"
        ))
        .bind(input.workspace_id.as_uuid())
        .bind(input.issue_id.as_uuid())
        .bind(input.parent_id.map(Id::as_uuid))
        .bind(input.author_type.as_str())
        .bind(&input.author_id)
        .bind(&input.body)
        .bind(input.source_task_id.map(Id::as_uuid))
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;

        touch_issue(&mut tx, row.issue_id, row.workspace_id).await?;
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(row)
    }

    /// 按 id 取（含软删 tombstone；调用方决定是否当 404）。
    pub async fn get(&self, id: Id) -> Result<CommentRow> {
        let row = sqlx::query_as::<_, CommentRow>(&format!(
            "SELECT {COLUMNS} FROM comment WHERE id = $1"
        ))
        .bind(id.as_uuid())
        .fetch_optional(self.pool())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)?;
        Ok(row)
    }

    /// 按 id + workspace 取（跨租户读一律 `NotFound`，不泄露存在性）。
    pub async fn get_in_workspace(&self, id: Id, workspace_id: Id) -> Result<CommentRow> {
        let row = sqlx::query_as::<_, CommentRow>(&format!(
            "SELECT {COLUMNS} FROM comment WHERE id = $1 AND workspace_id = $2"
        ))
        .bind(id.as_uuid())
        .bind(workspace_id.as_uuid())
        .fetch_optional(self.pool())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)?;
        Ok(row)
    }

    /// 更新 body（`revision += 1`，`expected_revision` 不匹配 → `Conflict`）。
    ///
    /// 不存在或已软删 → `NotFound`（tombstone 不可编辑）。
    pub async fn update(&self, id: Id, patch: CommentPatch) -> Result<CommentRow> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_err)?;
        let updated = sqlx::query_as::<_, CommentRow>(&format!(
            "UPDATE comment SET content = $2, revision = revision + 1, updated_at = now() \
             WHERE id = $1 AND deleted_at IS NULL \
               AND ($3::bigint IS NULL OR revision = $3) \
             RETURNING {COLUMNS}"
        ))
        .bind(id.as_uuid())
        .bind(&patch.body)
        .bind(patch.expected_revision)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;

        let Some(row) = updated else {
            // 0 行有两种原因：行不存在/已删（404）或 revision 不匹配（409）。
            let current: Option<(i64, Option<DateTime<Utc>>)> =
                sqlx::query_as("SELECT revision, deleted_at FROM comment WHERE id = $1")
                    .bind(id.as_uuid())
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(map_sqlx_err)?;
            return match current {
                None | Some((_, Some(_))) => Err(RepoError::NotFound),
                Some(_) => Err(RepoError::Conflict),
            };
        };

        touch_issue(&mut tx, row.issue_id, row.workspace_id).await?;
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(row)
    }

    /// 软删（tombstone：`body` 清空 + `deleted_at` + 清 resolution）。
    ///
    /// - `keep_replies = true`：只标自身 deleted，回复保持可见（仍挂在 tombstone 下）
    /// - `keep_replies = false`：级联软删整棵子树（自身 + 所有后代）
    ///
    /// 已软删 / 不存在 → `NotFound`（幂等地不重复变更）。
    pub async fn soft_delete(&self, id: Id, keep_replies: bool) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_err)?;
        let current: Option<(Option<DateTime<Utc>>, Uuid, Uuid)> = sqlx::query_as(
            "SELECT deleted_at, issue_id, workspace_id FROM comment WHERE id = $1 FOR UPDATE",
        )
        .bind(id.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        let Some((deleted_at, issue_id, workspace_id)) = current else {
            return Err(RepoError::NotFound);
        };
        if deleted_at.is_some() {
            return Err(RepoError::NotFound);
        }

        if keep_replies {
            sqlx::query(
                "UPDATE comment SET content = '', deleted_at = now(), resolved_at = NULL, \
                        revision = revision + 1, updated_at = now() \
                 WHERE id = $1 AND deleted_at IS NULL",
            )
            .bind(id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
        } else {
            sqlx::query(
                "WITH RECURSIVE subtree(id) AS ( \
                     SELECT id FROM comment WHERE id = $1 \
                     UNION \
                     SELECT c.id FROM comment c JOIN subtree s ON c.parent_id = s.id \
                 ) \
                 UPDATE comment SET content = '', deleted_at = now(), resolved_at = NULL, \
                        revision = revision + 1, updated_at = now() \
                 WHERE id IN (SELECT id FROM subtree) AND deleted_at IS NULL",
            )
            .bind(id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
        }

        touch_issue(&mut tx, issue_id, workspace_id).await?;
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(())
    }
}
