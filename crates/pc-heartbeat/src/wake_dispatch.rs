//! Heartbeat wakeup dispatch (DB-IO glue layer).
//!
//! Combines the pure functions from `wake_dedup` (decision + payload merge) with
//! the DB operations in `pc_repos::agent::AgentRepo` (create / find / coalesce).
//!
//! Mirrors Node `enqueueWakeup` `findActiveWakeupRequest` + `coalescedCount++` +
//! `agent_wakeup_requests INSERT` pattern, split into testable pure functions + IO boundary.
//!
//! Design:
//! - `WakeDispatchOutcome` enum expresses all outcomes (Created / Coalesced / Skipped / IdempotentHit)
//! - `plan_wakeup_dispatch` phase 1: pure-function decision (no DB), testable
//! - `apply_wakeup_plan` phase 2: persist decision to DB
//! - Caller controls transaction boundary (begin/commit); helper handles "do the right thing per outcome"

use serde_json::Value;
use uuid::Uuid;

use pc_repos::agent::{AgentRepo, AgentWakeupRequestRow, NewAgentWakeupRequest};

use crate::wake_dedup::{
    decide_wake_action, merge_wake_payloads, WakeAction, WakeInput, WakeSnapshot,
};

// ============================================================================
// Outcome types
// ============================================================================

/// `apply_wakeup_plan` execution result.
#[derive(Debug, Clone)]
pub enum WakeDispatchOutcome {
    /// Inserted new wakeup row.
    Created(AgentWakeupRequestRow),
    /// Coalesced into existing active wakeup (payload merged + coalesced_count +N).
    Coalesced(AgentWakeupRequestRow),
    /// Skipped (company/agent mismatch). Existing row untouched.
    Skipped { reason: String },
    /// idempotency_key already exists -> returned existing row directly.
    IdempotentHit(AgentWakeupRequestRow),
}

impl WakeDispatchOutcome {
    pub fn row(&self) -> Option<&AgentWakeupRequestRow> {
        match self {
            Self::Created(r) | Self::Coalesced(r) | Self::IdempotentHit(r) => Some(r),
            Self::Skipped { .. } => None,
        }
    }

    pub fn is_skipped(&self) -> bool {
        matches!(self, Self::Skipped { .. })
    }

    pub fn skipped_reason(&self) -> Option<&str> {
        match self {
            Self::Skipped { reason } => Some(reason.as_str()),
            _ => None,
        }
    }
}

// ============================================================================
// Decision (pure)
// ============================================================================

/// Combine `WakeInput` + existing wakeup snapshot into a complete dispatch plan.
///
/// Returns `(action, merged_payload)`: when action is Coalesce, merged_payload is
/// the pre-merged payload ready for the DB; otherwise merged_payload = incoming payload.
pub fn plan_wakeup_dispatch(incoming: &WakeInput, existing: Option<&WakeSnapshot>) -> WakePlan {
    let action = decide_wake_action(existing, incoming);
    let merged_payload = match &action {
        WakeAction::Coalesce { .. } => merge_wake_payloads(
            existing.and_then(|e| e.payload.as_ref()),
            incoming.payload.as_ref(),
        ),
        WakeAction::Create | WakeAction::Skip { .. } => {
            incoming.payload.clone().unwrap_or(Value::Null)
        }
    };
    WakePlan {
        action,
        merged_payload,
    }
}

/// `plan_wakeup_dispatch` output (pure data, testable).
#[derive(Debug, Clone, PartialEq)]
pub struct WakePlan {
    pub action: WakeAction,
    pub merged_payload: Value,
}

// ============================================================================
// Dispatch (IO)
// ============================================================================

