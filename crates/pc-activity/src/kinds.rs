//! Activity kinds enum.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    IssueCreated,
    IssueUpdated,
    IssueAssigned,
    IssueClosed,
    DecisionProposed,
    DecisionApproved,
    DecisionRejected,
    ApprovalRequested,
    ApprovalGranted,
    ApprovalDenied,
    AgentStarted,
    AgentStopped,
    AgentHeartbeat,
    AgentError,
    PluginInstalled,
    PluginEnabled,
    PluginDisabled,
    PluginError,
    CostRecorded,
    SecretAccessed,
    DocumentAnnotated,
    RoutineRan,
    PipelineRan,
    Other,
}

impl ActivityKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IssueCreated => "issue.created",
            Self::IssueUpdated => "issue.updated",
            Self::IssueAssigned => "issue.assigned",
            Self::IssueClosed => "issue.closed",
            Self::DecisionProposed => "decision.proposed",
            Self::DecisionApproved => "decision.approved",
            Self::DecisionRejected => "decision.rejected",
            Self::ApprovalRequested => "approval.requested",
            Self::ApprovalGranted => "approval.granted",
            Self::ApprovalDenied => "approval.denied",
            Self::AgentStarted => "agent.started",
            Self::AgentStopped => "agent.stopped",
            Self::AgentHeartbeat => "agent.heartbeat",
            Self::AgentError => "agent.error",
            Self::PluginInstalled => "plugin.installed",
            Self::PluginEnabled => "plugin.enabled",
            Self::PluginDisabled => "plugin.disabled",
            Self::PluginError => "plugin.error",
            Self::CostRecorded => "cost.recorded",
            Self::SecretAccessed => "secret.accessed",
            Self::DocumentAnnotated => "document.annotated",
            Self::RoutineRan => "routine.ran",
            Self::PipelineRan => "pipeline.ran",
            Self::Other => "other",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_string_is_stable() {
        assert_eq!(ActivityKind::IssueCreated.as_str(), "issue.created");
        assert_eq!(ActivityKind::AgentHeartbeat.as_str(), "agent.heartbeat");
        assert_eq!(ActivityKind::Other.as_str(), "other");
    }
}
