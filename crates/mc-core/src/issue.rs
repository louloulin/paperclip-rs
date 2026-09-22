//! Issue 领域类型（核心实体）。

use serde::{Deserialize, Serialize};

use super::id::Id;
use super::priority::Priority;
use super::status::IssueStatus;
use super::timestamp::Timestamp;

/// Issue assignee 的多态类型（与 multica 一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssigneeType {
    User,
    Agent,
    Squad,
    Autopilot,
}

impl AssigneeType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
            Self::Squad => "squad",
            Self::Autopilot => "autopilot",
        }
    }
}

/// Issue origin 标识：从哪里创建的（人工 / autopilot / channel / agent / squad）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueOrigin {
    Manual,
    QuickCreate,
    SlackChat,
    LarkChat,
    DingTalkChat,
    WeComChat,
    TelegramChat,
    AgentCreate,
    AutopilotRun,
    SquadHandoff,
}

impl IssueOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::QuickCreate => "quick_create",
            Self::SlackChat => "slack_chat",
            Self::LarkChat => "lark_chat",
            Self::DingTalkChat => "dingtalk_chat",
            Self::WeComChat => "wecom_chat",
            Self::TelegramChat => "telegram_chat",
            Self::AgentCreate => "agent_create",
            Self::AutopilotRun => "autopilot_run",
            Self::SquadHandoff => "squad_handoff",
        }
    }
}

/// Issue 主体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub id: Id,
    pub workspace_id: Id,
    pub number: i32,
    pub identifier: String,
    pub title: String,
    pub description: Option<String>,
    pub status: IssueStatus,
    pub status_name: String,
    pub priority: Priority,
    pub assignee_type: Option<AssigneeType>,
    pub assignee_id: Option<Id>,
    pub creator_type: String, // user / agent / system
    pub creator_id: String,   // user id or agent id
    pub parent_issue_id: Option<Id>,
    pub project_id: Option<Id>,
    pub position: f64,
    pub stage: Option<i32>,
    pub start_date: Option<chrono::NaiveDate>,
    pub due_date: Option<chrono::NaiveDate>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub last_activity_at: Option<Timestamp>,
    pub revision: i64,
    pub metadata: serde_json::Value,
    pub properties: serde_json::Value,
    pub triage_state: Option<String>,
    pub origin: Option<IssueOrigin>,
    pub origin_task_id: Option<Id>,
    pub source_context_id: Option<Id>,
}

/// Issue 创建请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewIssue {
    pub workspace_id: Id,
    pub title: String,
    pub description: Option<String>,
    pub priority: Priority,
    pub status: Option<IssueStatus>,
    pub assignee_type: Option<AssigneeType>,
    pub assignee_id: Option<Id>,
    pub parent_issue_id: Option<Id>,
    pub project_id: Option<Id>,
    pub start_date: Option<chrono::NaiveDate>,
    pub due_date: Option<chrono::NaiveDate>,
    pub metadata: Option<serde_json::Value>,
    pub properties: Option<serde_json::Value>,
}

/// Issue 更新请求。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueUpdate {
    pub title: Option<String>,
    pub description: Option<String>,
    pub priority: Option<Priority>,
    pub status: Option<IssueStatus>,
    pub assignee_type: Option<AssigneeType>,
    pub assignee_id: Option<Id>,
    pub project_id: Option<Id>,
    pub position: Option<f64>,
    pub stage: Option<i32>,
    pub start_date: Option<chrono::NaiveDate>,
    pub due_date: Option<chrono::NaiveDate>,
    pub metadata: Option<serde_json::Value>,
    pub properties: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_origin_str_round_trip() {
        let origins = [
            IssueOrigin::Manual,
            IssueOrigin::SlackChat,
            IssueOrigin::LarkChat,
            IssueOrigin::TelegramChat,
        ];
        for o in origins {
            assert!(!o.as_str().is_empty());
        }
    }

    #[test]
    fn assignee_type_str_round_trip() {
        for a in [AssigneeType::User, AssigneeType::Agent, AssigneeType::Squad] {
            assert!(!a.as_str().is_empty());
        }
    }
}
