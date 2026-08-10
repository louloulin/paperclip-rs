//! Heartbeat run stop metadata.
//!
//! 1:1 port of Node `paperclip/server/src/services/heartbeat-stop-metadata.ts`.
//!
//! This crate is pure logic — no DB / no I/O. Given an adapter type, its
//! config, and a run outcome, it computes:
//!
//! * the effective timeout policy (in seconds / milliseconds, depending on
//!   the adapter type),
//! * the inferred stop reason (one of a fixed enum), and
//! * a helper to merge the derived metadata back into a result JSON blob
//!   (preserving any existing "max_turns_exhausted" stop reason).
//!
//! Three adapter shapes are recognised:
//!
//! * `"http"`  — uses `timeoutMs` (milliseconds).
//! * anything else — uses `timeoutSec` (seconds), with a 120s default for
//!   the `"openclaw_gateway"` adapter and 0 for everything else.
//!
//! The crate exposes the inferred reason and policy as `Serialize`-able
//! DTOs so it can be embedded into structured run results or persisted
//! alongside heartbeat rows.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Final outcome of a heartbeat run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeartbeatRunOutcome {
    Succeeded,
    Interrupted,
    Failed,
    Cancelled,
    TimedOut,
}

/// Reason the heartbeat run stopped. Note: in `merge_*` the value
/// `"max_turns_exhausted"` wins over the freshly inferred one if it
/// already exists in the result JSON (to preserve the explicit
/// continuation signal that downstream tooling expects).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeartbeatRunStopReason {
    Completed,
    Interrupted,
    Timeout,
    Cancelled,
    BudgetPaused,
    Paused,
    MaxTurnsExhausted,
    ProcessLost,
    UnmanagedBackgroundTaskStopped,
    AdapterFailed,
}

impl HeartbeatRunStopReason {
    pub fn as_str(self) -> &'static str {
        match self {
            HeartbeatRunStopReason::Completed => "completed",
            HeartbeatRunStopReason::Interrupted => "interrupted",
            HeartbeatRunStopReason::Timeout => "timeout",
            HeartbeatRunStopReason::Cancelled => "cancelled",
            HeartbeatRunStopReason::BudgetPaused => "budget_paused",
            HeartbeatRunStopReason::Paused => "paused",
            HeartbeatRunStopReason::MaxTurnsExhausted => "max_turns_exhausted",
            HeartbeatRunStopReason::ProcessLost => "process_lost",
            HeartbeatRunStopReason::UnmanagedBackgroundTaskStopped => {
                "unmanaged_background_task_stopped"
            }
            HeartbeatRunStopReason::AdapterFailed => "adapter_failed",
        }
    }
}

/// Effective timeout policy derived from the adapter config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatRunTimeoutPolicy {
    pub effective_timeout_sec: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub effective_timeout_ms: Option<i64>,
    pub timeout_configured: bool,
    pub timeout_source: HeartbeatTimeoutSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HeartbeatTimeoutSource {
    Config,
    Default,
    Unknown,
}

impl HeartbeatTimeoutSource {
    pub fn as_str(self) -> &'static str {
        match self {
            HeartbeatTimeoutSource::Config => "config",
            HeartbeatTimeoutSource::Default => "default",
            HeartbeatTimeoutSource::Unknown => "unknown",
        }
    }
}

/// Full stop metadata: timeout policy + inferred stop reason + a flag
/// indicating whether the timeout actually fired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatRunStopMetadata {
    #[serde(flatten)]
    pub timeout: HeartbeatRunTimeoutPolicy,
    pub stop_reason: HeartbeatRunStopReason,
    pub timeout_fired: bool,
}

// ---------------------------------------------------------------------
// Helpers (private)
// ---------------------------------------------------------------------

fn read_finite_number(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(n) => n.as_i64().filter(|v| (*v as f64).is_finite()),
        Value::String(s) => {
            let trimmed = s.trim();
            trimmed.parse::<i64>().ok().filter(|v| (*v as f64).is_finite())
        }
        _ => None,
    }
}

fn has_own(record: &Map<String, Value>, key: &str) -> bool {
    record.contains_key(key)
}

fn default_timeout_sec_for_adapter(adapter_type: &str) -> i64 {
    if adapter_type == "openclaw_gateway" {
        120
    } else {
        0
    }
}

// ---------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------

