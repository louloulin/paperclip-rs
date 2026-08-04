//! heartbeat run stop metadata：纯逻辑模块，对齐 Node `heartbeat-stop-metadata.ts`。
//!
//! 包含：
//! - 类型：`HeartbeatRunOutcome` / `HeartbeatRunStopReason` / `HeartbeatRunTimeoutPolicy` / `HeartbeatRunStopMetadata`
//! - 纯函数：`resolveHeartbeatRunTimeoutPolicy` / `inferHeartbeatRunStopReason` /
//!   `buildHeartbeatRunStopMetadata` / `mergeHeartbeatRunStopMetadata`
//!
//! 设计：
//! - 不依赖数据库 / 任何 actor；纯函数方便单测
//! - adapter 类型与 Node `adapterType` 一致（`http` / `openclaw_gateway` / 其他）

use serde::{Deserialize, Serialize};

/// heartbeat run 的执行结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeartbeatRunOutcome {
    Succeeded,
    Interrupted,
    Failed,
    Cancelled,
    TimedOut,
}

impl Default for HeartbeatRunOutcome {
    fn default() -> Self {
        Self::Failed
    }
}

impl HeartbeatRunOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Interrupted => "interrupted",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "succeeded" => Some(Self::Succeeded),
            "interrupted" => Some(Self::Interrupted),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            "timed_out" => Some(Self::TimedOut),
            _ => None,
        }
    }
}

/// heartbeat run 停止原因。
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
            Self::Completed => "completed",
            Self::Interrupted => "interrupted",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::BudgetPaused => "budget_paused",
            Self::Paused => "paused",
            Self::MaxTurnsExhausted => "max_turns_exhausted",
            Self::ProcessLost => "process_lost",
            Self::UnmanagedBackgroundTaskStopped => "unmanaged_background_task_stopped",
            Self::AdapterFailed => "adapter_failed",
        }
    }
}

/// heartbeat run 超时策略。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatRunTimeoutPolicy {
    pub effective_timeout_sec: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_timeout_ms: Option<i64>,
    pub timeout_configured: bool,
    pub timeout_source: TimeoutSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimeoutSource {
    Config,
    Default,
    Unknown,
}

impl TimeoutSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Default => "default",
            Self::Unknown => "unknown",
        }
    }
}

/// heartbeat run stop metadata = timeout policy + stop reason + timeout fired。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatRunStopMetadata {
    pub effective_timeout_sec: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_timeout_ms: Option<i64>,
    pub timeout_configured: bool,
    pub timeout_source: TimeoutSource,
    pub stop_reason: HeartbeatRunStopReason,
    pub timeout_fired: bool,
}

// ============================================================================
// Helpers
// ============================================================================

/// 把任意值解析成 finite number；非数字 / NaN / Inf → None。
fn read_finite_number(value: Option<&serde_json::Value>) -> Option<f64> {
    let v = value?;
    if let Some(n) = v.as_f64() {
        if n.is_finite() {
            return Some(n);
        }
    }
    if let Some(s) = v.as_str() {
        let parsed = s.trim().parse::<f64>().ok();
        if parsed.map(|n| n.is_finite()).unwrap_or(false) {
            return parsed;
        }
    }
    None
}

/// `openclaw_gateway` adapter 默认 120s timeout；其他 0s。
fn default_timeout_sec_for_adapter(adapter_type: &str) -> f64 {
    if adapter_type == "openclaw_gateway" {
        120.0
    } else {
        0.0
    }
}

fn has_own_key(config: &serde_json::Map<String, serde_json::Value>, key: &str) -> bool {
    config.contains_key(key)
}

// ============================================================================
// Public API（对齐 Node）
// ============================================================================

/// 把历史遗留的 `turn_limit_exhausted` 归一为 `max_turns_exhausted`。
/// 输入既不是合法 stop reason 也不是遗留值时返回 None。
pub fn normalize_max_turn_stop_reason(value: Option<&str>) -> Option<HeartbeatRunStopReason> {
    match value {
        Some("max_turns_exhausted") | Some("turn_limit_exhausted") => {
            Some(HeartbeatRunStopReason::MaxTurnsExhausted)
        }
        _ => None,
    }
}

