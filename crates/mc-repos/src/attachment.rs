//! `attachment` 表的 DB-backed 仓储（**写者 M10-B1** / `LUM-2112` / `docs/64` §4.2 第 1 行）。
//!
//! 上游来源：multica `server/pkg/db/queries/attachment.sql`（pin `f41fae6b08fb`）。
//! 本文件按本仓既有约定（见 `crate::share_link` / `crate::comment`）落地：
//! - `Row` 用裸 `Uuid` / `String` 字段 + `Id` 访问器（`mc_core::Id` 没有 sqlx impl）；
//! - 错误统一走 `crate::workspace::map_sqlx_err`；
//! - Pg 实现 + `#[ignore]` 的 PG 集成测试（`MULTICA_TEST_DATABASE_URL`）。
//!
//! ## 四个查询与它们各自的**授权中性度**（本片最重要的一层语义）
//!
//! | 方法 | 上游查询 | 授权中性？ | 谁用 |
//! |---|---|:--:|---|
//! | [`AttachmentRepo::get`] | `GetAttachment` `WHERE id=$1 AND workspace_id=$2` | 是（调用方须已验成员） | `GET /api/attachments/{id}`、`/content`、`DELETE` |
//! | [`AttachmentRepo::get_by_id_only`] | `GetAttachmentByIDOnly` `WHERE id=$1` | **是**（上游注释逐字：access-neutral on purpose） | `/download`（自解析 workspace）与 `/signed-download`（凭签名，不验成员） |
//! | [`AttachmentRepo::list_by_issue`] | `ListAttachmentsByIssue` `WHERE issue_id=$1 AND workspace_id=$2 ORDER BY created_at ASC` | 是 | `GET /api/issues/{id}/attachments` |
//! | [`AttachmentRepo::delete`] | `DeleteAttachment`（CTE：删 + bump issue/comment revision） | 是 | `DELETE /api/attachments/{id}` |
//!
//! 🔴 `get_by_id_only` **不带 workspace 条件**是刻意的（上游 `attachment.sql:42-50` 的注释
//! 逐字说明）：`/api/attachments/{id}/download` 必须能在**不带** `X-Workspace-*` 头的
//! 原生 `<img>` / `<video>` 加载下工作 ⇒ workspace 只能从行本身解析。**代价是调用方
//! 必须自己验成员**——上游在 `loadAttachmentForDownload` 里用「404 形状的成员校验」
//! 兜住（不做成 IDOR oracle），本仓在 `routes/attachments/download.rs` 里照搬。
//!
//! ## `DeleteAttachment` 的 CTE 与本仓的两处取舍
//!
//! 上游 `attachment.sql:245-264` 是一个 `WITH deleted AS (DELETE … RETURNING …),
//! bumped_issue AS (UPDATE issue …), bumped_comment AS (UPDATE comment …)` 的三段 CTE，
//! 最后 `SELECT EXISTS(...) AS changed, COALESCE(revision) AS issue_revision,
//! COALESCE(revision) AS comment_revision`。
//!
//! 本仓**照搬整条 SQL**（含 `source_context_id IS NULL` 那一格），因为它的返回值
//! `changed` 不是装饰：handler 靠它决定**要不要发 realtime 事件**（上游 `file.go:1500+`）。
//! 但 `changed` 的语义差别已登记为偏离（`docs/32` §50）——见 [`DeleteOutcome`] 的字段注释。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use mc_core::id::Id;
use mc_db::Db;

use crate::workspace::map_sqlx_err;
use crate::{RepoError, Result};

/// 上传者类型（`attachment_uploader_type_check`：`member` | `agent`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploaderType {
    Member,
    Agent,
}

impl UploaderType {
    /// DB 里的字面量（`serde` 也按这个形状写）。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Agent => "agent",
        }
    }

    /// 解析 DB 字面量；**未知值按 `member` 兜底**（`CHECK` 约束保证只可能是这两个，
    /// 这里不因脏数据让整个列表 500）。
    #[must_use]
    pub fn from_db(raw: &str) -> Self {
        if raw.eq_ignore_ascii_case("agent") {
            Self::Agent
        } else {
            Self::Member
        }
    }
}

