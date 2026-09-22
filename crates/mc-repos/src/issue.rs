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

use chrono::{DateTime, NaiveDate, Utc};
use mc_core::issue::{AssigneeType, IssueOrigin};
use mc_core::priority::Priority;
use mc_core::status::{IssueStatus, StatusCategory};
use mc_core::Id;
use serde_json::Value as JsonValue;
use sqlx::FromRow;
use uuid::Uuid;

use mc_db::Db;

use crate::workspace::map_sqlx_err;
use crate::{RepoError, RepoWithDb, Result};

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
const ISSUE_COLUMNS: &str = "id, workspace_id, number, identifier, title, description, status, \
     status_name, priority, assignee_type, assignee_id, creator_type, creator_id, \
     parent_issue_id, project_id, position, stage, start_date, due_date, last_activity_at, \
     revision, metadata, properties, triage_state, origin, origin_task_id, source_context_id, \
     created_at, updated_at";

/// `list` / `count` 共用的 WHERE 片段（$1..$13，见 `bind_list_filters`）。
const LIST_WHERE: &str = "workspace_id = $1 \
     AND ($2::text[] IS NULL OR status = ANY($2::text[])) \
     AND ($3::text[] IS NULL OR priority = ANY($3::text[])) \
     AND ($4::text IS NULL OR assignee_type = $4::text) \
     AND ($5::text[] IS NULL OR assignee_id = ANY($5::text[])) \
     AND ($6::text IS NULL OR creator_id = $6::text) \
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

// ---------------------------------------------------------------------------
// 行结构
// ---------------------------------------------------------------------------

/// DB 行（镜像 `issue` 表）。
#[derive(Debug, Clone, FromRow)]
pub struct IssueRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub number: i32,
    pub identifier: String,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    pub status_name: Option<String>,
    pub priority: String,
    pub assignee_type: Option<String>,
    pub assignee_id: Option<String>,
    pub creator_type: String,
    pub creator_id: String,
    pub parent_issue_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub position: f64,
    pub stage: Option<i32>,
    pub start_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
    pub last_activity_at: Option<DateTime<Utc>>,
    pub revision: i64,
    pub metadata: JsonValue,
    pub properties: JsonValue,
    pub triage_state: Option<String>,
    pub origin: Option<String>,
    pub origin_task_id: Option<Uuid>,
    pub source_context_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl IssueRow {
    /// 主键。
    pub fn id(&self) -> Id {
        Id::from(self.id)
    }

    /// 所属 workspace。
    pub fn workspace_id(&self) -> Id {
        Id::from(self.workspace_id)
    }

    /// status key（可能是 workspace 自定义 key）。
    pub fn status_key(&self) -> &str {
        &self.status
    }

    /// status key → 内置枚举；自定义 key 返回 `None`（需要查 `issue_status` 目录）。
    pub fn status_enum(&self) -> Option<IssueStatus> {
        IssueStatus::from_key(&self.status)
    }

    /// status 生命周期分类。自定义 key 需要调用方查目录（返回 `None`）。
    pub fn status_category(&self) -> Option<StatusCategory> {
        self.status_enum().map(IssueStatus::category)
    }

    /// 展示名：`status_name` 优先，否则回退到 status key。
    pub fn display_status_name(&self) -> &str {
        self.status_name.as_deref().unwrap_or(&self.status)
    }

    /// priority（DB CHECK 保证 5 值之一，未知值保守回退 `None`）。
    pub fn priority(&self) -> Priority {
        Priority::from_str_opt(&self.priority).unwrap_or(Priority::None)
    }

    /// assignee 类型。
    pub fn assignee_type(&self) -> Option<AssigneeType> {
        self.assignee_type.as_deref().and_then(parse_assignee_type)
    }

    /// assignee id（原始字符串；本仓 `assignee_id` 是 TEXT 列）。
    pub fn assignee_id_str(&self) -> Option<&str> {
        self.assignee_id.as_deref()
    }

    /// 父 issue。
    pub fn parent_issue_id(&self) -> Option<Id> {
        self.parent_issue_id.map(Id::from)
    }

    /// 所属 project。
    pub fn project_id(&self) -> Option<Id> {
        self.project_id.map(Id::from)
    }

    /// origin。
    pub fn origin(&self) -> Option<IssueOrigin> {
        self.origin.as_deref().and_then(parse_issue_origin)
    }

    /// 是否终态（仅内置 status 可判定）。
    pub fn is_closed(&self) -> bool {
        matches!(self.status_category(), Some(StatusCategory::Closed))
    }
}

/// `issue_status` 行（workspace 自定义 status 目录）。
#[derive(Debug, Clone, FromRow)]
pub struct IssueStatusRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    pub key: String,
    pub category: String,
    pub icon: Option<String>,
    pub position: f64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl IssueStatusRow {
    /// 主键。
    pub fn id(&self) -> Id {
        Id::from(self.id)
    }

    /// 是否终态。
    pub fn is_closed(&self) -> bool {
        self.category == "closed"
    }

    /// 是否内置（7 个 canonical key）。
    pub fn is_builtin(&self) -> bool {
        mc_core::status::CANONICAL_KEYS.contains(&self.key.as_str())
    }
}

/// `child-progress` 聚合行。
#[derive(Debug, Clone, FromRow)]
pub struct ChildProgressRow {
    pub parent_issue_id: Uuid,
    pub total: i64,
    pub done: i64,
}

/// 分组计数行（`/api/issues/grouped`）。
#[derive(Debug, Clone, FromRow)]
pub struct GroupedCountRow {
    pub key: Option<String>,
    pub total: i64,
    pub done: i64,
}

/// `issue_reaction` 行。
#[derive(Debug, Clone, FromRow)]
pub struct IssueReactionRow {
    pub id: Uuid,
    pub issue_id: Uuid,
    pub workspace_id: Uuid,
    pub actor_type: String,
    pub actor_id: String,
    pub emoji: String,
    pub created_at: DateTime<Utc>,
}

impl IssueReactionRow {
    /// 主键。
    pub fn id(&self) -> Id {
        Id::from(self.id)
    }
}

// ---------------------------------------------------------------------------
// 输入结构
// ---------------------------------------------------------------------------

/// 新建 issue 的输入。
#[derive(Debug, Clone)]
pub struct NewIssue {
    pub workspace_id: Id,
    pub title: String,
    pub description: Option<String>,
    /// status key（内置或 workspace 自定义；调用方负责校验）
    pub status: String,
    pub status_name: Option<String>,
    pub priority: Priority,
    pub assignee_type: Option<AssigneeType>,
    pub assignee_id: Option<String>,
    pub parent_issue_id: Option<Id>,
    pub project_id: Option<Id>,
    pub stage: Option<i32>,
    pub start_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
    pub metadata: JsonValue,
    pub properties: JsonValue,
    /// `user` / `agent` / `system`
    pub creator_type: String,
    pub creator_id: String,
    pub origin: Option<IssueOrigin>,
}

impl NewIssue {
    /// 默认构造：`status = todo`、`priority = none`、`creator_type = user`。
    pub fn new(workspace_id: Id, title: impl Into<String>, creator_id: impl Into<String>) -> Self {
        Self {
            workspace_id,
            title: title.into(),
            description: None,
            status: "todo".to_string(),
            status_name: None,
            priority: Priority::None,
            assignee_type: None,
            assignee_id: None,
            parent_issue_id: None,
            project_id: None,
            stage: None,
            start_date: None,
            due_date: None,
            metadata: JsonValue::Object(serde_json::Map::new()),
            properties: JsonValue::Object(serde_json::Map::new()),
            creator_type: "user".to_string(),
            creator_id: creator_id.into(),
            origin: None,
        }
    }
}

