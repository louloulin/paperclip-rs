//! 决策 continuation 到 heartbeat wakeup 的适配。
//!
//! 对齐 Node `services/decision-wakeup.ts`：运行时未启用时拒绝产生唤醒，
//! 启用时只构造标准 `NewAgentWakeupRequest`，实际入队仍由调用方负责。

use serde_json::json;
use uuid::Uuid;

use crate::agent::{
    HeartbeatInvocationSource, NewAgentWakeupRequest, WakeupRequestStatus, WakeupTriggerDetail,
};

#[derive(Debug, Clone)]
pub struct DecisionWakeInput {
    pub company_id: Uuid,
    pub agent_id: Uuid,
    pub issue_id: Uuid,
    pub decision_id: Uuid,
    pub outcome: String,
}

#[derive(Debug, Clone, Copy)]
pub struct DecisionWakeOriginAgent {
    enabled: bool,
}

impl DecisionWakeOriginAgent {
    /// `enabled` 对应 Node 端 heartbeat runtime 是否存在。
    pub const fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    pub const fn is_enabled(self) -> bool {
        self.enabled
    }

    /// 构造与 heartbeat runtime 约定一致的 automation/system 唤醒。
    pub fn build_request(self, input: DecisionWakeInput) -> Option<NewAgentWakeupRequest> {
        if !self.enabled {
            return None;
        }

        let reason = format!("decision_{}", input.outcome);
        Some(NewAgentWakeupRequest {
            company_id: input.company_id,
            agent_id: input.agent_id,
            source: HeartbeatInvocationSource::Automation,
            trigger_detail: Some(WakeupTriggerDetail::System),
            reason: Some(reason),
            payload: Some(json!({
                "issueId": input.issue_id,
                "decisionId": input.decision_id,
                "outcome": input.outcome,
            })),
            status: WakeupRequestStatus::Queued,
            coalesced_count: 0,
            requested_by_actor_type: None,
            requested_by_actor_id: None,
            idempotency_key: None,
            run_id: None,
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> DecisionWakeInput {
        DecisionWakeInput {
            company_id: Uuid::from_u128(1),
            agent_id: Uuid::from_u128(2),
            issue_id: Uuid::from_u128(3),
            decision_id: Uuid::from_u128(4),
            outcome: "decided".into(),
        }
    }

    #[test]
    fn disabled_runtime_does_not_build_wakeup() {
        assert!(DecisionWakeOriginAgent::new(false)
            .build_request(input())
            .is_none());
    }

    #[test]
    fn enabled_runtime_maps_decision_to_automation_system_wakeup() {
        let request = DecisionWakeOriginAgent::new(true)
            .build_request(input())
            .expect("enabled runtime should build request");
        assert_eq!(request.source, HeartbeatInvocationSource::Automation);
        assert_eq!(request.trigger_detail, Some(WakeupTriggerDetail::System));
        assert_eq!(request.reason.as_deref(), Some("decision_decided"));
        assert_eq!(
            request.payload.expect("payload")["issueId"],
            json!(Uuid::from_u128(3))
        );
    }
}
