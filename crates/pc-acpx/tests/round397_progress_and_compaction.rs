//! Integration tests for R397: runtime-progress + session-compaction.

use pc_acpx::runtime_progress::{
    create_runtime_progress_reporter, RuntimeProgressDirection, RuntimeProgressPhase,
    RuntimeProgressReporterOptions, RuntimeProgressTarget,
};
use pc_acpx::session_compaction::{
    has_session_compaction_thresholds, resolve_session_compaction_policy, SessionCompactionSource,
};
use serde_json::json;
use std::sync::{Arc, Mutex};

// ============================================================================
// runtime_progress integration
// ============================================================================

#[tokio::test]
async fn progress_reporter_full_sync_lifecycle() {
    let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_clone = captured.clone();
    let sink = Arc::new(move |line: String| {
        captured_clone.lock().unwrap().push(line);
    });

    let counter = Arc::new(Mutex::new(0u64));
    let now = Arc::new(move || {
        let mut c = counter.lock().unwrap();
        let v = *c;
        *c += 100;
        v
    });

    let options = RuntimeProgressReporterOptions {
        sink,
        phase: RuntimeProgressPhase::Syncing,
        label: Some("workspace".to_string()),
        direction: RuntimeProgressDirection::To,
        target: RuntimeProgressTarget::Sandbox,
        step_percent: None,
        min_interval_ms: None,
        now_ms: Some(now),
    };

    let mut reporter = create_runtime_progress_reporter(options);
    reporter.report(250, Some(1000)).await; // 25% -> emit (step crossing)
    reporter.report(500, Some(1000)).await; // 50% -> emit (step crossing)
    reporter.report(750, Some(1000)).await; // 75% -> emit (step crossing)
    reporter.report(1000, Some(1000)).await; // 100% -> emit (terminal)

    let lines = captured.lock().unwrap();
    assert!(lines.len() >= 4);
    assert!(lines.last().unwrap().contains("100%"));
}

#[tokio::test]
async fn progress_reporter_fail_marks_completed() {
    let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_clone = captured.clone();
    let sink = Arc::new(move |line: String| {
        captured_clone.lock().unwrap().push(line);
    });

    let counter = Arc::new(Mutex::new(0u64));
    let now = Arc::new(move || {
        let mut c = counter.lock().unwrap();
        let v = *c;
        *c += 100;
        v
    });

    let options = RuntimeProgressReporterOptions {
        sink,
        phase: RuntimeProgressPhase::Restoring,
        label: None,
        direction: RuntimeProgressDirection::From,
        target: RuntimeProgressTarget::Ssh,
        step_percent: None,
        min_interval_ms: None,
        now_ms: Some(now),
    };

    let mut reporter = create_runtime_progress_reporter(options);
    reporter.report(100, Some(1000)).await;
    reporter.fail(Some(100), Some(1000)).await;

    let lines = captured.lock().unwrap();
    assert!(lines.iter().any(|l| l.contains("failed at")));
    // After fail(), reporter should have emitted a terminal line.
    let lines = captured.lock().unwrap();
    assert!(lines.iter().any(|l| l.contains("failed at")));
}

// ============================================================================
// session_compaction integration
// ============================================================================

#[test]
fn compaction_claude_uses_adapter_managed_policy() {
    let resolved = resolve_session_compaction_policy(Some("claude_local"), &json!({}));
    assert_eq!(resolved.source, SessionCompactionSource::AdapterDefault);
    assert!(resolved.adapter_session_management.is_some());
    assert!(!has_session_compaction_thresholds(&resolved.policy));
}

#[test]
fn compaction_gemini_uses_default_thresholds() {
    let resolved = resolve_session_compaction_policy(Some("gemini_local"), &json!({}));
    assert_eq!(resolved.source, SessionCompactionSource::AdapterDefault);
    assert!(has_session_compaction_thresholds(&resolved.policy));
    assert_eq!(resolved.policy.max_session_runs, 200);
    assert_eq!(resolved.policy.max_raw_input_tokens, 2_000_000);
    assert_eq!(resolved.policy.max_session_age_hours, 72);
}

#[test]
fn compaction_agent_override_takes_precedence() {
    let config = json!({
        "heartbeat": {
            "sessionCompaction": {
                "enabled": false,
                "maxSessionRuns": 5,
                "maxRawInputTokens": 100000,
                "maxSessionAgeHours": 12
            }
        }
    });
    let resolved = resolve_session_compaction_policy(Some("gemini_local"), &config);
    assert_eq!(resolved.source, SessionCompactionSource::AgentOverride);
    assert!(!resolved.policy.enabled);
    assert_eq!(resolved.policy.max_session_runs, 5);
    assert_eq!(resolved.policy.max_raw_input_tokens, 100_000);
    assert_eq!(resolved.policy.max_session_age_hours, 12);
}

#[test]
fn compaction_legacy_fallback_for_unknown_adapter() {
    let resolved = resolve_session_compaction_policy(Some("some_new_adapter"), &json!({}));
    assert_eq!(resolved.source, SessionCompactionSource::LegacyFallback);
    assert!(resolved.adapter_session_management.is_none());
    // Unknown adapter is not in legacy set, so fallback enabled = false
    assert!(!resolved.policy.enabled);
}

#[test]
fn compaction_all_legacy_adapters_have_enabled_policy() {
    for adapter_type in pc_acpx::session_compaction::LEGACY_SESSIONED_ADAPTER_TYPES {
        let resolved = resolve_session_compaction_policy(Some(adapter_type), &json!({}));
        assert!(
            resolved.policy.enabled,
            "{adapter_type} should have enabled policy"
        );
    }
}
