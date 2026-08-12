#![forbid(unsafe_code)]
//! hot-restart 的无 I/O 决策函数。

use crate::types::{HotRestartIntent, HotRestartIntentRun, ShutdownSignal};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;
use std::collections::HashSet;

/// 进程身份观测结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessObservation {
    /// PID 是否仍存活。
    pub alive: bool,
    /// 操作系统或 server 暴露的启动时间。
    pub started_at: Option<String>,
    /// replacement server 的旧进程身份。
    pub replacement: Option<ReplacementIdentity>,
}

/// replacement server 的旧进程信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplacementIdentity {
    /// 被 replacement 声称替换的 PID。
    pub previous_server_pid: i32,
    /// 被 replacement 声称替换的启动身份。
    pub previous_server_identity: Option<String>,
    /// 被 replacement 声称替换的启动时间。
    pub previous_server_started_at: Option<String>,
}

/// 可由文件内容恢复的进程身份错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProcessIdentityError {
    /// 存活进程缺少足够的身份证据。
    #[error("cannot establish process identity for live hot-restart target PID {pid}")]
    Unknown { pid: i32 },
}

/// 按 Node parseHotRestartIntent 规则解析 JSON。
pub fn parse_hot_restart_intent(value: &Value) -> Option<HotRestartIntent> {
    let object = value.as_object()?;
    if object.get("version").and_then(Value::as_u64) != Some(1) {
        return None;
    }
    let requested_at = non_empty_string(object.get("requestedAt")?)?;
    let previous_server_pid = positive_i32(object.get("previousServerPid")?)?;
    let mut intent = HotRestartIntent {
        version: 1,
        requested_at,
        previous_server_pid,
        previous_server_identity: optional_string(object.get("previousServerIdentity")),
        previous_server_started_at: optional_date(object.get("previousServerStartedAt")),
        previous_server_version: optional_string(object.get("previousServerVersion")),
        drain_required: object
            .get("drainRequired")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        requested_by_run_id: optional_string(object.get("requestedByRunId")),
        preflight_active_run_ids: string_array(object.get("preflightActiveRunIds")),
        shutdown_snapshot: None,
    };
    let Some(snapshot) = object.get("shutdownSnapshot").and_then(Value::as_object) else {
        return Some(intent);
    };
    let signal = match snapshot.get("signal").and_then(Value::as_str) {
        Some("SIGINT") => ShutdownSignal::SigInt,
        Some("SIGTERM") => ShutdownSignal::SigTerm,
        _ => return Some(intent),
    };
    let Some(captured_at) = snapshot
        .get("capturedAt")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    else {
        return Some(intent);
    };
    let active_runs = snapshot
        .get("activeRuns")
        .and_then(Value::as_array)
        .map(|runs| runs.iter().filter_map(parse_intent_run).collect())
        .unwrap_or_default();
    intent.shutdown_snapshot = Some(crate::types::ShutdownSnapshot {
        captured_at: captured_at.to_owned(),
        signal,
        active_runs,
    });
    Some(intent)
}

/// 按 Node parseRun 规则解析 snapshot run。
pub fn parse_intent_run(value: &Value) -> Option<HotRestartIntentRun> {
    let object = value.as_object()?;
    Some(HotRestartIntentRun {
        run_id: required_string(object.get("runId")?)?,
        company_id: required_string(object.get("companyId")?)?,
        agent_id: required_string(object.get("agentId")?)?,
        adapter_type: required_string(object.get("adapterType")?)?,
        status: required_string(object.get("status")?)?,
        process_pid: optional_positive_i32(object.get("processPid")),
        process_group_id: optional_positive_i32(object.get("processGroupId")),
        issue_id: optional_string(object.get("issueId")),
    })
}

/// 判断观测到的 PID 是否仍是 intent 所指向的原始 server。
pub fn is_observed_hot_restart_target_alive(
    intent: &HotRestartIntent,
    observation: &ProcessObservation,
) -> Result<bool, ProcessIdentityError> {
    if !observation.alive {
        return Ok(false);
    }

    if let Some(replacement) = &observation.replacement {
        if replacement.previous_server_pid == intent.previous_server_pid {
            if let (
                Some(replacement_identity),
                Some(intent_identity),
            ) = (
                &replacement.previous_server_identity,
                &intent.previous_server_identity,
            ) {
                return Ok(replacement_identity == intent_identity);
            }
            if let (Some(replacement_identity), None) = (
                &replacement.previous_server_identity,
                &intent.previous_server_identity,
            ) {
                if let (Some(replacement_started), Some(requested)) = (
                    parse_timestamp(replacement_identity),
                    parse_timestamp(&intent.requested_at),
                ) {
                    return Ok(replacement_started <= requested);
                }
            }
            if intent.previous_server_identity.is_none() {
                if let (Some(replacement_started), Some(requested)) = (
                    replacement
                        .previous_server_started_at
                        .as_deref()
                        .and_then(parse_timestamp),
                    parse_timestamp(&intent.requested_at),
                ) {
                    return Ok(replacement_started <= requested);
                }
            }
        }
    }

    let observed = observation.started_at.as_deref().and_then(parse_timestamp);
    let recorded = intent
        .previous_server_started_at
        .as_deref()
        .and_then(parse_timestamp);
    if let (Some(observed), Some(recorded)) = (observed, recorded) {
        return Ok(observed == recorded);
    }
    if let (Some(observed), Some(requested)) = (observed, parse_timestamp(&intent.requested_at)) {
        return Ok(observed <= requested);
    }
    Err(ProcessIdentityError::Unknown {
        pid: intent.previous_server_pid,
    })
}

