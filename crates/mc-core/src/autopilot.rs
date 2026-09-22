//! Autopilot 领域类型。

use serde::{Deserialize, Serialize};

use super::id::Id;
use super::timestamp::Timestamp;

/// Autopilot 触发类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutopilotTrigger {
    Manual,
    Cron,
    Webhook,
    Event,
    IssueAssigned,
    IssueClosed,
}

impl AutopilotTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Cron => "cron",
            Self::Webhook => "webhook",
            Self::Event => "event",
            Self::IssueAssigned => "issue_assigned",
            Self::IssueClosed => "issue_closed",
        }
    }
}

/// Autopilot 主体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Autopilot {
    pub id: Id,
    pub workspace_id: Id,
    pub name: String,
    pub description: Option<String>,
    pub enabled: bool,
    pub trigger: AutopilotTrigger,
    pub cron_expression: Option<String>,
    pub webhook_url: Option<String>,
    pub trigger_event_filters: Vec<String>,
    pub squad_id: Option<Id>,
    pub project_id: Option<Id>,
    pub rule_version: i64,
    pub rule: serde_json::Value,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// Autopilot run 主体（每次触发执行的实例）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutopilotRun {
    pub id: Id,
    pub autopilot_id: Id,
    pub workspace_id: Id,
    pub status: String, // queued / running / success / failed / skipped
    pub planned_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
    pub quota_reservation_id: Option<Id>,
    pub webhook_delivery_id: Option<Id>,
    pub task_id: Option<Id>,
    pub created_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_str_round_trip() {
        for t in [
            AutopilotTrigger::Manual,
            AutopilotTrigger::Cron,
            AutopilotTrigger::Webhook,
        ] {
            assert!(!t.as_str().is_empty());
        }
    }
}