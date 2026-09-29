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

use sqlx::PgPool;

mod crud;
mod input;
mod query;
mod reaction;
mod resolve;
mod row;
mod util;

#[cfg(test)]
mod tests;

pub use input::{CommentCursor, CommentFilter, CommentList, CommentPatch, NewComment};

pub use row::{CommentReactionRow, CommentRow};

/// `comment` / `comment_reaction` 仓储。
#[derive(Clone)]
pub struct CommentRepo {
    pool: PgPool,
}
