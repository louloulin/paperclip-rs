use chrono::{DateTime, NaiveDate, Utc};
use mc_core::issue::{AssigneeType, IssueOrigin};
use mc_core::priority::Priority;
use mc_core::status::{IssueStatus, StatusCategory};
use mc_core::Id;
use serde_json::Value as JsonValue;
use sqlx::FromRow;
use uuid::Uuid;

use super::util::{parse_assignee_type, parse_issue_origin};

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
    ///
    /// 上游的终态有两档（`done` / `closed`），只比 `"closed"` 会漏掉 `category =
    /// 'done'` 的自定义 status（兼容写法 `closed` 仍然算）。
    pub fn is_closed(&self) -> bool {
        crate::issue_status::parse_category(&self.category).is_some_and(StatusCategory::is_terminal)
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
