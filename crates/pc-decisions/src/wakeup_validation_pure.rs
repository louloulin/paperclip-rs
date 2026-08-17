#![forbid(unsafe_code)]

//! Decision wakeup validation + canonical helpers — 1:1 port of small
//! helpers in paperclip/server/src/services/decision-wakeup.ts.
//!
//! R738: 零依赖校验 wake input + outcome label 双向转换。

use crate::wakeup::DecisionOutcome;
use uuid::Uuid;

/// Wake origin agent 输入（精简版，避免 wakeup/types.rs 的复杂依赖）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeOriginInput {
    pub agent_id: String,
    pub issue_id: String,
    pub decision_id: String,
    pub outcome: String,
}

impl WakeOriginInput {
    /// 校验 wake origin input 字段非空。
    pub fn validate(&self) -> Result<(), String> {
        if self.agent_id.trim().is_empty() {
            return Err("agentId must not be empty".into());
        }
        if self.issue_id.trim().is_empty() {
            return Err("issueId must not be empty".into());
        }
        if self.decision_id.trim().is_empty() {
            return Err("decisionId must not be empty".into());
        }
        if self.outcome.trim().is_empty() {
            return Err("outcome must not be empty".into());
        }
        Ok(())
    }
}

/// Wakeup options 来源标识白名单（Node heartbeat wake source union）。
pub const ALLOWED_WAKEUP_SOURCES: &[&str] = &[
    "timer",
    "assignment",
    "on_demand",
    "automation",
];

/// Wakeup trigger detail 白名单。
pub const ALLOWED_TRIGGER_DETAILS: &[&str] = &[
    "manual",
    "ping",
    "callback",
    "system",
];

/// 校验 wakeup source 白名单。
pub fn validate_wakeup_source(source: &str) -> Result<(), String> {
    if !ALLOWED_WAKEUP_SOURCES.contains(&source) {
        return Err(format!(
            "wakeup source must be one of {ALLOWED_WAKEUP_SOURCES:?}, got {source:?}"
        ));
    }
    Ok(())
}

/// 校验 wakeup trigger detail 白名单。
pub fn validate_trigger_detail(detail: &str) -> Result<(), String> {
    if !ALLOWED_TRIGGER_DETAILS.contains(&detail) {
        return Err(format!(
            "trigger detail must be one of {ALLOWED_TRIGGER_DETAILS:?}, got {detail:?}"
        ));
    }
    Ok(())
}

/// DecisionOutcome → string label + reverse parse。
pub fn outcome_label(o: DecisionOutcome) -> &'static str {
    o.as_str()
}

pub fn outcome_from_label(s: &str) -> Option<DecisionOutcome> {
    match s.trim().to_lowercase().as_str() {
        "decided" => Some(DecisionOutcome::Decided),
        "expired" => Some(DecisionOutcome::Expired),
        "cancelled" | "canceled" => Some(DecisionOutcome::Cancelled),
        _ => None,
    }
}

/// 判断两个 wake inputs 关联同一资源（同 agent + issue + decision）。
pub fn same_wake_target(left: &WakeOriginInput, right: &WakeOriginInput) -> bool {
    left.agent_id == right.agent_id
        && left.issue_id == right.issue_id
        && left.decision_id == right.decision_id
}

/// 派生 idempotency key（agent + issue + decision + outcome）。
pub fn derive_wake_idempotency_key(input: &WakeOriginInput) -> String {
    format!("{}-{}-{}-{}", input.agent_id, input.issue_id, input.decision_id, input.outcome)
}

