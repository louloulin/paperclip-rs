//! E2E tests for `pc-decision-wakeup`.
//!
//! 与 Node `server/src/__tests__/decision-wakeup.test.ts` 1:1 对齐。

use std::sync::Arc;

use futures::future::BoxFuture;
use pc_decision_wakeup::{
    create_decision_wake_origin_agent, DecisionOutcome, DecisionWakeInput, HeartbeatWakeupFn,
    HeartbeatWakeupOptions,
};
use serde_json::{json, Value};
use tokio::sync::Mutex;

// ============================================================================
// Helpers
// ============================================================================

fn sample_input(outcome: DecisionOutcome) -> DecisionWakeInput {
    DecisionWakeInput::new("company-1", "agent-1", "issue-1", "decision-1", outcome)
}
/// 构造一个会记录所有调用的 heartbeat wakeup 函数。
fn recording_wakeup() -> (HeartbeatWakeupFn, Arc<Mutex<Vec<(String, HeartbeatWakeupOptions)>>>) {
    let calls: Arc<Mutex<Vec<(String, HeartbeatWakeupOptions)>>> = Arc::new(Mutex::new(Vec::new()));
    let calls_for_closure = calls.clone();
    let wakeup: HeartbeatWakeupFn = Arc::new(move |agent_id: String, opts: HeartbeatWakeupOptions| {
        let calls = calls_for_closure.clone();
        Box::pin(async move {
            calls.lock().await.push((agent_id, opts));
            Some(json!({ "id": "run-1" }))
        })
    });
    (wakeup, calls)
}

// ============================================================================
// Disabled runtime
// ============================================================================

#[tokio::test]
async fn r668_returns_null_when_heartbeat_disabled() {
    let wake_origin = create_decision_wake_origin_agent(None);
    let result = wake_origin(sample_input(DecisionOutcome::Decided)).await;
    assert_eq!(result, None);
}

#[tokio::test]
async fn r668_disabled_runtime_never_calls_wakeup_for_any_outcome() {
    // 即便传入多种 outcome，禁用的 runtime 也永不投递 wakeup。
    let wake_origin = create_decision_wake_origin_agent(None);
    for outcome in [
        DecisionOutcome::Decided,
        DecisionOutcome::Expired,
        DecisionOutcome::Cancelled,
    ] {
        let result = wake_origin(sample_input(outcome)).await;
        assert_eq!(result, None, "disabled runtime should return None for {outcome:?}");
    }
}

// ============================================================================
// Enabled runtime
// ============================================================================

#[tokio::test]
async fn r668_maps_decided_continuation_onto_heartbeat_runtime() {
    let (wakeup, calls) = recording_wakeup();
    let wake_origin = create_decision_wake_origin_agent(Some(wakeup));

    let result = wake_origin(sample_input(DecisionOutcome::Decided)).await;

    // 1. 返回值透传
    assert_eq!(result, Some(json!({ "id": "run-1" })));

    // 2. wakeup 被以正确参数调用
    let recorded = calls.lock().await.clone();
    assert_eq!(recorded.len(), 1);
    let (agent_id, opts) = &recorded[0];
    assert_eq!(agent_id, "agent-1");
    assert_eq!(opts.source, "automation");
    assert_eq!(opts.trigger_detail, "system");
    assert_eq!(opts.reason, "decision_decided");
    assert_eq!(
        opts.payload,
        json!({
            "issueId": "issue-1",
            "decisionId": "decision-1",
            "outcome": "decided",
        })
    );
}

#[tokio::test]
async fn r668_maps_expired_outcome_to_decision_expired_reason() {
    let (wakeup, calls) = recording_wakeup();
    let wake_origin = create_decision_wake_origin_agent(Some(wakeup));

    let result = wake_origin(sample_input(DecisionOutcome::Expired)).await;

    assert_eq!(result, Some(json!({ "id": "run-1" })));
    let recorded = calls.lock().await.clone();
    assert_eq!(recorded[0].1.reason, "decision_expired");
    assert_eq!(recorded[0].1.payload["outcome"], "expired");
}

#[tokio::test]
async fn r668_maps_cancelled_outcome_to_decision_cancelled_reason() {
    let (wakeup, calls) = recording_wakeup();
    let wake_origin = create_decision_wake_origin_agent(Some(wakeup));

    let result = wake_origin(sample_input(DecisionOutcome::Cancelled)).await;

    assert_eq!(result, Some(json!({ "id": "run-1" })));
    let recorded = calls.lock().await.clone();
    assert_eq!(recorded[0].1.reason, "decision_cancelled");
    assert_eq!(recorded[0].1.payload["outcome"], "cancelled");
}

// ============================================================================
// Reuse / clone semantics
// ============================================================================

