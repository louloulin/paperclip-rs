//! Heartbeat 域常量。
//!
//! 提供 heartbeat invocation source / wakeup trigger / run status / live event 等常量。

/// Heartbeat invocation source（谁发起的 heartbeat）。
pub const HEARTBEAT_INVOCATION_SOURCES: &[&str] = &[
    "scheduler",
    "manual",
    "callback",
    "wakeup",
    "continuation",
    "issue_change",
    "recovery",
];

/// Wakeup trigger detail。
pub const WAKEUP_TRIGGER_DETAILS: &[&str] = &["manual", "ping", "callback", "system"];

/// Wakeup request 状态。
pub const WAKEUP_REQUEST_STATUSES: &[&str] =
    &["pending", "scheduled", "fired", "cancelled", "expired"];

/// Heartbeat run 状态（与 `pc_repos::heartbeat::RunStatus` enum 对齐）。
pub const HEARTBEAT_RUN_STATUSES: &[&str] = &[
    "queued",
    "running",
    "scheduled_retry",
    "succeeded",
    "failed",
    "cancelled",
    "timed_out",
];

/// Run liveness 状态（用于 UI 显示 / 健康判定）。
pub const RUN_LIVENESS_STATES: &[&str] = &["alive", "silent", "stalled", "stuck", "done"];

/// Live event 类型（WebSocket 推送的事件分类）。
pub const LIVE_EVENT_TYPES: &[&str] = &[
    "heartbeat.scheduled",
    "heartbeat.started",
    "heartbeat.completed",
    "heartbeat.failed",
    "issue.created",
    "issue.updated",
    "issue.status_changed",
    "issue.comment_added",
    "agent.created",
    "agent.updated",
    "company.created",
    "company.updated",
    "decision.created",
    "decision.resolved",
    "approval.requested",
    "approval.granted",
    "approval.denied",
    "wakeup.fired",
    "wakeup.suppressed",
    "run.liveness.changed",
    "live.heartbeat",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invocation_sources_non_empty() {
        assert!(!HEARTBEAT_INVOCATION_SOURCES.is_empty());
        assert!(HEARTBEAT_INVOCATION_SOURCES.contains(&"scheduler"));
    }

    #[test]
    fn run_statuses_match_repos_enum() {
        // Must be in sync with pc_repos::heartbeat::RunStatus variants
        assert!(HEARTBEAT_RUN_STATUSES.contains(&"queued"));
        assert!(HEARTBEAT_RUN_STATUSES.contains(&"running"));
        assert!(HEARTBEAT_RUN_STATUSES.contains(&"succeeded"));
        assert!(HEARTBEAT_RUN_STATUSES.contains(&"failed"));
        assert!(HEARTBEAT_RUN_STATUSES.contains(&"cancelled"));
    }

    #[test]
    fn live_event_types_includes_heartbeat() {
        assert!(LIVE_EVENT_TYPES.contains(&"heartbeat.completed"));
        assert!(LIVE_EVENT_TYPES.contains(&"live.heartbeat"));
    }

    #[test]
    fn liveness_states_match_node() {
        assert_eq!(
            RUN_LIVENESS_STATES,
            &["alive", "silent", "stalled", "stuck", "done"]
        );
    }
}