/// Persist a `WakePlan` to the DB.
///
/// Caller provides `repo` (for IO) and `incoming` (for NewAgentWakeupRequest fields on Create).
/// `idempotency_key` match short-circuits to IdempotentHit.
pub async fn apply_wakeup_plan(
    repo: &AgentRepo<'_>,
    company_id: Uuid,
    plan: &WakePlan,
    incoming: &WakeInput,
    new_request_template: NewAgentWakeupRequest,
) -> sqlx::Result<WakeDispatchOutcome> {
    if let Some(key) = incoming.idempotency_key.as_deref() {
        let agent_uuid = Uuid::parse_str(&incoming.agent_id).ok();
        if let Some(agent_uuid) = agent_uuid {
            if let Some(existing) = repo
                .find_wakeup_by_idempotency_key(company_id, agent_uuid, key)
                .await?
            {
                return Ok(WakeDispatchOutcome::IdempotentHit(existing));
            }
        }
    }

    match &plan.action {
        WakeAction::Create => {
            let mut template = new_request_template;
            template.payload = Some(plan.merged_payload.clone());
            let row = repo.create_wakeup_request(template).await?;
            Ok(WakeDispatchOutcome::Created(row))
        }
        WakeAction::Coalesce { into_id, increment } => {
            let into_uuid = match Uuid::parse_str(into_id) {
                Ok(u) => u,
                Err(_) => {
                    return Ok(WakeDispatchOutcome::Skipped {
                        reason: format!("invalid wakeup id: {into_id}"),
                    });
                }
            };
            let row = repo
                .coalesce_wakeup_with_merge(company_id, into_uuid, &plan.merged_payload, *increment)
                .await?;
            match row {
                Some(r) => Ok(WakeDispatchOutcome::Coalesced(r)),
                None => {
                    // existing no longer active -> fall back to Create
                    let mut template = new_request_template;
                    template.payload = Some(plan.merged_payload.clone());
                    let row = repo.create_wakeup_request(template).await?;
                    Ok(WakeDispatchOutcome::Created(row))
                }
            }
        }
        WakeAction::Skip { reason } => Ok(WakeDispatchOutcome::Skipped {
            reason: reason.clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn incoming() -> WakeInput {
        WakeInput {
            agent_id: "00000000-0000-0000-0000-000000000001".to_string(),
            company_id: "00000000-0000-0000-0000-0000000000c1".to_string(),
            source: "assignment".to_string(),
            reason: Some("issue_assigned".to_string()),
            payload: Some(json!({"issueId": "iss-1", "wakeCommentIds": ["c-1"]})),
            idempotency_key: None,
        }
    }

    fn snapshot_active(id: &str, payload: Option<Value>) -> WakeSnapshot {
        WakeSnapshot {
            id: id.to_string(),
            agent_id: "00000000-0000-0000-0000-000000000001".to_string(),
            company_id: "00000000-0000-0000-0000-0000000000c1".to_string(),
            status: "queued".to_string(),
            coalesced_count: 0,
            payload,
        }
    }

    #[test]
    fn plan_create_when_no_existing() {
        let plan = plan_wakeup_dispatch(&incoming(), None);
        assert!(plan.action.is_create());
        assert_eq!(plan.merged_payload["issueId"], "iss-1");
    }

    #[test]
    fn plan_coalesce_merges_payload() {
        let existing_payload =
            json!({"issueId": "iss-1", "wakeCommentIds": ["c-0"], "taskKey": "tk-1"});
        let snapshot = snapshot_active("w-1", Some(existing_payload));
        let plan = plan_wakeup_dispatch(&incoming(), Some(&snapshot));
        assert!(plan.action.is_coalesce());
        let ids: Vec<&str> = plan.merged_payload["wakeCommentIds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["c-0", "c-1"]);
        assert_eq!(plan.merged_payload["taskKey"], "tk-1");
    }

    #[test]
    fn plan_skip_keeps_incoming_payload_intact() {
        let mut inc = incoming();
        inc.agent_id = "00000000-0000-0000-0000-000000000002".to_string();
        let snapshot = snapshot_active("w-1", None);
        let plan = plan_wakeup_dispatch(&inc, Some(&snapshot));
        assert!(plan.action.is_skip());
        assert_eq!(plan.merged_payload["issueId"], "iss-1");
    }
}
