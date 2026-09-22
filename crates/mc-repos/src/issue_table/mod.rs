//! `IssueTableRepo` —— issue table 查询面的规格 / SQL 编译 / 执行（M2-D / LUM-1355）。
//!
//! 对应上游 multica：
//! - `server/internal/handler/issue_table_query.go:437` `compileIssueTableQuery`（WHERE 编译 + 指纹）
//! - `server/internal/handler/issue_table_group.go:199` `resolveIssueTableGroup`、
//!   `:714` `predicate`、`:892` `ListIssueTableGroups`
//! - `server/internal/handler/issue_table_rows.go:45` `resolvedIssueTableSort`、
//!   `:253` `ListIssueTableRows`
//! - `server/internal/handler/issue_table_facets.go:180` `issueTableFacetsResponse`
//!
//! 职责边界：**这里只有“已校验的查询规格 → SQL → 行”**。HTTP 概念（`query_fingerprint`、
//! cursor 的 base64/JSON 编解码、400/422 判定、DTO 字段名）全在
//! `crates/mc-http/src/routes/issue_table.rs`，与上游把两者放在同一个 handler 文件的
//! 做法不同，理由是 R7（单文件 800 行）与 M2-A 的文件已经超标（见 `docs/14-M2-TABLE.md` §6）。
//!
//! 本模块按 R7 拆成三份（每个文件都在 800 行以内）：
//! - `issue_table/mod.rs`（本文件）：**查询规格与结果类型**（`TableGroupSpec` / `TableFilter` /
//!   `TableOrder` / `TableRow` / `Table*Page`），不含 SQL 文本，也不依赖 sqlx 的 builder
//! - `issue_table/sql.rs`：`$n` 参数绑定、WHERE 编译、分组/排序解析、行结构 —— `issue_table/sql.rs`
//! - `issue_table/repo.rs`：`IssueTableRepo` 的三个查询入口（groups / rows / facets）
//!
//! 约定与 M2-A（`crate::issue`）一致：运行时 sqlx builder（`query_as` / `query` + `.bind()`，
//! 不用 compile-time 宏，构建期不需要数据库）、错误走 `crate::workspace::map_sqlx_err`、
//! PG 集成测试 `#[ignore]` + `MULTICA_TEST_DATABASE_URL`。
//!
//! 与上游的**有意偏离**（逐条论证见 `docs/14-M2-TABLE.md` §4）：
//! 1. 不开启 `REPEATABLE READ READ ONLY` 快照事务（本仓 `Db` 没有 `TxStarter` 等价物）
//! 2. 角色列（`assignee_id` / `creator_id`）在本仓是 `TEXT`，上游是 `uuid`；
//!    谓词用 `::text` 而不是 `:::uuid`，非法 uuid 由 HTTP 层拦成 400
//! 3. `assignee_type` 取值是 `user|agent|squad|autopilot`（本仓 0001 的 CHECK），
//!    上游是 `member|agent|squad`；`my` 作用域因此用 `'user'`
//! 4. `label` / `property` / `parent` / `status_category` / `compound` 分组与
//!    `working_agents`、`property` facet 不支持（本仓没有 labels / properties 表；
//!    承接见 LUM-1370 + W0-B2），HTTP 层显式 422，绝不编造计数
//! 5. 没有 `squad_member` 表 → `scope.relation=involved` 只覆盖 agent 归属与
//!    squad leader 两条支路
//! 6. `priority` 分组是本仓**新增**维度（上游没有），见 §4.6
//! 7. facet 逐个查询而非上游的 `GROUPING SETS` 批量扫描（契约一致，代价是 N 次扫描）

use chrono::{DateTime, Utc};
use uuid::Uuid;

use mc_core::Id;

use crate::issue::IssueRow;

pub(crate) mod repo;
pub(crate) mod sql;

pub use repo::IssueTableRepo;

/// `/api/issues/table/*` 默认页大小（上游 `issueTableDefaultPageSize`）。
pub const TABLE_DEFAULT_PAGE_SIZE: i64 = 50;
/// `/api/issues/table/*` 页大小上限（上游 `issueTableMaxPageSize`）。
pub const TABLE_MAX_PAGE_SIZE: i64 = 100;
/// 单次请求的 facet 数量上限（上游 `issueTableMaxFacets`）。
pub const TABLE_MAX_FACETS: usize = 32;
/// 未指派分组的 `group_value`（上游 `__unassigned__`）。
pub const GROUP_VALUE_UNASSIGNED: &str = "__unassigned__";
/// 无 project 分组的 `group_value`（上游 `__no_project__`）。
pub const GROUP_VALUE_NO_PROJECT: &str = "__no_project__";
/// facet 值里“空”桶的 key（上游 `__none__`，注意与分组用的 `__unassigned__` 不同）。
pub const FACET_VALUE_NONE: &str = "__none__";