/// issue 更新补丁。
///
/// 可空列用 `Option<Option<T>>` 表达三态：`None` = 不动；`Some(None)` = 置 NULL；
/// `Some(Some(v))` = 写值。不可空列用 `Option<T>`。
#[derive(Debug, Clone, Default)]
pub struct IssueUpdate {
    /// 乐观并发：与 DB 中 revision 不一致 → `RepoError::Conflict`
    pub expected_revision: Option<i64>,
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    /// status key（调用方负责校验 + 迁移合法性）
    pub status: Option<String>,
    pub status_name: Option<Option<String>>,
    pub priority: Option<Priority>,
    pub assignee_type: Option<Option<AssigneeType>>,
    pub assignee_id: Option<Option<String>>,
    pub parent_issue_id: Option<Option<Id>>,
    pub project_id: Option<Option<Id>>,
    pub position: Option<f64>,
    pub stage: Option<Option<i32>>,
    pub start_date: Option<Option<NaiveDate>>,
    pub due_date: Option<Option<NaiveDate>>,
    pub metadata: Option<JsonValue>,
    pub properties: Option<JsonValue>,
}

impl IssueUpdate {
    /// 是否没有任何字段要改（batch-update 用它短路，避免「no-op 也报 updated: N」）。
    pub fn is_empty(&self) -> bool {
        let Self {
            expected_revision: _,
            title,
            description,
            status,
            status_name,
            priority,
            assignee_type,
            assignee_id,
            parent_issue_id,
            project_id,
            position,
            stage,
            start_date,
            due_date,
            metadata,
            properties,
        } = self;
        title.is_none()
            && description.is_none()
            && status.is_none()
            && status_name.is_none()
            && priority.is_none()
            && assignee_type.is_none()
            && assignee_id.is_none()
            && parent_issue_id.is_none()
            && project_id.is_none()
            && position.is_none()
            && stage.is_none()
            && start_date.is_none()
            && due_date.is_none()
            && metadata.is_none()
            && properties.is_none()
    }
}

/// 列表排序（白名单，避免拼接用户输入）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IssueOrderBy {
    /// `updated_at DESC, number DESC`（默认）
    #[default]
    UpdatedDesc,
    /// `created_at DESC, number DESC`
    CreatedDesc,
    /// `status ASC, position ASC, number ASC`（看板列序）
    PositionAsc,
    /// `number ASC`
    NumberAsc,
}

impl IssueOrderBy {
    fn as_sql(self) -> &'static str {
        match self {
            Self::UpdatedDesc => "updated_at DESC, number DESC",
            Self::CreatedDesc => "created_at DESC, number DESC",
            Self::PositionAsc => "status ASC, position ASC, number ASC",
            Self::NumberAsc => "number ASC",
        }
    }
}

/// `/api/issues/grouped?group_by=` 的白名单字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IssueGroupField {
    /// 按 status key
    #[default]
    Status,
    /// 按 priority
    Priority,
    /// 按 `assignee_id`
    Assignee,
    /// 按 `project_id`
    Project,
}

impl IssueGroupField {
    /// 解析 `group_by` 参数；未知值返回 `None`（handler 400）。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "" | "status" => Some(Self::Status),
            "priority" => Some(Self::Priority),
            "assignee" | "assignee_id" => Some(Self::Assignee),
            "project" | "project_id" => Some(Self::Project),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Priority => "priority",
            Self::Assignee => "assignee",
            Self::Project => "project",
        }
    }

    fn group_expr(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Priority => "priority",
            Self::Assignee => "assignee_id",
            Self::Project => "project_id::text",
        }
    }
}

/// 列表过滤条件。
#[derive(Debug, Clone)]
pub struct IssueFilter {
    pub workspace_id: Id,
    /// status key 集合（`statuses` 或 `status`，含自定义 key）
    pub statuses: Option<Vec<String>>,
    /// priority 集合
    pub priorities: Option<Vec<String>>,
    /// assignee 类型（user/agent/squad/autopilot）
    pub assignee_type: Option<String>,
    /// assignee id 集合（TEXT 列）
    pub assignee_ids: Option<Vec<String>>,
    /// creator id
    pub creator_id: Option<String>,
    /// 只看某个父 issue 的子 issue
    pub parent_issue_id: Option<Id>,
    /// 只看某个 project
    pub project_id: Option<Id>,
    /// stage
    pub stage: Option<i32>,
    /// 全文（title / description / identifier 的 ILIKE）
    pub q: Option<String>,
    /// `true` = 不过滤终态；`false` = 排除 `terminal_statuses`
    pub include_closed: bool,
    /// 终态 key 集合（由 `terminal_status_keys` 解析，含自定义 closed status）
    pub terminal_statuses: Vec<String>,
    /// 只看顶层 issue（`parent_issue_id IS NULL`）
    pub only_parentless: bool,
    /// 页大小（None → `LIST_DEFAULT_LIMIT`，并由调用方 clamp）
    pub limit: Option<i64>,
    /// 偏移
    pub offset: Option<i64>,
    /// 排序
    pub order: IssueOrderBy,
}

impl IssueFilter {
    /// 最小构造：只看一个 workspace 的 issue。
    pub fn new(workspace_id: Id) -> Self {
        Self {
            workspace_id,
            statuses: None,
            priorities: None,
            assignee_type: None,
            assignee_ids: None,
            creator_id: None,
            parent_issue_id: None,
            project_id: None,
            stage: None,
            q: None,
            include_closed: true,
            terminal_statuses: Vec::new(),
            only_parentless: false,
            limit: None,
            offset: None,
            order: IssueOrderBy::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// 工具函数
// ---------------------------------------------------------------------------

/// `workspace.slug` → issue identifier 前缀。
///
/// 上游把前缀存在 workspace 上（`getIssuePrefix`）；本仓 0001 没有该列，因此从
/// slug 派生：取字母数字、转大写、截断 8 位，空则 `ISS`。
pub fn issue_prefix_from_slug(slug: &str) -> String {
    let prefix: String = slug
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect::<String>()
        .to_ascii_uppercase();
    if prefix.is_empty() {
        "ISS".to_string()
    } else {
        prefix
    }
}

/// 拖拽排序的新 position。
///
/// `before` = 前一个锚点（更小 position），`after` = 后一个锚点（更大 position），
/// 与上游 `issueMovePosition` 语义一致；两者都 `None` 时保持原位。
/// 锚点顺序错乱 / 间距过小无法放值时返回 `None`（handler → 409）。
pub fn derive_move_position(before: Option<f64>, after: Option<f64>, current: f64) -> Option<f64> {
    let position = match (before, after) {
        (Some(b), Some(a)) => {
            if !matches!(b.partial_cmp(&a), Some(std::cmp::Ordering::Less)) {
                return None; // 锚点顺序错乱或不可比（NaN）
            }
            let mid = b + (a - b) / 2.0;
            if !mid.is_finite() || mid <= b || mid >= a {
                return None; // 间距太小，无法二分
            }
            mid
        }
        (Some(b), None) => b + 1.0,
        (None, Some(a)) => a - 1.0,
        (None, None) => current,
    };
    if position.is_finite() {
        Some(position)
    } else {
        None
    }
}

/// `assignee_type` 字符串 → 枚举。
pub fn parse_assignee_type(raw: &str) -> Option<AssigneeType> {
    match raw {
        "user" => Some(AssigneeType::User),
        "agent" => Some(AssigneeType::Agent),
        "squad" => Some(AssigneeType::Squad),
        "autopilot" => Some(AssigneeType::Autopilot),
        _ => None,
    }
}

/// origin 字符串 → 枚举。
pub fn parse_issue_origin(raw: &str) -> Option<IssueOrigin> {
    match raw {
        "manual" => Some(IssueOrigin::Manual),
        "quick_create" => Some(IssueOrigin::QuickCreate),
        "slack_chat" => Some(IssueOrigin::SlackChat),
        "lark_chat" => Some(IssueOrigin::LarkChat),
        "dingtalk_chat" => Some(IssueOrigin::DingTalkChat),
        "wecom_chat" => Some(IssueOrigin::WeComChat),
        "telegram_chat" => Some(IssueOrigin::TelegramChat),
        "agent_create" => Some(IssueOrigin::AgentCreate),
        "autopilot_run" => Some(IssueOrigin::AutopilotRun),
        "squad_handoff" => Some(IssueOrigin::SquadHandoff),
        _ => None,
    }
}

/// 逗号分隔参数 → 去空去重的 `Vec`（上游 `splitCommaParam`）。
pub fn split_comma_param(raw: &str) -> Option<Vec<String>> {
    let parts: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts)
    }
}

// ---------------------------------------------------------------------------
// Repo
// ---------------------------------------------------------------------------

impl IssueRepo {
    /// `workspace.slug` → identifier 前缀。
    pub async fn workspace_prefix(&self, workspace_id: Id) -> Result<String> {
        let slug: Option<String> = sqlx::query_scalar("SELECT slug FROM workspace WHERE id = $1")
            .bind(workspace_id.0)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(slug
            .as_deref()
            .map_or_else(|| "ISS".to_string(), issue_prefix_from_slug))
    }

