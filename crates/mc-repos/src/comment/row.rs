//! `comment` 的 SQL 列常量与行结构。

use super::util::parse_author_type;
use chrono::{DateTime, Utc};
use mc_core::comment::CommentAuthorType;
use mc_core::id::Id;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// W0-B2 对齐上游：`content`→API 字段 `body`（`AS body`）、`author_id` 上游是 `UUID` ⇒ `::text` 投影；
/// `routing_escalation` 是 compat 列（`migrations/compat/537_local_only_columns.up.sql`）。
pub(super) const COLUMNS: &str =
    "id, workspace_id, issue_id, parent_id, author_type, author_id::text AS author_id, \
                       content AS body, source_task_id, routing_escalation, revision, \
                       resolved_at, deleted_at, created_at, updated_at";

pub(super) const REACTION_COLUMNS: &str =
    "id, comment_id, workspace_id, actor_type, actor_id::text AS actor_id, emoji, created_at";

/// “该评论仍挂着活后代”的相关子查询（外层表必须别名成 `c`）。
///
/// 向下递归走 `comment_parent_idx`，代价按**该评论自己的子树**计，
/// 不是整个 issue；所以窗口筛选和线程回补可以共用同一条谓词。
pub(super) const LIVE_DESCENDANT_EXISTS: &str = "EXISTS ( \
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
