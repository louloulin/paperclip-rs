//! `inbox` 的输入 / 过滤 / 游标 / 页与 facet 结构。

use std::collections::BTreeMap;

use super::row::InboxItemRow;
use chrono::{DateTime, Utc};
use mc_core::Id;

/// 新建 inbox item 的入参（item 的**产生**逻辑属 M3，本仓只提供写入面 + 测试夹具）。
#[derive(Debug, Clone)]
pub struct NewInboxItem {
    /// 缺省时由仓储生成 v4 UUID。
    pub id: Option<Id>,
    pub workspace_id: Id,
    pub user_id: Id,
    pub issue_id: Option<Id>,
    pub actor_type: String,
    pub actor_id: String,
    pub category: String,
    pub title: String,
    pub body: Option<String>,
}

/// 归档视图的过滤条件（上游 `ArchivedInboxFacetsParams` 的可过滤部分）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArchivedInboxFilter {
    /// 有序、去重后的 issue status 集合（空 = 不过滤）。
    pub statuses: Vec<String>,
    /// 有序、去重后的 issue priority 集合（空 = 不过滤）。
    pub priorities: Vec<String>,
    /// 有序、去重后的 actor 集合，形如 `user:<uuid>` / `system`（空 = 不过滤）。
    pub actors: Vec<String>,
    /// 只看未读组。
    pub unread_only: bool,
    /// 只取某个 issue 组（`None` = 全部）。
    pub group_id: Option<Id>,
}

/// `archived/page` 的游标（`(created_at, id)` 位置，倒序翻页）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedCursor {
    pub created_at: DateTime<Utc>,
    pub id: Id,
}

/// `archived/page` 的一页。
#[derive(Debug, Clone)]
pub struct ArchivedInboxPage {
    pub items: Vec<InboxItemRow>,
    /// 还有下一页（内部多取一行判断，返回时已截断）。
    pub has_more: bool,
}

/// 归档视图的 facet 计数（`archived/facets`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArchivedInboxFacets {
    pub statuses: BTreeMap<String, i64>,
    pub priorities: BTreeMap<String, i64>,
    pub actors: BTreeMap<String, i64>,
    pub unread_count: i64,
}

/// 跨 workspace 的未读汇总项（`unread-summary`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceUnread {
    pub workspace_id: Id,
    pub count: i64,
}
