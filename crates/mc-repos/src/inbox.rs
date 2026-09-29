//! `InboxRepo` — DB-backed inbox 仓储（表 `inbox_item`）。
//!
//! 对应 upstream `multica/server/pkg/db/queries/{inbox,inbox_archive}.sql` 与
//! `server/internal/handler/{inbox,inbox_archive}.go`。
//!
//! ## 与上游的字段偏离（本仓 `0001_init.up.sql` 的 `inbox_item` 词汇表）
//!
//! | 上游列 | 本仓列 | 说明 |
//! | --- | --- | --- |
//! | `recipient_type` + `recipient_id` | `user_id UUID` | 收件人恒为人类 user，写入时固定 `recipient_type = 'user'`（agent 收件箱属 M3+） |
//! | `read BOOLEAN` | `read_at TIMESTAMPTZ` | 语义等价（`read_at IS NOT NULL` ⟺ 已读），且多保留"何时读的"；写入时**双写**上游布尔，本地以 `read_at` 为准 |
//! | `archived BOOLEAN` | `archived_at TIMESTAMPTZ` | 同上 |
//! | `type` | `category TEXT` | 本地字段仍叫 `category`（读出时 `type AS category`） |
//! | `severity` / `details` | **不存在** | 故 archived 视图没有 comment anchor 行（上游靠 `details->>'comment_id'` 二次补行）；本仓一组只返回最新一行 |
//! | `actor_type/actor_id` | 同名 | 本仓 `actor_id` 是 `TEXT`（上游 `UUID`） |
//!
//! 所有"幂等"语义（`mark_read` / `mark_unread` / `archive` / `unarchive`）靠
//! `COALESCE` + `IS NULL` 条件实现：重复调用第二次是空写，返回值不变。
//!
//! ## 分组（group）语义
//!
//! 上游 inbox 是 **Linear 式按 issue 分组**：同一个 issue 的多条通知在 UI 上只渲染
//! 最新一条，且已读/归档都作用在**整组**。本仓照搬该分组键：
//! `COALESCE(i.issue_id, i.id)`（无 issue 的通知自成一组）。因此：
//! - `unread_count` 数的是**原始未读行**（与上游 `CountUnreadInbox` 一致）；
//! - `unread_summary` 与 `archive_all_read` 数的是**组**（与上游
//!   `CountUnreadInboxByWorkspace` / `ArchiveAllReadInbox` 一致）；
//! - `archive` / `unarchive` 是 issue 级（同组一起动），`mark_read` / `mark_unread`
//!   是 item 级（上游注释里明确解释了为什么这两者粒度不同）。

mod crud;
mod input;
mod query;
mod row;
mod state;

#[cfg(test)]
mod tests;

pub use crud::InboxRepo;
pub use input::{
    ArchivedCursor, ArchivedInboxFacets, ArchivedInboxFilter, ArchivedInboxPage, NewInboxItem,
    WorkspaceUnread,
};
pub use row::InboxItemRow;
pub use state::BUILTIN_TERMINAL_STATUS_KEYS;
