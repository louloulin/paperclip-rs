//! Heartbeat retry policy (unified retry reason classifier + schedule decision).
//!
//! Mirrors Node `services/heartbeat.ts` retry reason constants and adds three
//! new reasons (dependency_unavailable / workspace_locked / quota_exceeded)
//! to round out the production retry taxonomy.

use serde::{Deserialize, Serialize};

use pc_core::Timestamp;

pub const TRANSIENT_FAILURE_RETRY_REASON: &str = "transient_failure";
pub const TRANSIENT_FAILURE_RETRY_WAKE_REASON: &str = "transient_failure_retry";

pub const MAX_TURN_CONTINUATION_RETRY_REASON: &str = "max_turns_continuation";
pub const MAX_TURN_CONTINUATION_WAKE_REASON: &str = "max_turns_continuation_retry";

pub const INTERACTION_CONTINUATION_INFRA_RETRY_REASON: &str =
    "interaction_continuation_infra_retry";

pub const EXECUTION_REVIEW_PARTICIPANT_RECOVERY_RETRY_REASON: &str =
    "execution_review_participant_recovery";

pub const WORKSPACE_VALIDATION_FAILED_RETRY_REASON: &str = "workspace_validation_failed";

pub const CONFIGURATION_INCOMPLETE_RETRY_REASON: &str = "configuration_incomplete";

pub const DEPENDENCY_UNAVAILABLE_RETRY_REASON: &str = "dependency_unavailable";

pub const WORKSPACE_LOCKED_RETRY_REASON: &str = "workspace_locked";

pub const QUOTA_EXCEEDED_RETRY_REASON: &str = "quota_exceeded";

pub const TRANSIENT_FAILURE_DELAYS_MS: [i64; 4] = [
    2 * 60 * 1_000,
    10 * 60 * 1_000,
    30 * 60 * 1_000,
    2 * 60 * 60 * 1_000,
];
pub const TRANSIENT_FAILURE_MAX_ATTEMPTS: i32 = TRANSIENT_FAILURE_DELAYS_MS.len() as i32;
pub const TRANSIENT_FAILURE_JITTER_RATIO: f64 = 0.25;

pub const INTERACTION_CONTINUATION_INFRA_DELAYS_MS: [i64; 3] =
    [60 * 1_000, 5 * 60 * 1_000, 15 * 60 * 1_000];
pub const INTERACTION_CONTINUATION_INFRA_MAX_ATTEMPTS: i32 =
    INTERACTION_CONTINUATION_INFRA_DELAYS_MS.len() as i32;
pub const INTERACTION_CONTINUATION_INFRA_JITTER_RATIO: f64 = 0.20;

pub const MAX_TURN_CONTINUATION_DEFAULT_DELAY_MS: i64 = 1_000;
pub const MAX_TURN_CONTINUATION_MAX_DELAY_MS: i64 = 5 * 60 * 1_000;
pub const MAX_TURN_CONTINUATION_DEFAULT_MAX_ATTEMPTS: i32 = 2;
pub const MAX_TURN_CONTINUATION_MAX_ATTEMPTS_CAP: i32 = 10;

pub const WORKSPACE_RECOVERY_DELAYS_MS: [i64; 3] = [30 * 1_000, 2 * 60 * 1_000, 10 * 60 * 1_000];
pub const WORKSPACE_RECOVERY_MAX_ATTEMPTS: i32 = WORKSPACE_RECOVERY_DELAYS_MS.len() as i32;
pub const WORKSPACE_RECOVERY_JITTER_RATIO: f64 = 0.15;

pub const DEPENDENCY_UNAVAILABLE_DELAYS_MS: [i64; 4] =
    [5 * 1_000, 30 * 1_000, 2 * 60 * 1_000, 10 * 60 * 1_000];
pub const DEPENDENCY_UNAVAILABLE_MAX_ATTEMPTS: i32 = DEPENDENCY_UNAVAILABLE_DELAYS_MS.len() as i32;

pub const WORKSPACE_LOCKED_DELAYS_MS: [i64; 3] = [10 * 1_000, 60 * 1_000, 5 * 60 * 1_000];
pub const WORKSPACE_LOCKED_MAX_ATTEMPTS: i32 = WORKSPACE_LOCKED_DELAYS_MS.len() as i32;

