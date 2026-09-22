//! `comment` + `comment_reaction` 表的 DB-backed 仓储（M2-B / LUM-1350）。
//!
//! 对应上游 multica `server/pkg/db/queries/comment.sql` + `reaction.sql` 的简化版：
//! - `create`（支持 `parent_id` 线程回复；`revision` 从 1 起）
//! - `list_for_issue`（根评论窗口分页 + 线程拼装，见 `CommentFilter`）
//! - `update`（`expected_revision` 乐观锁 → `Conflict`）
//! - `soft_delete`（`deleted_at` tombstone；`keep_replies` 决定是否级联软删回复）
//! - `resolve` / `unresolve`（幂等）
//! - `add_reaction` / `remove_reaction`（幂等；`comment_reaction` 有
//!   `UNIQUE(comment_id, actor_type, actor_id, emoji)`）
//!
//! 约定与 M1 各 Repo 保持一致（见 `crate::share_link`）：
//! - `Row` 用裸 `Uuid` / `String` 字段 + `Id` 访问器，`sqlx::FromRow` 派生
//!   （`mc_core::Id` 没有 sqlx impl）
//! - 错误统一走 `crate::workspace::map_sqlx_err`
//! - Pg 实现 + `#[ignore]` 的 PG 集成测试（`MULTICA_TEST_DATABASE_URL`）
//!
//! **与上游的有意简化**（详见 `docs/12-M2-COMMENT.md`）：
//! - 上游 `comment` 表有 `type` / `resolved_by_type` / `resolved_by_id` /
//!   `quick_action_id` 等列，本仓 `0001_init.up.sql` 没有 → 本 Repo 只读写本仓列
//! - 上游 resolve 会顺带清掉同线程内的其它 resolution（single-resolution invariant），
//!   本切片只做"幂等 resolve/unresolve"，该不变式留 TODO
//! - 上游删除"有回复则 tombstone、无回复则物理删 + 剪枝"，本切片统一软删

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use mc_core::comment::CommentAuthorType;
use mc_core::id::Id;
use mc_db::Db;

use crate::workspace::map_sqlx_err;
use crate::{RepoError, Result};

/// 默认窗口大小（根评论条数，非评论总数）。
pub const COMMENT_DEFAULT_LIMIT: u32 = 50;
/// 单次请求允许的最大窗口（防止 agent 一次拉爆整个 issue）。
pub const COMMENT_MAX_LIMIT: u32 = 200;

/// W0-B2 对齐上游：`content`→API 字段 `body`（`AS body`）、`author_id` 上游是 `UUID` ⇒ `::text` 投影；
/// `routing_escalation` 是 compat 列（`migrations/compat/537_local_only_columns.up.sql`）。
const COLUMNS: &str =
    "id, workspace_id, issue_id, parent_id, author_type, author_id::text AS author_id, \
                       content AS body, source_task_id, routing_escalation, revision, \
                       resolved_at, deleted_at, created_at, updated_at";

const REACTION_COLUMNS: &str =
    "id, comment_id, workspace_id, actor_type, actor_id::text AS actor_id, emoji, created_at";

/// “该评论仍挂着活后代”的相关子查询（外层表必须别名成 `c`）。
///
/// 向下递归走 `comment_parent_idx`，代价按**该评论自己的子树**计，
/// 不是整个 issue；所以窗口筛选和线程回补可以共用同一条谓词。
const LIVE_DESCENDANT_EXISTS: &str = "EXISTS ( \
        WITH RECURSIVE sub(id) AS ( \
            SELECT id FROM comment WHERE parent_id = c.id \
            UNION \
            SELECT ch.id FROM comment ch JOIN sub s ON ch.parent_id = s.id \
        ) \
        SELECT 1 FROM sub JOIN comment sc ON sc.id = sub.id WHERE sc.deleted_at IS NULL \
    )";

