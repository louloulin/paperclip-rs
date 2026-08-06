//! 恢复动作持久化编排边界。
//! 该层只串联 recovery plan 与 `pc-repos`，不直接负责唤醒外部 actor。

use crate::wake_dedup::WakeInput;
use crate::wake_dispatch::{
    apply_wakeup_plan, plan_wakeup_dispatch, WakeDispatchOutcome, WakePlan,
};
use pc_repos::{
    agent::{AgentRepo, NewAgentWakeupRequest},
    issue::{IssueRecoveryActionRow, IssueRepo, UpsertRecoveryAction},
    Db,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryDispatchIntent {
    WakeOwner { agent_id: uuid::Uuid },
    MonitorOnly,
    ManualRepair,
    BoardEscalation,
}

#[derive(Debug, Clone)]
pub struct PersistedRecoveryAction {
    pub action: IssueRecoveryActionRow,
    pub dispatch: RecoveryDispatchIntent,
}

#[derive(Debug, Clone)]
pub struct RecoveryOrchestrationResult {
    pub persisted: PersistedRecoveryAction,
    pub wake: Option<WakeDispatchOutcome>,
}

pub async fn ensure_source_scoped_recovery_action(
    db: &Db,
    agent_repo: &AgentRepo<'_>,
    input: &UpsertRecoveryAction,
    existing_wake: Option<&crate::wake_dedup::WakeSnapshot>,
    wake_template: NewAgentWakeupRequest,
) -> sqlx::Result<RecoveryOrchestrationResult> {
    let persisted = persist_source_scoped_recovery_action(db, input).await?;
    let wake =
        persist_recovery_wake(agent_repo, &persisted.action, existing_wake, wake_template).await?;
    Ok(RecoveryOrchestrationResult { persisted, wake })
}

pub fn recovery_action_wake_input(action: &IssueRecoveryActionRow) -> Option<WakeInput> {
    let agent_id = action.owner_agent_id?;
    let policy_type = action
        .wake_policy
        .as_ref()
        .and_then(|value| value.get("type"))
        .and_then(|value| value.as_str());
    if matches!(
        policy_type,
        Some("monitor_only") | Some("manual_repair_required")
    ) {
        return None;
    }
    Some(WakeInput {
        agent_id: agent_id.to_string(),
        company_id: action.company_id.to_string(),
        source: "assignment".to_owned(),
        reason: Some("source_scoped_recovery_action".to_owned()),
        payload: Some(serde_json::json!({
            "issueId": action.source_issue_id,
            "sourceIssueId": action.source_issue_id,
            "recoveryActionId": action.id,
            "recoveryCause": action.cause,
        })),
        idempotency_key: Some(format!(
            "source_scoped_recovery_action:{}:{}",
            action.id, action.attempt_count
        )),
    })
}

pub async fn persist_recovery_wake(
    repo: &AgentRepo<'_>,
    action: &IssueRecoveryActionRow,
    existing: Option<&crate::wake_dedup::WakeSnapshot>,
    template: NewAgentWakeupRequest,
) -> sqlx::Result<Option<WakeDispatchOutcome>> {
    let Some(incoming) = recovery_action_wake_input(action) else {
        return Ok(None);
    };
    let plan: WakePlan = plan_wakeup_dispatch(&incoming, existing);
    let mut template = template;
    template.company_id = action.company_id;
    template.agent_id = action
        .owner_agent_id
        .expect("wake input requires owner agent");
    template.source = pc_repos::agent::HeartbeatInvocationSource::OnDemand;
    template.reason = incoming.reason.clone();
    template.idempotency_key = incoming.idempotency_key.clone();
    Ok(Some(
        apply_wakeup_plan(repo, action.company_id, &plan, &incoming, template).await?,
    ))
}

pub async fn persist_source_scoped_recovery_action(
    db: &Db,
    input: &UpsertRecoveryAction,
) -> sqlx::Result<PersistedRecoveryAction> {
    let action = IssueRepo::new(db).upsert_recovery_action(input).await?;
    let dispatch = match action
        .wake_policy
        .as_ref()
        .and_then(|value| value.get("type"))
        .and_then(|value| value.as_str())
    {
        Some("wake_owner") => action
            .owner_agent_id
            .map_or(RecoveryDispatchIntent::BoardEscalation, |agent_id| {
                RecoveryDispatchIntent::WakeOwner { agent_id }
            }),
        Some("monitor_only") => RecoveryDispatchIntent::MonitorOnly,
        Some("manual_repair_required") => RecoveryDispatchIntent::ManualRepair,
        _ if action.owner_type == "board" => RecoveryDispatchIntent::BoardEscalation,
        _ => RecoveryDispatchIntent::MonitorOnly,
    };
    Ok(PersistedRecoveryAction { action, dispatch })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn dispatch_intent_enum_is_explicit() {
        let value = RecoveryDispatchIntent::ManualRepair;
        assert_eq!(value, RecoveryDispatchIntent::ManualRepair);
        assert_eq!(
            json!({"type":"manual_repair_required"})["type"],
            "manual_repair_required"
        );
    }

    #[test]
    fn monitor_and_manual_actions_do_not_create_wakes() {
        assert!(recovery_action_wake_input(&fixture_action("monitor_only")).is_none());
        assert!(recovery_action_wake_input(&fixture_action("manual_repair_required")).is_none());
    }

    fn fixture_action(policy: &str) -> IssueRecoveryActionRow {
        IssueRecoveryActionRow {
            id: uuid::Uuid::nil(),
            company_id: uuid::Uuid::nil(),
            source_issue_id: uuid::Uuid::nil(),
            recovery_issue_id: None,
            kind: "test".into(),
            status: "active".into(),
            owner_type: "system".into(),
            owner_agent_id: Some(uuid::Uuid::nil()),
            owner_user_id: None,
            previous_owner_agent_id: None,
            return_owner_agent_id: None,
            cause: "test".into(),
            fingerprint: "fp".into(),
            evidence: json!({}),
            next_action: "test".into(),
            wake_policy: Some(json!({"type":policy})),
            monitor_policy: None,
            attempt_count: 1,
            max_attempts: None,
            timeout_at: None,
            last_attempt_at: None,
            outcome: None,
            resolution_note: None,
            resolved_at: None,
            created_at: pc_core::Timestamp::now(),
            updated_at: pc_core::Timestamp::now(),
        }
    }
}
