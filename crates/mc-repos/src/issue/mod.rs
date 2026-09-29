//! `IssueRepo` —— `issue` 表的 DB-backed 仓储（M2-A / LUM-1348）。
//!
//! 对应上游 multica `server/pkg/db/queries/issue.sql` + `server/internal/handler/issue.go`
//! 的数据访问部分。覆盖：
//!
//! - CRUD（`create` / `get` / `get_by_identifier` / `update` / `delete`）
//! - 过滤列表（`list` / `list_with_total`）：status(es) / priorit(ies) / assignee /
//!   creator / parent / project / stage / 全文 `q` / `include_closed` / 分页 / 排序
//! - 子 issue（`children_of` / `children_of_parents` / `child_progress`）
//! - 聚合（`grouped_counts`）+ 目录（`terminal_status_keys`）
//! - 批量（`batch_update` / `batch_delete`）、拖拽排序（`move_issue`）
//! - 元数据 / 属性 JSONB（`get_metadata` / `set_metadata_key` / … / `set_property`）
//! - reactions（`list_reactions` / `add_reaction` / `remove_reaction`，表在 `0004`）
//!
//! 约定与 M1 各 Repo 保持一致（见 `crate::invitation` / `crate::workspace`）：
//! - Row 用原始 `Uuid` / `String` 字段 + `Id` / 领域类型访问器（`mc_core::Id` 没有
//!   sqlx impl，所以行结构不直接用 `Id`）
//! - 错误统一走 `crate::workspace::map_sqlx_err`（`RowNotFound` → `NotFound`，
//!   `23505` → `Conflict`）
//! - 全部走 sqlx 运行时 builder（`query_as` / `query` + `.bind()`），不用 compile-time
//!   宏，因此构建期不需要数据库连接
//! - PG 集成测试标 `#[ignore]`，由 `MULTICA_TEST_DATABASE_URL` 触发
//!
//! 与上游的**有意偏离**（详见 `docs/11-M2-ISSUE.md` §5）：
//! - `identifier` 前缀取自 `workspace.slug`（本仓 0001 的 `workspace` 表没有
//!   `issue_prefix` 列），上游是 workspace 上独立配置的前缀
//! - `properties` 用 `issue.properties` JSONB 列（上游是独立 `issue_properties` 表）
//! - reaction 的 `actor_type` 用 `'user'`（不是上游的 `'member'`），跟随 0004 的 CHECK

use mc_db::Db;

use crate::workspace::map_sqlx_err;
use crate::{RepoError, RepoWithDb, Result};

// ---------------------------------------------------------------------------
// 子模块（门 ⑩ 拆分：原单文件 1949 行 → 13 个文件，最大的 547 行）
//
// 这是一次**纯搬家**：函数体、SQL 字面量、注释、可见性一律逐字保留；
// 对外的公开面由下面的 `pub use` 原样再导出，因此 `crate::issue::*` 的
// 所有既有引用（`crate::issue_table`、`mc-http`、`mc-bench` …）零变化。
//
// 拆法按**职责**而不是按行数（先例 D10）：行结构 / 输入结构 / 纯函数各归一，
// `impl IssueRepo` 的方法按 CRUD・查询・批量・JSONB・reactions 五块分文件。
// ---------------------------------------------------------------------------

mod bulk;
mod crud;
mod input;
mod jsonb;
mod query;
mod reaction;
mod row;
mod util;

#[cfg(test)]
mod db_tests;
#[cfg(test)]
mod tests;

pub use input::{IssueFilter, IssueGroupField, IssueOrderBy, IssueUpdate, NewIssue};
pub use row::{ChildProgressRow, GroupedCountRow, IssueReactionRow, IssueRow, IssueStatusRow};
pub use util::{
    derive_move_position, issue_prefix_from_slug, parse_assignee_type, parse_issue_origin,
    split_comma_param,
};

/// `GET /api/issues` 默认页大小（与上游一致：100 / 上限 100）。
pub const LIST_DEFAULT_LIMIT: i64 = 100;
/// `GET /api/issues` 页大小上限。
pub const LIST_MAX_LIMIT: i64 = 100;
/// `GET /api/issues/search` 默认页大小。
pub const SEARCH_DEFAULT_LIMIT: i64 = 20;
/// `GET /api/issues/search` 页大小上限。
pub const SEARCH_MAX_LIMIT: i64 = 50;
/// `GET /api/issues/children?parent_ids=` 的父节点数量上限（上游 `listChildrenByParentsLimit`）。
pub const CHILDREN_PARENTS_MAX: usize = 200;
/// 并发创建时 `UNIQUE(workspace_id, number)` 冲突后的重试次数。
const NUMBER_ALLOC_RETRIES: usize = 4;

/// `issue` 表全列（所有 `SELECT` 共用，避免列顺序漂移）。
///
/// M2-D 起 `pub(crate)`：`crate::issue_table` 的 `/rows` 查询复用同一份列清单。
/// W0-B2：上游 `assignee_id`/`creator_id` 是 `UUID` ⇒ 读出时 `::text`（`prefixed_issue_columns()` 仍合法）。
pub(crate) const ISSUE_COLUMNS: &str =
    "id, workspace_id, number, identifier, title, description, status, \
     status_name, priority, assignee_type, assignee_id::text AS assignee_id, creator_type, creator_id::text AS creator_id, \
     parent_issue_id, project_id, position, stage, start_date, due_date, last_activity_at, \
     revision, metadata, properties, triage_state, origin, origin_task_id, source_context_id, \
     created_at, updated_at";

/// `list` / `count` 共用的 WHERE 片段（$1..$13，见 `bind_list_filters`）。过滤值来自查询串（任意字符串）⇒ 比较侧保留 `::text`：非法 UUID 照旧「匹配不到任何行」（不 500）。
const LIST_WHERE: &str = "workspace_id = $1 \
     AND ($2::text[] IS NULL OR status = ANY($2::text[])) \
     AND ($3::text[] IS NULL OR priority = ANY($3::text[])) \
     AND ($4::text IS NULL OR assignee_type = $4::text) \
     AND ($5::text[] IS NULL OR assignee_id::text = ANY($5::text[])) \
     AND ($6::text IS NULL OR creator_id::text = $6::text) \
     AND ($7::uuid IS NULL OR parent_issue_id = $7::uuid) \
     AND ($8::uuid IS NULL OR project_id = $8::uuid) \
     AND ($9::int4 IS NULL OR stage = $9::int4) \
     AND ($10::text IS NULL OR (title ILIKE '%' || $10::text || '%' \
          OR COALESCE(description, '') ILIKE '%' || $10::text || '%' \
          OR identifier ILIKE '%' || $10::text || '%')) \
     AND ($11::boolean OR NOT (status = ANY($12::text[]))) \
     AND (NOT $13::boolean OR parent_issue_id IS NULL)";

/// `IssueRepo`。
#[derive(Clone)]
pub struct IssueRepo {
    db: Db,
}

impl IssueRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

impl RepoWithDb for IssueRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}