pub const QUOTA_EXCEEDED_DELAYS_MS: [i64; 3] = [60 * 1_000, 10 * 60 * 1_000, 30 * 60 * 1_000];
pub const QUOTA_EXCEEDED_MAX_ATTEMPTS: i32 = QUOTA_EXCEEDED_DELAYS_MS.len() as i32;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryReason {
    TransientFailure,
    MaxTurnsContinuation,
    InteractionContinuationInfra,
    ExecutionReviewParticipantRecovery,
    WorkspaceValidationFailed,
    ConfigurationIncomplete,
    DependencyUnavailable,
    WorkspaceLocked,
    QuotaExceeded,
    Other(String),
}

impl RetryReason {
    pub fn as_str(&self) -> &str {
        match self {
            Self::TransientFailure => TRANSIENT_FAILURE_RETRY_REASON,
            Self::MaxTurnsContinuation => MAX_TURN_CONTINUATION_RETRY_REASON,
            Self::InteractionContinuationInfra => INTERACTION_CONTINUATION_INFRA_RETRY_REASON,
            Self::ExecutionReviewParticipantRecovery => {
                EXECUTION_REVIEW_PARTICIPANT_RECOVERY_RETRY_REASON
            }
            Self::WorkspaceValidationFailed => WORKSPACE_VALIDATION_FAILED_RETRY_REASON,
            Self::ConfigurationIncomplete => CONFIGURATION_INCOMPLETE_RETRY_REASON,
            Self::DependencyUnavailable => DEPENDENCY_UNAVAILABLE_RETRY_REASON,
            Self::WorkspaceLocked => WORKSPACE_LOCKED_RETRY_REASON,
            Self::QuotaExceeded => QUOTA_EXCEEDED_RETRY_REASON,
            Self::Other(s) => s.as_str(),
        }
    }

    pub fn is_continuation_retry(&self) -> bool {
        matches!(
            self,
            Self::MaxTurnsContinuation | Self::InteractionContinuationInfra
        )
    }

    pub fn requires_wake_agent(&self) -> bool {
        matches!(
            self,
            Self::ExecutionReviewParticipantRecovery
                | Self::WorkspaceValidationFailed
                | Self::ConfigurationIncomplete
        )
    }

    pub fn max_attempts(&self) -> i32 {
        match self {
            Self::TransientFailure => TRANSIENT_FAILURE_MAX_ATTEMPTS,
            Self::MaxTurnsContinuation => MAX_TURN_CONTINUATION_DEFAULT_MAX_ATTEMPTS,
            Self::InteractionContinuationInfra => INTERACTION_CONTINUATION_INFRA_MAX_ATTEMPTS,
            Self::ExecutionReviewParticipantRecovery => 1,
            Self::WorkspaceValidationFailed => WORKSPACE_RECOVERY_MAX_ATTEMPTS,
            Self::ConfigurationIncomplete => WORKSPACE_RECOVERY_MAX_ATTEMPTS,
            Self::DependencyUnavailable => DEPENDENCY_UNAVAILABLE_MAX_ATTEMPTS,
            Self::WorkspaceLocked => WORKSPACE_LOCKED_MAX_ATTEMPTS,
            Self::QuotaExceeded => QUOTA_EXCEEDED_MAX_ATTEMPTS,
            Self::Other(_) => 1,
        }
    }
}