/// `comment` 行映射（`mc_core::Id` 没有 sqlx `Decode`，故保留裸 `Uuid`）。
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct CommentRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub issue_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub author_type: String,
    pub author_id: String,
    pub body: String,
    pub source_task_id: Option<Uuid>,
    pub routing_escalation: Option<String>,
    pub revision: i64,
    pub resolved_at: Option<DateTime<Utc>>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl CommentRow {
    pub fn id(&self) -> Id {
        Id(self.id)
    }

    pub fn workspace_id(&self) -> Id {
        Id(self.workspace_id)
    }

    pub fn issue_id(&self) -> Id {
        Id(self.issue_id)
    }

    /// 父评论 id；`None` = 线程根。
    pub fn parent_id(&self) -> Option<Id> {
        self.parent_id.map(Id)
    }

    pub fn source_task_id(&self) -> Option<Id> {
        self.source_task_id.map(Id)
    }

    /// 线程根：`parent_id` 为空的节点。
    pub fn is_root(&self) -> bool {
        self.parent_id.is_none()
    }

    pub fn is_deleted(&self) -> bool {
        self.deleted_at.is_some()
    }

    pub fn is_resolved(&self) -> bool {
        self.resolved_at.is_some()
    }

    /// 解析后的 author type（未知取值回落到 `User`，与 0001 的 CHECK 取值域保持一致）。
    pub fn author_type(&self) -> CommentAuthorType {
        parse_author_type(&self.author_type)
    }
}

/// `comment_reaction` 行映射。
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct CommentReactionRow {
    pub id: Uuid,
    pub comment_id: Uuid,
    pub workspace_id: Uuid,
    pub actor_type: String,
    pub actor_id: String,
    pub emoji: String,
    pub created_at: DateTime<Utc>,
}

impl CommentReactionRow {
    pub fn id(&self) -> Id {
        Id(self.id)
    }

    pub fn comment_id(&self) -> Id {
        Id(self.comment_id)
    }

    pub fn workspace_id(&self) -> Id {
        Id(self.workspace_id)
    }
}

/// 创建评论的输入。
#[derive(Debug, Clone)]
pub struct NewComment {
    pub workspace_id: Id,
    pub issue_id: Id,
    /// `Some` = 线程回复；父评论必须属于同一 issue 且未软删。
    pub parent_id: Option<Id>,
    pub author_type: CommentAuthorType,
    /// user / agent uuid-as-string（`comment.author_id` 是 TEXT）。
    pub author_id: String,
    pub body: String,
    pub source_task_id: Option<Id>,
}

/// 更新评论的输入（乐观锁）。
#[derive(Debug, Clone)]
pub struct CommentPatch {
    pub body: String,
    /// `Some` = expected-revision 条件写；不匹配返回 `Conflict`。
    pub expected_revision: Option<i64>,
}

/// 窗口分页游标：按 `(created_at, id)` 取"更旧"的根评论。
#[derive(Debug, Clone, Copy)]
pub struct CommentCursor {
    pub created_at: DateTime<Utc>,
    pub id: Id,
}

/// `list_for_issue` 的过滤条件。
///
/// 语义（简化版上游 `fetchCommentsForList`）：
/// - `limit` 限制的是**根评论**条数；`has_more` 表示窗口外还有更早的根
/// - 默认取**最新**的 `limit` 条根评论（`before` 游标向更旧翻页）
/// - `since` 只作用于根评论（命中窗口的线程会整条回补，避免读到一个断头线程）
/// - `roots_only = true` 时不回补回复
/// - `thread = Some(root_id)` 只读该根评论所在线程
/// - 软删可见性：活评论总是可见；tombstone 仅在**仍挂着活后代**时作为占位返回
///   （否则它的活回复会变成读不到的孤儿）；`include_deleted = true` 时连
///   死线程（自身与后代全软删）也一并返回，供审计 / 历史读用
#[derive(Debug, Clone)]
pub struct CommentFilter {
    pub issue_id: Id,
    pub since: Option<DateTime<Utc>>,
    pub before: Option<CommentCursor>,
    pub roots_only: bool,
    pub thread: Option<Id>,
    pub include_deleted: bool,
    pub limit: u32,
}