/// 7 个内置 status key 与其生命周期分类（本仓 `issue_status.category` 只有 open/closed）。
const BUILTIN_STATUSES: [(&str, &str); 7] = [
    ("backlog", "open"),
    ("todo", "open"),
    ("in_progress", "open"),
    ("in_review", "open"),
    ("blocked", "open"),
    ("done", "closed"),
    ("cancelled", "closed"),
];

/// 5 个内置 priority（本仓 `issue.priority` 的 CHECK），也是 `priority` 分组的固定顺序。
const BUILTIN_PRIORITIES: [&str; 5] = ["urgent", "high", "medium", "low", "none"];

/// 可作为 assignee / creator 的角色类型（本仓 0001 的 CHECK）。
pub const ACTOR_TYPES: [&str; 4] = ["user", "agent", "squad", "autopilot"];

// ---------------------------------------------------------------------------
// 查询规格
// ---------------------------------------------------------------------------

/// 分组维度。上游的 `none|status|status_category|assignee|project|parent|compound|property`
/// 里，本仓只支持前 5 个中的 4 个（无 `parent`），外加本仓新增的 `priority`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableGroupKind {
    /// 分组头（上游 `group.kind=none`）：仅 `/rows` 允许，`/groups` 会 400。
    None,
    Status,
    /// 本仓新增（上游无此分组维度）。
    Priority,
    Assignee,
    Project,
}

impl TableGroupKind {
    /// 上游 JSON 里的 `group.kind` 取值。
    pub fn key(self) -> &'static str {
        match self {
            TableGroupKind::None => "none",
            TableGroupKind::Status => "status",
            TableGroupKind::Priority => "priority",
            TableGroupKind::Assignee => "assignee",
            TableGroupKind::Project => "project",
        }
    }

    /// 解析 `group.kind`。`None` = 未知取值，由 HTTP 层决定 400 还是 422。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "none" => Some(TableGroupKind::None),
            "status" => Some(TableGroupKind::Status),
            "priority" => Some(TableGroupKind::Priority),
            "assignee" => Some(TableGroupKind::Assignee),
            "project" => Some(TableGroupKind::Project),
            _ => None,
        }
    }
}

/// 分组规格（HTTP 层已校验：`property_id`/`primary`/`secondary` 等不支持字段已在 DTO 层拒收）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableGroupSpec {
    pub kind: TableGroupKind,
    /// 上游只有 property 分组消费它；本仓扩展到 status（目录 key）与 priority（5 个固定值）。
    pub include_empty: bool,
}

/// 已解析的 `group_key`（类型层面保证与 `group.kind` 一致，非法 key 在 HTTP 层 400）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableGroupKey {
    /// `group.kind=none`，且 key 为空。
    None,
    Status(String),
    Priority(String),
    /// `None` = `assignee:unassigned`。
    Assignee(Option<TableActor>),
    /// `None` = `project:none`。
    Project(Option<Id>),
}

/// 角色引用（`{"type": "...", "id": "..."}`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableActor {
    pub kind: String,
    pub id: Uuid,
}

/// 查询作用域（上游 `issueTableScope`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableScope {
    Workspace {
        assignee_types: Vec<String>,
    },
    Project {
        project_id: Id,
        assignee_types: Vec<String>,
    },
    Assignee(TableActor),
    Creator(TableActor),
    My {
        actor: TableActor,
        relation: TableMyRelation,
    },
}

impl Default for TableScope {
    fn default() -> Self {
        TableScope::Workspace {
            assignee_types: Vec::new(),
        }
    }
}

/// `scope.relation`（仅 `scope.kind=my`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableMyRelation {
    Assigned,
    Created,
    Involved,
    Any,
}

impl TableMyRelation {
    /// 解析，空串 = `any`（上游默认值）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "" | "any" => Some(TableMyRelation::Any),
            "assigned" => Some(TableMyRelation::Assigned),
            "created" => Some(TableMyRelation::Created),
            "involved" => Some(TableMyRelation::Involved),
            _ => None,
        }
    }
}

/// `filters.date` 的字段（上游只允许 `created_at` / `updated_at`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableDateField {
    CreatedAt,
    UpdatedAt,
}

impl TableDateField {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "created_at" => Some(TableDateField::CreatedAt),
            "updated_at" => Some(TableDateField::UpdatedAt),
            _ => None,
        }
    }

    fn column(self) -> &'static str {
        match self {
            TableDateField::CreatedAt => "i.created_at",
            TableDateField::UpdatedAt => "i.updated_at",
        }
    }
}