/// `attachment` 行（裸 `Uuid` + `Id` 访问器，先例 = `crate::share_link::ShareLinkRow`）。
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AttachmentRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub issue_id: Option<Uuid>,
    pub comment_id: Option<Uuid>,
    pub uploader_type: String,
    pub uploader_id: Uuid,
    pub filename: String,
    pub url: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub created_at: DateTime<Utc>,
    pub chat_session_id: Option<Uuid>,
    pub chat_message_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    /// 抓取上下文的历史副本标记（迁移 `411` 的索引列）。
    ///
    /// 🔴 `DELETE` 与 `GET` 都把它当**不存在**处理（上游 `file.go:1450-1455` 逐字：
    /// "Captured-context attachments are immutable historical copies"）。
    pub source_context_id: Option<Uuid>,
}

impl AttachmentRow {
    #[must_use]
    pub fn id(&self) -> Id {
        Id(self.id)
    }

    #[must_use]
    pub fn workspace_id(&self) -> Id {
        Id(self.workspace_id)
    }

    #[must_use]
    pub fn uploader(&self) -> UploaderType {
        UploaderType::from_db(&self.uploader_type)
    }

    /// 是不是「抓取上下文的历史副本」（这类行对 `GET` / `DELETE` 都不可见）。
    #[must_use]
    pub fn is_captured(&self) -> bool {
        self.source_context_id.is_some()
    }
}

/// `DeleteAttachment` 的返回物（上游 `DeleteAttachmentRow`，逐字段）。
///
/// ⚠️ `changed == false` 时另外两个字段恒 **0**（上游用 `COALESCE(…, 0)`，逐字）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeleteOutcome {
    /// 真的删掉了一行（`EXISTS(SELECT 1 FROM deleted)`）。
    ///
    /// 语义差别已登记为偏离（`docs/32` §50）：上游在**持有 owner lock** 的事务里删
    /// （`withAttachmentOwnerLock`），所以"等锁期间被别人删掉"会走 `pgx.ErrNoRows` 分支；
    /// 本仓**不实现**那个 owner lock（它是并发护栏，不是契约），因此**并发双删**里
    /// 后到的那条会拿到 `changed = false` 而不是 404。handler 对 `changed = false`
    /// **按 404 应答**（与上游那条分支同一个状态码），故对外可观测行为一致。
    pub changed: bool,
    /// 被 bump 的 `issue.revision`（`changed == false` 时为 0）。
    pub issue_revision: i64,
    /// 被 bump 的 `comment.revision`（`changed == false` 时为 0）。
    pub comment_revision: i64,
}

/// `attachment` 表仓储。
#[derive(Clone, Debug)]
pub struct AttachmentRepo {
    pool: PgPool,
}

impl AttachmentRepo {
    #[must_use]
    pub fn new(db: &Db) -> Self {
        Self {
            pool: db.pool().clone(),
        }
    }

    fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// `GetAttachment`：`WHERE id = $1 AND workspace_id = $2`。
    ///
    /// 跨 workspace ⇒ `RepoError::NotFound`（handler 翻译成 **404**，不是 403）。
    pub async fn get(&self, workspace_id: Id, id: Id) -> Result<AttachmentRow> {
        sqlx::query_as::<_, AttachmentRow>(
            "SELECT id, workspace_id, issue_id, comment_id, uploader_type, uploader_id, \
                    filename, url, content_type, size_bytes, created_at, chat_session_id, \
                    chat_message_id, task_id, source_context_id \
             FROM attachment WHERE id = $1 AND workspace_id = $2",
        )
        .bind(id.0)
        .bind(workspace_id.0)
        .fetch_optional(self.pool())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)
    }

    /// `GetAttachmentByIDOnly`：`WHERE id = $1`（**故意不带 workspace 条件**）。
    ///
    /// 🔴 **授权中性**：调用方**必须**自己验成员。见模块头的表。
    pub async fn get_by_id_only(&self, id: Id) -> Result<AttachmentRow> {
        sqlx::query_as::<_, AttachmentRow>(
            "SELECT id, workspace_id, issue_id, comment_id, uploader_type, uploader_id, \
                    filename, url, content_type, size_bytes, created_at, chat_session_id, \
                    chat_message_id, task_id, source_context_id \
             FROM attachment WHERE id = $1",
        )
        .bind(id.0)
        .fetch_optional(self.pool())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)
    }

    /// `ListAttachmentsByIssue`：`WHERE issue_id = $1 AND workspace_id = $2 ORDER BY created_at ASC`。
    ///
    /// ⚠️ 上游**不带** `source_context_id IS NULL` 那一格（那是两个
    /// `ListSourceContext*Attachments` 专用查询的口径）⇒ 本仓照搬：列表**会**包含
    /// 抓取上下文的历史副本。已登记为偏离（`docs/32` §50）。
    pub async fn list_by_issue(
        &self,
        issue_id: Id,
        workspace_id: Id,
    ) -> Result<Vec<AttachmentRow>> {
        sqlx::query_as::<_, AttachmentRow>(
            "SELECT id, workspace_id, issue_id, comment_id, uploader_type, uploader_id, \
                    filename, url, content_type, size_bytes, created_at, chat_session_id, \
                    chat_message_id, task_id, source_context_id \
             FROM attachment WHERE issue_id = $1 AND workspace_id = $2 ORDER BY created_at ASC",
        )
        .bind(issue_id.0)
        .bind(workspace_id.0)
        .fetch_all(self.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// `DeleteAttachment`：删一行（`source_context_id IS NULL` 才删得动）+ bump 两侧 revision。
    ///
    /// 整条 SQL 与上游 `attachment.sql:245-264` **逐字**对应（含 CTE 形状）。
    /// `changed == false` = 行不存在、或是抓取上下文的副本、或已被并发删掉。
    pub async fn delete(&self, workspace_id: Id, id: Id) -> Result<DeleteOutcome> {
        let row: Option<(bool, i64, i64)> = sqlx::query_as(
            "WITH deleted AS ( \
                 DELETE FROM attachment \
                 WHERE attachment.id = $1 AND attachment.workspace_id = $2 \
                   AND attachment.source_context_id IS NULL \
                 RETURNING issue_id, comment_id \
             ), bumped_issue AS ( \
                 UPDATE issue SET revision = revision + 1 \
                 WHERE id IN (SELECT issue_id FROM deleted WHERE issue_id IS NOT NULL) \
                 RETURNING revision \
             ), bumped_comment AS ( \
                 UPDATE comment SET revision = revision + 1 \
                 WHERE id IN (SELECT comment_id FROM deleted WHERE comment_id IS NOT NULL) \
                 RETURNING revision \
             ) \
             SELECT EXISTS(SELECT 1 FROM deleted) AS changed, \
                    COALESCE((SELECT revision FROM bumped_issue), 0)::bigint AS issue_revision, \
                    COALESCE((SELECT revision FROM bumped_comment), 0)::bigint AS comment_revision",
        )
        .bind(id.0)
        .bind(workspace_id.0)
        .fetch_optional(self.pool())
        .await
        .map_err(map_sqlx_err)?;

        Ok(match row {
            Some((changed, issue_revision, comment_revision)) => DeleteOutcome {
                changed,
                issue_revision,
                comment_revision,
            },
            // CTE 无 RETURNING 后的行 ⇒ 只在并发下可达（上游同款，走 404 分支）。
            None => DeleteOutcome::default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uploader_type_round_trips() {
        assert_eq!(UploaderType::from_db("member"), UploaderType::Member);
        assert_eq!(UploaderType::from_db("agent"), UploaderType::Agent);
        assert_eq!(UploaderType::Member.as_str(), "member");
        assert_eq!(UploaderType::Agent.as_str(), "agent");
    }

    #[test]
    fn uploader_type_defaults_to_member_on_garbage() {
        // CHECK 约束保证只可能是两个值；这里不因脏数据让整个列表 500。
        assert_eq!(UploaderType::from_db("MEMBER"), UploaderType::Member);
        assert_eq!(UploaderType::from_db(""), UploaderType::Member);
    }

    #[test]
    fn default_delete_outcome_is_unchanged() {
        let o = DeleteOutcome::default();
        assert!(!o.changed);
        assert_eq!(o.issue_revision, 0);
        assert_eq!(o.comment_revision, 0);
    }
}