pub fn classify_retry_reason(s: &str) -> RetryReason {
    match s {
        TRANSIENT_FAILURE_RETRY_REASON => RetryReason::TransientFailure,
        MAX_TURN_CONTINUATION_RETRY_REASON => RetryReason::MaxTurnsContinuation,
        INTERACTION_CONTINUATION_INFRA_RETRY_REASON => RetryReason::InteractionContinuationInfra,
        EXECUTION_REVIEW_PARTICIPANT_RECOVERY_RETRY_REASON => {
            RetryReason::ExecutionReviewParticipantRecovery
        }
        WORKSPACE_VALIDATION_FAILED_RETRY_REASON => RetryReason::WorkspaceValidationFailed,
        CONFIGURATION_INCOMPLETE_RETRY_REASON => RetryReason::ConfigurationIncomplete,
        DEPENDENCY_UNAVAILABLE_RETRY_REASON => RetryReason::DependencyUnavailable,
        WORKSPACE_LOCKED_RETRY_REASON => RetryReason::WorkspaceLocked,
        QUOTA_EXCEEDED_RETRY_REASON => RetryReason::QuotaExceeded,
        other => RetryReason::Other(other.to_string()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryPolicySchedule {
    pub reason: RetryReason,
    pub attempt: i32,
    pub base_delay_ms: i64,
    pub delay_ms: i64,
    pub due_at: Timestamp,
    pub max_attempts: i32,
}

pub fn decide_retry_schedule(
    reason: RetryReason,
    attempt: i32,
    now: Timestamp,
    sample: f64,
) -> Option<RetryPolicySchedule> {
    if attempt <= 0 {
        return None;
    }

    let max = reason.max_attempts();
    if attempt > max {
        return None;
    }

    let schedule = match &reason {
        RetryReason::TransientFailure => bounded_schedule(
            &TRANSIENT_FAILURE_DELAYS_MS,
            TRANSIENT_FAILURE_JITTER_RATIO,
            attempt,
            sample,
            now,
            max,
            reason.clone(),
        ),
        RetryReason::InteractionContinuationInfra => bounded_schedule(
            &INTERACTION_CONTINUATION_INFRA_DELAYS_MS,
            INTERACTION_CONTINUATION_INFRA_JITTER_RATIO,
            attempt,
            sample,
            now,
            max,
            reason.clone(),
        ),
        RetryReason::WorkspaceValidationFailed | RetryReason::ConfigurationIncomplete => {
            bounded_schedule(
                &WORKSPACE_RECOVERY_DELAYS_MS,
                WORKSPACE_RECOVERY_JITTER_RATIO,
                attempt,
                sample,
                now,
                max,
                reason.clone(),
            )
        }
        RetryReason::DependencyUnavailable => bounded_schedule(
            &DEPENDENCY_UNAVAILABLE_DELAYS_MS,
            0.0,
            attempt,
            sample,
            now,
            max,
            reason.clone(),
        ),
        RetryReason::WorkspaceLocked => bounded_schedule(
            &WORKSPACE_LOCKED_DELAYS_MS,
            0.10,
            attempt,
            sample,
            now,
            max,
            reason.clone(),
        ),
        RetryReason::QuotaExceeded => bounded_schedule(
            &QUOTA_EXCEEDED_DELAYS_MS,
            0.20,
            attempt,
            sample,
            now,
            max,
            reason.clone(),
        ),
        RetryReason::MaxTurnsContinuation => {
            let base_delay_ms = MAX_TURN_CONTINUATION_DEFAULT_DELAY_MS;
            let attempt_capped = attempt.min(MAX_TURN_CONTINUATION_MAX_ATTEMPTS_CAP);
            let delay_ms = base_delay_ms.saturating_mul(attempt_capped as i64);
            let delay_ms = delay_ms.min(MAX_TURN_CONTINUATION_MAX_DELAY_MS).max(1_000);
            RetryPolicySchedule {
                reason: reason.clone(),
                attempt,
                base_delay_ms,
                delay_ms,
                due_at: Timestamp::from_dt(
                    now.as_datetime() + chrono::Duration::milliseconds(delay_ms),
                ),
                max_attempts: max,
            }
        }
        RetryReason::ExecutionReviewParticipantRecovery => return None,
        RetryReason::Other(_) => return None,
    };
    Some(schedule)
}

fn bounded_schedule(
    delays: &[i64],
    jitter_ratio: f64,
    attempt: i32,
    sample: f64,
    now: Timestamp,
    max_attempts: i32,
    reason: RetryReason,
) -> RetryPolicySchedule {
    let base_delay_ms = delays
        .get((attempt - 1) as usize)
        .copied()
        .or_else(|| delays.last().copied())
        .unwrap_or(0);
    let sample = sample.clamp(0.0, 1.0);
    let jitter_multiplier = if jitter_ratio > 0.0 {
        1.0 + (((sample * 2.0) - 1.0) * jitter_ratio)
    } else {
        1.0
    };
    let delay_ms = ((base_delay_ms as f64 * jitter_multiplier).round() as i64).max(1_000);
    RetryPolicySchedule {
        reason,
        attempt,
        base_delay_ms,
        delay_ms,
        due_at: Timestamp::from_dt(now.as_datetime() + chrono::Duration::milliseconds(delay_ms)),
        max_attempts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> Timestamp {
        Timestamp::from_dt(chrono::Utc.timestamp_millis_opt(1_700_000_000_000).unwrap())
    }

    #[test]
    fn retry_reason_as_str_round_trip() {
        for r in [
            RetryReason::TransientFailure,
            RetryReason::MaxTurnsContinuation,
            RetryReason::InteractionContinuationInfra,
            RetryReason::ExecutionReviewParticipantRecovery,
            RetryReason::WorkspaceValidationFailed,
            RetryReason::ConfigurationIncomplete,
            RetryReason::DependencyUnavailable,
            RetryReason::WorkspaceLocked,
            RetryReason::QuotaExceeded,
        ] {
            assert_eq!(classify_retry_reason(r.as_str()), r);
        }
    }

    #[test]
    fn classify_unknown_falls_back_to_other() {
        match classify_retry_reason("custom_thing") {
            RetryReason::Other(s) => assert_eq!(s, "custom_thing"),
            _ => panic!("expected Other"),
        }
    }

    #[test]
    fn continuation_retry_recognises_only_continuation_reasons() {
        assert!(RetryReason::MaxTurnsContinuation.is_continuation_retry());
        assert!(RetryReason::InteractionContinuationInfra.is_continuation_retry());
        assert!(!RetryReason::TransientFailure.is_continuation_retry());
        assert!(!RetryReason::WorkspaceValidationFailed.is_continuation_retry());
        assert!(!RetryReason::DependencyUnavailable.is_continuation_retry());
        assert!(!RetryReason::WorkspaceLocked.is_continuation_retry());
        assert!(!RetryReason::QuotaExceeded.is_continuation_retry());
    }

    #[test]
    fn requires_wake_only_for_recovery_class() {
        assert!(RetryReason::ExecutionReviewParticipantRecovery.requires_wake_agent());
        assert!(RetryReason::WorkspaceValidationFailed.requires_wake_agent());
        assert!(RetryReason::ConfigurationIncomplete.requires_wake_agent());
        assert!(!RetryReason::TransientFailure.requires_wake_agent());
        assert!(!RetryReason::MaxTurnsContinuation.requires_wake_agent());
        assert!(!RetryReason::InteractionContinuationInfra.requires_wake_agent());
        assert!(!RetryReason::DependencyUnavailable.requires_wake_agent());
        assert!(!RetryReason::WorkspaceLocked.requires_wake_agent());
        assert!(!RetryReason::QuotaExceeded.requires_wake_agent());
    }

    #[test]
    fn max_attempts_per_reason() {
        assert_eq!(RetryReason::TransientFailure.max_attempts(), 4);
        assert_eq!(RetryReason::MaxTurnsContinuation.max_attempts(), 2);
        assert_eq!(RetryReason::InteractionContinuationInfra.max_attempts(), 3);
        assert_eq!(
            RetryReason::ExecutionReviewParticipantRecovery.max_attempts(),
            1
        );
        assert_eq!(RetryReason::WorkspaceValidationFailed.max_attempts(), 3);
        assert_eq!(RetryReason::ConfigurationIncomplete.max_attempts(), 3);
        assert_eq!(RetryReason::DependencyUnavailable.max_attempts(), 4);
        assert_eq!(RetryReason::WorkspaceLocked.max_attempts(), 3);
        assert_eq!(RetryReason::QuotaExceeded.max_attempts(), 3);
    }

    #[test]
    fn transient_failure_schedule_matches_node_delay_table() {
        let n = now();
        let s = decide_retry_schedule(RetryReason::TransientFailure, 1, n, 0.5).unwrap();
        assert_eq!(s.base_delay_ms, 120_000);
        assert_eq!(s.delay_ms, 120_000);
        assert_eq!(s.attempt, 1);
        assert_eq!(s.max_attempts, 4);
    }

    #[test]
    fn transient_failure_schedule_jitter_applied() {
        let n = now();
        let low = decide_retry_schedule(RetryReason::TransientFailure, 1, n, 0.0).unwrap();
        let high = decide_retry_schedule(RetryReason::TransientFailure, 1, n, 1.0).unwrap();
        assert_eq!(low.delay_ms, 90_000);
        assert_eq!(high.delay_ms, 150_000);
    }

    #[test]
    fn transient_failure_rejects_out_of_range_attempts() {
        let n = now();
        assert!(decide_retry_schedule(RetryReason::TransientFailure, 0, n, 0.5).is_none());
        assert!(decide_retry_schedule(RetryReason::TransientFailure, 5, n, 0.5).is_none());
    }

    #[test]
    fn dependency_unavailable_uses_short_delays() {
        let n = now();
        let s = decide_retry_schedule(RetryReason::DependencyUnavailable, 1, n, 0.5).unwrap();
        assert_eq!(s.base_delay_ms, 5_000);
        assert_eq!(s.delay_ms, 5_000);
        assert_eq!(s.max_attempts, 4);
    }

    #[test]
    fn workspace_locked_uses_short_delays_with_small_jitter() {
        let n = now();
        let s = decide_retry_schedule(RetryReason::WorkspaceLocked, 1, n, 0.5).unwrap();
        assert_eq!(s.base_delay_ms, 10_000);
        assert_eq!(s.delay_ms, 10_000);
        let s_high = decide_retry_schedule(RetryReason::WorkspaceLocked, 1, n, 1.0).unwrap();
        assert_eq!(s_high.delay_ms, 11_000);
    }

    #[test]
    fn quota_exceeded_uses_long_delays() {
        let n = now();
        let s1 = decide_retry_schedule(RetryReason::QuotaExceeded, 1, n, 0.5).unwrap();
        assert_eq!(s1.base_delay_ms, 60_000);
        let s3 = decide_retry_schedule(RetryReason::QuotaExceeded, 3, n, 0.5).unwrap();
        assert_eq!(s3.base_delay_ms, 30 * 60_000);
        assert_eq!(s3.max_attempts, 3);
    }

    #[test]
    fn interaction_continuation_infra_has_3_attempts() {
        let n = now();
        let s =
            decide_retry_schedule(RetryReason::InteractionContinuationInfra, 1, n, 0.5).unwrap();
        assert_eq!(s.max_attempts, 3);
        assert_eq!(s.base_delay_ms, 60_000);
    }

    #[test]
    fn workspace_validation_failed_uses_workspace_recovery_delays() {
        let n = now();
        let s = decide_retry_schedule(RetryReason::WorkspaceValidationFailed, 1, n, 0.5).unwrap();
        assert_eq!(s.base_delay_ms, 30_000);
        assert_eq!(s.max_attempts, 3);
    }

    #[test]
    fn configuration_incomplete_uses_workspace_recovery_delays() {
        let n = now();
        let s = decide_retry_schedule(RetryReason::ConfigurationIncomplete, 1, n, 0.5).unwrap();
        assert_eq!(s.base_delay_ms, 30_000);
    }

    #[test]
    fn execution_review_participant_recovery_returns_none() {
        let n = now();
        assert!(
            decide_retry_schedule(RetryReason::ExecutionReviewParticipantRecovery, 1, n, 0.5)
                .is_none()
        );
    }

    #[test]
    fn unknown_reason_returns_none() {
        let n = now();
        assert!(
            decide_retry_schedule(RetryReason::Other("custom".to_string()), 1, n, 0.5).is_none()
        );
    }

    #[test]
    fn max_turns_continuation_uses_linear_delay() {
        let n = now();
        let s1 = decide_retry_schedule(RetryReason::MaxTurnsContinuation, 1, n, 0.5).unwrap();
        assert_eq!(s1.delay_ms, 1_000);
        let s2 = decide_retry_schedule(RetryReason::MaxTurnsContinuation, 2, n, 0.5).unwrap();
        assert_eq!(s2.delay_ms, 2_000);
        assert!(s2.delay_ms <= MAX_TURN_CONTINUATION_MAX_DELAY_MS);
    }

    #[test]
    fn max_turns_continuation_rejects_attempts_above_max_attempts() {
        let n = now();
        assert!(decide_retry_schedule(RetryReason::MaxTurnsContinuation, 0, n, 0.5).is_none());
        assert!(decide_retry_schedule(RetryReason::MaxTurnsContinuation, 3, n, 0.5).is_none());
    }

    #[test]
    fn retry_schedule_due_at_is_now_plus_delay() {
        let n = now();
        let s = decide_retry_schedule(RetryReason::TransientFailure, 2, n, 0.5).unwrap();
        let due_at_ms = s.due_at.as_datetime().timestamp_millis();
        let now_ms = n.as_datetime().timestamp_millis();
        assert_eq!(due_at_ms - now_ms, s.delay_ms);
        assert_eq!(s.base_delay_ms, 10 * 60_000);
    }

    #[test]
    fn retry_reason_string_form_is_stable() {
        assert_eq!(RetryReason::TransientFailure.as_str(), "transient_failure");
        assert_eq!(
            RetryReason::MaxTurnsContinuation.as_str(),
            "max_turns_continuation"
        );
        assert_eq!(
            RetryReason::InteractionContinuationInfra.as_str(),
            "interaction_continuation_infra_retry"
        );
        assert_eq!(
            RetryReason::WorkspaceValidationFailed.as_str(),
            "workspace_validation_failed"
        );
        assert_eq!(
            RetryReason::ConfigurationIncomplete.as_str(),
            "configuration_incomplete"
        );
        assert_eq!(
            RetryReason::DependencyUnavailable.as_str(),
            "dependency_unavailable"
        );
        assert_eq!(RetryReason::WorkspaceLocked.as_str(), "workspace_locked");
        assert_eq!(RetryReason::QuotaExceeded.as_str(), "quota_exceeded");
    }
}