/// Map any of the known "max-turn" sentinel strings to the canonical
/// [`HeartbeatRunStopReason::MaxTurnsExhausted`]. Returns `None` for
/// anything else.
pub fn normalize_max_turn_stop_reason(value: &str) -> Option<HeartbeatRunStopReason> {
    match value {
        "max_turns_exhausted" | "turn_limit_exhausted" => Some(HeartbeatRunStopReason::MaxTurnsExhausted),
        _ => None,
    }
}

/// Resolve the effective timeout policy for the given adapter + config.
///
/// * For `"http"` adapters the value is read from `config.timeoutMs`.
/// * For every other adapter the value is read from `config.timeoutSec`,
///   defaulting to 120s for `openclaw_gateway` and 0s otherwise.
pub fn resolve_heartbeat_run_timeout_policy(
    adapter_type: &str,
    adapter_config: Option<&Map<String, Value>>,
) -> HeartbeatRunTimeoutPolicy {
    let empty = Map::new();
    let config = adapter_config.unwrap_or(&empty);

    if adapter_type == "http" {
        let has_timeout_ms = has_own(config, "timeoutMs");
        let raw = if has_timeout_ms {
            read_finite_number(config.get("timeoutMs")).unwrap_or(0)
        } else {
            0
        };
        let timeout_ms = raw.max(0);
        return HeartbeatRunTimeoutPolicy {
            effective_timeout_sec: Some(timeout_ms / 1000),
            effective_timeout_ms: Some(timeout_ms),
            timeout_configured: timeout_ms > 0,
            timeout_source: if has_timeout_ms {
                HeartbeatTimeoutSource::Config
            } else {
                HeartbeatTimeoutSource::Default
            },
        };
    }

    let has_timeout_sec = has_own(config, "timeoutSec");
    let default = default_timeout_sec_for_adapter(adapter_type);
    let raw = if has_timeout_sec {
        read_finite_number(config.get("timeoutSec")).unwrap_or(default)
    } else {
        default
    };
    let timeout_sec = raw.max(0);

    HeartbeatRunTimeoutPolicy {
        effective_timeout_sec: Some(timeout_sec),
        effective_timeout_ms: None,
        timeout_configured: timeout_sec > 0,
        timeout_source: if has_timeout_sec {
            HeartbeatTimeoutSource::Config
        } else {
            HeartbeatTimeoutSource::Default
        },
    }
}

/// Infer the stop reason for a run from its outcome + structured error
/// fields. The decision tree mirrors the Node implementation verbatim.
pub fn infer_heartbeat_run_stop_reason(input: StopReasonInput<'_>) -> HeartbeatRunStopReason {
    if input.outcome == HeartbeatRunOutcome::Succeeded {
        return HeartbeatRunStopReason::Completed;
    }
    if input.outcome == HeartbeatRunOutcome::Interrupted {
        return HeartbeatRunStopReason::Interrupted;
    }
    if let Some(reason) = input.error_code.and_then(normalize_max_turn_stop_reason) {
        return reason;
    }
    if input.outcome == HeartbeatRunOutcome::TimedOut {
        return HeartbeatRunStopReason::Timeout;
    }
    if input.outcome == HeartbeatRunOutcome::Failed
        && input.error_code == Some("unmanaged_background_task_stopped")
    {
        return HeartbeatRunStopReason::UnmanagedBackgroundTaskStopped;
    }
    if input.outcome == HeartbeatRunOutcome::Failed
        && input.error_code == Some("process_lost")
    {
        return HeartbeatRunStopReason::ProcessLost;
    }
    if input.outcome == HeartbeatRunOutcome::Cancelled {
        let message = input
            .error_message
            .map(|m| m.to_ascii_lowercase())
            .unwrap_or_default();
        if message.contains("budget") {
            return HeartbeatRunStopReason::BudgetPaused;
        }
        if message.contains("pause") {
            return HeartbeatRunStopReason::Paused;
        }
        return HeartbeatRunStopReason::Cancelled;
    }
    HeartbeatRunStopReason::AdapterFailed
}

#[derive(Debug, Clone, Copy)]
pub struct StopReasonInput<'a> {
    pub outcome: HeartbeatRunOutcome,
    pub error_code: Option<&'a str>,
    pub error_message: Option<&'a str>,
}

