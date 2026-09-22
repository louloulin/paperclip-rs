//! `IssueTableRepo` —— issue table 查询面的 SQL 编译与执行（M2-D / LUM-1355）。
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
//! 约定与 M2-A（`crate::issue`）一致：运行时 sqlx builder（`query_as` / `query` + `.bind()`，
//! 不用 compile-time 宏，构建期不需要数据库）、错误走 `crate::workspace::map_sqlx_err`、
//! PG 集成测试 `#[ignore]` + `MULTICA_TEST_DATABASE_URL`。
//!
//! 与上游的**有意偏离**（逐条论证见 `docs/14-M2-TABLE.md` §4）：
//! 1. 不开启 `REPEATABLE READ READ ONLY` 快照事务（本仓 `Db` 没有 TxStarter 等价物）
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

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::postgres::{PgArguments, PgRow};
use sqlx::query::{QueryAs, QueryScalar};
use sqlx::{FromRow, Postgres, Row};
use uuid::Uuid;

use mc_core::Id;
use mc_db::Db;

use crate::issue::{IssueRow, ISSUE_COLUMNS};
use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

/// `/api/issues/table/*` 默认页大小（上游 `issueTableDefaultPageSize`）。
pub const TABLE_DEFAULT_PAGE_SIZE: i64 = 50;
/// `/api/issues/table/*` 页大小上限（上游 `issueTableMaxPageSize`）。
pub const TABLE_MAX_PAGE_SIZE: i64 = 100;
/// 单次请求的 facet 数量上限（上游 `issueTableMaxFacets`）。
pub const TABLE_MAX_FACETS: usize = 32;
/// 未指派分组的 group_value（上游 `__unassigned__`）。
pub const GROUP_VALUE_UNASSIGNED: &str = "__unassigned__";
/// 无 project 分组的 group_value（上游 `__no_project__`）。
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
// 参数绑定
// ---------------------------------------------------------------------------

/// 运行时绑定参数。用 enum 而不是 sqlx 的 `Any` 泛型，是为了让 `$n` 的 SQL 片段与
/// `.bind()` 调用一一对应、类型由编译器检查。
#[derive(Debug, Clone)]
enum Param {
    Text(String),
    TextArray(Vec<String>),
    Uuid(Uuid),
    UuidArray(Vec<Uuid>),
    Timestamp(DateTime<Utc>),
    Date(NaiveDate),
    Int(i64),
    Float(f64),
}

impl Param {
    fn bind<'q, O>(self, query: QueryAs<'q, Postgres, O, PgArguments>) -> QueryAs<'q, Postgres, O, PgArguments> {
        match self {
            Param::Text(value) => query.bind(value),
            Param::TextArray(value) => query.bind(value),
            Param::Uuid(value) => query.bind(value),
            Param::UuidArray(value) => query.bind(value),
            Param::Timestamp(value) => query.bind(value),
            Param::Date(value) => query.bind(value),
            Param::Int(value) => query.bind(value),
            Param::Float(value) => query.bind(value),
        }
    }

    fn bind_scalar<'q>(self, query: QueryScalar<'q, Postgres, i64, PgArguments>) -> QueryScalar<'q, Postgres, i64, PgArguments> {
        match self {
            Param::Text(value) => query.bind(value),
            Param::TextArray(value) => query.bind(value),
            Param::Uuid(value) => query.bind(value),
            Param::UuidArray(value) => query.bind(value),
            Param::Timestamp(value) => query.bind(value),
            Param::Date(value) => query.bind(value),
            Param::Int(value) => query.bind(value),
            Param::Float(value) => query.bind(value),
        }
    }
}

/// `$n` 占位符分配器 + 参数列表。
#[derive(Debug, Default)]
struct QueryBuilder {
    args: Vec<Param>,
}

impl QueryBuilder {
    fn push(&mut self, param: Param) -> String {
        self.args.push(param);
        format!("${}", self.args.len())
    }

    fn text(&mut self, value: impl Into<String>) -> String {
        self.push(Param::Text(value.into()))
    }

    fn uuid(&mut self, value: Uuid) -> String {
        self.push(Param::Uuid(value))
    }

    fn text_array(&mut self, value: Vec<String>) -> String {
        self.push(Param::TextArray(value))
    }

    fn uuid_array(&mut self, value: Vec<Uuid>) -> String {
        self.push(Param::UuidArray(value))
    }

    fn timestamp(&mut self, value: DateTime<Utc>) -> String {
        self.push(Param::Timestamp(value))
    }

    fn date(&mut self, value: NaiveDate) -> String {
        self.push(Param::Date(value))
    }
}

fn query_as_with<'q, O>(sql: &'q str, args: &[Param]) -> QueryAs<'q, Postgres, O, PgArguments>
where
    O: for<'r> FromRow<'r, PgRow> + Send + Unpin,
{
    let mut query = sqlx::query_as::<Postgres, O>(sql);
    for param in args {
        query = param.clone().bind(query);
    }
    query
}