    /// `MAX(number) + 1`（并发安全由 `UNIQUE(workspace_id, number)` + 重试兜底）。
    pub async fn next_number(&self, workspace_id: Id) -> Result<i32> {
        let next: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(number), 0) + 1 FROM issue WHERE workspace_id = $1",
        )
        .bind(workspace_id.0)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(next)
    }

    /// 终态 status key 集合：内置 done/cancelled + 目录里 `category = 'closed'` 的自定义 status。
    pub async fn terminal_status_keys(&self, workspace_id: Id) -> Result<Vec<String>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT key FROM issue_status WHERE workspace_id = $1 AND category = 'closed' ORDER BY position",
        )
        .bind(workspace_id.0)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        let mut keys = vec!["done".to_string(), "cancelled".to_string()];
        for (key,) in rows {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        Ok(keys)
    }

    /// 新建 issue：分配 `number` + `identifier`，position 追加到同 (workspace, status) 列尾。
    ///
    /// `UNIQUE(workspace_id, number)` 冲突时重试（并发创建同 workspace 的两个 issue）。
    pub async fn create(&self, input: NewIssue) -> Result<IssueRow> {
        let prefix = self.workspace_prefix(input.workspace_id).await?;
        let mut last_err = RepoError::Conflict;
        for _ in 0..NUMBER_ALLOC_RETRIES {
            let number = self.next_number(input.workspace_id).await?;
            let identifier = format!("{prefix}-{number}");
            match self.insert_new(&input, number, &identifier).await {
                Ok(row) => return Ok(row),
                Err(e) => {
                    if !matches!(e, RepoError::Conflict) {
                        return Err(e);
                    }
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }

    /// 单次 INSERT（`create` 的重试体）。
    async fn insert_new(
        &self,
        input: &NewIssue,
        number: i32,
        identifier: &str,
    ) -> Result<IssueRow> {
        sqlx::query_as::<_, IssueRow>(&format!(
            "INSERT INTO issue (workspace_id, number, identifier, title, description, status, \
                 status_name, priority, assignee_type, assignee_id, creator_type, creator_id, \
                 parent_issue_id, project_id, position, stage, start_date, due_date, \
                 metadata, properties, origin, last_activity_at, revision) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, \
                 (SELECT COALESCE(MAX(position), 0) + 1 FROM issue \
                   WHERE workspace_id = $1 AND status = $6), \
                 $15, $16, $17, $18, $19, $20, now(), 1) \
             RETURNING {ISSUE_COLUMNS}"
        ))
        .bind(input.workspace_id.0)
        .bind(number)
        .bind(identifier)
        .bind(input.title.as_str())
        .bind(input.description.as_deref())
        .bind(input.status.as_str())
        .bind(input.status_name.as_deref())
        .bind(input.priority.as_str())
        .bind(input.assignee_type.map(AssigneeType::as_str))
        .bind(input.assignee_id.as_deref())
        .bind(input.creator_type.as_str())
        .bind(input.creator_id.as_str())
        .bind(input.parent_issue_id.map(Id::as_uuid))
        .bind(input.project_id.map(Id::as_uuid))
        .bind(input.stage)
        .bind(input.start_date)
        .bind(input.due_date)
        .bind(input.metadata.clone())
        .bind(input.properties.clone())
        .bind(input.origin.map(IssueOrigin::as_str))
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 按 id 取（workspace 隔离：跨 workspace 的 id 一律 `NotFound`）。
    pub async fn get(&self, workspace_id: Id, id: Id) -> Result<IssueRow> {
        sqlx::query_as::<_, IssueRow>(&format!(
            "SELECT {ISSUE_COLUMNS} FROM issue WHERE workspace_id = $1 AND id = $2"
        ))
        .bind(workspace_id.0)
        .bind(id.0)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)
    }

    /// 按 identifier（如 `LUM-1`）取。
    pub async fn get_by_identifier(&self, workspace_id: Id, identifier: &str) -> Result<IssueRow> {
        sqlx::query_as::<_, IssueRow>(&format!(
            "SELECT {ISSUE_COLUMNS} FROM issue WHERE workspace_id = $1 AND identifier = $2"
        ))
        .bind(workspace_id.0)
        .bind(identifier)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)
    }

    /// 更新 issue（`revision + 1`；`expected_revision` 不匹配 → `Conflict`，行不存在 → `NotFound`）。
    ///
    /// 每次成功更新都会刷新 `updated_at` / `last_activity_at`（一次写入即一次活动）。
    pub async fn update(&self, workspace_id: Id, id: Id, patch: &IssueUpdate) -> Result<IssueRow> {
        let sql = format!(
            "UPDATE issue SET \
                 title = CASE WHEN $3::boolean THEN $4::text ELSE title END, \
                 description = CASE WHEN $5::boolean THEN $6::text ELSE description END, \
                 status = CASE WHEN $7::boolean THEN $8::text ELSE status END, \
                 status_name = CASE WHEN $7::boolean THEN $9::text ELSE status_name END, \
                 priority = CASE WHEN $10::boolean THEN $11::text ELSE priority END, \
                 assignee_type = CASE WHEN $12::boolean THEN $13::text ELSE assignee_type END, \
                 assignee_id = CASE WHEN $12::boolean THEN $14::text ELSE assignee_id END, \
                 parent_issue_id = CASE WHEN $15::boolean THEN $16::uuid ELSE parent_issue_id END, \
                 project_id = CASE WHEN $17::boolean THEN $18::uuid ELSE project_id END, \
                 position = CASE WHEN $19::boolean THEN $20::double precision ELSE position END, \
                 stage = CASE WHEN $21::boolean THEN $22::int4 ELSE stage END, \
                 start_date = CASE WHEN $23::boolean THEN $24::date ELSE start_date END, \
                 due_date = CASE WHEN $25::boolean THEN $26::date ELSE due_date END, \
                 metadata = CASE WHEN $27::boolean THEN $28::jsonb ELSE metadata END, \
                 properties = CASE WHEN $29::boolean THEN $30::jsonb ELSE properties END, \
                 revision = revision + 1, \
                 updated_at = now(), \
                 last_activity_at = now() \
             WHERE workspace_id = $1 AND id = $2 \
               AND ($31::bigint IS NULL OR revision = $31::bigint) \
             RETURNING {ISSUE_COLUMNS}"
        );

        let row = sqlx::query_as::<_, IssueRow>(&sql)
            .bind(workspace_id.0)
            .bind(id.0)
            .bind(patch.title.is_some())
            .bind(patch.title.as_deref())
            .bind(patch.description.is_some())
            .bind(patch.description.clone().flatten())
            .bind(patch.status.is_some())
            .bind(patch.status.as_deref())
            .bind(patch.status_name.clone().flatten())
            .bind(patch.priority.is_some())
            .bind(patch.priority.map(Priority::as_str))
            .bind(patch.assignee_type.is_some() || patch.assignee_id.is_some())
            .bind(patch.assignee_type.flatten().map(AssigneeType::as_str))
            .bind(patch.assignee_id.clone().flatten())
            .bind(patch.parent_issue_id.is_some())
            .bind(patch.parent_issue_id.flatten().map(Id::as_uuid))
            .bind(patch.project_id.is_some())
            .bind(patch.project_id.flatten().map(Id::as_uuid))
            .bind(patch.position.is_some())
            .bind(patch.position)
            .bind(patch.stage.is_some())
            .bind(patch.stage.flatten())
            .bind(patch.start_date.is_some())
            .bind(patch.start_date.flatten())
            .bind(patch.due_date.is_some())
            .bind(patch.due_date.flatten())
            .bind(patch.metadata.is_some())
            .bind(patch.metadata.clone())
            .bind(patch.properties.is_some())
            .bind(patch.properties.clone())
            .bind(patch.expected_revision)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;

        if let Some(row) = row {
            return Ok(row);
        }

        // 没有更新到行：区分「不存在」和「revision 不匹配」。
        let current: Option<i64> =
            sqlx::query_scalar("SELECT revision FROM issue WHERE workspace_id = $1 AND id = $2")
                .bind(workspace_id.0)
                .bind(id.0)
                .fetch_optional(self.db.pool())
                .await
                .map_err(map_sqlx_err)?;
        match current {
            None => Err(RepoError::NotFound),
            Some(_) => Err(RepoError::Conflict),
        }
    }

    /// 删除 issue（子 issue 的 `parent_issue_id` 由 FK `ON DELETE SET NULL` 处理）。
    pub async fn delete(&self, workspace_id: Id, id: Id) -> Result<()> {
        let affected = sqlx::query("DELETE FROM issue WHERE workspace_id = $1 AND id = $2")
            .bind(workspace_id.0)
            .bind(id.0)
            .execute(self.db.pool())
            .await
            .map_err(map_sqlx_err)?
            .rows_affected();
        if affected == 0 {
            return Err(RepoError::NotFound);
        }
        Ok(())
    }

    /// 过滤列表。
    pub async fn list(&self, filter: &IssueFilter) -> Result<Vec<IssueRow>> {
        let limit = filter
            .limit
            .unwrap_or(LIST_DEFAULT_LIMIT)
            .clamp(1, LIST_MAX_LIMIT);
        let offset = filter.offset.unwrap_or(0).max(0);
        let sql = format!(
            "SELECT {ISSUE_COLUMNS} FROM issue WHERE {LIST_WHERE} ORDER BY {} LIMIT $14 OFFSET $15",
            filter.order.as_sql()
        );
        sqlx::query_as::<_, IssueRow>(&sql)
            .bind(filter.workspace_id.0)
            .bind(filter.statuses.clone())
            .bind(filter.priorities.clone())
            .bind(filter.assignee_type.clone())
            .bind(filter.assignee_ids.clone())
            .bind(filter.creator_id.clone())
            .bind(filter.parent_issue_id.map(Id::as_uuid))
            .bind(filter.project_id.map(Id::as_uuid))
            .bind(filter.stage)
            .bind(filter.q.clone())
            .bind(filter.include_closed)
            .bind(filter.terminal_statuses.clone())
            .bind(filter.only_parentless)
            .bind(limit)
            .bind(offset)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 过滤列表 + 总数（同一 WHERE，两条查询）。
    pub async fn list_with_total(&self, filter: &IssueFilter) -> Result<(Vec<IssueRow>, i64)> {
        let rows = self.list(filter).await?;
        let total: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*)::bigint FROM issue WHERE {LIST_WHERE}"
        ))
        .bind(filter.workspace_id.0)
        .bind(filter.statuses.clone())
        .bind(filter.priorities.clone())
        .bind(filter.assignee_type.clone())
        .bind(filter.assignee_ids.clone())
        .bind(filter.creator_id.clone())
        .bind(filter.parent_issue_id.map(Id::as_uuid))
        .bind(filter.project_id.map(Id::as_uuid))
        .bind(filter.stage)
        .bind(filter.q.clone())
        .bind(filter.include_closed)
        .bind(filter.terminal_statuses.clone())
        .bind(filter.only_parentless)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok((rows, total))
    }

    /// 某个父 issue 的直接子 issue。
    pub async fn children_of(&self, workspace_id: Id, parent_id: Id) -> Result<Vec<IssueRow>> {
        let mut filter = IssueFilter::new(workspace_id);
        filter.parent_issue_id = Some(parent_id);
        filter.order = IssueOrderBy::PositionAsc;
        filter.include_closed = true;
        self.list(&filter).await
    }

    /// 多个父 issue 的子 issue（`GET /api/issues/children?parent_ids=`）。
    ///
    /// workspace 隔离在 SQL 层完成：不属于本 workspace 的 `parent_id` 自然返回 0 行。
    pub async fn children_of_parents(
        &self,
        workspace_id: Id,
        parent_ids: &[Uuid],
    ) -> Result<Vec<IssueRow>> {
        if parent_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, IssueRow>(&format!(
            "SELECT {ISSUE_COLUMNS} FROM issue \
             WHERE workspace_id = $1 AND parent_issue_id = ANY($2::uuid[]) \
             ORDER BY parent_issue_id, position ASC, number ASC"
        ))
        .bind(workspace_id.0)
        .bind(parent_ids)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 每个父 issue 的子 issue 进度（total / done）。`terminal_statuses` 决定什么算 done。
    pub async fn child_progress(
        &self,
        workspace_id: Id,
        terminal_statuses: &[String],
    ) -> Result<Vec<ChildProgressRow>> {
        sqlx::query_as::<_, ChildProgressRow>(
            "SELECT parent_issue_id, \
                    COUNT(*)::bigint AS total, \
                    COUNT(*) FILTER (WHERE status = ANY($2::text[]))::bigint AS done \
             FROM issue \
             WHERE workspace_id = $1 AND parent_issue_id IS NOT NULL \
             GROUP BY parent_issue_id \
             ORDER BY parent_issue_id",
        )
        .bind(workspace_id.0)
        .bind(terminal_statuses)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 分组计数（`/api/issues/grouped`、`table/facets`）。
    ///
    /// 只返回计数（key/total/done）；上游还会回传每组 issue 列表，M2-A 不做——见 docs/11 §5。
    pub async fn grouped_counts(
        &self,
        filter: &IssueFilter,
        group_by: IssueGroupField,
    ) -> Result<Vec<GroupedCountRow>> {
        let sql = format!(
            "SELECT {}::text AS key, COUNT(*)::bigint AS total, \
                    COUNT(*) FILTER (WHERE status = ANY($12::text[]))::bigint AS done \
             FROM issue WHERE {LIST_WHERE} \
             GROUP BY {} ORDER BY total DESC",
            group_by.group_expr(),
            group_by.group_expr()
        );
        sqlx::query_as::<_, GroupedCountRow>(&sql)
            .bind(filter.workspace_id.0)
            .bind(filter.statuses.clone())
            .bind(filter.priorities.clone())
            .bind(filter.assignee_type.clone())
            .bind(filter.assignee_ids.clone())
            .bind(filter.creator_id.clone())
            .bind(filter.parent_issue_id.map(Id::as_uuid))
            .bind(filter.project_id.map(Id::as_uuid))
            .bind(filter.stage)
            .bind(filter.q.clone())
            .bind(filter.include_closed)
            .bind(filter.terminal_statuses.clone())
            .bind(filter.only_parentless)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 分组字段名（用于响应回显）。
    pub fn group_field_name(field: IssueGroupField) -> &'static str {
        field.as_str()
    }

    /// 批量更新：逐行复用 `update`，返回成功行数。
    ///
    /// 与上游一致：patch 为空时返回 0（不做 no-op 写入）。
    pub async fn batch_update(
        &self,
        workspace_id: Id,
        ids: &[Id],
        patch: &IssueUpdate,
    ) -> Result<u64> {
        if patch.is_empty() {
            return Ok(0);
        }
        let mut updated = 0_u64;
        for id in ids {
            if self.update(workspace_id, *id, patch).await.is_ok() {
                updated += 1;
            }
        }
        Ok(updated)
    }

    /// 批量删除：返回删除行数（workspace 隔离）。
    pub async fn batch_delete(&self, workspace_id: Id, ids: &[Id]) -> Result<u64> {
        if ids.is_empty() {
            return Ok(0);
        }
        let raw: Vec<Uuid> = ids.iter().map(|id| id.0).collect();
        let affected =
            sqlx::query("DELETE FROM issue WHERE workspace_id = $1 AND id = ANY($2::uuid[])")
                .bind(workspace_id.0)
                .bind(&raw)
                .execute(self.db.pool())
                .await
                .map_err(map_sqlx_err)?
                .rows_affected();
        Ok(affected)
    }

    /// 拖拽排序：用 `before_id` / `after_id` 两个锚点推导新 position 并落库。
    ///
    /// 锚点不属于本 workspace → `NotFound`；锚点顺序错乱 / 间距过小 → `Conflict`。
    pub async fn move_issue(
        &self,
        workspace_id: Id,
        id: Id,
        before_id: Option<Id>,
        after_id: Option<Id>,
    ) -> Result<IssueRow> {
        self.move_issue_with_update(
            workspace_id,
            id,
            before_id,
            after_id,
            &IssueUpdate::default(),
        )
        .await
    }

    /// 拖拽排序 + 同一次写入里合并其它字段补丁（上游 `MoveIssue` 把 `position` 塞进
    /// `UpdateIssueRequest` 后委托 `UpdateIssue`，只产生一次 revision 自增）。
    ///
    /// `patch.position` 会被锚点推导出的值覆盖；`patch.expected_revision` 生效
    /// （不匹配 → `Conflict`）。
    pub async fn move_issue_with_update(
        &self,
        workspace_id: Id,
        id: Id,
        before_id: Option<Id>,
        after_id: Option<Id>,
        patch: &IssueUpdate,
    ) -> Result<IssueRow> {
        let current = self.get(workspace_id, id).await?;
        let before = match before_id {
            Some(anchor) => Some(self.anchor_position(workspace_id, anchor).await?),
            None => None,
        };
        let after = match after_id {
            Some(anchor) => Some(self.anchor_position(workspace_id, anchor).await?),
            None => None,
        };
        let position =
            derive_move_position(before, after, current.position).ok_or(RepoError::Conflict)?;
        let merged = IssueUpdate {
            position: Some(position),
            ..patch.clone()
        };
        self.update(workspace_id, id, &merged).await
    }

    /// `candidate_id` 是否是 `issue_id` 的祖先（含直接父）。
    ///
    /// 用于拒绝会在父子链上成环的 reparent（上游在 `issueCycleError` 里做同样的检查）。
    /// 递归用 `UNION`（不是 `UNION ALL`）——万一历史数据已有环也能终止。
    pub async fn has_ancestor(
        &self,
        workspace_id: Id,
        issue_id: Id,
        candidate_id: Id,
    ) -> Result<bool> {
        let found: Option<Uuid> = sqlx::query_scalar(
            "WITH RECURSIVE up(parent_issue_id) AS ( \
                 SELECT parent_issue_id FROM issue WHERE workspace_id = $1 AND id = $2 \
                 UNION \
                 SELECT i.parent_issue_id FROM issue i JOIN up ON i.id = up.parent_issue_id \
             ) SELECT parent_issue_id FROM up WHERE parent_issue_id = $3 LIMIT 1",
        )
        .bind(workspace_id.0)
        .bind(issue_id.0)
        .bind(candidate_id.0)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(found.is_some())
    }

    async fn anchor_position(&self, workspace_id: Id, id: Id) -> Result<f64> {
        let position: Option<f64> =
            sqlx::query_scalar("SELECT position FROM issue WHERE workspace_id = $1 AND id = $2")
                .bind(workspace_id.0)
                .bind(id.0)
                .fetch_optional(self.db.pool())
                .await
                .map_err(map_sqlx_err)?;
        position.ok_or(RepoError::NotFound)
    }

    /// workspace 内 issue 总数（`GET /api/issues/limit-usage`）。
    pub async fn count_in_workspace(&self, workspace_id: Id) -> Result<i64> {
        sqlx::query_scalar("SELECT COUNT(*)::bigint FROM issue WHERE workspace_id = $1")
            .bind(workspace_id.0)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    // ---- metadata / properties（JSONB） -----------------------------------

    /// 读取 `metadata`。
    pub async fn get_metadata(&self, workspace_id: Id, id: Id) -> Result<JsonValue> {
        let row = self.get(workspace_id, id).await?;
        Ok(row.metadata)
    }

    /// 写单个 metadata key，返回写入后的完整 metadata。
    pub async fn set_metadata_key(
        &self,
        workspace_id: Id,
        id: Id,
        key: &str,
        value: &JsonValue,
    ) -> Result<JsonValue> {
        let meta: Option<JsonValue> = sqlx::query_scalar(
            "UPDATE issue SET metadata = jsonb_set(metadata, ARRAY[$3::text], $4::jsonb, true), \
                    revision = revision + 1, updated_at = now(), last_activity_at = now() \
             WHERE workspace_id = $1 AND id = $2 RETURNING metadata",
        )
        .bind(workspace_id.0)
        .bind(id.0)
        .bind(key)
        .bind(value)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        meta.ok_or(RepoError::NotFound)
    }

    /// 删除单个 metadata key，返回删除后的完整 metadata。
    pub async fn delete_metadata_key(
        &self,
        workspace_id: Id,
        id: Id,
        key: &str,
    ) -> Result<JsonValue> {
        let meta: Option<JsonValue> = sqlx::query_scalar(
            "UPDATE issue SET metadata = metadata - $3::text, \
                    revision = revision + 1, updated_at = now(), last_activity_at = now() \
             WHERE workspace_id = $1 AND id = $2 RETURNING metadata",
        )
        .bind(workspace_id.0)
        .bind(id.0)
        .bind(key)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        meta.ok_or(RepoError::NotFound)
    }

    /// 写单个 property，返回写入后的完整 properties。
    pub async fn set_property(
        &self,
        workspace_id: Id,
        id: Id,
        property_id: &str,
        value: &JsonValue,
    ) -> Result<JsonValue> {
        let props: Option<JsonValue> = sqlx::query_scalar(
            "UPDATE issue SET properties = jsonb_set(properties, ARRAY[$3::text], $4::jsonb, true), \
                    revision = revision + 1, updated_at = now(), last_activity_at = now() \
             WHERE workspace_id = $1 AND id = $2 RETURNING properties",
        )
        .bind(workspace_id.0)
        .bind(id.0)
        .bind(property_id)
        .bind(value)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        props.ok_or(RepoError::NotFound)
    }

    /// 删除单个 property，返回删除后的完整 properties。
    pub async fn delete_property(
        &self,
        workspace_id: Id,
        id: Id,
        property_id: &str,
    ) -> Result<JsonValue> {
        let props: Option<JsonValue> = sqlx::query_scalar(
            "UPDATE issue SET properties = properties - $3::text, \
                    revision = revision + 1, updated_at = now(), last_activity_at = now() \
             WHERE workspace_id = $1 AND id = $2 RETURNING properties",
        )
        .bind(workspace_id.0)
        .bind(id.0)
        .bind(property_id)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        props.ok_or(RepoError::NotFound)
    }

    // ---- reactions -------------------------------------------------------

    /// issue 的 reactions。
    pub async fn list_reactions(&self, issue_id: Id) -> Result<Vec<IssueReactionRow>> {
        sqlx::query_as::<_, IssueReactionRow>(
            "SELECT id, issue_id, workspace_id, actor_type, actor_id, emoji, created_at \
             FROM issue_reaction WHERE issue_id = $1 ORDER BY created_at",
        )
        .bind(issue_id.0)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 幂等加 reaction（同一 actor + emoji 重复 POST 返回同一条）。
    pub async fn add_reaction(
        &self,
        workspace_id: Id,
        issue_id: Id,
        actor_type: &str,
        actor_id: &str,
        emoji: &str,
    ) -> Result<IssueReactionRow> {
        sqlx::query_as::<_, IssueReactionRow>(
            "INSERT INTO issue_reaction (issue_id, workspace_id, actor_type, actor_id, emoji) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (issue_id, actor_type, actor_id, emoji) \
             DO UPDATE SET emoji = EXCLUDED.emoji \
             RETURNING id, issue_id, workspace_id, actor_type, actor_id, emoji, created_at",
        )
        .bind(issue_id.0)
        .bind(workspace_id.0)
        .bind(actor_type)
        .bind(actor_id)
        .bind(emoji)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 幂等删 reaction；不存在 → `NotFound`。
    pub async fn remove_reaction(
        &self,
        issue_id: Id,
        actor_type: &str,
        actor_id: &str,
        emoji: &str,
    ) -> Result<IssueReactionRow> {
        sqlx::query_as::<_, IssueReactionRow>(
            "DELETE FROM issue_reaction \
             WHERE issue_id = $1 AND actor_type = $2 AND actor_id = $3 AND emoji = $4 \
             RETURNING id, issue_id, workspace_id, actor_type, actor_id, emoji, created_at",
        )
        .bind(issue_id.0)
        .bind(actor_type)
        .bind(actor_id)
        .bind(emoji)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn prefix_from_slug_uppercases_and_truncates() {
        assert_eq!(issue_prefix_from_slug("acme-corp"), "ACMECORP");
        assert_eq!(issue_prefix_from_slug("lum"), "LUM");
        assert_eq!(issue_prefix_from_slug("abcdefghijkl"), "ABCDEFGH");
        assert_eq!(issue_prefix_from_slug("!!!"), "ISS");
        assert_eq!(issue_prefix_from_slug(""), "ISS");
    }

    #[test]
    fn move_position_between_two_anchors() {
        assert_eq!(derive_move_position(Some(1.0), Some(2.0), 5.0), Some(1.5));
        assert_eq!(derive_move_position(Some(1.0), None, 5.0), Some(2.0));
        assert_eq!(derive_move_position(None, Some(4.0), 5.0), Some(3.0));
        assert_eq!(derive_move_position(None, None, 7.5), Some(7.5));
    }

    #[test]
    fn move_position_rejects_bad_anchors() {
        // 顺序错乱
        assert_eq!(derive_move_position(Some(2.0), Some(1.0), 0.0), None);
        // 相邻浮点无法二分（间距过小）
        assert_eq!(derive_move_position(Some(1.0), Some(1.0), 0.0), None);
        // 非有限值 / 溢出为 NaN
        assert_eq!(derive_move_position(Some(f64::INFINITY), None, 0.0), None);
        assert_eq!(
            derive_move_position(Some(f64::MAX), Some(f64::INFINITY), 0.0),
            None
        );
    }

    #[test]
    fn split_comma_param_trims_and_drops_empties() {
        assert_eq!(
            split_comma_param(" todo , in_progress ,, "),
            Some(vec!["todo".into(), "in_progress".into()])
        );
        assert_eq!(split_comma_param("   "), None);
        assert_eq!(split_comma_param(""), None);
    }

    #[test]
    fn update_is_empty_ignores_expected_revision() {
        let patch = IssueUpdate {
            expected_revision: Some(3),
            ..IssueUpdate::default()
        };
        assert!(patch.is_empty());
        let patch = IssueUpdate {
            title: Some("x".into()),
            ..IssueUpdate::default()
        };
        assert!(!patch.is_empty());
        let patch = IssueUpdate {
            description: Some(None), // 显式置空也是变更
            ..IssueUpdate::default()
        };
        assert!(!patch.is_empty());
    }

    #[test]
    fn group_field_whitelist() {
        assert_eq!(IssueGroupField::parse(""), Some(IssueGroupField::Status));
        assert_eq!(
            IssueGroupField::parse("priority"),
            Some(IssueGroupField::Priority)
        );
        assert_eq!(
            IssueGroupField::parse("assignee"),
            Some(IssueGroupField::Assignee)
        );
        assert_eq!(
            IssueGroupField::parse("project_id"),
            Some(IssueGroupField::Project)
        );
        assert_eq!(IssueGroupField::parse("drop table"), None);
        assert_eq!(IssueGroupField::Status.as_str(), "status");
    }

    #[test]
    fn list_where_placeholders_are_unique() {
        // 防回归：WHERE 片段里 $1..$13 各出现至少一次，且不含 $14/$15（留给 LIMIT/OFFSET）。
        for n in 1..=13 {
            assert!(
                LIST_WHERE.contains(&format!("${n}")),
                "missing ${n} in LIST_WHERE"
            );
        }
        assert!(!LIST_WHERE.contains("$14"));
    }

    #[test]
    fn filter_defaults_include_closed() {
        let f = IssueFilter::new(Id::nil());
        assert!(f.include_closed);
        assert!(f.terminal_statuses.is_empty());
        assert_eq!(f.limit, None);
        assert_eq!(f.order, IssueOrderBy::UpdatedDesc);
    }
}

// ---------------------------------------------------------------------------
// PG 集成测试（需要真库）
//
// 运行：
//   MULTICA_TEST_DATABASE_URL=postgres://... \
//     cargo test -p mc-repos --lib -- --ignored issue
// ---------------------------------------------------------------------------
#[cfg(test)]
mod db_tests {
    use super::*;
    use std::env;

    struct Fixture {
        db: Db,
        workspace_id: Id,
        user_id: Id,
    }

    async fn setup() -> Option<Fixture> {
        let url = env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        let db = Db::connect(&url, 4, 1).await.ok()?;
        let pool = db.pool();
        let workspace_id: Uuid = sqlx::query_scalar(
            "INSERT INTO workspace(name, slug) VALUES ('itest-m2a', $1) RETURNING id",
        )
        .bind(format!("itest-m2a-{}", Uuid::new_v4()))
        .fetch_one(pool)
        .await
        .ok()?;
        let user_id: Uuid = sqlx::query_scalar(
            r#"INSERT INTO "user"(name, email) VALUES ('itest-m2a', $1) RETURNING id"#,
        )
        .bind(format!("itest-m2a-{}@example.com", Uuid::new_v4()))
        .fetch_one(pool)
        .await
        .ok()?;
        Some(Fixture {
            db,
            workspace_id: Id::from(workspace_id),
            user_id: Id::from(user_id),
        })
    }

    async fn teardown(fx: &Fixture) {
        let pool = fx.db.pool();
        let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
            .bind(fx.workspace_id.0)
            .execute(pool)
            .await;
        let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
            .bind(fx.user_id.0)
            .execute(pool)
            .await;
    }

    fn new_issue(fx: &Fixture, title: &str) -> NewIssue {
        NewIssue::new(fx.workspace_id, title, fx.user_id.0.to_string())
    }

    macro_rules! fixture {
        () => {
            match setup().await {
                Some(fx) => fx,
                None => {
                    eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
                    return;
                }
            }
        };
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_create_get_update_delete_roundtrip() {
        let fx = fixture!();
        let repo = IssueRepo::new(fx.db.clone());

        let created = repo.create(new_issue(&fx, "first")).await.expect("create");
        assert_eq!(created.number, 1);
        assert!(created.identifier.ends_with("-1"), "{}", created.identifier);
        assert_eq!(created.status, "todo");
        assert_eq!(created.priority, "none");
        assert_eq!(created.revision, 1);
        assert!((created.position - 1.0).abs() < f64::EPSILON);

        let by_id = repo
            .get(fx.workspace_id, created.id())
            .await
            .expect("get by id");
        assert_eq!(by_id.id, created.id);
        assert_eq!(by_id.title, "first");

        let by_ident = repo
            .get_by_identifier(fx.workspace_id, &created.identifier)
            .await
            .expect("get by identifier");
        assert_eq!(by_ident.id, created.id);

        // 更新：标题 + 显式清空 description + priority
        let patch = IssueUpdate {
            title: Some("renamed".into()),
            description: Some(None),
            priority: Some(Priority::High),
            ..IssueUpdate::default()
        };
        let updated = repo
            .update(fx.workspace_id, created.id(), &patch)
            .await
            .expect("update");
        assert_eq!(updated.title, "renamed");
        assert_eq!(updated.description, None);
        assert_eq!(updated.priority, "high");
        assert_eq!(updated.revision, 2);

        // 乐观并发：旧 revision 被拒
        let stale = IssueUpdate {
            expected_revision: Some(1),
            title: Some("nope".into()),
            ..IssueUpdate::default()
        };
        assert!(matches!(
            repo.update(fx.workspace_id, created.id(), &stale).await,
            Err(RepoError::Conflict)
        ));

        // 正确 revision 通过
        let fresh = IssueUpdate {
            expected_revision: Some(2),
            title: Some("renamed-again".into()),
            ..IssueUpdate::default()
        };
        assert_eq!(
            repo.update(fx.workspace_id, created.id(), &fresh)
                .await
                .expect("update with rev")
                .revision,
            3
        );

        // 不存在的 id → NotFound（而不是 Conflict）
        assert!(matches!(
            repo.update(fx.workspace_id, Id::new(), &fresh).await,
            Err(RepoError::NotFound)
        ));

        repo.delete(fx.workspace_id, created.id())
            .await
            .expect("delete");
        assert!(matches!(
            repo.get(fx.workspace_id, created.id()).await,
            Err(RepoError::NotFound)
        ));
        assert!(matches!(
            repo.delete(fx.workspace_id, created.id()).await,
            Err(RepoError::NotFound)
        ));

        teardown(&fx).await;
        fx.db.close().await;
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_number_is_per_workspace_and_monotonic() {
        let fx = fixture!();
        let repo = IssueRepo::new(fx.db.clone());

        let mut numbers = Vec::new();
        for i in 0..3 {
            numbers.push(
                repo.create(new_issue(&fx, &format!("n{i}")))
                    .await
                    .expect("create")
                    .number,
            );
        }
        assert_eq!(numbers, vec![1, 2, 3]);
        assert_eq!(repo.next_number(fx.workspace_id).await.expect("next"), 4);

        // 另一个 workspace 的编号互不影响，identifier 前缀也不同
        let other_ws: Uuid = sqlx::query_scalar(
            "INSERT INTO workspace(name, slug) VALUES ('itest-m2a-other', $1) RETURNING id",
        )
        .bind(format!("itest-m2a-b-{}", Uuid::new_v4()))
        .fetch_one(fx.db.pool())
        .await
        .expect("other ws");
        let other_ws = Id::from(other_ws);
        let mut other = NewIssue::new(other_ws, "b1", fx.user_id.0.to_string());
        other.status = "backlog".into();
        let other_row = repo.create(other).await.expect("create other");
        assert_eq!(other_row.number, 1);
        assert_ne!(other_row.identifier, "ISS-1");

        // 同 (workspace, number) 唯一约束仍在
        let dup = sqlx::query(
            "INSERT INTO issue (workspace_id, number, identifier, title, creator_type, creator_id) \
             VALUES ($1, $2, $3, 'dup', 'user', $4)",
        )
        .bind(fx.workspace_id.0)
        .bind(1_i32)
        .bind("DUP-1")
        .bind(fx.user_id.0.to_string())
        .execute(fx.db.pool())
        .await;
        assert!(dup.is_err(), "duplicate (workspace_id, number) must fail");

        let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
            .bind(other_ws.0)
            .execute(fx.db.pool())
            .await;
        teardown(&fx).await;
        fx.db.close().await;
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_list_search_and_total() {
        let fx = fixture!();
        let repo = IssueRepo::new(fx.db.clone());

        let mut backlog = new_issue(&fx, "backlog item");
        backlog.status = "backlog".into();
        let mut done = new_issue(&fx, "finished item");
        done.status = "done".into();
        done.priority = Priority::Urgent;
        let mut assigned = new_issue(&fx, "searchable needle");
        assigned.assignee_type = Some(AssigneeType::Agent);
        assigned.assignee_id = Some("agent-1".into());
        for issue in [backlog, done, assigned] {
            repo.create(issue).await.expect("create");
        }

        // 默认：包含终态
        let all = IssueFilter::new(fx.workspace_id);
        let (rows, total) = repo.list_with_total(&all).await.expect("list");
        assert_eq!(rows.len(), 3);
        assert_eq!(total, 3);

        // 排除终态（模拟 handler 传入 terminal_statuses）
        let mut open_only = IssueFilter::new(fx.workspace_id);
        open_only.include_closed = false;
        open_only.terminal_statuses = repo
            .terminal_status_keys(fx.workspace_id)
            .await
            .expect("terminal keys");
        assert!(open_only.terminal_statuses.contains(&"done".to_string()));
        let open_rows = repo.list(&open_only).await.expect("list open");
        assert_eq!(open_rows.len(), 2);

        // status 过滤
        let mut by_status = IssueFilter::new(fx.workspace_id);
        by_status.statuses = Some(vec!["backlog".into(), "done".into()]);
        assert_eq!(repo.list(&by_status).await.expect("by status").len(), 2);

        // priority 过滤
        let mut by_priority = IssueFilter::new(fx.workspace_id);
        by_priority.priorities = Some(vec!["urgent".into()]);
        let urgent = repo.list(&by_priority).await.expect("by priority");
        assert_eq!(urgent.len(), 1);
        assert_eq!(urgent[0].title, "finished item");

        // assignee 过滤
        let mut by_assignee = IssueFilter::new(fx.workspace_id);
        by_assignee.assignee_type = Some("agent".into());
        by_assignee.assignee_ids = Some(vec!["agent-1".into()]);
        assert_eq!(repo.list(&by_assignee).await.expect("by assignee").len(), 1);

        // q 搜索（title / description）
        let mut by_q = IssueFilter::new(fx.workspace_id);
        by_q.q = Some("needle".into());
        let found = repo.list(&by_q).await.expect("by q");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].title, "searchable needle");

        // 分页
        let mut paged = IssueFilter::new(fx.workspace_id);
        paged.limit = Some(2);
        paged.offset = Some(1);
        paged.order = IssueOrderBy::NumberAsc;
        let page = repo.list(&paged).await.expect("paged");
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].number, 2);

        teardown(&fx).await;
        fx.db.close().await;
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_children_progress_and_grouped() {
        let fx = fixture!();
        let repo = IssueRepo::new(fx.db.clone());
        let status_repo = crate::issue_status::IssueStatusRepo::new(fx.db.clone());
        status_repo
            .ensure_defaults(fx.workspace_id)
            .await
            .expect("ensure defaults");

        let parent = repo.create(new_issue(&fx, "parent")).await.expect("parent");
        let mut child_a = new_issue(&fx, "child a");
        child_a.parent_issue_id = Some(parent.id());
        let mut child_b = new_issue(&fx, "child b");
        child_b.parent_issue_id = Some(parent.id());
        child_b.status = "done".into();
        let child_a = repo.create(child_a).await.expect("child a");
        let child_b = repo.create(child_b).await.expect("child b");

        let children = repo
            .children_of(fx.workspace_id, parent.id())
            .await
            .expect("children");
        assert_eq!(children.len(), 2);

        let many = repo
            .children_of_parents(fx.workspace_id, &[parent.id().0])
            .await
            .expect("children of parents");
        assert_eq!(many.len(), 2);
        assert!(repo
            .children_of_parents(fx.workspace_id, &[])
            .await
            .expect("empty parents")
            .is_empty());

        let progress = repo
            .child_progress(
                fx.workspace_id,
                &repo
                    .terminal_status_keys(fx.workspace_id)
                    .await
                    .expect("terminal"),
            )
            .await
            .expect("progress");
        assert_eq!(progress.len(), 1);
        assert_eq!(progress[0].parent_issue_id, parent.id().0);
        assert_eq!(progress[0].total, 2);
        assert_eq!(progress[0].done, 1);

        // 只看顶层
        let mut top = IssueFilter::new(fx.workspace_id);
        top.only_parentless = true;
        assert_eq!(repo.list(&top).await.expect("top level").len(), 1);

        // 分组计数
        let groups = repo
            .grouped_counts(&IssueFilter::new(fx.workspace_id), IssueGroupField::Status)
            .await
            .expect("grouped");
        let todo = groups.iter().find(|g| g.key.as_deref() == Some("todo"));
        assert_eq!(todo.map(|g| g.total), Some(2));

        // 删除父节点：子节点 parent_issue_id 置空，不级联删子节点
        repo.delete(fx.workspace_id, parent.id())
            .await
            .expect("delete parent");
        let orphan = repo
            .get(fx.workspace_id, child_a.id())
            .await
            .expect("orphan");
        assert_eq!(orphan.parent_issue_id, None);
        assert!(repo.get(fx.workspace_id, child_b.id()).await.is_ok());

        teardown(&fx).await;
        fx.db.close().await;
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_move_batch_and_jsonb() {
        let fx = fixture!();
        let repo = IssueRepo::new(fx.db.clone());

        let a = repo.create(new_issue(&fx, "a")).await.expect("a");
        let b = repo.create(new_issue(&fx, "b")).await.expect("b");
        let c = repo.create(new_issue(&fx, "c")).await.expect("c");
        assert!(a.position < b.position && b.position < c.position);

        // 把 c 移到 a 之前
        let moved = repo
            .move_issue(fx.workspace_id, c.id(), Some(a.id()), Some(b.id()))
            .await
            .expect("move");
        assert!(moved.position > a.position && moved.position < b.position);

        // 锚点顺序错乱 → Conflict；锚点不存在 → NotFound
        assert!(matches!(
            repo.move_issue(fx.workspace_id, c.id(), Some(b.id()), Some(a.id()))
                .await,
            Err(RepoError::Conflict)
        ));
        assert!(matches!(
            repo.move_issue(fx.workspace_id, c.id(), Some(Id::new()), None)
                .await,
            Err(RepoError::NotFound)
        ));

        // 批量更新 / 删除
        let patch = IssueUpdate {
            priority: Some(Priority::Medium),
            ..IssueUpdate::default()
        };
        let updated = repo
            .batch_update(fx.workspace_id, &[a.id(), b.id(), Id::new()], &patch)
            .await
            .expect("batch update");
        assert_eq!(updated, 2);
        assert_eq!(
            repo.batch_update(fx.workspace_id, &[a.id()], &IssueUpdate::default())
                .await
                .expect("empty patch"),
            0
        );

        // metadata / properties
        let meta = repo
            .set_metadata_key(
                fx.workspace_id,
                a.id(),
                "branch",
                &serde_json::json!("main"),
            )
            .await
            .expect("set metadata");
        assert_eq!(meta["branch"], serde_json::json!("main"));
        let meta = repo
            .delete_metadata_key(fx.workspace_id, a.id(), "branch")
            .await
            .expect("del metadata");
        assert!(meta.get("branch").is_none());

        let props = repo
            .set_property(fx.workspace_id, a.id(), "size", &serde_json::json!(3))
            .await
            .expect("set property");
        assert_eq!(props["size"], serde_json::json!(3));
        let props = repo
            .delete_property(fx.workspace_id, a.id(), "size")
            .await
            .expect("del property");
        assert!(props.get("size").is_none());
        assert!(matches!(
            repo.set_metadata_key(fx.workspace_id, Id::new(), "x", &serde_json::json!(1))
                .await,
            Err(RepoError::NotFound)
        ));

        // reactions：幂等加 / 删
        let r1 = repo
            .add_reaction(fx.workspace_id, a.id(), "user", "u1", "👍")
            .await
            .expect("react");
        let r2 = repo
            .add_reaction(fx.workspace_id, a.id(), "user", "u1", "👍")
            .await
            .expect("react again");
        assert_eq!(r1.id, r2.id, "duplicate reaction must be idempotent");
        assert_eq!(repo.list_reactions(a.id()).await.expect("list").len(), 1);
        repo.remove_reaction(a.id(), "user", "u1", "👍")
            .await
            .expect("unreact");
        assert!(repo.list_reactions(a.id()).await.expect("list").is_empty());
        assert!(matches!(
            repo.remove_reaction(a.id(), "user", "u1", "👍").await,
            Err(RepoError::NotFound)
        ));

        assert_eq!(
            repo.batch_delete(fx.workspace_id, &[b.id(), c.id()])
                .await
                .expect("batch delete"),
            2
        );

        teardown(&fx).await;
        fx.db.close().await;
    }

    #[tokio::test]
    #[ignore = "needs MULTICA_TEST_DATABASE_URL"]
    async fn db_move_merges_patch_and_detects_ancestor() {
        let fx = fixture!();
        let repo = IssueRepo::new(fx.db.clone());

        let parent = repo.create(new_issue(&fx, "parent")).await.expect("parent");
        let child = repo.create(new_issue(&fx, "child")).await.expect("child");
        let anchor = repo.create(new_issue(&fx, "anchor")).await.expect("anchor");

        // has_ancestor 的方向：child 的祖先里有 parent，反之不成立
        assert!(!repo
            .has_ancestor(fx.workspace_id, child.id(), parent.id())
            .await
            .expect("ancestor before"));
        let reparented = repo
            .update(
                fx.workspace_id,
                child.id(),
                &IssueUpdate {
                    parent_issue_id: Some(Some(parent.id())),
                    ..IssueUpdate::default()
                },
            )
            .await
            .expect("reparent");
        assert_eq!(reparented.parent_issue_id, Some(parent.id().0));
        assert!(repo
            .has_ancestor(fx.workspace_id, child.id(), parent.id())
            .await
            .expect("ancestor after"));
        assert!(!repo
            .has_ancestor(fx.workspace_id, parent.id(), child.id())
            .await
            .expect("reverse"));

        // move + 其它字段补丁 = 一次写入（revision 只 +1）
        let moved = repo
            .move_issue_with_update(
                fx.workspace_id,
                child.id(),
                None,
                Some(anchor.id()),
                &IssueUpdate {
                    title: Some("renamed".to_string()),
                    priority: Some(Priority::High),
                    ..IssueUpdate::default()
                },
            )
            .await
            .expect("move + patch");
        assert_eq!(moved.title, "renamed");
        assert_eq!(moved.priority, "high");
        assert_eq!(
            moved.revision,
            reparented.revision + 1,
            "move + patch must be a single revision bump"
        );
        assert_eq!(
            moved.parent_issue_id,
            Some(parent.id().0),
            "patch 不应丢掉已有 parent"
        );

        teardown(&fx).await;
        fx.db.close().await;
    }
}