/// Build the full stop metadata for a heartbeat run.
pub fn build_heartbeat_run_stop_metadata(
    input: BuildStopMetadataInput<'_>,
) -> HeartbeatRunStopMetadata {
    let timeout = resolve_heartbeat_run_timeout_policy(input.adapter_type, input.adapter_config);
    let stop_reason = infer_heartbeat_run_stop_reason(StopReasonInput {
        outcome: input.outcome,
        error_code: input.error_code,
        error_message: input.error_message,
    });
    HeartbeatRunStopMetadata {
        timeout,
        timeout_fired: stop_reason == HeartbeatRunStopReason::Timeout,
        stop_reason,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BuildStopMetadataInput<'a> {
    pub adapter_type: &'a str,
    pub adapter_config: Option<&'a Map<String, Value>>,
    pub outcome: HeartbeatRunOutcome,
    pub error_code: Option<&'a str>,
    pub error_message: Option<&'a str>,
}

/// Merge the freshly computed stop metadata into an existing result JSON
/// object. Returns a new `Map` (does not mutate `result_json` in place).
///
/// Special rule: if the existing JSON already contains a "max_turns"
/// stop reason it is preserved — this guards against the heartbeat actor
/// losing the explicit continuation signal that downstream watchers rely
/// on.
pub fn merge_heartbeat_run_stop_metadata(
    result_json: Option<&Map<String, Value>>,
    metadata: &HeartbeatRunStopMetadata,
) -> Map<String, Value> {
    let mut out: Map<String, Value> = result_json
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();

    let existing_max_turn: Option<HeartbeatRunStopReason> = result_json
        .and_then(|m| m.get("stopReason"))
        .and_then(|v| v.as_str())
        .and_then(normalize_max_turn_stop_reason);

    let stop_reason = existing_max_turn.unwrap_or(metadata.stop_reason);
    out.insert(
        "stopReason".to_string(),
        Value::String(stop_reason.as_str().to_string()),
    );
    out.insert(
        "effectiveTimeoutSec".to_string(),
        match metadata.timeout.effective_timeout_sec {
            Some(v) => Value::Number(serde_json::Number::from(v)),
            None => Value::Null,
        },
    );
    out.insert(
        "timeoutConfigured".to_string(),
        Value::Bool(metadata.timeout.timeout_configured),
    );
    out.insert(
        "timeoutSource".to_string(),
        Value::String(metadata.timeout.timeout_source.as_str().to_string()),
    );
    out.insert(
        "timeoutFired".to_string(),
        Value::Bool(metadata.timeout_fired),
    );
    if let Some(ms) = metadata.timeout.effective_timeout_ms {
        out.insert(
            "effectiveTimeoutMs".to_string(),
            Value::Number(serde_json::Number::from(ms)),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cfg_from(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            Value::Null => Map::new(),
            other => panic!("expected object, got {other:?}"),
        }
    }

    fn empty_cfg() -> Map<String, Value> {
        Map::new()
    }

    // -------- normalize_max_turn_stop_reason --------

    #[test]
    fn normalize_max_turn_accepts_canonical_and_legacy() {
        assert_eq!(
            normalize_max_turn_stop_reason("max_turns_exhausted"),
            Some(HeartbeatRunStopReason::MaxTurnsExhausted)
        );
        assert_eq!(
            normalize_max_turn_stop_reason("turn_limit_exhausted"),
            Some(HeartbeatRunStopReason::MaxTurnsExhausted)
        );
        assert_eq!(normalize_max_turn_stop_reason("timeout"), None);
        assert_eq!(normalize_max_turn_stop_reason(""), None);
    }

    // -------- resolve_heartbeat_run_timeout_policy (http) --------

    #[test]
    fn http_uses_milliseconds_when_present() {
        let cfg = cfg_from(json!({"timeoutMs": 2500}));
        let p = resolve_heartbeat_run_timeout_policy("http", Some(&cfg));
        assert_eq!(p.effective_timeout_ms, Some(2500));
        assert_eq!(p.effective_timeout_sec, Some(2));
        assert!(p.timeout_configured);
        assert_eq!(p.timeout_source, HeartbeatTimeoutSource::Config);
    }

    #[test]
    fn http_defaults_when_missing_or_zero() {
        let cfg = empty_cfg();
        let p = resolve_heartbeat_run_timeout_policy("http", Some(&cfg));
        assert_eq!(p.effective_timeout_ms, Some(0));
        assert_eq!(p.effective_timeout_sec, Some(0));
        assert!(!p.timeout_configured);
        assert_eq!(p.timeout_source, HeartbeatTimeoutSource::Default);

        let cfg_zero = cfg_from(json!({"timeoutMs": 0}));
        let p = resolve_heartbeat_run_timeout_policy("http", Some(&cfg_zero));
        assert!(!p.timeout_configured);
        assert_eq!(p.timeout_source, HeartbeatTimeoutSource::Config);
    }

    #[test]
    fn http_string_timeout_ms_is_parsed() {
        let cfg = cfg_from(json!({"timeoutMs": "30000"}));
        let p = resolve_heartbeat_run_timeout_policy("http", Some(&cfg));
        assert_eq!(p.effective_timeout_ms, Some(30000));
        assert_eq!(p.effective_timeout_sec, Some(30));
    }

    #[test]
    fn http_negative_timeout_clamps_to_zero() {
        let cfg = cfg_from(json!({"timeoutMs": -100}));
        let p = resolve_heartbeat_run_timeout_policy("http", Some(&cfg));
        assert_eq!(p.effective_timeout_ms, Some(0));
        assert!(!p.timeout_configured);
    }

    // -------- resolve_heartbeat_run_timeout_policy (other) --------

    #[test]
    fn openclaw_gateway_default_120s() {
        let cfg = empty_cfg();
        let p = resolve_heartbeat_run_timeout_policy("openclaw_gateway", Some(&cfg));
        assert_eq!(p.effective_timeout_sec, Some(120));
        assert_eq!(p.effective_timeout_ms, None);
        assert!(p.timeout_configured);
        assert_eq!(p.timeout_source, HeartbeatTimeoutSource::Default);
    }

    #[test]
    fn unknown_adapter_default_zero_when_missing() {
        let cfg = empty_cfg();
        let p = resolve_heartbeat_run_timeout_policy("codex_local", Some(&cfg));
        assert_eq!(p.effective_timeout_sec, Some(0));
        assert!(!p.timeout_configured);
        assert_eq!(p.timeout_source, HeartbeatTimeoutSource::Default);
    }

    #[test]
    fn custom_timeout_sec_overrides_default() {
        let cfg = cfg_from(json!({"timeoutSec": 300}));
        let p = resolve_heartbeat_run_timeout_policy("claude_local", Some(&cfg));
        assert_eq!(p.effective_timeout_sec, Some(300));
        assert!(p.timeout_configured);
        assert_eq!(p.timeout_source, HeartbeatTimeoutSource::Config);
    }

    #[test]
    fn unknown_adapter_with_null_config_still_resolves() {
        let p = resolve_heartbeat_run_timeout_policy("codex_local", None);
        assert_eq!(p.effective_timeout_sec, Some(0));
        assert_eq!(p.timeout_source, HeartbeatTimeoutSource::Default);
    }

    // -------- infer_heartbeat_run_stop_reason --------

    #[test]
    fn succeeded_maps_to_completed() {
        let r = infer_heartbeat_run_stop_reason(StopReasonInput {
            outcome: HeartbeatRunOutcome::Succeeded,
            error_code: Some("ignored"),
            error_message: None,
        });
        assert_eq!(r, HeartbeatRunStopReason::Completed);
    }

    #[test]
    fn interrupted_maps_to_interrupted() {
        let r = infer_heartbeat_run_stop_reason(StopReasonInput {
            outcome: HeartbeatRunOutcome::Interrupted,
            error_code: None,
            error_message: None,
        });
        assert_eq!(r, HeartbeatRunStopReason::Interrupted);
    }

    #[test]
    fn timed_out_maps_to_timeout() {
        let r = infer_heartbeat_run_stop_reason(StopReasonInput {
            outcome: HeartbeatRunOutcome::TimedOut,
            error_code: None,
            error_message: None,
        });
        assert_eq!(r, HeartbeatRunStopReason::Timeout);
    }

    #[test]
    fn failed_with_unmanaged_error_code() {
        let r = infer_heartbeat_run_stop_reason(StopReasonInput {
            outcome: HeartbeatRunOutcome::Failed,
            error_code: Some("unmanaged_background_task_stopped"),
            error_message: Some(""),
        });
        assert_eq!(r, HeartbeatRunStopReason::UnmanagedBackgroundTaskStopped);
    }

    #[test]
    fn failed_with_process_lost() {
        let r = infer_heartbeat_run_stop_reason(StopReasonInput {
            outcome: HeartbeatRunOutcome::Failed,
            error_code: Some("process_lost"),
            error_message: None,
        });
        assert_eq!(r, HeartbeatRunStopReason::ProcessLost);
    }

    #[test]
    fn max_turns_code_wins_even_with_failed_outcome() {
        let r = infer_heartbeat_run_stop_reason(StopReasonInput {
            outcome: HeartbeatRunOutcome::Failed,
            error_code: Some("max_turns_exhausted"),
            error_message: None,
        });
        assert_eq!(r, HeartbeatRunStopReason::MaxTurnsExhausted);
    }

    #[test]
    fn cancelled_with_budget_message() {
        let r = infer_heartbeat_run_stop_reason(StopReasonInput {
            outcome: HeartbeatRunOutcome::Cancelled,
            error_code: None,
            error_message: Some("Hit monthly BUDGET limit"),
        });
        assert_eq!(r, HeartbeatRunStopReason::BudgetPaused);
    }

    #[test]
    fn cancelled_with_pause_message() {
        let r = infer_heartbeat_run_stop_reason(StopReasonInput {
            outcome: HeartbeatRunOutcome::Cancelled,
            error_code: None,
            error_message: Some("user paused the run"),
        });
        assert_eq!(r, HeartbeatRunStopReason::Paused);
    }

    #[test]
    fn cancelled_with_unrelated_message() {
        let r = infer_heartbeat_run_stop_reason(StopReasonInput {
            outcome: HeartbeatRunOutcome::Cancelled,
            error_code: None,
            error_message: Some("operator killed process"),
        });
        assert_eq!(r, HeartbeatRunStopReason::Cancelled);
    }

    #[test]
    fn failed_without_specific_code_falls_through_to_adapter_failed() {
        let r = infer_heartbeat_run_stop_reason(StopReasonInput {
            outcome: HeartbeatRunOutcome::Failed,
            error_code: Some("unknown"),
            error_message: Some("kaboom"),
        });
        assert_eq!(r, HeartbeatRunStopReason::AdapterFailed);
    }

    #[test]
    fn budget_check_is_case_insensitive() {
        let r = infer_heartbeat_run_stop_reason(StopReasonInput {
            outcome: HeartbeatRunOutcome::Cancelled,
            error_code: None,
            error_message: Some("BUDGET exceeded"),
        });
        assert_eq!(r, HeartbeatRunStopReason::BudgetPaused);
    }

    // -------- build_heartbeat_run_stop_metadata --------

    #[test]
    fn builder_combines_timeout_and_reason() {
        let cfg = cfg_from(json!({"timeoutMs": 5000}));
        let m = build_heartbeat_run_stop_metadata(BuildStopMetadataInput {
            adapter_type: "http",
            adapter_config: Some(&cfg),
            outcome: HeartbeatRunOutcome::TimedOut,
            error_code: None,
            error_message: None,
        });
        assert_eq!(m.timeout.effective_timeout_ms, Some(5000));
        assert_eq!(m.stop_reason, HeartbeatRunStopReason::Timeout);
        assert!(m.timeout_fired);
    }

    #[test]
    fn builder_does_not_fire_timeout_for_other_reasons() {
        let cfg = cfg_from(json!({"timeoutMs": 5000}));
        let m = build_heartbeat_run_stop_metadata(BuildStopMetadataInput {
            adapter_type: "http",
            adapter_config: Some(&cfg),
            outcome: HeartbeatRunOutcome::Succeeded,
            error_code: None,
            error_message: None,
        });
        assert!(!m.timeout_fired);
        assert_eq!(m.stop_reason, HeartbeatRunStopReason::Completed);
    }

    // -------- merge_heartbeat_run_stop_metadata --------

    #[test]
    fn merge_writes_into_empty_object() {
        let cfg = cfg_from(json!({"timeoutSec": 60}));
        let m = build_heartbeat_run_stop_metadata(BuildStopMetadataInput {
            adapter_type: "codex_local",
            adapter_config: Some(&cfg),
            outcome: HeartbeatRunOutcome::Cancelled,
            error_code: None,
            error_message: Some("paused by user"),
        });
        let out = merge_heartbeat_run_stop_metadata(None, &m);
        assert_eq!(out.get("stopReason").unwrap(), "paused");
        assert_eq!(out.get("effectiveTimeoutSec").unwrap(), 60);
        assert_eq!(out.get("timeoutConfigured").unwrap(), true);
        assert_eq!(out.get("timeoutSource").unwrap(), "config");
        assert_eq!(out.get("timeoutFired").unwrap(), false);
        assert!(out.get("effectiveTimeoutMs").is_none());
    }

    #[test]
    fn merge_preserves_existing_max_turn_signal() {
        let cfg = cfg_from(json!({"timeoutSec": 0}));
        let m = build_heartbeat_run_stop_metadata(BuildStopMetadataInput {
            adapter_type: "claude_local",
            adapter_config: Some(&cfg),
            outcome: HeartbeatRunOutcome::Succeeded,
            error_code: None,
            error_message: None,
        });
        let existing = cfg_from(json!({"stopReason": "max_turns_exhausted", "custom": 42}));
        let out = merge_heartbeat_run_stop_metadata(Some(&existing), &m);
        assert_eq!(out.get("stopReason").unwrap(), "max_turns_exhausted");
        assert_eq!(out.get("custom").unwrap(), 42);
        assert_eq!(out.get("timeoutSource").unwrap(), "config");
    }

    #[test]
    fn merge_does_not_preserve_unrelated_stop_reason() {
        let cfg = cfg_from(json!({"timeoutSec": 30}));
        let m = build_heartbeat_run_stop_metadata(BuildStopMetadataInput {
            adapter_type: "claude_local",
            adapter_config: Some(&cfg),
            outcome: HeartbeatRunOutcome::Cancelled,
            error_code: None,
            error_message: Some("operator killed"),
        });
        let existing = cfg_from(json!({"stopReason": "completed"}));
        let out = merge_heartbeat_run_stop_metadata(Some(&existing), &m);
        assert_eq!(out.get("stopReason").unwrap(), "cancelled");
    }

    #[test]
    fn merge_writes_effective_timeout_ms_when_set() {
        let cfg = cfg_from(json!({"timeoutMs": 7500}));
        let m = build_heartbeat_run_stop_metadata(BuildStopMetadataInput {
            adapter_type: "http",
            adapter_config: Some(&cfg),
            outcome: HeartbeatRunOutcome::TimedOut,
            error_code: None,
            error_message: None,
        });
        let out = merge_heartbeat_run_stop_metadata(None, &m);
        assert_eq!(out.get("effectiveTimeoutMs").unwrap(), 7500);
        assert_eq!(out.get("effectiveTimeoutSec").unwrap(), 7);
        assert_eq!(out.get("timeoutFired").unwrap(), true);
    }

    #[test]
    fn merge_accepts_legacy_turn_limit_signal() {
        let cfg = cfg_from(json!({"timeoutSec": 0}));
        let m = build_heartbeat_run_stop_metadata(BuildStopMetadataInput {
            adapter_type: "claude_local",
            adapter_config: Some(&cfg),
            outcome: HeartbeatRunOutcome::Succeeded,
            error_code: None,
            error_message: None,
        });
        let existing = cfg_from(json!({"stopReason": "turn_limit_exhausted"}));
        let out = merge_heartbeat_run_stop_metadata(Some(&existing), &m);
        assert_eq!(out.get("stopReason").unwrap(), "max_turns_exhausted");
    }

    // -------- serde roundtrip --------

    #[test]
    fn outcome_serializes_snake_case() {
        let v = serde_json::to_value(HeartbeatRunOutcome::TimedOut).unwrap();
        assert_eq!(v, json!("timed_out"));
    }

    #[test]
    fn stop_reason_serializes_snake_case() {
        let v = serde_json::to_value(HeartbeatRunStopReason::BudgetPaused).unwrap();
        assert_eq!(v, json!("budget_paused"));
    }

    #[test]
    fn timeout_source_serializes_lowercase() {
        let v = serde_json::to_value(HeartbeatTimeoutSource::Config).unwrap();
        assert_eq!(v, json!("config"));
    }

    #[test]
    fn full_metadata_serializes_camel_case() {
        let cfg = cfg_from(json!({"timeoutMs": 5000}));
        let m = build_heartbeat_run_stop_metadata(BuildStopMetadataInput {
            adapter_type: "http",
            adapter_config: Some(&cfg),
            outcome: HeartbeatRunOutcome::TimedOut,
            error_code: None,
            error_message: None,
        });
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["effectiveTimeoutSec"], json!(5));
        assert_eq!(v["effectiveTimeoutMs"], json!(5000));
        assert_eq!(v["timeoutConfigured"], json!(true));
        assert_eq!(v["timeoutSource"], json!("config"));
        assert_eq!(v["stopReason"], json!("timeout"));
        assert_eq!(v["timeoutFired"], json!(true));
    }
}