fn count_with<'q>(sql: &'q str, args: &[Param]) -> QueryScalar<'q, Postgres, i64, PgArguments> {
    let mut query = sqlx::query_scalar::<Postgres, i64>(sql);
    for param in args {
        query = param.clone().bind_scalar(query);
    }
    query
}

/// `ISSUE_COLUMNS` 加 `i.` 前缀（`SELECT ... FROM page i` 需要限定列）。
fn prefixed_issue_columns() -> String {
    ISSUE_COLUMNS
        .split(',')
        .map(|column| format!("i.{}", column.trim()))
        .collect::<Vec<_>>()
        .join(", ")
}

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

/// 已解析的 group_key（类型层面保证与 `group.kind` 一致，非法 key 在 HTTP 层 400）。
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

/// `filters.date` 的字段（上游只允许 created_at / updated_at）。
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
            TableSortField::CreatedAt | TableSortField::UpdatedAt | TableSortField::LastActivity => {
                "timestamptz"
            }
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

/// 已解析的排序（表达式 + 方向 + cursor 语义）。
#[derive(Debug, Clone)]
struct ResolvedSort {
    expression: String,
    direction: TableSortDirection,
    cast: &'static str,
    nulls_last: bool,
    id_only_tie: bool,
}

impl ResolvedSort {
    fn order_by(&self) -> String {
        let mut order = format!("{} {}", self.expression, self.direction.sql());
        if self.nulls_last {
            order.push_str(" NULLS LAST");
        }
        if self.id_only_tie {
            order.push_str(", i.id DESC");
        } else {
            order.push_str(", i.created_at DESC, i.id DESC");
        }
        order
    }

    /// keyset 分页谓词（上游 `cursorPredicate`，逐分支镜像）。
    fn cursor_predicate(
        &self,
        builder: &mut QueryBuilder,
        cursor: &TableCursor,
    ) -> String {
        let id_ref = builder.uuid(cursor.row_id.0);
        let mut tie = format!("i.id < {id_ref}::uuid");
        if !self.id_only_tie {
            let created_ref = builder.timestamp(cursor.row_created_at);
            tie = format!(
                "(i.created_at < {created_ref}::timestamptz \
                 OR (i.created_at = {created_ref}::timestamptz AND i.id < {id_ref}::uuid))"
            );
        }
        if cursor.sort_is_null {
            return format!("({} IS NULL AND {})", self.expression, tie);
        }
        let Some(sort_value) = cursor.sort_value.as_deref() else {
            // HTTP 层已保证 sort_is_null / sort_value 二者必居其一。
            return "FALSE".to_string();
        };
        let value_ref = builder.text(sort_value);
        let value_expr = format!("{value_ref}::{}", self.cast);
        let comparison = if self.direction == TableSortDirection::Desc {
            "<"
        } else {
            ">"
        };
        let mut predicate = format!(
            "({expr} {comparison} {value_expr} OR ({expr} = {value_expr} AND {tie}))",
            expr = self.expression
        );
        if self.expression == "i.position" {
            // 上游注释：混合方向的 keyset 谓词本身不可索引，这个冗余下界让
            // PostgreSQL 从 cursor 处开始扫 position 索引。
            predicate = format!("({} >= {} AND {})", self.expression, value_expr, predicate);
        }
        if self.nulls_last {
            predicate = format!("({} IS NULL OR {})", self.expression, predicate);
        }
        predicate
    }
}

/// `/rows` 的 keyset cursor（不含 fingerprint / group_key / branch，那些在 HTTP 层）。
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

impl<'r> FromRow<'r, PgRow> for TableRow {
    fn from_row(row: &'r PgRow) -> std::result::Result<Self, sqlx::Error> {
        Ok(Self {
            issue: IssueRow::from_row(row)?,
            direct_child_count: row.try_get("direct_child_count")?,
            sort_key: row.try_get("table_sort_key")?,
        })
    }
}

