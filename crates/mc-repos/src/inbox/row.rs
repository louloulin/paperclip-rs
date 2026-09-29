//! `InboxItemRow` 与它依赖的 SQL 片段常量（`ITEM_COLUMNS` / `ITEM_FROM` /
//! `NEWEST_ARCHIVED_CTE`），以及手写的 `sqlx::FromRow` 映射。

use chrono::{DateTime, Utc};
use mc_core::Id;
use sqlx::Row;
use uuid::Uuid;

/// item 列表/单条的公共列清单 + issue 投影。
///
/// 所有查询共用同一份列清单（`ITEM_COLUMNS`），任一查询的列漂移都会在运行时
/// 立刻暴露（`try_get` 失败），与上游"两个查询列不一致就编译不过"的意图一致。
pub(super) const ITEM_COLUMNS: &str = "i.id, i.workspace_id, i.recipient_id AS user_id, i.issue_id, i.actor_type, COALESCE(i.actor_id::text, '') AS actor_id, i.type AS category, i.title, i.body, i.read_at, \
     i.archived_at, i.created_at, iss.status AS issue_status, iss.priority AS issue_priority";

/// `inbox_item` 连接 `issue` 的 FROM 子句（issue 投影必需）。
pub(super) const ITEM_FROM: &str = "FROM inbox_item i \
     LEFT JOIN issue iss ON iss.id = i.issue_id AND iss.workspace_id = i.workspace_id";

/// 归档视图的"分组代表行"CTE：每个 issue 组只取最新一条，且排除了本组仍有
/// 活跃（未归档）行的 issue。
///
/// 排除逻辑与上游 `ListArchivedInboxItems` 完全一致：归档是 issue 级的，某 issue
/// 归档后又来了新通知，旧归档行会留在原处、新活跃行进入主列表——若不排除，同一个
/// issue 会同时出现在两个列表里。
pub(super) const NEWEST_ARCHIVED_CTE: &str = "WITH newest AS MATERIALIZED ( \
        SELECT DISTINCT ON (COALESCE(i.issue_id, i.id)) \
               i.id, i.issue_id, i.created_at, (i.read_at IS NOT NULL) AS is_read, \
               CASE WHEN i.actor_type = 'system' THEN 'system' \
                    ELSE i.actor_type || ':' || i.actor_id END AS actor \
        FROM inbox_item i \
        WHERE i.workspace_id = $1 AND i.recipient_id = $2 AND i.archived_at IS NOT NULL \
          AND (i.issue_id IS NULL OR NOT EXISTS ( \
              SELECT 1 FROM inbox_item active \
              WHERE active.workspace_id = i.workspace_id \
                AND active.recipient_id = i.recipient_id \
                AND active.issue_id = i.issue_id \
                AND active.archived_at IS NULL)) \
        ORDER BY COALESCE(i.issue_id, i.id), i.created_at DESC, i.id DESC \
     ), projected AS ( \
        SELECT newest.*, iss.status AS issue_status, iss.priority AS issue_priority \
        FROM newest \
        LEFT JOIN issue iss ON iss.id = newest.issue_id AND iss.workspace_id = $1 \
     ), matched AS ( \
        SELECT projected.*, \
               (cardinality($3::text[]) = 0 OR issue_status = ANY($3::text[])) AS status_match, \
               (cardinality($4::text[]) = 0 OR issue_priority = ANY($4::text[])) AS priority_match, \
               (cardinality($5::text[]) = 0 OR actor = ANY($5::text[])) AS actor_match, \
               (NOT $6::boolean OR NOT is_read) AS read_match \
        FROM projected \
     )";

/// 一个 inbox item（含关联 issue 的 status / priority 投影）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InboxItemRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub user_id: Uuid,
    pub issue_id: Option<Uuid>,
    pub actor_type: String,
    pub actor_id: String,
    /// 上游的 `type`（本仓列名 `category`）。
    pub category: String,
    pub title: String,
    pub body: Option<String>,
    pub read_at: Option<DateTime<Utc>>,
    pub archived_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    /// 关联 issue 的 `status`；无 issue 时 `None`。
    pub issue_status: Option<String>,
    /// 关联 issue 的 `priority`；无 issue 时 `None`。
    pub issue_priority: Option<String>,
}

impl InboxItemRow {
    pub fn id(&self) -> Id {
        Id::from(self.id)
    }

    pub fn is_read(&self) -> bool {
        self.read_at.is_some()
    }

    pub fn is_archived(&self) -> bool {
        self.archived_at.is_some()
    }
}

// `mc_core::Id` 尚未实现 sqlx `Decode`/`Encode`，故本结构用原始 `Uuid`/`String`
// 字段（见模块头与 M1 各 Repo 的同一约定）。
impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for InboxItemRow {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> sqlx::Result<Self> {
        Ok(Self {
            id: row.try_get("id")?,
            workspace_id: row.try_get("workspace_id")?,
            user_id: row.try_get("user_id")?,
            issue_id: row.try_get("issue_id")?,
            actor_type: row.try_get("actor_type")?,
            actor_id: row.try_get("actor_id")?,
            category: row.try_get("category")?,
            title: row.try_get("title")?,
            body: row.try_get("body")?,
            read_at: row.try_get("read_at")?,
            archived_at: row.try_get("archived_at")?,
            created_at: row.try_get("created_at")?,
            // 单条的 `RETURNING` 没有 issue 投影列；列表查询有。
            issue_status: opt_text(row, "issue_status")?,
            issue_priority: opt_text(row, "issue_priority")?,
        })
    }
}

/// `try_get` 一个可能不在结果集里的可空文本列。
fn opt_text(row: &sqlx::postgres::PgRow, name: &str) -> sqlx::Result<Option<String>> {
    match row.try_get::<Option<String>, _>(name) {
        Ok(v) => Ok(v),
        // 列不存在（`RETURNING` 子句）→ None；其他错误（类型不符）原样抛出。
        Err(sqlx::Error::ColumnNotFound(_)) => Ok(None),
        Err(e) => Err(e),
    }
}