impl Default for CommentFilter {
    fn default() -> Self {
        Self {
            issue_id: Id::nil(),
            since: None,
            before: None,
            roots_only: false,
            thread: None,
            include_deleted: false,
            limit: COMMENT_DEFAULT_LIMIT,
        }
    }
}

impl CommentFilter {
    /// 按 issue 构造默认窗口。
    pub fn for_issue(issue_id: Id) -> Self {
        Self {
            issue_id,
            ..Default::default()
        }
    }

    /// 夹到 `[1, COMMENT_MAX_LIMIT]`。
    pub fn effective_limit(&self) -> i64 {
        i64::from(self.limit.clamp(1, COMMENT_MAX_LIMIT))
    }
}

/// `list_for_issue` 的返回：窗口内的评论（根 + 补回的线程回复，按时间升序）。
#[derive(Debug, Clone)]
pub struct CommentList {
    pub comments: Vec<CommentRow>,
    /// 窗口外还有更早的根评论（`before` 游标可继续翻页）。
    pub has_more: bool,
}

/// `comment` / `comment_reaction` 仓储。
#[derive(Clone)]
pub struct CommentRepo {
    pool: PgPool,
}

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

/// 评论即 issue 活动：bump `revision` + `last_activity_at`（对齐上游 `CreateComment`）。
async fn touch_issue(
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
async fn bump_comment_revision(conn: &mut sqlx::PgConnection, comment_id: Id) -> Result<()> {
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
fn parse_author_type(raw: &str) -> CommentAuthorType {
    match raw {
        "agent" => CommentAuthorType::Agent,
        "system" => CommentAuthorType::System,
        "plugin" => CommentAuthorType::Plugin,
        "squad" => CommentAuthorType::Squad,
        "autopilot" => CommentAuthorType::Autopilot,
        _ => CommentAuthorType::User,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> CommentRow {
        CommentRow {
            id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            issue_id: Uuid::new_v4(),
            parent_id: None,
            author_type: "user".into(),
            author_id: Uuid::new_v4().to_string(),
            body: "hi".into(),
            source_task_id: None,
            routing_escalation: None,
            revision: 1,
            resolved_at: None,
            deleted_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn parse_author_type_covers_schema_domain() {
        assert_eq!(parse_author_type("user"), CommentAuthorType::User);
        assert_eq!(parse_author_type("agent"), CommentAuthorType::Agent);
        assert_eq!(parse_author_type("system"), CommentAuthorType::System);
        assert_eq!(parse_author_type("plugin"), CommentAuthorType::Plugin);
        assert_eq!(parse_author_type("squad"), CommentAuthorType::Squad);
        assert_eq!(parse_author_type("autopilot"), CommentAuthorType::Autopilot);
        // 未知/未来取值回落到 User，不 panic（0001 的 CHECK 会先拦下来）。
        assert_eq!(parse_author_type("bogus"), CommentAuthorType::User);
    }

    #[test]
    fn row_accessors_and_flags() {
        let mut r = row();
        assert!(r.is_root());
        assert!(!r.is_deleted());
        assert!(!r.is_resolved());
        assert_eq!(r.parent_id(), None);

        r.parent_id = Some(Uuid::new_v4());
        r.deleted_at = Some(Utc::now());
        r.resolved_at = Some(Utc::now());
        assert!(!r.is_root());
        assert!(r.parent_id().is_some());
        assert!(r.is_deleted());
        assert!(r.is_resolved());
        assert_eq!(r.workspace_id(), Id(r.workspace_id));
        assert_eq!(r.issue_id(), Id(r.issue_id));
        assert_eq!(r.author_type(), CommentAuthorType::User);
    }

    #[test]
    fn filter_defaults_and_limit_clamp() {
        let mut f = CommentFilter::for_issue(Id::new());
        assert_eq!(f.limit, COMMENT_DEFAULT_LIMIT);
        assert_eq!(f.effective_limit(), i64::from(COMMENT_DEFAULT_LIMIT));
        assert!(!f.roots_only);
        assert!(!f.include_deleted);
        assert!(f.since.is_none());
        assert!(f.before.is_none());
        assert!(f.thread.is_none());

        f.limit = 0;
        assert_eq!(f.effective_limit(), 1);
        f.limit = u32::MAX;
        assert_eq!(f.effective_limit(), i64::from(COMMENT_MAX_LIMIT));
    }

    #[test]
    fn reaction_row_accessors() {
        let r = CommentReactionRow {
            id: Uuid::new_v4(),
            comment_id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            actor_type: "user".into(),
            actor_id: Uuid::new_v4().to_string(),
            emoji: "👍".into(),
            created_at: Utc::now(),
        };
        assert_eq!(r.comment_id(), Id(r.comment_id));
        assert_eq!(r.workspace_id(), Id(r.workspace_id));
        assert_eq!(r.id(), Id(r.id));
    }

    // ---- DB 集成测试（`cargo test -- --ignored` + MULTICA_TEST_DATABASE_URL）----
    //
    // 运行示例：
    //   MULTICA_TEST_DATABASE_URL=postgres://multica:multica@127.0.0.1:5432/<db> \
    //     cargo test -p mc-repos comment::tests::db_ -- --ignored

    use crate::Repository;

    /// 测试夹具：一个 workspace + 一个 user + 一个 issue（comment 的最小合法父级）。
    ///
    /// M2-A（issue 域）未合并，所以 issue 行直接 SQL 插入 —— 只依赖 0001 的列。
    struct IssueFixture {
        db: mc_db::Db,
        workspace_id: Id,
        issue_id: Id,
        owner: Id,
    }

    async fn test_pool() -> mc_db::Db {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL")
            .expect("set MULTICA_TEST_DATABASE_URL to enable DB tests");
        mc_db::Db::connect(&url, 4, 1)
            .await
            .expect("connect test db")
    }

    fn unique_slug(prefix: &str) -> mc_core::Slug {
        let s = Id::new().to_string().replace('-', "");
        mc_core::Slug::parse(&format!("{prefix}-{}", &s[..10])).expect("slug")
    }

    async fn new_issue(db: &mc_db::Db, tag: &str) -> IssueFixture {
        let workspace = crate::workspace::WorkspaceRepo::new(db.clone())
            .create(mc_core::workspace::NewWorkspace {
                name: format!("M2B {tag}"),
                slug: unique_slug("m2b"),
                description: None,
            })
            .await
            .expect("create workspace");
        let s = Id::new().to_string().replace('-', "");
        let owner = crate::user::UserRepo::new(db.clone())
            .create(crate::user::NewUser {
                name: format!("m2b-{tag}"),
                email: format!("{tag}-{}@example.com", &s[..12]),
                avatar_url: None,
            })
            .await
            .expect("create user")
            .id;

        let number = i32::try_from(Uuid::new_v4().as_u128() % 1_000_000).unwrap_or(1);
        let issue_id: Uuid = sqlx::query_scalar(
            "INSERT INTO issue (workspace_id, number, identifier, title, creator_type, creator_id) VALUES ($1, $2, $3, $4, 'user', $5::uuid) RETURNING id",
        )
        .bind(workspace.id.as_uuid())
        .bind(number)
        .bind(format!("M2B-{number}"))
        .bind(format!("fixture {tag}"))
        .bind(owner.to_string())
        .fetch_one(db.pool())
        .await
        .expect("insert issue");

        IssueFixture {
            db: db.clone(),
            workspace_id: workspace.id,
            issue_id: Id(issue_id),
            owner,
        }
    }

    /// 硬删夹具（`WorkspaceRepo::delete` 只是软删，会留下 comment 行污染后续断言）。
    async fn cleanup_issue_fixture(db: &mc_db::Db, f: &IssueFixture) {
        sqlx::query("DELETE FROM workspace WHERE id = $1")
            .bind(f.workspace_id.as_uuid())
            .execute(db.pool())
            .await
            .expect("cleanup workspace");
        sqlx::query("DELETE FROM \"user\" WHERE id = $1")
            .bind(f.owner.as_uuid())
            .execute(db.pool())
            .await
            .expect("cleanup user");
    }

    fn author(fixture: &IssueFixture) -> String {
        fixture.owner.to_string()
    }

    #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
    #[tokio::test]
    async fn db_create_list_and_thread_assembly() {
        let pool = test_pool().await;
        let f = new_issue(&pool, "comment-create").await;
        let repo = CommentRepo::new(&f.db);

        let root = repo
            .create(NewComment {
                workspace_id: f.workspace_id,
                issue_id: f.issue_id,
                parent_id: None,
                author_type: CommentAuthorType::User,
                author_id: author(&f),
                body: "root".into(),
                source_task_id: None,
            })
            .await
            .expect("create root");
        assert_eq!(root.revision, 1);
        assert!(root.is_root());

        let reply = repo
            .create(NewComment {
                workspace_id: f.workspace_id,
                issue_id: f.issue_id,
                parent_id: Some(root.id()),
                author_type: CommentAuthorType::User,
                author_id: author(&f),
                body: "reply".into(),
                source_task_id: None,
            })
            .await
            .expect("create reply");
        assert_eq!(reply.parent_id(), Some(root.id()));

        // 线程拼装：窗口按根评论取，回复整条补回，时间升序。
        let list = repo
            .list_for_issue(CommentFilter::for_issue(f.issue_id))
            .await
            .expect("list");
        assert_eq!(list.comments.len(), 2);
        assert!(!list.has_more);
        assert_eq!(list.comments[0].id(), root.id());
        assert_eq!(list.comments[1].id(), reply.id());

        // roots_only：只给根。
        let roots = repo
            .list_for_issue(CommentFilter {
                roots_only: true,
                ..CommentFilter::for_issue(f.issue_id)
            })
            .await
            .expect("list roots");
        assert_eq!(roots.comments.len(), 1);
        assert_eq!(roots.comments[0].id(), root.id());

        // thread 过滤：只看该根所在线程。
        let thread = repo
            .list_for_issue(CommentFilter {
                thread: Some(root.id()),
                ..CommentFilter::for_issue(f.issue_id)
            })
            .await
            .expect("list thread");
        assert_eq!(thread.comments.len(), 2);

        // 父评论必须属于同一 issue。
        let other = new_issue(&pool, "comment-create-other").await;
        let err = repo
            .create(NewComment {
                workspace_id: f.workspace_id,
                issue_id: other.issue_id,
                parent_id: Some(root.id()),
                author_type: CommentAuthorType::User,
                author_id: author(&f),
                body: "cross-issue".into(),
                source_task_id: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, RepoError::NotFound));

        cleanup_issue_fixture(&pool, &f).await;
        cleanup_issue_fixture(&pool, &other).await;
    }

    #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
    #[tokio::test]
    async fn db_list_cursor_pagination_reports_has_more() {
        let pool = test_pool().await;
        let f = new_issue(&pool, "comment-page").await;
        let repo = CommentRepo::new(&f.db);
        for i in 0..3 {
            repo.create(NewComment {
                workspace_id: f.workspace_id,
                issue_id: f.issue_id,
                parent_id: None,
                author_type: CommentAuthorType::User,
                author_id: author(&f),
                body: format!("c{i}"),
                source_task_id: None,
            })
            .await
            .expect("create");
        }

        let page1 = repo
            .list_for_issue(CommentFilter {
                limit: 2,
                roots_only: true,
                ..CommentFilter::for_issue(f.issue_id)
            })
            .await
            .expect("page1");
        assert_eq!(page1.comments.len(), 2);
        assert!(page1.has_more, "3 roots with limit 2 → has_more");

        let oldest_of_page1 = page1.comments[0].clone();
        let page2 = repo
            .list_for_issue(CommentFilter {
                limit: 2,
                roots_only: true,
                before: Some(CommentCursor {
                    created_at: oldest_of_page1.created_at,
                    id: oldest_of_page1.id(),
                }),
                ..CommentFilter::for_issue(f.issue_id)
            })
            .await
            .expect("page2");
        assert_eq!(page2.comments.len(), 1);
        assert!(!page2.has_more);
        assert!(!page2
            .comments
            .iter()
            .any(|c| c.id() == oldest_of_page1.id()));

        cleanup_issue_fixture(&pool, &f).await;
    }

    #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
    #[tokio::test]
    async fn db_soft_delete_visibility_and_keep_replies() {
        let pool = test_pool().await;
        let f = new_issue(&pool, "comment-delete").await;
        let repo = CommentRepo::new(&f.db);

        let keep_root = repo
            .create(NewComment {
                workspace_id: f.workspace_id,
                issue_id: f.issue_id,
                parent_id: None,
                author_type: CommentAuthorType::User,
                author_id: author(&f),
                body: "keep-root".into(),
                source_task_id: None,
            })
            .await
            .expect("root");
        let keep_reply = repo
            .create(NewComment {
                workspace_id: f.workspace_id,
                issue_id: f.issue_id,
                parent_id: Some(keep_root.id()),
                author_type: CommentAuthorType::User,
                author_id: author(&f),
                body: "kept-reply".into(),
                source_task_id: None,
            })
            .await
            .expect("reply");

        // keep_replies = true：只有自身变 tombstone，回复仍可见。
        repo.soft_delete(keep_root.id(), true)
            .await
            .expect("delete");
        let hidden = repo.get(keep_root.id()).await.expect("tombstone row");
        assert!(hidden.is_deleted());
        assert!(hidden.body.is_empty());
        let list = repo
            .list_for_issue(CommentFilter::for_issue(f.issue_id))
            .await
            .expect("list");
        // tombstone 作为线程锚点保留（否则活回复会成孤儿），但 body 已清空。
        let anchor = list
            .comments
            .iter()
            .find(|c| c.id() == keep_root.id())
            .expect("tombstone kept as thread anchor");
        assert!(anchor.is_deleted());
        assert!(anchor.body.is_empty());
        assert!(
            list.comments.iter().any(|c| c.id() == keep_reply.id()),
            "kept reply still visible"
        );
        // 二次软删幂等 → NotFound（不重复变更）。
        assert!(matches!(
            repo.soft_delete(keep_root.id(), true).await.unwrap_err(),
            RepoError::NotFound
        ));

        cleanup_issue_fixture(&pool, &f).await;
    }

    #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
    #[tokio::test]
    async fn db_cascade_soft_delete_hides_whole_thread() {
        let pool = test_pool().await;
        let f = new_issue(&pool, "comment-cascade").await;
        let repo = CommentRepo::new(&f.db);

        // 级联：删除根时一并软删所有后代。
        let cascade_root = repo
            .create(NewComment {
                workspace_id: f.workspace_id,
                issue_id: f.issue_id,
                parent_id: None,
                author_type: CommentAuthorType::User,
                author_id: author(&f),
                body: "cascade-root".into(),
                source_task_id: None,
            })
            .await
            .expect("root");
        let cascade_child = repo
            .create(NewComment {
                workspace_id: f.workspace_id,
                issue_id: f.issue_id,
                parent_id: Some(cascade_root.id()),
                author_type: CommentAuthorType::User,
                author_id: author(&f),
                body: "cascade-child".into(),
                source_task_id: None,
            })
            .await
            .expect("child");
        repo.soft_delete(cascade_root.id(), false)
            .await
            .expect("cascade delete");
        assert!(repo.get(cascade_root.id()).await.unwrap().is_deleted());
        assert!(repo.get(cascade_child.id()).await.unwrap().is_deleted());
        // 整条线程（自身 + 后代）全软删 → 默认列表里不再出现。
        let after_cascade = repo
            .list_for_issue(CommentFilter::for_issue(f.issue_id))
            .await
            .expect("list after cascade");
        assert!(after_cascade
            .comments
            .iter()
            .all(|c| c.id() != cascade_root.id() && c.id() != cascade_child.id()));

        // include_deleted = true 时 tombstone 仍可读（审计 / 折叠需要）。
        let with_deleted = repo
            .list_for_issue(CommentFilter {
                include_deleted: true,
                ..CommentFilter::for_issue(f.issue_id)
            })
            .await
            .expect("list with deleted");
        assert!(with_deleted
            .comments
            .iter()
            .any(|c| c.id() == cascade_root.id()));

        cleanup_issue_fixture(&pool, &f).await;
    }

    #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
    #[tokio::test]
    async fn db_update_revision_conflict_and_tombstone_not_editable() {
        let pool = test_pool().await;
        let f = new_issue(&pool, "comment-update").await;
        let repo = CommentRepo::new(&f.db);
        let c = repo
            .create(NewComment {
                workspace_id: f.workspace_id,
                issue_id: f.issue_id,
                parent_id: None,
                author_type: CommentAuthorType::User,
                author_id: author(&f),
                body: "v1".into(),
                source_task_id: None,
            })
            .await
            .expect("create");

        // 带 correct expected_revision → 成功并自增。
        let updated = repo
            .update(
                c.id(),
                CommentPatch {
                    body: "v2".into(),
                    expected_revision: Some(1),
                },
            )
            .await
            .expect("update");
        assert_eq!(updated.body, "v2");
        assert_eq!(updated.revision, 2);

        // 旧 revision → Conflict。
        let err = repo
            .update(
                c.id(),
                CommentPatch {
                    body: "v3".into(),
                    expected_revision: Some(1),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, RepoError::Conflict));
        assert_eq!(repo.get(c.id()).await.unwrap().body, "v2");

        // 不带 expected_revision → 无条件写。
        let blind = repo
            .update(
                c.id(),
                CommentPatch {
                    body: "v4".into(),
                    expected_revision: None,
                },
            )
            .await
            .expect("blind update");
        assert_eq!(blind.revision, 3);

        // 已软删的 tombstone 不可编辑 → NotFound。
        repo.soft_delete(c.id(), true).await.expect("delete");
        let err = repo
            .update(
                c.id(),
                CommentPatch {
                    body: "v5".into(),
                    expected_revision: None,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, RepoError::NotFound));

        cleanup_issue_fixture(&pool, &f).await;
    }

    #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
    #[tokio::test]
    async fn db_resolve_unresolve_is_idempotent() {
        let pool = test_pool().await;
        let f = new_issue(&pool, "comment-resolve").await;
        let repo = CommentRepo::new(&f.db);
        let c = repo
            .create(NewComment {
                workspace_id: f.workspace_id,
                issue_id: f.issue_id,
                parent_id: None,
                author_type: CommentAuthorType::User,
                author_id: author(&f),
                body: "resolve me".into(),
                source_task_id: None,
            })
            .await
            .expect("create");

        let resolved = repo.resolve(c.id()).await.expect("resolve");
        assert!(resolved.is_resolved());
        assert_eq!(resolved.revision, 2);
        let resolved_at = resolved.resolved_at;

        // 幂等：第二次 resolve 不推进 resolved_at，也不 bump revision。
        let again = repo.resolve(c.id()).await.expect("re-resolve");
        assert_eq!(again.resolved_at, resolved_at);
        assert_eq!(again.revision, 2);

        let unresolved = repo.unresolve(c.id()).await.expect("unresolve");
        assert!(!unresolved.is_resolved());
        assert_eq!(unresolved.revision, 3);

        // 幂等：已 unresolved 再 unresolve 是 no-op。
        let noop = repo.unresolve(c.id()).await.expect("re-unresolve");
        assert_eq!(noop.revision, 3);

        cleanup_issue_fixture(&pool, &f).await;
    }

    #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
    #[tokio::test]
    async fn db_reaction_add_remove_is_idempotent() {
        let pool = test_pool().await;
        let f = new_issue(&pool, "comment-reaction").await;
        let repo = CommentRepo::new(&f.db);
        let c = repo
            .create(NewComment {
                workspace_id: f.workspace_id,
                issue_id: f.issue_id,
                parent_id: None,
                author_type: CommentAuthorType::User,
                author_id: author(&f),
                body: "react to me".into(),
                source_task_id: None,
            })
            .await
            .expect("create");
        let actor_id = author(&f);

        let r1 = repo
            .add_reaction(c.id(), "user", &actor_id, "👍")
            .await
            .expect("add");
        // 首次插入 bump 评论 revision。
        assert_eq!(repo.get(c.id()).await.unwrap().revision, 2);

        // 重复 POST → 同一行（幂等），不再 bump revision。
        let r2 = repo
            .add_reaction(c.id(), "user", &actor_id, "👍")
            .await
            .expect("re-add");
        assert_eq!(r1.id(), r2.id());
        assert_eq!(repo.get(c.id()).await.unwrap().revision, 2);
        assert_eq!(repo.list_reactions(&[c.id()]).await.expect("list").len(), 1);

        // 不同 emoji 是不同行。
        repo.add_reaction(c.id(), "user", &actor_id, "🎉")
            .await
            .expect("add second emoji");
        assert_eq!(repo.list_reactions(&[c.id()]).await.expect("list").len(), 2);

        // remove：第一次 true，第二次 false（幂等 no-op）。
        assert!(repo
            .remove_reaction(c.id(), "user", &actor_id, "👍")
            .await
            .expect("remove"));
        assert!(!repo
            .remove_reaction(c.id(), "user", &actor_id, "👍")
            .await
            .expect("re-remove"));

        // tombstone 不能加 reaction。
        repo.soft_delete(c.id(), true).await.expect("delete");
        let err = repo
            .add_reaction(c.id(), "user", &actor_id, "👍")
            .await
            .unwrap_err();
        assert!(matches!(err, RepoError::NotFound));

        cleanup_issue_fixture(&pool, &f).await;
    }

    #[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
    #[tokio::test]
    async fn db_list_since_filters_and_comment_bumps_issue_activity() {
        let pool = test_pool().await;
        let f = new_issue(&pool, "comment-since").await;
        let repo = CommentRepo::new(&f.db);

        let issue_before = current_issue_revision(&f.db, f.issue_id).await;

        let older = repo
            .create(NewComment {
                workspace_id: f.workspace_id,
                issue_id: f.issue_id,
                parent_id: None,
                author_type: CommentAuthorType::User,
                author_id: author(&f),
                body: "older".into(),
                source_task_id: None,
            })
            .await
            .expect("older");
        // 评论即 issue 活动：父 issue revision 前进。
        assert!(current_issue_revision(&f.db, f.issue_id).await > issue_before);

        // since = 稍后于 older 的时间点 → 只剩新评论。
        let since = older.created_at + chrono::Duration::milliseconds(1);
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let newer = repo
            .create(NewComment {
                workspace_id: f.workspace_id,
                issue_id: f.issue_id,
                parent_id: None,
                author_type: CommentAuthorType::User,
                author_id: author(&f),
                body: "newer".into(),
                source_task_id: None,
            })
            .await
            .expect("newer");

        let list = repo
            .list_for_issue(CommentFilter {
                since: Some(since),
                ..CommentFilter::for_issue(f.issue_id)
            })
            .await
            .expect("list since");
        assert_eq!(list.comments.len(), 1);
        assert_eq!(list.comments[0].id(), newer.id());

        cleanup_issue_fixture(&pool, &f).await;
    }

    async fn current_issue_revision(db: &mc_db::Db, issue_id: Id) -> i64 {
        sqlx::query_scalar::<_, i64>("SELECT revision FROM issue WHERE id = $1")
            .bind(issue_id.as_uuid())
            .fetch_one(db.pool())
            .await
            .expect("issue revision")
    }
}