/// `/rows` 结果。
#[derive(Debug, Clone)]
pub struct TableRowsPage {
    /// 仅“无 cursor + group.kind=none + 无 parent_id”的页头会带值（与上游一致）。
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
    fn without_facet(&self, facet: TableFacetKind) -> Self {
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

// ---------------------------------------------------------------------------
// WHERE 编译
// ---------------------------------------------------------------------------

/// 作用域里的 `assignee_types` 收窄（workspace / project 两种 scope 共用）。
fn push_assignee_types(builder: &mut QueryBuilder, parts: &mut Vec<String>, assignee_types: &[String]) {
    if !assignee_types.is_empty() {
        let mut types = assignee_types.to_vec();
        types.sort();
        types.dedup();
        parts.push(format!(
            "i.assignee_type = ANY({}::text[])",
            builder.text_array(types)
        ));
    }
}

/// 编译 `i.workspace_id = $1 AND ...`（$1 固定是 workspace，分组/排序的子查询硬编码引用 $1）。
#[allow(clippy::too_many_lines)]
fn compile_where(builder: &mut QueryBuilder, workspace_id: Id, filter: &TableFilter) -> String {
    let mut parts = vec![format!("i.workspace_id = {}", builder.uuid(workspace_id.0))];
    match &filter.scope {
        TableScope::Workspace { assignee_types } => {
            push_assignee_types(builder, &mut parts, assignee_types);
        }
        TableScope::Project {
            project_id,
            assignee_types,
        } => {
            parts.push(format!("i.project_id = {}::uuid", builder.uuid(project_id.0)));
            push_assignee_types(builder, &mut parts, assignee_types);
        }
        TableScope::Assignee(actor) => {
            let kind_ref = builder.text(actor.kind.clone());
            let id_ref = builder.text(actor.id.to_string());
            parts.push(format!(
                "(i.assignee_type = {kind_ref}::text AND i.assignee_id = {id_ref}::text)"
            ));
        }
        TableScope::Creator(actor) => {
            let kind_ref = builder.text(actor.kind.clone());
            let id_ref = builder.text(actor.id.to_string());
            parts.push(format!(
                "(i.creator_type = {kind_ref}::text AND i.creator_id = {id_ref}::text)"
            ));
        }
        TableScope::My { actor, relation } => {
            let assigned = my_assigned_predicate(builder, actor);
            match relation {
                TableMyRelation::Assigned => parts.push(assigned),
                TableMyRelation::Created => parts.push(my_created_predicate(builder, actor)),
                TableMyRelation::Involved => parts.push(my_involved_predicate(builder, actor)),
                TableMyRelation::Any => {
                    let created = my_created_predicate(builder, actor);
                    let involved = my_involved_predicate(builder, actor);
                    parts.push(format!("({assigned} OR {created} OR {involved})"));
                }
            }
        }
    }

    if !filter.statuses.is_empty() {
        let mut statuses = filter.statuses.clone();
        statuses.sort();
        statuses.dedup();
        parts.push(format!("i.status = ANY({}::text[])", builder.text_array(statuses)));
    }
    if !filter.priorities.is_empty() {
        let mut priorities = filter.priorities.clone();
        priorities.sort();
        priorities.dedup();
        parts.push(format!(
            "i.priority = ANY({}::text[])",
            builder.text_array(priorities)
        ));
    }

    if filter.assignees.is_some() || filter.include_no_assignee {
        let mut ors: Vec<String> = Vec::new();
        for actor in filter.assignees.iter().flatten() {
            let kind_ref = builder.text(actor.kind.clone());
            let id_ref = builder.text(actor.id.to_string());
            ors.push(format!(
                "(i.assignee_type = {kind_ref}::text AND i.assignee_id = {id_ref}::text)"
            ));
        }
        if filter.include_no_assignee {
            ors.push("(i.assignee_type IS NULL AND i.assignee_id IS NULL)".to_string());
        }
        // 显式空数组 = “匹配 0 行”（上游同语义，让客户端能表达“空选中集”）。
        parts.push(if ors.is_empty() {
            "FALSE".to_string()
        } else {
            format!("({})", ors.join(" OR "))
        });
    }

    if !filter.creators.is_empty() {
        let ors: Vec<String> = filter
            .creators
            .iter()
            .map(|actor| {
                let kind_ref = builder.text(actor.kind.clone());
                let id_ref = builder.text(actor.id.to_string());
                format!("(i.creator_type = {kind_ref}::text AND i.creator_id = {id_ref}::text)")
            })
            .collect();
        parts.push(format!("({})", ors.join(" OR ")));
    }

    if !filter.project_ids.is_empty() || filter.include_no_project {
        let mut ors: Vec<String> = Vec::new();
        if !filter.project_ids.is_empty() {
            let ids: Vec<Uuid> = filter.project_ids.iter().map(|id| id.0).collect();
            ors.push(format!("i.project_id = ANY({}::uuid[])", builder.uuid_array(ids)));
        }
        if filter.include_no_project {
            ors.push("i.project_id IS NULL".to_string());
        }
        parts.push(format!("({})", ors.join(" OR ")));
    }

    if let Some(date) = &filter.date {
        let column = date.field.column();
        let start_ref = builder.timestamp(date.start);
        let end_ref = builder.timestamp(date.end);
        parts.push(format!(
            "{column} >= {start_ref}::timestamptz AND {column} < {end_ref}::timestamptz"
        ));
    }

    if filter.include_sub_issues == Some(false) {
        parts.push("i.parent_issue_id IS NULL".to_string());
    }

    parts.extend(search_predicates(builder, &filter.search));
    parts.join(" AND ")
}

fn my_assigned_predicate(builder: &mut QueryBuilder, actor: &TableActor) -> String {
    let kind_ref = builder.text(actor.kind.clone());
    let id_ref = builder.text(actor.id.to_string());
    format!("(i.assignee_type = {kind_ref}::text AND i.assignee_id = {id_ref}::text)")
}

fn my_created_predicate(builder: &mut QueryBuilder, actor: &TableActor) -> String {
    let kind_ref = builder.text(actor.kind.clone());
    let id_ref = builder.text(actor.id.to_string());
    format!("(i.creator_type = {kind_ref}::text AND i.creator_id = {id_ref}::text)")
}

/// 上游 `appendIssueTableInvolvedPredicate`：把本人（`user`）拥有的 agent 及其带队的 squad
/// 也算作“我参与的”。本仓没有 `squad_member` 表，所以只保留 agent 归属与 squad leader 两条支路。
fn my_involved_predicate(builder: &mut QueryBuilder, actor: &TableActor) -> String {
    let owner_ref = builder.uuid(actor.id);
    format!(
        "((i.assignee_type = 'agent' AND i.assignee_id IN (\
           SELECT a.id::text FROM agent a \
            WHERE a.workspace_id = $1 AND a.owner_id = {owner_ref}::uuid)) \
         OR (i.assignee_type = 'squad' AND i.assignee_id IN (\
           SELECT s.id::text FROM squad s JOIN agent a ON a.id = s.leader_agent_id \
            WHERE s.workspace_id = $1 AND a.workspace_id = $1 AND a.owner_id = {owner_ref}::uuid)))"
    )
}

/// 上游 `appendIssueTableSearchFilter`：所有词都命中 title，或命中 issue number。
fn search_predicates(builder: &mut QueryBuilder, raw: &str) -> Vec<String> {
    let query = raw.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let mut ors: Vec<String> = Vec::new();
    let lower = query.to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().filter(|w| !w.is_empty()).collect();
    if !words.is_empty() {
        let matches: Vec<String> = words
            .iter()
            .map(|word| {
                let pattern = format!("%{}%", escape_like(word));
                format!("LOWER(i.title) LIKE {}", builder.text(pattern))
            })
            .collect();
        ors.push(format!("({})", matches.join(" AND ")));
    }
    if let Some(number) = parse_query_number(query) {
        ors.push(format!("i.number = {}", builder.push(Param::Int(number))));
    }
    if ors.is_empty() {
        return Vec::new();
    }
    vec![format!("({})", ors.join(" OR "))]
}

fn escape_like(value: &str) -> String {
    value.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// 上游 `parseQueryNumber`：`ABC-45` 或裸数字。
fn parse_query_number(query: &str) -> Option<i64> {
    let trimmed = query.trim();
    if let Some((prefix, digits)) = trimmed.split_once('-') {
        if !prefix.is_empty()
            && prefix.chars().all(|c| c.is_ascii_alphabetic())
            && !digits.is_empty()
            && digits.chars().all(|c| c.is_ascii_digit())
        {
            return digits.parse::<i64>().ok().filter(|n| *n > 0);
        }
    }
    trimmed.parse::<i64>().ok().filter(|n| *n > 0)
}

// ---------------------------------------------------------------------------
// 分组表达式 / 排序解析
// ---------------------------------------------------------------------------

/// 分组的 `group_value` 表达式（上游 `resolvedIssueTableGroup.expression`）。
fn group_expression(group: TableGroupSpec) -> String {
    match group.kind {
        TableGroupKind::None => "''::text".to_string(),
        TableGroupKind::Status => "i.status".to_string(),
        TableGroupKind::Priority => "i.priority".to_string(),
        TableGroupKind::Assignee => format!(
            "CASE WHEN i.assignee_type IS NULL OR i.assignee_id IS NULL \
             THEN '{GROUP_VALUE_UNASSIGNED}' ELSE i.assignee_type || ':' || i.assignee_id END"
        ),
        TableGroupKind::Project => {
            format!("COALESCE(i.project_id::text, '{GROUP_VALUE_NO_PROJECT}')")
        }
    }
}

/// 分组的次要排序键（上游 `sortExpression`：默认就是 `group_value`）。
fn group_sort_expression(group: TableGroupSpec) -> String {
    match group.kind {
        // 上游用 `p.title`，本仓 `project` 表列名是 `name`。
        TableGroupKind::Project => format!(
            "CASE WHEN group_value = '{GROUP_VALUE_NO_PROJECT}' THEN '' ELSE LOWER(COALESCE(\
               (SELECT p.name FROM project p WHERE p.workspace_id = $1 AND p.id = group_value::uuid), '')) END"
        ),
        TableGroupKind::Assignee => "LOWER(COALESCE(CASE split_part(group_value, ':', 1) \
             WHEN 'user' THEN (SELECT u.name FROM \"user\" u WHERE u.id = split_part(group_value, ':', 2)::uuid) \
             WHEN 'agent' THEN (SELECT a.name FROM agent a WHERE a.workspace_id = $1 AND a.id = split_part(group_value, ':', 2)::uuid) \
             WHEN 'squad' THEN (SELECT s.name FROM squad s WHERE s.workspace_id = $1 AND s.id = split_part(group_value, ':', 2)::uuid) \
             WHEN 'autopilot' THEN (SELECT ap.name FROM autopilot ap WHERE ap.workspace_id = $1 AND ap.id = split_part(group_value, ':', 2)::uuid) \
             END, ''))"
            .to_string(),
        _ => "group_value".to_string(),
    }
}

/// 已解析分组（表达式 + 顺序数组）。
struct ResolvedGroup {
    kind: TableGroupKind,
    expression: String,
    sort_expression: String,
    order_expression: String,
    /// `status` / `priority` 的固定顺序（用于 `array_position` 与 `include_empty`）。
    order_values: Vec<String>,
    include_empty: bool,
}

fn resolve_group(
    group: TableGroupSpec,
    status_order: &[String],
    builder: &mut QueryBuilder,
) -> ResolvedGroup {
    let expression = group_expression(group);
    let sort_expression = group_sort_expression(group);
    let order_values: Vec<String> = match group.kind {
        TableGroupKind::Status => status_order.to_vec(),
        TableGroupKind::Priority => BUILTIN_PRIORITIES.iter().map(|p| (*p).to_string()).collect(),
        _ => Vec::new(),
    };
    let order_expression = match group.kind {
        TableGroupKind::Status | TableGroupKind::Priority => {
            let order_ref = builder.text_array(order_values.clone());
            format!("COALESCE(array_position({order_ref}::text[], group_value), 100000)")
        }
        TableGroupKind::Assignee => "CASE split_part(group_value, ':', 1) WHEN 'user' THEN 0 \
             WHEN 'agent' THEN 1 WHEN 'squad' THEN 2 ELSE 3 END"
            .to_string(),
        TableGroupKind::Project => {
            format!("CASE WHEN group_value = '{GROUP_VALUE_NO_PROJECT}' THEN 0 ELSE 1 END")
        }
        TableGroupKind::None => "0".to_string(),
    };
    ResolvedGroup {
        kind: group.kind,
        expression,
        sort_expression,
        order_expression,
        order_values,
        include_empty: group.include_empty
            && matches!(group.kind, TableGroupKind::Status | TableGroupKind::Priority),
    }
}

/// group_key → 谓词（上游 `predicate`）。
fn group_predicate(key: &TableGroupKey, builder: &mut QueryBuilder) -> String {
    match key {
        TableGroupKey::None => "TRUE".to_string(),
        TableGroupKey::Status(status) => {
            let status_ref = builder.text(status.clone());
            format!("i.status = {status_ref}::text")
        }
        TableGroupKey::Priority(priority) => {
            let priority_ref = builder.text(priority.clone());
            format!("i.priority = {priority_ref}::text")
        }
        TableGroupKey::Assignee(None) => {
            "i.assignee_type IS NULL AND i.assignee_id IS NULL".to_string()
        }
        TableGroupKey::Assignee(Some(actor)) => {
            let kind_ref = builder.text(actor.kind.clone());
            let id_ref = builder.text(actor.id.to_string());
            format!("i.assignee_type = {kind_ref}::text AND i.assignee_id = {id_ref}::text")
        }
        TableGroupKey::Project(None) => "i.project_id IS NULL".to_string(),
        TableGroupKey::Project(Some(project_id)) => {
            let project_ref = builder.uuid(project_id.0);
            format!("i.project_id = {project_ref}::uuid")
        }
    }
}

/// `group_value` → wire group key（上游 `descriptor` 的 key 部分）。
pub fn group_key_of(kind: TableGroupKind, group_value: &str) -> String {
    match kind {
        TableGroupKind::None => String::new(),
        TableGroupKind::Status => format!("status:{group_value}"),
        TableGroupKind::Priority => format!("priority:{group_value}"),
        TableGroupKind::Assignee if group_value == GROUP_VALUE_UNASSIGNED => {
            "assignee:unassigned".to_string()
        }
        TableGroupKind::Assignee => format!("assignee:{group_value}"),
        TableGroupKind::Project if group_value == GROUP_VALUE_NO_PROJECT => "project:none".to_string(),
        TableGroupKind::Project => format!("project:{group_value}"),
    }
}

/// 解析排序（上游 `issueTableOrderBy`）。`position` 的方向被上游强制为 asc，这里一并镜像。
fn resolve_sort(
    order: TableOrder,
    builder: &mut QueryBuilder,
    status_order: &[String],
) -> ResolvedSort {
    let expression = match order.field.plain_column() {
        Some(column) => column.to_string(),
        None => match order.field {
            TableSortField::Status => {
                let order_ref = builder.text_array(status_order.to_vec());
                format!("COALESCE(array_position({order_ref}::text[], i.status), 100000)")
            }
            TableSortField::Priority => "CASE i.priority WHEN 'urgent' THEN 0 WHEN 'high' THEN 1 \
                 WHEN 'medium' THEN 2 WHEN 'low' THEN 3 ELSE 4 END"
                .to_string(),
            _ => unreachable!("status/priority are the only non-column sort fields"),
        },
    };
    let direction = if order.field == TableSortField::Position {
        TableSortDirection::Asc
    } else {
        order.direction.unwrap_or_else(|| order.field.default_direction())
    };
    ResolvedSort {
        expression,
        direction,
        cast: order.field.cast(),
        nulls_last: order.field.nulls_last(),
        id_only_tie: order.field.id_only_tie(),
    }
}

fn encode_group_cursor(group_value: &str, order: i64, sort_key: &str) -> TableGroupCursor {
    TableGroupCursor {
        order,
        sort_key: sort_key.to_string(),
        value: group_value.to_string(),
    }
}

// ---------------------------------------------------------------------------
// 内部行结构
// ---------------------------------------------------------------------------

/// `issue_status` 目录行（`status_order`）。
#[derive(FromRow)]
struct StatusRow {
    key: String,
    category: String,
    position: f64,
}

/// `/groups` 内层查询行。
#[derive(FromRow)]
struct GroupRow {
    group_value: String,
    issue_count: i64,
    group_sort: String,
    group_order: i32,
    total: i64,
}

/// `/facets` 每个 facet 的查询行。
#[derive(FromRow)]
struct FacetRow {
    facet_value: String,
    facet_count: i64,
}

// ---------------------------------------------------------------------------
// Repo
// ---------------------------------------------------------------------------

/// issue table 查询仓储。
#[derive(Clone)]
pub struct IssueTableRepo {
    db: Db,
}

impl IssueTableRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// workspace 的 status 顺序（上游 `issueTableStatusOrder`）：目录按
    /// (分类, position, 内置优先, 内置序, key) 排序，内建缺失时补齐。
    pub async fn status_order(&self, workspace_id: Id) -> Result<Vec<String>> {
        // 本仓 0001 的 issue_status 没有 archived / is_system 列，无法过滤归档目录项；
        // 分类 CHECK 只有 open/closed（上游是 unstarted/started/done/closed 四档）。
        let rows: Vec<StatusRow> = sqlx::query_as(
            "SELECT key, category, position FROM issue_status WHERE workspace_id = $1",
        )
        .bind(workspace_id.0)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(merge_status_order(
            rows.into_iter()
                .map(|row| (row.key, row.category, row.position))
                .collect(),
        ))
    }

    /// `/api/issues/table/groups`：分组计数 + 总数 + 下一页 cursor。
    pub async fn table_groups(&self, query: &TableGroupsQuery) -> Result<TableGroupsPage> {
        let status_order = if query.group.kind == TableGroupKind::Status {
            self.status_order(query.workspace_id).await?
        } else {
            Vec::new()
        };
        let mut builder = QueryBuilder::default();
        let where_sql = compile_where(&mut builder, query.workspace_id, &query.filter);
        let group = resolve_group(query.group, &status_order, &mut builder);
        let limit_ref = builder.push(Param::Int(query.limit + 1));
        let cursor_predicate = match &query.cursor {
            None => "TRUE".to_string(),
            Some(cursor) => {
                let order_ref = builder.push(Param::Int(cursor.order));
                let sort_ref = builder.text(cursor.sort_key.clone());
                let key_ref = builder.text(cursor.value.clone());
                format!(
                    "(group_order > {order_ref}::int OR (group_order = {order_ref}::int \
                     AND (group_sort > {sort_ref}::text \
                     OR (group_sort = {sort_ref}::text AND group_value > {key_ref}::text))))"
                )
            }
        };
        let grouped_cte = if group.include_empty {
            let expected_ref = builder.text_array(group.order_values.clone());
            format!(
                "actual AS (SELECT {expr} AS group_value, COUNT(*)::bigint AS issue_count \
                   FROM issue i WHERE {where_sql} GROUP BY 1), \
                 expected AS (SELECT unnest({expected_ref}::text[]) AS group_value), \
                 grouped AS (SELECT e.group_value, COALESCE(a.issue_count, 0)::bigint AS issue_count \
                   FROM expected e LEFT JOIN actual a USING (group_value) \
                   UNION ALL \
                   SELECT a.group_value, a.issue_count FROM actual a \
                   WHERE NOT (a.group_value = ANY({expected_ref}::text[])))",
                expr = group.expression
            )
        } else {
            format!(
                "grouped AS (SELECT {expr} AS group_value, COUNT(*)::bigint AS issue_count \
                   FROM issue i WHERE {where_sql} GROUP BY 1)",
                expr = group.expression
            )
        };
        let sql = format!(
            "WITH {grouped_cte}, sorted AS (SELECT group_value, issue_count, \
               ({sort_expr})::text AS group_sort FROM grouped), \
             ranked AS (SELECT group_value, issue_count, group_sort, ({order_expr})::int AS group_order, \
               SUM(issue_count) OVER ()::bigint AS total FROM sorted) \
             SELECT group_value, issue_count, group_sort, group_order, total FROM ranked \
             WHERE {cursor_predicate} \
             ORDER BY group_order ASC, group_sort ASC, group_value ASC LIMIT {limit_ref}",
            sort_expr = group.sort_expression,
            order_expr = group.order_expression,
        );

        let rows: Vec<GroupRow> = query_as_with(&sql, &builder.args)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;

        let total = rows.first().map_or(0, |row| row.total);
        let limit = usize::try_from(query.limit).unwrap_or(0);
        let mut groups: Vec<TableGroupCount> = Vec::with_capacity(rows.len().min(limit + 1));
        let mut next = None;
        for (index, row) in rows.iter().enumerate() {
            if index == limit {
                let last = &rows[limit - 1];
                next = Some(encode_group_cursor(
                    &last.group_value,
                    i64::from(last.group_order),
                    &last.group_sort,
                ));
                break;
            }
            groups.push(TableGroupCount {
                key: group_key_of(group.kind, &row.group_value),
                count: row.issue_count,
            });
        }
        Ok(TableGroupsPage {
            total,
            groups,
            next,
        })
    }

    /// `/api/issues/table/rows`：一行 issue + 直接子 issue 数 + 下一页 cursor。
    pub async fn table_rows(&self, query: &TableRowsQuery) -> Result<TableRowsPage> {
        let status_order = if query.order.field == TableSortField::Status {
            self.status_order(query.workspace_id).await?
        } else {
            Vec::new()
        };
        let mut builder = QueryBuilder::default();
        let where_sql = compile_where(&mut builder, query.workspace_id, &query.filter);
        let group = resolve_group(query.group, &status_order, &mut builder);
        let group_predicate = group_predicate(&query.group_key, &mut builder);
        let order = resolve_sort(query.order, &mut builder, &status_order);
        let cursor_predicate = match &query.cursor {
            None => "TRUE".to_string(),
            Some(cursor) => order.cursor_predicate(&mut builder, cursor),
        };

        // 有层级请求时先物化“当前分支的成员集合”，子计数与父子判定都基于它；
        // 没有层级请求时 page 直接扫 issue，不做多余聚合（上游同）。
        let (cte_prefix, page_source, page_predicate) = if query.hierarchy {
            (
                format!(
                    "WITH membership AS NOT MATERIALIZED (SELECT i.* FROM issue i \
                     WHERE {where_sql} AND ({group_predicate})), "
                ),
                "membership",
                "TRUE".to_string(),
            )
        } else {
            (
                "WITH ".to_string(),
                "issue",
                format!("({where_sql}) AND ({group_predicate})"),
            )
        };
        // 层级分支谓词：有 parent_id 时看它的直接子；无 parent_id 时看“父不在当前分支里”的根。
        // 注意 `parent_id` 只有 `hierarchy.enabled=true` 才有意义（HTTP 层否则 400）。
        let page_clause = if query.hierarchy {
            let branch_predicate = match &query.parent_id {
                Some(parent_id) => {
                    let parent_ref = builder.uuid(parent_id.0);
                    format!(
                        "i.parent_issue_id = {parent_ref}::uuid \
                         AND EXISTS (SELECT 1 FROM membership parent WHERE parent.id = {parent_ref}::uuid)"
                    )
                }
                None => "i.parent_issue_id IS NULL OR \
                     (SELECT parent.id FROM membership parent WHERE parent.id = i.parent_issue_id) IS NULL"
                    .to_string(),
            };
            format!("({page_predicate}) AND ({branch_predicate})")
        } else {
            page_predicate
        };
        let limit_ref = builder.push(Param::Int(query.limit + 1));
        let child_count_expr = if query.hierarchy {
            "(SELECT COUNT(*)::bigint FROM membership child WHERE child.parent_issue_id = i.id)"
        } else {
            "0::bigint"
        };
        let order_by = order.order_by();
        let sql = format!(
            "{cte_prefix}page AS MATERIALIZED (SELECT i.*, ({sort_expr})::text AS table_sort_key \
               FROM {page_source} i WHERE ({page_clause}) AND {cursor_predicate} \
               ORDER BY {order_by} LIMIT {limit_ref}) \
             SELECT {columns}, {child_count_expr} AS direct_child_count, i.table_sort_key \
               FROM page i ORDER BY {order_by}",
            sort_expr = order.expression,
            columns = prefixed_issue_columns(),
        );
        let rows: Vec<TableRow> = query_as_with(&sql, &builder.args)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;

        // 只有“无 cursor + 未分组 + 无 parent”的页头才付全量 COUNT 的代价（上游同）。
        let total = if query.cursor.is_none()
            && query.group.kind == TableGroupKind::None
            && query.parent_id.is_none()
        {
            let mut counter = QueryBuilder::default();
            let count_where = compile_where(&mut counter, query.workspace_id, &query.filter);
            let count_sql = format!("SELECT COUNT(*)::bigint FROM issue i WHERE {count_where}");
            Some(
                count_with(&count_sql, &counter.args)
                    .fetch_one(self.db.pool())
                    .await
                    .map_err(map_sqlx_err)?,
            )
        } else {
            None
        };

        let limit = usize::try_from(query.limit).unwrap_or(0);
        let mut next = None;
        let mut page = rows;
        if page.len() > limit {
            page.truncate(limit);
            let last = page.last().expect("page has at least one row");
            next = Some(TableCursor {
                sort_value: last.sort_key.clone(),
                sort_is_null: last.sort_key.is_none(),
                row_created_at: last.issue.created_at,
                row_id: last.issue.id(),
            });
        }
        Ok(TableRowsPage {
            total,
            rows: page,
            next,
        })
    }

    /// `/api/issues/table/facets`：每个 facet 的取值计数（disjunctive）+ 可选总数。
    pub async fn table_facets(&self, query: &TableFacetsQuery) -> Result<TableFacetsPage> {
        let mut facets = Vec::with_capacity(query.facets.len());
        for facet in &query.facets {
            let mut builder = QueryBuilder::default();
            let filter = query.filter.without_facet(*facet);
            let where_sql = compile_where(&mut builder, query.workspace_id, &filter);
            let expression = match facet {
                TableFacetKind::Status => "i.status".to_string(),
                TableFacetKind::Priority => "i.priority".to_string(),
                TableFacetKind::Assignee => format!(
                    "CASE WHEN i.assignee_type IS NULL OR i.assignee_id IS NULL \
                     THEN '{FACET_VALUE_NONE}' ELSE i.assignee_type || ':' || i.assignee_id END"
                ),
                TableFacetKind::Creator => "i.creator_type || ':' || i.creator_id".to_string(),
                TableFacetKind::Project => {
                    format!("COALESCE(i.project_id::text, '{FACET_VALUE_NONE}')")
                }
            };
            let sql = format!(
                "SELECT {expression} AS facet_value, COUNT(*)::bigint AS facet_count \
                 FROM issue i WHERE {where_sql} GROUP BY 1 ORDER BY 1"
            );
            let rows: Vec<FacetRow> = query_as_with(&sql, &builder.args)
                .fetch_all(self.db.pool())
                .await
                .map_err(map_sqlx_err)?;
            facets.push(TableFacetCount {
                values: rows
                    .into_iter()
                    .map(|row| TableFacetValue {
                        key: row.facet_value,
                        count: row.facet_count,
                    })
                    .collect(),
            });
        }
        let total = if query.include_total {
            let mut builder = QueryBuilder::default();
            let where_sql = compile_where(&mut builder, query.workspace_id, &query.filter);
            let sql = format!("SELECT COUNT(*)::bigint FROM issue i WHERE {where_sql}");
            count_with(&sql, &builder.args)
                .fetch_one(self.db.pool())
                .await
                .map_err(map_sqlx_err)?
        } else {
            0
        };
        Ok(TableFacetsPage { total, facets })
    }
}

impl RepoWithDb for IssueTableRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

/// 上游 `issueTableStatusOrder`：目录（key, category, position）合并 7 个内建 key 后排序。
///
/// 排序键：分类（open < closed，上游是 unstarted < started < done < closed 四档，
/// 本仓 `issue_status.category` 的 CHECK 只有两档）→ position → 内置优先 → 内置序 → key。
fn merge_status_order(mut entries: Vec<(String, String, f64)>) -> Vec<String> {
    let builtin_rank = |key: &str| BUILTIN_STATUSES.iter().position(|(k, _)| *k == key);
    let seen: Vec<String> = entries.iter().map(|(key, _, _)| key.clone()).collect();
    for (key, category) in BUILTIN_STATUSES {
        if !seen.iter().any(|existing| existing == key) {
            entries.push((key.to_string(), category.to_string(), 0.0));
        }
    }
    entries.sort_by(|a, b| {
        let category_rank = |category: &str| match category {
            "closed" => 1,
            _ => 0,
        };
        category_rank(&a.1)
            .cmp(&category_rank(&b.1))
            .then(a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| {
                let a_builtin = builtin_rank(&a.0);
                let b_builtin = builtin_rank(&b.0);
                b_builtin.is_some().cmp(&a_builtin.is_some())
            })
            .then_with(|| {
                builtin_rank(&a.0)
                    .unwrap_or(usize::MAX)
                    .cmp(&builtin_rank(&b.0).unwrap_or(usize::MAX))
            })
            .then_with(|| a.0.cmp(&b.0))
    });
    entries.into_iter().map(|(key, _, _)| key).collect()
}