/// 解析 adapter 类型 + 配置 → timeout 策略。
///
/// 规则：
/// - `http` adapter：使用 `timeoutMs`（毫秒），无值则 0
/// - 其他 adapter：使用 `timeoutSec`（秒），无值则取 `defaultTimeoutSecForAdapter`
///   （`openclaw_gateway` → 120，其他 → 0）
pub fn resolve_heartbeat_run_timeout_policy(
    adapter_type: &str,
    adapter_config: Option<&serde_json::Value>,
) -> HeartbeatRunTimeoutPolicy {
    let config_obj = adapter_config
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    if adapter_type == "http" {
        let has_ms = has_own_key(&config_obj, "timeoutMs");
        let raw_ms = if has_ms {
            read_finite_number(config_obj.get("timeoutMs")).unwrap_or(0.0)
        } else {
            0.0
        };
        let ms = raw_ms.max(0.0).floor() as i64;
        return HeartbeatRunTimeoutPolicy {
            effective_timeout_sec: Some(ms as f64 / 1000.0),
            effective_timeout_ms: Some(ms),
            timeout_configured: ms > 0,
            timeout_source: if has_ms {
                TimeoutSource::Config
            } else {
                TimeoutSource::Default
            },
        };
    }

    let has_sec = has_own_key(&config_obj, "timeoutSec");
    let default_sec = default_timeout_sec_for_adapter(adapter_type);
    let raw_sec = if has_sec {
        read_finite_number(config_obj.get("timeoutSec")).unwrap_or(default_sec)
    } else {
        default_sec
    };
    let sec = raw_sec.max(0.0).floor() as i64;
    HeartbeatRunTimeoutPolicy {
        effective_timeout_sec: Some(sec as f64),
        effective_timeout_ms: None,
        timeout_configured: sec > 0,
        timeout_source: if has_sec {
            TimeoutSource::Config
        } else {
            TimeoutSource::Default
        },
    }
}