#[tokio::test]
async fn r668_wake_origin_agent_is_reusable_across_invocations() {
    let (wakeup, calls) = recording_wakeup();
    let wake_origin = create_decision_wake_origin_agent(Some(wakeup));

    let _ = wake_origin(sample_input(DecisionOutcome::Decided)).await;
    let _ = wake_origin(sample_input(DecisionOutcome::Expired)).await;
    let _ = wake_origin(sample_input(DecisionOutcome::Cancelled)).await;

    let recorded = calls.lock().await.clone();
    assert_eq!(recorded.len(), 3);
    assert_eq!(recorded[0].1.reason, "decision_decided");
    assert_eq!(recorded[1].1.reason, "decision_expired");
    assert_eq!(recorded[2].1.reason, "decision_cancelled");
}

#[tokio::test]
async fn r668_independent_agents_get_independent_calls() {
    // 验证传入两个独立构造的 wake_origin agent 不会共享状态。
    let (wakeup_a, calls_a) = recording_wakeup();
    let (wakeup_b, calls_b) = recording_wakeup();
    let agent_a = create_decision_wake_origin_agent(Some(wakeup_a));
    let agent_b = create_decision_wake_origin_agent(Some(wakeup_b));

    let _ = agent_a(sample_input(DecisionOutcome::Decided)).await;
    let _ = agent_b(sample_input(DecisionOutcome::Decided)).await;

    assert_eq!(calls_a.lock().await.len(), 1);
    assert_eq!(calls_b.lock().await.len(), 1);
}

#[tokio::test]
async fn r668_wake_origin_agent_uses_correct_ids_per_call() {
    // 验证 agent_id / issue_id / decision_id 被正确透传。
    let (wakeup, calls) = recording_wakeup();
    let wake_origin = create_decision_wake_origin_agent(Some(wakeup));

    let input = DecisionWakeInput::new(
        "company-xyz",
        "agent-42",
        "issue-9001",
        "decision-9001",
        DecisionOutcome::Decided,
    );

    let _ = wake_origin(input).await;
    let recorded = calls.lock().await.clone();
    assert_eq!(recorded[0].0, "agent-42");
    assert_eq!(recorded[0].1.payload["issueId"], "issue-9001");
    assert_eq!(recorded[0].1.payload["decisionId"], "decision-9001");
}

// ============================================================================
// HeartbeatWakeupOptions helpers
// ============================================================================

#[tokio::test]
async fn r668_decision_reason_helper_matches_node_template_literal() {
    assert_eq!(
        HeartbeatWakeupOptions::decision_reason(DecisionOutcome::Decided),
        "decision_decided"
    );
    assert_eq!(
        HeartbeatWakeupOptions::decision_reason(DecisionOutcome::Expired),
        "decision_expired"
    );
    assert_eq!(
        HeartbeatWakeupOptions::decision_reason(DecisionOutcome::Cancelled),
        "decision_cancelled"
    );
}

#[tokio::test]
async fn r668_decision_payload_helper_emits_camel_case_keys() {
    let payload =
        HeartbeatWakeupOptions::decision_payload("iss-1", "dec-1", DecisionOutcome::Decided);
    assert_eq!(
        payload,
        json!({
            "issueId": "iss-1",
            "decisionId": "dec-1",
            "outcome": "decided",
        })
    );
}

#[tokio::test]
async fn r668_outcome_as_str_matches_node_values() {
    assert_eq!(DecisionOutcome::Decided.as_str(), "decided");
    assert_eq!(DecisionOutcome::Expired.as_str(), "expired");
    assert_eq!(DecisionOutcome::Cancelled.as_str(), "cancelled");
}

// ============================================================================
// Wakeup returning None
// ============================================================================

#[tokio::test]
async fn r668_propagates_none_from_underlying_wakeup() {
    // 当底层 heartbeat wakeup 返回 null 时，wake_origin agent 也应返回 null。
    let wakeup: HeartbeatWakeupFn = Arc::new(
        |_agent_id: String, _opts: HeartbeatWakeupOptions| -> BoxFuture<'static, Option<Value>> {
            Box::pin(async move { None })
        },
    );
    let wake_origin = create_decision_wake_origin_agent(Some(wakeup));
    let result = wake_origin(sample_input(DecisionOutcome::Decided)).await;
    assert_eq!(result, None);
}

#[tokio::test]
async fn r668_propagates_json_value_from_underlying_wakeup() {
    // 验证返回值透传：任何 JSON 都应原样返回。
    let wakeup: HeartbeatWakeupFn = Arc::new(
        |_agent_id: String, _opts: HeartbeatWakeupOptions| -> BoxFuture<'static, Option<Value>> {
            Box::pin(async move { Some(json!({ "runId": "abc-123", "scheduledAt": "2026-01-01T00:00:00Z" })) })
        },
    );
    let wake_origin = create_decision_wake_origin_agent(Some(wakeup));
    let result = wake_origin(sample_input(DecisionOutcome::Decided)).await;
    assert_eq!(
        result,
        Some(json!({ "runId": "abc-123", "scheduledAt": "2026-01-01T00:00:00Z" }))
    );
}

