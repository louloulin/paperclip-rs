//! `comment` 的输入 / 过滤 / 游标结构。

use super::row::CommentRow;
use chrono::{DateTime, Utc};
use mc_core::comment::CommentAuthorType;
use mc_core::id::Id;

/// 默认窗口大小（根评论条数，非评论总数）。
pub const COMMENT_DEFAULT_LIMIT: u32 = 50;
/// 单次请求允许的最大窗口（防止 agent 一次拉爆整个 issue）。
pub const COMMENT_MAX_LIMIT: u32 = 200;
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