/// 校验 uuid string 合法（用于 issue_id / decision_id）。
pub fn is_valid_uuid(s: &str) -> bool {
    Uuid::parse_str(s.trim()).is_ok()
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    fn make_valid_input() -> WakeOriginInput {
        WakeOriginInput {
            agent_id: "agent-1".into(),
            issue_id: Uuid::new_v4().to_string(),
            decision_id: Uuid::new_v4().to_string(),
            outcome: "decided".into(),
        }
    }

    #[test]
    fn validate_wake_input_accepts() {
        assert!(make_valid_input().validate().is_ok());
    }

    #[test]
    fn validate_wake_input_rejects_empty_agent() {
        let mut i = make_valid_input();
        i.agent_id = "".into();
        assert!(i.validate().is_err());
    }

    #[test]
    fn validate_wake_input_rejects_empty_issue() {
        let mut i = make_valid_input();
        i.issue_id = "".into();
        assert!(i.validate().is_err());
    }

    #[test]
    fn validate_wake_input_rejects_empty_decision() {
        let mut i = make_valid_input();
        i.decision_id = "".into();
        assert!(i.validate().is_err());
    }

    #[test]
    fn validate_wake_input_rejects_empty_outcome() {
        let mut i = make_valid_input();
        i.outcome = "".into();
        assert!(i.validate().is_err());
    }

    #[test]
    fn validate_wakeup_source_known() {
        for s in ALLOWED_WAKEUP_SOURCES {
            assert!(validate_wakeup_source(s).is_ok());
        }
    }

    #[test]
    fn validate_wakeup_source_unknown() {
        assert!(validate_wakeup_source("unknown").is_err());
    }

    #[test]
    fn validate_trigger_detail_known() {
        for s in ALLOWED_TRIGGER_DETAILS {
            assert!(validate_trigger_detail(s).is_ok());
        }
    }

    #[test]
    fn validate_trigger_detail_unknown() {
        assert!(validate_trigger_detail("random").is_err());
    }

    #[test]
    fn outcome_label_round_trip() {
        for o in [
            DecisionOutcome::Decided,
            DecisionOutcome::Expired,
            DecisionOutcome::Cancelled,
        ] {
            assert_eq!(outcome_from_label(outcome_label(o)), Some(o));
        }
    }

    #[test]
    fn outcome_from_label_case_insensitive() {
        assert_eq!(
            outcome_from_label("DECIDED"),
            Some(DecisionOutcome::Decided)
        );
        assert_eq!(
            outcome_from_label("  cancelled  "),
            Some(DecisionOutcome::Cancelled)
        );
    }

    #[test]
    fn outcome_from_label_canceled_american() {
        assert_eq!(
            outcome_from_label("canceled"),
            Some(DecisionOutcome::Cancelled)
        );
    }

    #[test]
    fn outcome_from_label_unknown() {
        assert_eq!(outcome_from_label("unknown"), None);
    }

    #[test]
    fn same_wake_target_same_inputs() {
        let i = make_valid_input();
        assert!(same_wake_target(&i, &i));
    }

    #[test]
    fn same_wake_target_different_outcome() {
        let a = make_valid_input();
        let mut b = a.clone();
        b.outcome = "expired".into();
        // outcome 不同但 wake target 视为相同（同 agent+issue+decision）
        assert!(same_wake_target(&a, &b));
    }

    #[test]
    fn same_wake_target_different_agent() {
        let a = make_valid_input();
        let mut b = a.clone();
        b.agent_id = "agent-2".into();
        assert!(!same_wake_target(&a, &b));
    }

    #[test]
    fn derive_wake_idempotency_key_format() {
        let i = WakeOriginInput {
            agent_id: "a".into(),
            issue_id: "i".into(),
            decision_id: "d".into(),
            outcome: "decided".into(),
        };
        assert_eq!(derive_wake_idempotency_key(&i), "a-i-d-decided");
    }

    #[test]
    fn is_valid_uuid_accepts() {
        assert!(is_valid_uuid("550e8400-e29b-41d4-a716-446655440000"));
        assert!(is_valid_uuid(&Uuid::new_v4().to_string()));
    }

    #[test]
    fn is_valid_uuid_rejects() {
        assert!(!is_valid_uuid("not-a-uuid"));
        assert!(!is_valid_uuid(""));
        assert!(!is_valid_uuid("  "));
    }
}
