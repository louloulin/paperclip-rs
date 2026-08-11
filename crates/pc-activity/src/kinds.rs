//! Activity kinds enum.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    IssueCreated,
    IssueUpdated,
    IssueAssigned,
    IssueClosed,
    IssueCommented,
    PipelineCreated,
    PipelineUpdated,
    PipelineArchived,
    PipelineRemoved,
    // R603 v2: stage 子资源 lifecycle
    PipelineStageCreated,
    PipelineStageUpdated,
    PipelineStageRemoved,
    // R603 v3: transition 子资源 lifecycle
    PipelineTransitionCreated,
    PipelineTransitionRemoved,
    // R603 v4: case 子资源 lifecycle
    PipelineCaseCreated,
    PipelineCaseStageTransitioned,
    PipelineCaseRemoved,
    PipelineCaseEventRecorded,
    // R603 v6.1: case 与 issue 的 link 子资源 lifecycle
    PipelineCaseIssueLinked,
    PipelineCaseIssueUnlinked,
    // R603 v6.5: document 子资源 lifecycle
    PipelineDocumentUpserted,
    PipelineDocumentRevisionRestored,
    // R603 v6.6: case 批量审阅 + automation retry
    PipelineCasesBulkReviewed,
    PipelineCaseAutomationRetryRequested,
    PipelineCaseAutomationSpecificRetryRequested,
    PipelineCaseAutomationCurrentStageRerunRequested,
    DecisionProposed,
    DecisionApproved,
    DecisionRejected,
    DecisionDismissed,
    DecisionCancelled,
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
    /// R591: company 生命周期事件
    CompanyCreated,
    CompanyUpdated,
    CompanyArchived,
    CompanyRemoved,
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
            Self::IssueCommented => "issue.commented",
            Self::PipelineCreated => "pipeline.created",
            Self::PipelineUpdated => "pipeline.updated",
            Self::PipelineArchived => "pipeline.archived",
            Self::PipelineRemoved => "pipeline.removed",
            Self::PipelineStageCreated => "pipeline.stage.created",
            Self::PipelineStageUpdated => "pipeline.stage.updated",
            Self::PipelineStageRemoved => "pipeline.stage.removed",
            Self::PipelineTransitionCreated => "pipeline.transition.created",
            Self::PipelineTransitionRemoved => "pipeline.transition.removed",
            Self::PipelineCaseCreated => "pipeline.case.created",
            Self::PipelineCaseStageTransitioned => "pipeline.case.stage_transitioned",
            Self::PipelineCaseRemoved => "pipeline.case.removed",
            Self::PipelineCaseEventRecorded => "pipeline.case.event_recorded",
            Self::PipelineCaseIssueLinked => "pipeline.case.issue_linked",
            Self::PipelineCaseIssueUnlinked => "pipeline.case.issue_unlinked",
            Self::PipelineDocumentUpserted => "pipeline.document.upserted",
            Self::PipelineDocumentRevisionRestored => "pipeline.document.revision_restored",
            Self::PipelineCasesBulkReviewed => "pipeline.cases.bulk_reviewed",
            Self::PipelineCaseAutomationRetryRequested => {
                "pipeline.case.automation.retry_requested"
            }
            Self::PipelineCaseAutomationSpecificRetryRequested => {
                "pipeline.case.automation.specific_retry_requested"
            }
            Self::PipelineCaseAutomationCurrentStageRerunRequested => {
                "pipeline.case.automation.current_stage_rerun_requested"
            }
            Self::DecisionProposed => "decision.proposed",
            Self::DecisionApproved => "decision.approved",
            Self::DecisionRejected => "decision.rejected",
            Self::DecisionDismissed => "decision.dismissed",
            Self::DecisionCancelled => "decision.cancelled",
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
            Self::CompanyCreated => "company.created",
            Self::CompanyUpdated => "company.updated",
            Self::CompanyArchived => "company.archived",
            Self::CompanyRemoved => "company.removed",
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