/// `filters.date` 区间（`start <= x < end`）。
#[derive(Debug, Clone, PartialEq)]
pub struct TableDateFilter {
    pub field: TableDateField,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

/// issue table 过滤条件。字段名与上游 `issueTableFiltersRequest` 一致，
/// 只保留本仓能诚实实现的部分（其余字段在 HTTP DTO 层 `deny_unknown_fields` 拒收）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TableFilter {
    pub scope: TableScope,
    /// `Option` 语义：`None` = 无过滤；`Some(空)` = 上游的“显式空数组 = 匹配 0 行”。
    pub assignees: Option<Vec<TableActor>>,
    pub include_no_assignee: bool,
    pub statuses: Vec<String>,
    pub priorities: Vec<String>,
    pub creators: Vec<TableActor>,
    pub project_ids: Vec<Id>,
    pub include_no_project: bool,
    pub date: Option<TableDateFilter>,
    pub include_sub_issues: Option<bool>,
    pub search: String,
}

/// facet 维度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableFacetKind {
    Status,
    Priority,
    Assignee,
    Creator,
    Project,
}

impl TableFacetKind {
    /// 上游 JSON 里的 `facets[].kind`。
    pub fn key(self) -> &'static str {
        match self {
            TableFacetKind::Status => "status",
            TableFacetKind::Priority => "priority",
            TableFacetKind::Assignee => "assignee",
            TableFacetKind::Creator => "creator",
            TableFacetKind::Project => "project",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "status" => Some(TableFacetKind::Status),
            "priority" => Some(TableFacetKind::Priority),
            "assignee" => Some(TableFacetKind::Assignee),
            "creator" => Some(TableFacetKind::Creator),
            "project" => Some(TableFacetKind::Project),
            _ => None,
        }
    }
}

/// 排序方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableSortDirection {
    Asc,
    Desc,
}

impl TableSortDirection {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "asc" => Some(TableSortDirection::Asc),
            "desc" => Some(TableSortDirection::Desc),
            _ => None,
        }
    }

    fn sql(self) -> &'static str {
        match self {
            TableSortDirection::Asc => "ASC",
            TableSortDirection::Desc => "DESC",
        }
    }
}

/// 排序字段（上游 `issueTableSortRequest.field` 里本仓支持的部分；
/// `property:<id>` 之类上游走属性排序，见 `docs/14-M2-TABLE.md` §5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableSortField {
    Position,
    Title,
    CreatedAt,
    UpdatedAt,
    LastActivity,
    StartDate,
    DueDate,
    Status,
    Priority,
}

impl TableSortField {
    pub fn key(self) -> &'static str {
        match self {
            TableSortField::Position => "position",
            TableSortField::Title => "title",
            TableSortField::CreatedAt => "created_at",
            TableSortField::UpdatedAt => "updated_at",
            TableSortField::LastActivity => "last_activity",
            TableSortField::StartDate => "start_date",
            TableSortField::DueDate => "due_date",
            TableSortField::Status => "status",
            TableSortField::Priority => "priority",
        }
    }

    /// 解析（空 = 上游默认 `position`）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "" | "position" => Some(TableSortField::Position),
            "title" => Some(TableSortField::Title),
            "created_at" => Some(TableSortField::CreatedAt),
            "updated_at" => Some(TableSortField::UpdatedAt),
            "last_activity" => Some(TableSortField::LastActivity),
            "start_date" => Some(TableSortField::StartDate),
            "due_date" => Some(TableSortField::DueDate),
            "status" => Some(TableSortField::Status),
            "priority" => Some(TableSortField::Priority),
            _ => None,
        }
    }

    /// cursor 里 `sort_value` 的 SQL 转换类型（上游 `resolvedIssueTableSort.castType`）。
    pub fn cast(self) -> &'static str {
        match self {
            TableSortField::Position => "double precision",
            TableSortField::Title => "text",
            TableSortField::CreatedAt
            | TableSortField::UpdatedAt
            | TableSortField::LastActivity => "timestamptz",
            TableSortField::StartDate | TableSortField::DueDate => "date",
            TableSortField::Status | TableSortField::Priority => "integer",
        }
    }

    /// 空值排最后（上游 `nullsLast`）。
    pub fn nulls_last(self) -> bool {
        matches!(
            self,
            TableSortField::LastActivity | TableSortField::StartDate | TableSortField::DueDate
        )
    }

    /// 只按 `i.id` 打破平局（上游 `idOnlyTie`，用于可为空的时间列）。
    pub fn id_only_tie(self) -> bool {
        self == TableSortField::LastActivity
    }

    /// 未显式指定方向时的默认值（上游：`idOnlyTie` ⇒ desc，否则 asc）。
    pub fn default_direction(self) -> TableSortDirection {
        if self.id_only_tie() {
            TableSortDirection::Desc
        } else {
            TableSortDirection::Asc
        }
    }

    /// 字段映射到 `issue` 上的列（status/priority 走 rank 表达式，不在这里）。
    fn plain_column(self) -> Option<&'static str> {
        match self {
            TableSortField::Position => Some("i.position"),
            TableSortField::Title => Some("i.title"),
            TableSortField::CreatedAt => Some("i.created_at"),
            TableSortField::UpdatedAt => Some("i.updated_at"),
            TableSortField::LastActivity => Some("i.last_activity_at"),
            TableSortField::StartDate => Some("i.start_date"),
            TableSortField::DueDate => Some("i.due_date"),
            TableSortField::Status | TableSortField::Priority => None,
        }
    }
}