/// 返回预检 active runs 中未出现在 shutdown snapshot 的 id。
pub fn find_missing_hot_restart_snapshot_run_ids(intent: &HotRestartIntent) -> Vec<String> {
    let snapshot_ids: HashSet<&str> = intent
        .shutdown_snapshot
        .as_ref()
        .map(|snapshot| {
            snapshot
                .active_runs
                .iter()
                .map(|run| run.run_id.as_str())
                .collect()
        })
        .unwrap_or_default();
    intent
        .preflight_active_run_ids
        .iter()
        .filter(|run_id| !snapshot_ids.contains(run_id.as_str()))
        .cloned()
        .collect()
}

/// 判断当前 server 是否应消费 intent。
pub fn should_honor_hot_restart_intent_for_process(intent: &HotRestartIntent, pid: i32) -> bool {
    !intent.drain_required && intent.previous_server_pid == pid
}

/// 将时间规范化成 Node toISOString 的毫秒精度。
pub fn normalize_date(value: &str) -> Option<String> {
    parse_timestamp(value).map(|date| date.to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn required_string(value: &Value) -> Option<String> {
    non_empty_string(value)
}

fn non_empty_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
}

fn optional_string(value: Option<&Value>) -> Option<String> {
    value.and_then(non_empty_string)
}

fn positive_i32(value: &Value) -> Option<i32> {
    value
        .as_i64()
        .filter(|value| *value > 0 && *value <= i32::MAX as i64)
        .map(|value| value as i32)
}

fn optional_positive_i32(value: Option<&Value>) -> Option<i32> {
    value.and_then(positive_i32)
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    let mut result = Vec::new();
    let Some(values) = value.and_then(Value::as_array) else {
        return result;
    };
    for value in values {
        if let Some(value) = non_empty_string(value) {
            if !result.contains(&value) {
                result.push(value);
            }
        }
    }
    result
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn optional_date(value: Option<&Value>) -> Option<String> {
    let value = optional_string(value)?;
    normalize_date(&value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_intent() -> HotRestartIntent {
        HotRestartIntent {
            version: 1,
            requested_at: "2026-08-01T01:05:00.000Z".into(),
            previous_server_pid: 123,
            previous_server_identity: Some("server-boot-a".into()),
            previous_server_started_at: Some("2026-08-01T01:00:00.000Z".into()),
            previous_server_version: Some("old".into()),
            drain_required: false,
            requested_by_run_id: None,
            preflight_active_run_ids: vec![],
            shutdown_snapshot: None,
        }
    }

    #[test]
    fn parser_filters_invalid_shape_and_deduplicates_ids() {
        let value = serde_json::json!({
            "version": 1,
            "requestedAt": "2026-08-01T01:05:00.000Z",
            "previousServerPid": 123,
            "preflightActiveRunIds": ["a", "a", 3, " "],
            "shutdownSnapshot": {
                "signal": "SIGTERM",
                "capturedAt": "now",
                "activeRuns": [{
                    "runId": "run", "companyId": "company", "agentId": "agent",
                    "adapterType": "codex_local", "status": "running", "processPid": 0
                }]
            }
        });
        let intent = parse_hot_restart_intent(&value).expect("valid intent");
        assert_eq!(intent.preflight_active_run_ids, vec!["a"]);
        assert_eq!(intent.shutdown_snapshot.as_ref().expect("snapshot").active_runs.len(), 1);
        assert_eq!(intent.shutdown_snapshot.as_ref().expect("snapshot").active_runs[0].process_pid, None);
    }

    #[test]
    fn parser_rejects_wrong_version_or_pid() {
        assert!(parse_hot_restart_intent(&serde_json::json!({"version": 2})).is_none());
        assert!(parse_hot_restart_intent(&serde_json::json!({"version": 1, "requestedAt": "x", "previousServerPid": 0})).is_none());
    }

    #[test]
    fn identity_rejects_recycled_pid_and_accepts_matching_identity() {
        let intent = base_intent();
        let recycled = ProcessObservation {
            alive: true,
            started_at: None,
            replacement: Some(ReplacementIdentity {
                previous_server_pid: 123,
                previous_server_identity: Some("server-boot-b".into()),
                previous_server_started_at: None,
            }),
        };
        assert!(!is_observed_hot_restart_target_alive(&intent, &recycled).expect("observation"));
        let matching = ProcessObservation {
            alive: true,
            started_at: None,
            replacement: Some(ReplacementIdentity {
                previous_server_pid: 123,
                previous_server_identity: Some("server-boot-a".into()),
                previous_server_started_at: None,
            }),
        };
        assert!(is_observed_hot_restart_target_alive(&intent, &matching).expect("observation"));
    }

    #[test]
    fn identity_requires_evidence_for_live_unknown_process() {
        let mut intent = base_intent();
        intent.previous_server_started_at = None;
        intent.previous_server_identity = None;
        assert!(matches!(
            is_observed_hot_restart_target_alive(
                &intent,
                &ProcessObservation { alive: true, started_at: None, replacement: None }
            ),
            Err(ProcessIdentityError::Unknown { .. })
        ));
    }

    #[test]
    fn missing_snapshot_ids_preserve_preflight_order() {
        let mut intent = base_intent();
        intent.preflight_active_run_ids = vec!["captured".into(), "missing".into(), "missing".into()];
        intent.shutdown_snapshot = Some(crate::types::ShutdownSnapshot {
            captured_at: "now".into(),
            signal: ShutdownSignal::SigTerm,
            active_runs: vec![HotRestartIntentRun {
                run_id: "captured".into(), company_id: "c".into(), agent_id: "a".into(),
                adapter_type: "x".into(), status: "running".into(), process_pid: None,
                process_group_id: None, issue_id: None,
            }],
        });
        assert_eq!(find_missing_hot_restart_snapshot_run_ids(&intent), vec!["missing", "missing"]);
    }
}
