//! inbox 响应 DTO：字段名与上游 `InboxItemResponse` 逐字对齐。
//!
//! 从 `routes/inbox.rs` 拆出（门 ⑩ 第 8 批）。**0 路由、0 行为变更**：
//! 父模块用 `pub(crate) use` / `pub use` 把符号原样重导出，外部路径逐字不变。

use std::collections::BTreeMap;

use mc_core::Id;
use mc_repos::inbox::InboxItemRow;
use serde::{Deserialize, Serialize};

use crate::routes::inbox::query::list_body_preview;
use crate::routes::inbox::{RECIPIENT_TYPE_USER, SEVERITY_INFO};

// ---------------------------------------------------------------------------
// DTO（字段名与上游 `InboxItemResponse` 逐字对齐）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct InboxItemDto {
    pub id: String,
    pub workspace_id: String,
    pub recipient_type: String,
    pub recipient_id: String,
    /// 上游字段名是 `type`（本仓列名 `category`）。
    pub r#type: String,
    pub severity: String,
    pub issue_id: Option<String>,
    pub title: String,
    pub body: Option<String>,
    pub read: bool,
    pub archived: bool,
    pub created_at: String,
    pub issue_status: Option<String>,
    pub issue_priority: Option<String>,
    pub actor_type: String,
    pub actor_id: String,
    pub details: serde_json::Value,
}

impl InboxItemDto {
    /// 单条响应（保留完整 body）。
    pub(super) fn from_row(row: &InboxItemRow) -> Self {
        Self {
            id: row.id().as_string(),
            workspace_id: Id::from(row.workspace_id).as_string(),
            recipient_type: RECIPIENT_TYPE_USER.to_string(),
            recipient_id: Id::from(row.user_id).as_string(),
            r#type: row.category.clone(),
            severity: SEVERITY_INFO.to_string(),
            issue_id: row.issue_id.map(|id| Id::from(id).as_string()),
            title: row.title.clone(),
            body: row.body.clone(),
            read: row.is_read(),
            archived: row.is_archived(),
            created_at: row.created_at.to_rfc3339(),
            issue_status: row.issue_status.clone(),
            issue_priority: row.issue_priority.clone(),
            actor_type: row.actor_type.clone(),
            actor_id: row.actor_id.clone(),
            details: serde_json::json!({}),
        }
    }

    /// 列表响应：`new_comment` 且有 issue 的行按上游规则截断 body。
    pub(super) fn from_list_row(row: &InboxItemRow) -> Self {
        let mut dto = Self::from_row(row);
        dto.body = list_body_preview(&row.category, row.issue_id.is_some(), row.body.as_deref());
        dto
    }
}

#[derive(Debug, Serialize)]
pub(super) struct ArchivedPageDto {
    pub(super) items: Vec<InboxItemDto>,
    pub(super) next_cursor: Option<String>,
    pub(super) has_more: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct CountDto {
    pub(super) count: i64,
}

#[derive(Debug, Serialize)]
pub(super) struct WorkspaceUnreadDto {
    pub(super) workspace_id: String,
    pub(super) count: i64,
}

#[derive(Debug, Serialize)]
pub(super) struct ArchivedFacetsDto {
    pub(super) statuses: BTreeMap<String, i64>,
    pub(super) priorities: BTreeMap<String, i64>,
    pub(super) actors: BTreeMap<String, i64>,
    pub(super) unread_count: i64,
}

/// `archived/page` 游标载荷（上游同名结构的字段一致；外层编码换成 hex）。
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ArchivedCursorWire {
    pub(super) time: String,
    pub(super) id: String,
    pub(super) scope: String,
}
