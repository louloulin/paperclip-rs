use mc_core::issue::{AssigneeType, IssueOrigin};
use mc_core::priority::Priority;
use mc_core::Id;
use serde_json::Value as JsonValue;

use chrono::NaiveDate;

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
    pub(super) fn as_sql(self) -> &'static str {
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

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Priority => "priority",
            Self::Assignee => "assignee",
            Self::Project => "project",
        }
    }

    pub(super) fn group_expr(self) -> &'static str {
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
