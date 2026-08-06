//! source-scoped stranded recovery action 的纯计划层。
//! 对齐 Node `ensureSourceScopedStrandedRecoveryAction`，不负责数据库写入或 wake dispatch。

use chrono::Utc;
use pc_core::Timestamp;
use pc_repos::issue::UpsertRecoveryAction;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrandedRecoveryCause {
    SuccessfulRunMissingState,
    WorkspaceValidationFailed,
    ConfigurationIncomplete,
    ProviderQuota,
    ProcessLost,
    CodexOutputInactivityMonitor,
    ExecutionReviewParticipantRecovery,
    RuntimeFailure,
}
impl StrandedRecoveryCause {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SuccessfulRunMissingState => "successful_run_missing_state",
            Self::WorkspaceValidationFailed => "workspace_validation_failed",
            Self::ConfigurationIncomplete => "configuration_incomplete",
            Self::ProviderQuota => "provider_quota",
            Self::ProcessLost => "process_lost",
            Self::CodexOutputInactivityMonitor => "codex_output_inactivity_monitor",
            Self::ExecutionReviewParticipantRecovery => "execution_review_participant_recovery",
            Self::RuntimeFailure => "runtime_failure",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceScopedRecoveryActionPlan {
    pub kind: String,
    pub owner_type: String,
    pub cause: String,
    pub fingerprint: String,
    pub next_action: String,
    pub wake_policy: Value,
    pub monitor_policy: Option<Value>,
}

pub fn plan_to_upsert_recovery_action(
    plan: &SourceScopedRecoveryActionPlan,
    company_id: Uuid,
    source_issue_id: Uuid,
    recovery_issue_id: Option<Uuid>,
    owner_agent_id: Option<Uuid>,
    previous_owner_agent_id: Option<Uuid>,
    return_owner_agent_id: Option<Uuid>,
    evidence: Value,
) -> UpsertRecoveryAction {
    UpsertRecoveryAction {
        company_id,
        source_issue_id,
        recovery_issue_id,
        kind: plan.kind.clone(),
        owner_type: Some(plan.owner_type.clone()),
        owner_agent_id,
        owner_user_id: None,
        previous_owner_agent_id,
        return_owner_agent_id,
        cause: plan.cause.clone(),
        fingerprint: plan.fingerprint.clone(),
        evidence: Some(evidence),
        next_action: plan.next_action.clone(),
        wake_policy: Some(plan.wake_policy.clone()),
        monitor_policy: plan.monitor_policy.clone(),
        max_attempts: None,
        timeout_at: None,
        last_attempt_at: Some(Timestamp::from_dt(Utc::now())),
    }
}

pub fn build_source_scoped_recovery_action_plan(
    company_id: Uuid,
    source_issue_id: Uuid,
    cause: StrandedRecoveryCause,
    owner_agent_id: Option<Uuid>,
    return_owner_agent_id: Option<Uuid>,
    workspace_fingerprint: Option<&str>,
) -> SourceScopedRecoveryActionPlan {
    let cause_name = cause.as_str();
    let kind = match cause {
        StrandedRecoveryCause::SuccessfulRunMissingState => "missing_disposition",
        StrandedRecoveryCause::WorkspaceValidationFailed => "workspace_validation",
        StrandedRecoveryCause::ConfigurationIncomplete => "configuration_validation",
        _ => "stranded_assigned_issue",
    }
    .to_owned();
    let fingerprint = if cause == StrandedRecoveryCause::WorkspaceValidationFailed {
        if let Some(value) = workspace_fingerprint.filter(|v| !v.trim().is_empty()) {
            format!("source_scoped_recovery:{company_id}:{source_issue_id}:{cause_name}:{value}")
        } else {
            format!("source_scoped_recovery:{company_id}:{source_issue_id}:{cause_name}")
        }
    } else {
        format!("source_scoped_recovery:{company_id}:{source_issue_id}:{cause_name}")
    };
    let owner_type = if cause == StrandedRecoveryCause::ProviderQuota && owner_agent_id.is_none() {
        "system"
    } else if owner_agent_id.is_some() {
        "agent"
    } else {
        "board"
    };
    let next_action = match cause {
        StrandedRecoveryCause::SuccessfulRunMissingState => "Choose and record a valid issue disposition without copying transcript content.",
        StrandedRecoveryCause::ProcessLost => "Retry the original assignee from durable progress without redoing completed steps.",
        StrandedRecoveryCause::ProviderQuota => "Wait for provider quota recovery, then retry the original assignee; do not wake a takeover owner.",
        StrandedRecoveryCause::CodexOutputInactivityMonitor => "Retry the same agent from durable progress after the output-inactivity termination.",
        StrandedRecoveryCause::WorkspaceValidationFailed => "Repair the source issue workspace link, project workspace cwd, or git checkout before resuming adapter execution.",
        StrandedRecoveryCause::ConfigurationIncomplete => "Bind the missing secret(s) named in the run failure to the agent/project/routine env before resuming adapter execution.",
        StrandedRecoveryCause::ExecutionReviewParticipantRecovery => "Repair the failed review participant path, restore the source issue to in_review with a live reviewer, or record an intentional manual resolution.",
        StrandedRecoveryCause::RuntimeFailure => "Restore a live execution path, fix the runtime/adapter failure, or record an intentional manual resolution.",
    }.to_owned();
    let wake_policy = if cause == StrandedRecoveryCause::ProviderQuota && owner_agent_id.is_none() {
        json!({"type":"monitor_only","reason":cause_name})
    } else if cause == StrandedRecoveryCause::ConfigurationIncomplete {
        json!({"type":"manual_repair_required","reason":cause_name,"ownerAgentId":owner_agent_id})
    } else if let Some(agent_id) = owner_agent_id {
        json!({"type":"wake_owner","reason":"source_scoped_recovery_action","ownerAgentId":agent_id})
    } else {
        json!({"type":"board_escalation","reason":"no_invokable_recovery_owner"})
    };
    let monitor_policy =
        if cause == StrandedRecoveryCause::ProviderQuota && owner_agent_id.is_none() {
            Some(json!({"type":"wait_recovery","retryAgentId":return_owner_agent_id}))
        } else {
            None
        };
    SourceScopedRecoveryActionPlan {
        kind,
        owner_type: owner_type.to_owned(),
        cause: cause_name.to_owned(),
        fingerprint,
        next_action,
        wake_policy,
        monitor_policy,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quota_without_owner_is_monitor_only() {
        let p = build_source_scoped_recovery_action_plan(
            Uuid::nil(),
            Uuid::nil(),
            StrandedRecoveryCause::ProviderQuota,
            None,
            Some(Uuid::nil()),
            None,
        );
        assert_eq!(p.owner_type, "system");
        assert_eq!(p.wake_policy["type"], "monitor_only");
        assert_eq!(p.monitor_policy.as_ref().unwrap()["type"], "wait_recovery");
    }
    #[test]
    fn configuration_requires_manual_repair() {
        let p = build_source_scoped_recovery_action_plan(
            Uuid::nil(),
            Uuid::nil(),
            StrandedRecoveryCause::ConfigurationIncomplete,
            None,
            None,
            None,
        );
        assert_eq!(p.kind, "configuration_validation");
        assert_eq!(p.wake_policy["type"], "manual_repair_required");
    }
    #[test]
    fn workspace_fingerprint_is_part_of_identity() {
        let p = build_source_scoped_recovery_action_plan(
            Uuid::nil(),
            Uuid::nil(),
            StrandedRecoveryCause::WorkspaceValidationFailed,
            None,
            None,
            Some("branch"),
        );
        assert!(p.fingerprint.ends_with(":branch"));
    }
    #[test]
    fn plan_maps_to_repository_dto() {
        let p = build_source_scoped_recovery_action_plan(
            Uuid::nil(),
            Uuid::nil(),
            StrandedRecoveryCause::RuntimeFailure,
            Some(Uuid::nil()),
            None,
            None,
        );
        let dto = plan_to_upsert_recovery_action(
            &p,
            Uuid::nil(),
            Uuid::nil(),
            None,
            Some(Uuid::nil()),
            None,
            None,
            json!({"reason":"test"}),
        );
        assert_eq!(dto.owner_type.as_deref(), Some("agent"));
        assert_eq!(dto.evidence.unwrap()["reason"], "test");
    }
}