/// `/rows` 的排序请求（方向 `None` = 走字段默认值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableOrder {
    pub field: TableSortField,
    pub direction: Option<TableSortDirection>,
}

impl Default for TableOrder {
    fn default() -> Self {
        Self {
            field: TableSortField::Position,
            direction: None,
        }
    }
}

/// `/rows` 的 keyset cursor（不含 `fingerprint` / `group_key` / branch，那些在 HTTP 层）。
#[derive(Debug, Clone, PartialEq)]
pub struct TableCursor {
    pub sort_value: Option<String>,
    pub sort_is_null: bool,
    pub row_created_at: DateTime<Utc>,
    pub row_id: Id,
}

/// `/groups` 的 keyset cursor。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableGroupCursor {
    pub order: i64,
    pub sort_key: String,
    pub value: String,
}

/// 分组计数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableGroupCount {
    pub key: String,
    pub count: i64,
}

/// `/groups` 结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableGroupsPage {
    /// 全部匹配 issue 数（分组计数之和，不是页内之和）。
    pub total: i64,
    pub groups: Vec<TableGroupCount>,
    pub next: Option<TableGroupCursor>,
}

/// `/rows` 的一行。
#[derive(Debug, Clone)]
pub struct TableRow {
    pub issue: IssueRow,
    pub direct_child_count: i64,
    /// 该行排序键的 `::text`（用于 mint 下一页 cursor；不上面）。
    pub sort_key: Option<String>,
}

/// `/rows` 结果。
#[derive(Debug, Clone)]
pub struct TableRowsPage {
    /// 仅“无 cursor + `group.kind=none` + 无 `parent_id`”的页头会带值（与上游一致）。
    pub total: Option<i64>,
    pub rows: Vec<TableRow>,
    pub next: Option<TableCursor>,
}

/// facet 值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableFacetValue {
    pub key: String,
    pub count: i64,
}

/// 一个 facet 的结果（与请求里的 facets 顺序一一对应）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableFacetCount {
    pub values: Vec<TableFacetValue>,
}

/// `/facets` 结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableFacetsPage {
    pub total: i64,
    pub facets: Vec<TableFacetCount>,
}

/// `/groups` 请求。
#[derive(Debug, Clone)]
pub struct TableGroupsQuery {
    pub workspace_id: Id,
    pub filter: TableFilter,
    pub group: TableGroupSpec,
    pub limit: i64,
    pub cursor: Option<TableGroupCursor>,
}

/// `/rows` 请求。
#[derive(Debug, Clone)]
pub struct TableRowsQuery {
    pub workspace_id: Id,
    pub filter: TableFilter,
    pub group: TableGroupSpec,
    pub group_key: TableGroupKey,
    pub hierarchy: bool,
    pub parent_id: Option<Id>,
    pub order: TableOrder,
    pub limit: i64,
    pub cursor: Option<TableCursor>,
}

/// `/facets` 请求。
#[derive(Debug, Clone)]
pub struct TableFacetsQuery {
    pub workspace_id: Id,
    pub filter: TableFilter,
    pub facets: Vec<TableFacetKind>,
    pub include_total: bool,
}

impl TableFilter {
    /// 去掉某个 facet 自己的维度（上游 `issueTableQueryWithoutFacet`）：
    /// facet 计数是“把这一维过滤打开后会看到什么”，所以算 A 的 facet 时不能带 A 自己的过滤。
    pub(crate) fn without_facet(&self, facet: TableFacetKind) -> Self {
        let mut next = self.clone();
        match facet {
            TableFacetKind::Status => next.statuses.clear(),
            TableFacetKind::Priority => next.priorities.clear(),
            TableFacetKind::Assignee => {
                next.assignees = None;
                next.include_no_assignee = false;
            }
            TableFacetKind::Creator => next.creators.clear(),
            TableFacetKind::Project => {
                next.project_ids.clear();
                next.include_no_project = false;
            }
        }
        next
    }
}