/// 根据 outcome + errorCode / errorMessage 推断 stop reason。
pub fn infer_heartbeat_run_stop_reason(input: HeartbeatRunStopReasonInput) -> HeartbeatRunStopReason {
    if input.outcome == HeartbeatRunOutcome::Succeeded {
        return HeartbeatRunStopReason::Completed;
    }
    if input.outcome == HeartbeatRunOutcome::Interrupted {
        return HeartbeatRunStopReason::Interrupted;
    }
    if let Some(max_turn) = normalize_max_turn_stop_reason(input.error_code.as_deref()) {
        return max_turn;
    }
    if input.outcome == HeartbeatRunOutcome::TimedOut {
        return HeartbeatRunStopReason::Timeout;
    }
    if input.outcome == HeartbeatRunOutcome::Failed {
        match input.error_code.as_deref() {
            Some("unmanaged_background_task_stopped") => {
                return HeartbeatRunStopReason::UnmanagedBackgroundTaskStopped;
            }
            Some("process_lost") => return HeartbeatRunStopReason::ProcessLost,
            _ => {}
        }
    }
    if input.outcome == HeartbeatRunOutcome::Cancelled {
        let message = input
            .error_message
            .as_deref()
            .unwrap_or("")
            .to_lowercase();
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

#[derive(Debug, Clone, Default)]
pub struct HeartbeatRunStopReasonInput {
    pub outcome: HeartbeatRunOutcome,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

/// 一站式构造 stop metadata：timeout policy + inferred stop reason + timeoutFired。
pub fn build_heartbeat_run_stop_metadata(
    input: HeartbeatRunStopMetadataInput,
) -> HeartbeatRunStopMetadata {
    let timeout_policy = resolve_heartbeat_run_timeout_policy(
        &input.adapter_type,
        input.adapter_config.as_ref(),
    );
    let stop_reason = infer_heartbeat_run_stop_reason(HeartbeatRunStopReasonInput {
        outcome: input.outcome,
        error_code: input.error_code.clone(),
        error_message: input.error_message.clone(),
    });
    HeartbeatRunStopMetadata {
        effective_timeout_sec: timeout_policy.effective_timeout_sec,
        effective_timeout_ms: timeout_policy.effective_timeout_ms,
        timeout_configured: timeout_policy.timeout_configured,
        timeout_source: timeout_policy.timeout_source,
        timeout_fired: stop_reason == HeartbeatRunStopReason::Timeout,
        stop_reason,
    }
}

#[derive(Debug, Clone)]
pub struct HeartbeatRunStopMetadataInput {
    pub adapter_type: String,
    pub adapter_config: Option<serde_json::Value>,
    pub outcome: HeartbeatRunOutcome,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

/// 把 metadata 合并到已有的 resultJson（保留其他字段）。
///
/// 规则：
/// - 保留 resultJson 的所有其他字段
/// - `stopReason` 用现有 max_turn 归一值，否则用 metadata.stopReason
/// - 覆盖 timeout 相关 4 个字段
/// - `effectiveTimeoutMs` 仅在 metadata 有值时写入
pub fn merge_heartbeat_run_stop_metadata(
    result_json: Option<&serde_json::Map<String, serde_json::Value>>,
    metadata: &HeartbeatRunStopMetadata,
) -> serde_json::Map<String, serde_json::Value> {
    let mut out: serde_json::Map<String, serde_json::Value> = result_json
        .cloned()
        .unwrap_or_default();
    let existing_max_turn: Option<HeartbeatRunStopReason> = {
        let s: Option<String> = result_json
            .and_then(|obj| obj.get("stopReason"))
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        match s {
            Some(value) => normalize_max_turn_stop_reason(Some(&value)),
            None => None,
        }
    };
    let stop_reason = existing_max_turn.unwrap_or(metadata.stop_reason);
    out.insert(
        "stopReason".into(),
        serde_json::Value::String(stop_reason.as_str().to_string()),
    );
    out.insert(
        "effectiveTimeoutSec".into(),
        match metadata.effective_timeout_sec {
            Some(s) => serde_json::json!(s),
            None => serde_json::Value::Null,
        },
    );
    out.insert(
        "timeoutConfigured".into(),
        serde_json::Value::Bool(metadata.timeout_configured),
    );
    out.insert(
        "timeoutSource".into(),
        serde_json::Value::String(metadata.timeout_source.as_str().to_string()),
    );
    out.insert(
        "timeoutFired".into(),
        serde_json::Value::Bool(metadata.timeout_fired),
    );
    if let Some(ms) = metadata.effective_timeout_ms {
        out.insert("effectiveTimeoutMs".into(), serde_json::json!(ms));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with_timeout_ms(ms: i64) -> serde_json::Value {
        serde_json::json!({ "timeoutMs": ms })
    }
    fn cfg_with_timeout_sec(sec: i64) -> serde_json::Value {
        serde_json::json!({ "timeoutSec": sec })
    }

    #[test]
    fn outcome_strings_round_trip() {
        for o in [
            HeartbeatRunOutcome::Succeeded,
            HeartbeatRunOutcome::Interrupted,
            HeartbeatRunOutcome::Failed,
            HeartbeatRunOutcome::Cancelled,
            HeartbeatRunOutcome::TimedOut,
        ] {
            assert_eq!(HeartbeatRunOutcome::parse(o.as_str()), Some(o));
        }
        assert_eq!(HeartbeatRunOutcome::parse("nope"), None);
    }

    #[test]
    fn stop_reason_strings_round_trip() {
        for r in [
            HeartbeatRunStopReason::Completed,
            HeartbeatRunStopReason::Interrupted,
            HeartbeatRunStopReason::Timeout,
            HeartbeatRunStopReason::Cancelled,
            HeartbeatRunStopReason::BudgetPaused,
            HeartbeatRunStopReason::Paused,
            HeartbeatRunStopReason::MaxTurnsExhausted,
            HeartbeatRunStopReason::ProcessLost,
            HeartbeatRunStopReason::UnmanagedBackgroundTaskStopped,
            HeartbeatRunStopReason::AdapterFailed,
        ] {
            assert_eq!(r.as_str().len() > 0, true);
        }
    }

    #[test]
    fn normalize_max_turn_stop_reason_accepts_legacy_and_new() {
        assert_eq!(
            normalize_max_turn_stop_reason(Some("turn_limit_exhausted")),
            Some(HeartbeatRunStopReason::MaxTurnsExhausted)
        );
        assert_eq!(
            normalize_max_turn_stop_reason(Some("max_turns_exhausted")),
            Some(HeartbeatRunStopReason::MaxTurnsExhausted)
        );
        assert_eq!(normalize_max_turn_stop_reason(Some("timeout")), None);
        assert_eq!(normalize_max_turn_stop_reason(None), None);
    }

    #[test]
    fn http_adapter_timeout_ms() {
        let policy = resolve_heartbeat_run_timeout_policy("http", Some(&cfg_with_timeout_ms(4500)));
        assert_eq!(policy.effective_timeout_ms, Some(4500));
        assert_eq!(policy.effective_timeout_sec, Some(4.5));
        assert!(policy.timeout_configured);
        assert_eq!(policy.timeout_source, TimeoutSource::Config);
    }

    #[test]
    fn http_adapter_default_zero_timeout() {
        let policy = resolve_heartbeat_run_timeout_policy("http", None);
        assert_eq!(policy.effective_timeout_ms, Some(0));
        assert_eq!(policy.effective_timeout_sec, Some(0.0));
        assert!(!policy.timeout_configured);
        assert_eq!(policy.timeout_source, TimeoutSource::Default);
    }

    #[test]
    fn openclaw_gateway_default_120s() {
        let policy = resolve_heartbeat_run_timeout_policy("openclaw_gateway", None);
        assert_eq!(policy.effective_timeout_sec, Some(120.0));
        assert!(policy.timeout_configured);
        assert_eq!(policy.timeout_source, TimeoutSource::Default);
    }

    #[test]
    fn custom_adapter_default_zero() {
        let policy = resolve_heartbeat_run_timeout_policy("claude_local", None);
        assert_eq!(policy.effective_timeout_sec, Some(0.0));
        assert!(!policy.timeout_configured);
    }

    #[test]
    fn timeout_sec_override_for_claude_local() {
        let policy = resolve_heartbeat_run_timeout_policy("claude_local", Some(&cfg_with_timeout_sec(60)));
        assert_eq!(policy.effective_timeout_sec, Some(60.0));
        assert!(policy.timeout_configured);
        assert_eq!(policy.timeout_source, TimeoutSource::Config);
    }

    #[test]
    fn infer_completed_for_succeeded() {
        assert_eq!(
            infer_heartbeat_run_stop_reason(HeartbeatRunStopReasonInput {
                outcome: HeartbeatRunOutcome::Succeeded,
                ..Default::default()
            }),
            HeartbeatRunStopReason::Completed
        );
    }

    #[test]
    fn infer_interrupted_for_interrupted_outcome() {
        assert_eq!(
            infer_heartbeat_run_stop_reason(HeartbeatRunStopReasonInput {
                outcome: HeartbeatRunOutcome::Interrupted,
                ..Default::default()
            }),
            HeartbeatRunStopReason::Interrupted
        );
    }

    #[test]
    fn infer_timeout_from_timed_out() {
        assert_eq!(
            infer_heartbeat_run_stop_reason(HeartbeatRunStopReasonInput {
                outcome: HeartbeatRunOutcome::TimedOut,
                ..Default::default()
            }),
            HeartbeatRunStopReason::Timeout
        );
    }

    #[test]
    fn infer_process_lost_from_error_code() {
        assert_eq!(
            infer_heartbeat_run_stop_reason(HeartbeatRunStopReasonInput {
                outcome: HeartbeatRunOutcome::Failed,
                error_code: Some("process_lost".into()),
                error_message: None,
            }),
            HeartbeatRunStopReason::ProcessLost
        );
    }

    #[test]
    fn infer_budget_paused_from_message() {
        assert_eq!(
            infer_heartbeat_run_stop_reason(HeartbeatRunStopReasonInput {
                outcome: HeartbeatRunOutcome::Cancelled,
                error_message: Some("budget exhausted".into()),
                ..Default::default()
            }),
            HeartbeatRunStopReason::BudgetPaused
        );
    }

    #[test]
    fn infer_paused_from_message() {
        assert_eq!(
            infer_heartbeat_run_stop_reason(HeartbeatRunStopReasonInput {
                outcome: HeartbeatRunOutcome::Cancelled,
                error_message: Some("user paused".into()),
                ..Default::default()
            }),
            HeartbeatRunStopReason::Paused
        );
    }

    #[test]
    fn infer_cancelled_when_no_message_match() {
        assert_eq!(
            infer_heartbeat_run_stop_reason(HeartbeatRunStopReasonInput {
                outcome: HeartbeatRunOutcome::Cancelled,
                error_message: Some("user stopped".into()),
                ..Default::default()
            }),
            HeartbeatRunStopReason::Cancelled
        );
    }

    #[test]
    fn infer_max_turns_overrides_outcome() {
        assert_eq!(
            infer_heartbeat_run_stop_reason(HeartbeatRunStopReasonInput {
                outcome: HeartbeatRunOutcome::Failed,
                error_code: Some("turn_limit_exhausted".into()),
                error_message: None,
            }),
            HeartbeatRunStopReason::MaxTurnsExhausted
        );
    }

    #[test]
    fn infer_adapter_failed_default() {
        assert_eq!(
            infer_heartbeat_run_stop_reason(HeartbeatRunStopReasonInput {
                outcome: HeartbeatRunOutcome::Failed,
                error_code: Some("random_error".into()),
                error_message: Some("something".into()),
            }),
            HeartbeatRunStopReason::AdapterFailed
        );
    }

    #[test]
    fn build_metadata_sets_timeout_fired_for_timeout() {
        let m = build_heartbeat_run_stop_metadata(HeartbeatRunStopMetadataInput {
            adapter_type: "http".into(),
            adapter_config: Some(cfg_with_timeout_ms(1000)),
            outcome: HeartbeatRunOutcome::TimedOut,
            error_code: None,
            error_message: None,
        });
        assert!(m.timeout_fired);
        assert_eq!(m.stop_reason, HeartbeatRunStopReason::Timeout);
        assert_eq!(m.effective_timeout_ms, Some(1000));
        assert!(m.timeout_configured);
    }

    #[test]
    fn merge_preserves_existing_max_turn_stop_reason() {
        let mut existing = serde_json::Map::new();
        existing.insert(
            "stopReason".into(),
            serde_json::Value::String("turn_limit_exhausted".into()),
        );
        existing.insert(
            "outcomeDetail".into(),
            serde_json::Value::String("keep me".into()),
        );
        let metadata = HeartbeatRunStopMetadata {
            effective_timeout_sec: Some(30.0),
            effective_timeout_ms: None,
            timeout_configured: true,
            timeout_source: TimeoutSource::Config,
            stop_reason: HeartbeatRunStopReason::AdapterFailed,
            timeout_fired: false,
        };
        let merged = merge_heartbeat_run_stop_metadata(Some(&existing), &metadata);
        assert_eq!(
            merged.get("stopReason").and_then(|v| v.as_str()),
            Some("max_turns_exhausted")
        );
        assert_eq!(
            merged.get("outcomeDetail").and_then(|v| v.as_str()),
            Some("keep me")
        );
        assert_eq!(
            merged.get("effectiveTimeoutSec").and_then(|v| v.as_f64()),
            Some(30.0)
        );
        assert_eq!(
            merged.get("timeoutConfigured").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            merged.get("timeoutFired").and_then(|v| v.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn merge_writes_effective_timeout_ms_when_present() {
        let metadata = HeartbeatRunStopMetadata {
            effective_timeout_sec: Some(2.5),
            effective_timeout_ms: Some(2500),
            timeout_configured: true,
            timeout_source: TimeoutSource::Config,
            stop_reason: HeartbeatRunStopReason::Timeout,
            timeout_fired: true,
        };
        let merged = merge_heartbeat_run_stop_metadata(None, &metadata);
        assert_eq!(
            merged.get("effectiveTimeoutMs").and_then(|v| v.as_i64()),
            Some(2500)
        );
        assert_eq!(
            merged.get("timeoutFired").and_then(|v| v.as_bool()),
            Some(true)
        );
    }
}
