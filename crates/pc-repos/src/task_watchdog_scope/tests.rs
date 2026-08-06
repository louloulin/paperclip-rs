//! 顶层单测（types + scope kind label）。

use super::*;

#[test]
fn scope_kind_label_round_trip() {
    for k in [
        TaskWatchdogMutationScopeKind::None,
        TaskWatchdogMutationScopeKind::Invalid,
        TaskWatchdogMutationScopeKind::Watchdog,
    ] {
        let json = serde_json::to_value(k).unwrap();
        assert_eq!(json, serde_json::Value::String(k.as_str().to_string()));
    }
}

#[test]
fn scope_kind_method_matches_variant() {
    let none = TaskWatchdogMutationScope::None;
    assert_eq!(none.kind(), TaskWatchdogMutationScopeKind::None);
    let invalid = TaskWatchdogMutationScope::Invalid { detail: "x".into() };
    assert_eq!(invalid.kind(), TaskWatchdogMutationScopeKind::Invalid);
    let wd = TaskWatchdogMutationScope::Watchdog {
        watchdog_id: "w-1".into(),
        company_id: "c-1".into(),
        watched_issue_id: "i-1".into(),
        watchdog_issue_id: None,
        stop_fingerprint: None,
    };
    assert_eq!(wd.kind(), TaskWatchdogMutationScopeKind::Watchdog);
}

#[test]
fn default_scope_is_none() {
    assert_eq!(
        TaskWatchdogMutationScope::default(),
        TaskWatchdogMutationScope::None
    );
}

#[test]
fn scope_serializes_with_kind_tag() {
    let scope = TaskWatchdogMutationScope::Watchdog {
        watchdog_id: "w-1".into(),
        company_id: "c-1".into(),
        watched_issue_id: "i-1".into(),
        watchdog_issue_id: Some("wi-1".into()),
        stop_fingerprint: Some("fp-1".into()),
    };
    let json = serde_json::to_value(&scope).unwrap();
    assert_eq!(json["kind"], "watchdog");
    assert_eq!(json["watchdogId"], "w-1");
    assert_eq!(json["watchedIssueId"], "i-1");
    assert_eq!(json["watchdogIssueId"], "wi-1");
    assert_eq!(json["stopFingerprint"], "fp-1");
    assert_eq!(json["companyId"], "c-1");
}

#[test]
fn none_scope_serializes_with_kind_none() {
    let scope = TaskWatchdogMutationScope::None;
    let json = serde_json::to_value(&scope).unwrap();
    assert_eq!(json["kind"], "none");
}

#[test]
fn invalid_scope_serializes_with_kind_invalid_and_detail() {
    let scope = TaskWatchdogMutationScope::Invalid {
        detail: "reason".into(),
    };
    let json = serde_json::to_value(&scope).unwrap();
    assert_eq!(json["kind"], "invalid");
    assert_eq!(json["detail"], "reason");
}

#[test]
fn agent_run_actor_agent_helper() {
    let actor = AgentRunActor::agent("agent-1", "run-1");
    assert_eq!(actor.actor_type, "agent");
    assert_eq!(actor.agent_id.as_deref(), Some("agent-1"));
    assert_eq!(actor.run_id.as_deref(), Some("run-1"));
    assert!(actor.company_id.is_none());
}

#[test]
fn options_default_allows_watchdog_issue() {
    let opts = TaskWatchdogScopeAllowsOptions::default();
    assert!(opts.allow_watchdog_issue.is_none());
}

#[test]
fn options_new_sets_flag() {
    let opts = TaskWatchdogScopeAllowsOptions::new(false);
    assert_eq!(opts.allow_watchdog_issue, Some(false));
}
