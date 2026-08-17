#![forbid(unsafe_code)]

//! Decision lifecycle pure helpers — 1:1 port of
//! paperclip/server/src/services/decisions.ts::DecisionService lifecycle
//! methods (resumeDecision, deliverContinuation, sweepExpired, decide replay
//! guards).
//!
//! R744: 把 sweep / resume / continuation 的核心判断拆为纯函数。
//! - 不依赖 DB / signing / wakeup，仅消费 row 字段
//! - 与 `bundle_validation_pure` / `wakeup_validation_pure` 同级
//! - 行为对齐 Node：
//!   - `should_resume_decision` —— execution_status == "running" 才 resume
//!   - `is_pending_continuation` —— continuationPolicy == "wake_origin_agent"
//!     && metadata.continuationPending == true
//!   - `parse_sweep_batch_size` / `parse_recovery_grace_ms` ——
//!     process.env 风格配置解析，与 Node `Number(...)` + `Math.max(1, ...)`
//!     + fallback 行为对齐
//!   - `expiration_reason_for` —— "target_gone" | "ttl"
//!   - `continuation_outcome_for` —— status + execution_status → outcome
//!   - `next_target_sweep_cursor` —— id-based cursor 推进
//!   - `merge_continuation_metadata` —— deliveredAt 写入 metadata
//!   - `merge_expired_metadata` —— expiredReason + 可选 continuationPending
//!   - `validate_decide_replay` —— idempotency + 同人校验

use chrono::{DateTime, Utc};
use serde_json::Value;

/// 一个决策是否需要重新启动 effects（resume 入口判断）。
///
/// 与 Node `resumeDecision` 对齐：当 execution_status == "running" 时调用
/// `runEffects` 重新跑；否则只返回 outcome。
pub fn should_resume_decision(execution_status: Option<&str>) -> bool {
    matches!(execution_status, Some(s) if s == "running")
}

/// 一个 open 决策是否到期（TTL 过期）。
pub fn is_decision_expired(status: &str, expires_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    status == "open" && expires_at <= now
}

/// 从 metadata JSON 中读 `continuationPending` flag。
pub fn extract_continuation_pending(metadata: &Value) -> bool {
    metadata
        .as_object()
        .and_then(|m| m.get("continuationPending"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// 一个决策是否处于待投递的 continuation 状态。
pub fn is_pending_continuation(
    continuation_policy: &str,
    metadata: &Value,
) -> bool {
    continuation_policy == "wake_origin_agent" && extract_continuation_pending(metadata)
}

/// 一个决策是否需要投递 continuation（含已 decided/expired/cancelled 的最终态）。
///
/// Node 行为：continuationPolicy == "wake_origin_agent" 且
///   metadata.continuationPending == true 时投递，
///   但只在决策进入终态时调用。
pub fn should_dispatch_continuation(
    decision_status: &str,
    execution_status: Option<&str>,
    continuation_policy: &str,
    metadata: &Value,
) -> bool {
    if continuation_policy != "wake_origin_agent" {
        return false;
    }
    if !extract_continuation_pending(metadata) {
        return false;
    }
    // 终态：decided+succeeded/partial/failed, expired, cancelled
    matches!(
        (decision_status, execution_status),
        ("decided", Some("succeeded") | Some("partial") | Some("failed"))
            | ("expired", _)
            | ("cancelled", _)
    )
}

/// 把 status + execution_status 映射到 continuation outcome。
pub fn continuation_outcome_for(
    decision_status: &str,
    _execution_status: Option<&str>,
) -> &'static str {
    match decision_status {
        "decided" => "decided",
        "expired" => "expired",
        "cancelled" => "cancelled",
        // dismissed 不投递 continuation
        _ => "cancelled",
    }
}

/// 解析 sweep batch size（与 Node `Number.isFinite` + `Math.max(1, Math.trunc())` 对齐）。
pub fn parse_sweep_batch_size(raw: Option<&str>, default: usize) -> usize {
    let Some(raw) = raw else {
        return default;
    };
    match raw.trim().parse::<f64>() {
        Ok(n) if n.is_finite() && n >= 1.0 => n as usize,
        _ => default,
    }
}

/// 解析 recovery grace ms（与 Node `>= 0` + `isFinite` 对齐）。
pub fn parse_recovery_grace_ms(raw: Option<&str>, default: u64) -> u64 {
    let Some(raw) = raw else {
        return default;
    };
    match raw.trim().parse::<f64>() {
        Ok(n) if n.is_finite() && n >= 0.0 => n as u64,
        _ => default,
    }
}

/// 过期原因。
///
/// Node 行为：strict target 都已删除 / 任意一个 cancelled → "target_gone"；
/// 否则 expires_at <= now → "ttl"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpirationReason {
    TargetGone,
    Ttl,
}

pub fn expiration_reason_for(
    has_strict_targets: bool,
    strict_targets_all_present: bool,
    any_strict_target_cancelled: bool,
    is_ttl: bool,
) -> Option<ExpirationReason> {
    if has_strict_targets && (!strict_targets_all_present || any_strict_target_cancelled) {
        return Some(ExpirationReason::TargetGone);
    }
    if is_ttl {
        return Some(ExpirationReason::Ttl);
    }
    None
}

impl ExpirationReason {
    pub fn as_str(self) -> &'static str {
        match self {
            ExpirationReason::TargetGone => "target_gone",
            ExpirationReason::Ttl => "ttl",
        }
    }
}

/// 决定下一轮 target_sweep cursor（id-based pagination）。
///
/// Node 行为：当本批取到恰好 `expected_count` 条且最后一条 id 存在时，
/// cursor 推进到该 id；否则 cursor 重置为 null（已扫完一轮）。
pub fn next_target_sweep_cursor(
    rows: &[(String, String)],
    expected_count: usize,
) -> Option<String> {
    if rows.len() == expected_count && !rows.is_empty() {
        rows.last().map(|(_, id)| id.clone())
    } else {
        None
    }
}

/// 判断一条 open 决策是否落在 target_sweep 当前 cursor 之后。
pub fn is_after_cursor(decision_id: &str, cursor: Option<&str>) -> bool {
    match cursor {
        Some(c) => decision_id > c,
        None => true,
    }
}

/// TTL 集合 + target 集合按 id 去重合并（Node `new Map(...).values()` 行为）。
///
/// 返回按 id 升序的 ids 列表（保留输入顺序：ttl 在前）。
pub fn merge_unique_ids(ttl_ids: &[String], target_ids: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(ttl_ids.len() + target_ids.len());
    for id in ttl_ids.iter().chain(target_ids.iter()) {
        if seen.insert(id.clone()) {
            out.push(id.clone());
        }
    }
    out
}

/// 合并 deliverContinuation metadata（写入 continuationDeliveredAt + 清 continuationPending）。
pub fn merge_continuation_metadata(
    metadata: Value,
    delivered_at: DateTime<Utc>,
) -> Value {
    let mut obj = metadata
        .as_object()
        .cloned()
        .unwrap_or_default();
    obj.insert("continuationPending".into(), Value::Bool(false));
    obj.insert(
        "continuationDeliveredAt".into(),
        Value::String(delivered_at.to_rfc3339()),
    );
    Value::Object(obj)
}

/// 合并 expired metadata（写入 expiredReason + 可选 continuationPending）。
pub fn merge_expired_metadata(
    metadata: Value,
    reason: ExpirationReason,
    continuation_pending: bool,
) -> Value {
    let mut obj = metadata
        .as_object()
        .cloned()
        .unwrap_or_default();
    obj.insert(
        "expiredReason".into(),
        Value::String(reason.as_str().to_string()),
    );
    if continuation_pending {
        obj.insert("continuationPending".into(), Value::Bool(true));
    }
    Value::Object(obj)
}

/// 合并 decided metadata（写入 decideIdempotencyKey + 可选 dismissed）。
pub fn merge_decided_metadata(
    metadata: Value,
    decide_idempotency_key: Option<&str>,
    dismissed: bool,
    dismiss_reason: Option<&str>,
    continuation_pending: bool,
) -> Value {
    let mut obj = metadata
        .as_object()
        .cloned()
        .unwrap_or_default();
    if let Some(key) = decide_idempotency_key {
        obj.insert(
            "decideIdempotencyKey".into(),
            Value::String(key.to_string()),
        );
    } else {
        obj.insert("decideIdempotencyKey".into(), Value::Null);
    }
    if continuation_pending {
        obj.insert("continuationPending".into(), Value::Bool(true));
    }
    if dismissed {
        obj.insert("dismissed".into(), Value::Bool(true));
        if let Some(r) = dismiss_reason {
            obj.insert("dismissReason".into(), Value::String(r.to_string()));
        } else {
            obj.insert("dismissReason".into(), Value::Null);
        }
    }
    Value::Object(obj)
}

/// Decide replay 校验结果。
///
/// Node 行为：
/// - 已 decided 且 idempotencyKey 命中 + 决定人相同 → 允许 replay
/// - 已 decided 且 chosenOptionId/inputValues 一致 + 决定人相同 → 允许 replay
/// - 否则按 status 报错
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecideReplay {
    /// idempotencyKey 命中且同人 → replay
    IdempotentReplay,
    /// chosenOptionId + inputValues 一致 + 同人 → replay
    OptionReplay,
    /// 不在 replay 路径
    NotReplay,
}

pub fn detect_decide_replay(
    current_status: &str,
    current_chosen_option_id: Option<&str>,
    current_decide_idempotency_key: Option<&str>,
    current_decided_by_user_id: Option<&str>,
    new_idempotency_key: Option<&str>,
    new_chosen_option_id: &str,
    new_input_values: Option<&Value>,
    current_input_values: Option<&Value>,
    new_decided_by_user_id: &str,
) -> DecideReplay {
    if current_status != "decided" {
        return DecideReplay::NotReplay;
    }
    if current_decided_by_user_id.unwrap_or("") != new_decided_by_user_id {
        return DecideReplay::NotReplay;
    }
    if let (Some(cur_key), Some(new_key)) = (
        current_decide_idempotency_key,
        new_idempotency_key,
    ) {
        if !cur_key.is_empty() && cur_key == new_key {
            return DecideReplay::IdempotentReplay;
        }
    }
    if current_chosen_option_id == Some(new_chosen_option_id)
        && same_input_values_for_replay(current_input_values, new_input_values)
    {
        return DecideReplay::OptionReplay;
    }
    DecideReplay::NotReplay
}

fn same_input_values_for_replay(
    left: Option<&Value>,
    right: Option<&Value>,
) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(l), Some(r)) => l == r,
        // 缺失视为空对象
        (None, Some(r)) => r.as_object().map(|o| o.is_empty()).unwrap_or(true),
        (Some(l), None) => l.as_object().map(|o| o.is_empty()).unwrap_or(true),
    }
}

/// 输入字段校验（与 Node decide 内联校验对齐）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputValidationError {
    Required { id: String },
    TooLong { id: String, max_length: i64, actual: usize },
}

pub fn validate_decision_inputs(
    fields: &Value,
    values: &Value,
) -> Result<(), InputValidationError> {
    let arr = match fields.as_array() {
        Some(a) => a,
        None => return Ok(()),
    };
    let values_obj = values.as_object();
    for field in arr {
        let id = field
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if id.is_empty() {
            continue;
        }
        let value = values_obj
            .and_then(|o| o.get(&id))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let required = field
            .get("required")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if required && value.trim().is_empty() {
            return Err(InputValidationError::Required { id });
        }
        if let Some(max_length) = field.get("maxLength").and_then(|v| v.as_i64()) {
            if value.len() > max_length as usize {
                return Err(InputValidationError::TooLong {
                    id,
                    max_length,
                    actual: value.len(),
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(year: i32, month: u32, day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, 0, 0, 0).unwrap()
    }

    fn meta_with_pending(pending: bool) -> Value {
        serde_json::json!({ "continuationPending": pending })
    }

    #[test]
    fn r744_should_resume_running() {
        assert!(should_resume_decision(Some("running")));
    }

    #[test]
    fn r744_should_not_resume_when_succeeded() {
        assert!(!should_resume_decision(Some("succeeded")));
        assert!(!should_resume_decision(None));
    }

    #[test]
    fn r744_is_decision_expired_open_and_past() {
        assert!(is_decision_expired("open", ts(2026, 1, 1), ts(2026, 1, 2)));
        assert!(is_decision_expired("open", ts(2026, 1, 1), ts(2026, 1, 1)));
    }

    #[test]
    fn r744_is_decision_expired_false_when_future_or_closed() {
        assert!(!is_decision_expired("open", ts(2026, 1, 2), ts(2026, 1, 1)));
        assert!(!is_decision_expired("decided", ts(2026, 1, 1), ts(2026, 1, 2)));
    }

    #[test]
    fn r744_extract_continuation_pending() {
        assert!(extract_continuation_pending(&meta_with_pending(true)));
        assert!(!extract_continuation_pending(&meta_with_pending(false)));
        assert!(!extract_continuation_pending(&serde_json::json!({})));
        assert!(!extract_continuation_pending(&serde_json::json!(null)));
    }

    #[test]
    fn r744_is_pending_continuation() {
        assert!(is_pending_continuation(
            "wake_origin_agent",
            &meta_with_pending(true)
        ));
        assert!(!is_pending_continuation(
            "wake_origin_agent",
            &meta_with_pending(false)
        ));
        assert!(!is_pending_continuation("stop", &meta_with_pending(true)));
    }

    #[test]
    fn r744_should_dispatch_continuation_terminal_states() {
        let pending = meta_with_pending(true);
        assert!(should_dispatch_continuation(
            "decided",
            Some("succeeded"),
            "wake_origin_agent",
            &pending
        ));
        assert!(should_dispatch_continuation(
            "decided",
            Some("partial"),
            "wake_origin_agent",
            &pending
        ));
        assert!(should_dispatch_continuation(
            "decided",
            Some("failed"),
            "wake_origin_agent",
            &pending
        ));
        assert!(should_dispatch_continuation(
            "expired",
            None,
            "wake_origin_agent",
            &pending
        ));
        assert!(should_dispatch_continuation(
            "cancelled",
            None,
            "wake_origin_agent",
            &pending
        ));
    }

    #[test]
    fn r744_should_dispatch_continuation_blocks_open() {
        let pending = meta_with_pending(true);
        assert!(!should_dispatch_continuation(
            "open",
            None,
            "wake_origin_agent",
            &pending
        ));
        assert!(!should_dispatch_continuation(
            "decided",
            Some("running"),
            "wake_origin_agent",
            &pending
        ));
    }

    #[test]
    fn r744_should_dispatch_continuation_blocks_wrong_policy() {
        let pending = meta_with_pending(true);
        assert!(!should_dispatch_continuation(
            "decided",
            Some("succeeded"),
            "stop",
            &pending
        ));
    }

    #[test]
    fn r744_should_dispatch_continuation_blocks_no_pending() {
        let m = meta_with_pending(false);
        assert!(!should_dispatch_continuation(
            "decided",
            Some("succeeded"),
            "wake_origin_agent",
            &m
        ));
    }

    #[test]
    fn r744_continuation_outcome_for() {
        assert_eq!(continuation_outcome_for("decided", None), "decided");
        assert_eq!(continuation_outcome_for("expired", None), "expired");
        assert_eq!(continuation_outcome_for("cancelled", None), "cancelled");
        assert_eq!(continuation_outcome_for("dismissed", None), "cancelled");
        assert_eq!(continuation_outcome_for("open", None), "cancelled");
    }

    #[test]
    fn r744_parse_sweep_batch_size_defaults() {
        assert_eq!(parse_sweep_batch_size(None, 100), 100);
        assert_eq!(parse_sweep_batch_size(Some(""), 100), 100);
        assert_eq!(parse_sweep_batch_size(Some("not a number"), 100), 100);
    }

    #[test]
    fn r744_parse_sweep_batch_size_validates() {
        assert_eq!(parse_sweep_batch_size(Some("0"), 100), 100);
        assert_eq!(parse_sweep_batch_size(Some("-5"), 100), 100);
        assert_eq!(parse_sweep_batch_size(Some("3.7"), 100), 3);
        assert_eq!(parse_sweep_batch_size(Some("  42  "), 100), 42);
    }

    #[test]
    fn r744_parse_recovery_grace_ms_defaults() {
        assert_eq!(parse_recovery_grace_ms(None, 60_000), 60_000);
        assert_eq!(parse_recovery_grace_ms(Some(""), 60_000), 60_000);
        assert_eq!(parse_recovery_grace_ms(Some("xxx"), 60_000), 60_000);
    }

    #[test]
    fn r744_parse_recovery_grace_ms_zero_allowed() {
        assert_eq!(parse_recovery_grace_ms(Some("0"), 60_000), 0);
    }

    #[test]
    fn r744_parse_recovery_grace_ms_negative_rejected() {
        assert_eq!(parse_recovery_grace_ms(Some("-1"), 60_000), 60_000);
    }

    #[test]
    fn r744_expiration_reason_target_gone_when_missing() {
        let r = expiration_reason_for(true, false, false, true).unwrap();
        assert_eq!(r, ExpirationReason::TargetGone);
    }

    #[test]
    fn r744_expiration_reason_target_gone_when_cancelled() {
        let r = expiration_reason_for(true, true, true, true).unwrap();
        assert_eq!(r, ExpirationReason::TargetGone);
    }

    #[test]
    fn r744_expiration_reason_ttl_when_all_present() {
        let r = expiration_reason_for(true, true, false, true).unwrap();
        assert_eq!(r, ExpirationReason::Ttl);
    }

    #[test]
    fn r744_expiration_reason_none_when_no_strict_and_not_ttl() {
        assert!(expiration_reason_for(false, true, false, false).is_none());
    }

    #[test]
    fn r744_expiration_reason_str() {
        assert_eq!(ExpirationReason::TargetGone.as_str(), "target_gone");
        assert_eq!(ExpirationReason::Ttl.as_str(), "ttl");
    }

    #[test]
    fn r744_next_target_sweep_cursor_advances_on_full_batch() {
        let rows = vec![
            ("d1".to_string(), "id-1".to_string()),
            ("d2".to_string(), "id-2".to_string()),
        ];
        assert_eq!(
            next_target_sweep_cursor(&rows, 2),
            Some("id-2".to_string())
        );
    }

    #[test]
    fn r744_next_target_sweep_cursor_resets_on_partial() {
        let rows = vec![("d1".to_string(), "id-1".to_string())];
        assert_eq!(next_target_sweep_cursor(&rows, 5), None);
    }

    #[test]
    fn r744_next_target_sweep_cursor_resets_on_empty() {
        let rows: Vec<(String, String)> = vec![];
        assert_eq!(next_target_sweep_cursor(&rows, 5), None);
    }

    #[test]
    fn r744_is_after_cursor() {
        assert!(is_after_cursor("id-2", Some("id-1")));
        assert!(!is_after_cursor("id-1", Some("id-1")));
        assert!(!is_after_cursor("id-1", Some("id-2")));
        assert!(is_after_cursor("anything", None));
    }

    #[test]
    fn r744_merge_unique_ids_dedupes() {
        let ttl = vec!["a".to_string(), "b".to_string()];
        let target = vec!["b".to_string(), "c".to_string()];
        let m = merge_unique_ids(&ttl, &target);
        assert_eq!(m, vec!["a", "b", "c"]);
    }

    #[test]
    fn r744_merge_unique_ids_empty() {
        let m = merge_unique_ids(&[], &[]);
        assert!(m.is_empty());
    }

    #[test]
    fn r744_merge_continuation_metadata() {
        let m = merge_continuation_metadata(
            serde_json::json!({"continuationPending": true, "foo": 1}),
            ts(2026, 8, 17),
        );
        assert_eq!(m["continuationPending"], Value::Bool(false));
        assert_eq!(
            m["continuationDeliveredAt"],
            Value::String("2026-08-17T00:00:00+00:00".to_string())
        );
        assert_eq!(m["foo"], Value::Number(1.into()));
    }

    #[test]
    fn r744_merge_expired_metadata_ttl() {
        let m = merge_expired_metadata(
            serde_json::json!({}),
            ExpirationReason::Ttl,
            false,
        );
        assert_eq!(m["expiredReason"], Value::String("ttl".to_string()));
        assert!(m.get("continuationPending").is_none());
    }

    #[test]
    fn r744_merge_expired_metadata_target_gone_with_continuation() {
        let m = merge_expired_metadata(
            serde_json::json!({}),
            ExpirationReason::TargetGone,
            true,
        );
        assert_eq!(
            m["expiredReason"],
            Value::String("target_gone".to_string())
        );
        assert_eq!(m["continuationPending"], Value::Bool(true));
    }

    #[test]
    fn r744_merge_decided_metadata_with_idempotency() {
        let m = merge_decided_metadata(
            serde_json::json!({}),
            Some("key-1"),
            false,
            None,
            true,
        );
        assert_eq!(
            m["decideIdempotencyKey"],
            Value::String("key-1".to_string())
        );
        assert_eq!(m["continuationPending"], Value::Bool(true));
        assert!(m.get("dismissed").is_none());
    }

    #[test]
    fn r744_merge_decided_metadata_with_dismiss() {
        let m = merge_decided_metadata(
            serde_json::json!({}),
            None,
            true,
            Some("no longer needed"),
            false,
        );
        assert_eq!(m["decideIdempotencyKey"], Value::Null);
        assert_eq!(m["dismissed"], Value::Bool(true));
        assert_eq!(
            m["dismissReason"],
            Value::String("no longer needed".to_string())
        );
    }

    #[test]
    fn r744_detect_decide_replay_idempotency_match() {
        let v = serde_json::json!({});
        let r = detect_decide_replay(
            "decided",
            Some("opt-1"),
            Some("key-1"),
            Some("u1"),
            Some("key-1"),
            "opt-1",
            None,
            Some(&v),
            "u1",
        );
        assert_eq!(r, DecideReplay::IdempotentReplay);
    }

    #[test]
    fn r744_detect_decide_replay_idempotency_different_user_blocked() {
        let r = detect_decide_replay(
            "decided",
            Some("opt-1"),
            Some("key-1"),
            Some("u1"),
            Some("key-1"),
            "opt-1",
            None,
            None,
            "u2",
        );
        assert_eq!(r, DecideReplay::NotReplay);
    }

    #[test]
    fn r744_detect_decide_replay_option_match() {
        let v = serde_json::json!({"x": "1"});
        let r = detect_decide_replay(
            "decided",
            Some("opt-1"),
            None,
            Some("u1"),
            None,
            "opt-1",
            Some(&v),
            Some(&v),
            "u1",
        );
        assert_eq!(r, DecideReplay::OptionReplay);
    }

    #[test]
    fn r744_detect_decide_replay_option_mismatch() {
        let r = detect_decide_replay(
            "decided",
            Some("opt-1"),
            None,
            Some("u1"),
            None,
            "opt-2",
            None,
            None,
            "u1",
        );
        assert_eq!(r, DecideReplay::NotReplay);
    }

    #[test]
    fn r744_detect_decide_replay_not_decided() {
        let r = detect_decide_replay(
            "open",
            None,
            None,
            None,
            None,
            "opt-1",
            None,
            None,
            "u1",
        );
        assert_eq!(r, DecideReplay::NotReplay);
    }

    #[test]
    fn r744_detect_decide_replay_empty_current_idempotency() {
        // empty current idempotency key does not match a new key,
        // but option matches → OptionReplay path (still a valid replay).
        let r = detect_decide_replay(
            "decided",
            Some("opt-1"),
            Some(""),
            Some("u1"),
            Some("key-1"),
            "opt-1",
            None,
            None,
            "u1",
        );
        assert_eq!(r, DecideReplay::OptionReplay);
    }

    #[test]
    fn r744_detect_decide_replay_treats_missing_inputs_as_empty() {
        let r = detect_decide_replay(
            "decided",
            Some("opt-1"),
            None,
            Some("u1"),
            None,
            "opt-1",
            None,
            None,
            "u1",
        );
        assert_eq!(r, DecideReplay::OptionReplay);
    }

    #[test]
    fn r744_validate_decision_inputs_required() {
        let fields = serde_json::json!([
            {"id": "reason", "required": true},
        ]);
        let values = serde_json::json!({});
        let r = validate_decision_inputs(&fields, &values);
        assert!(matches!(
            r,
            Err(InputValidationError::Required { ref id }) if id == "reason"
        ));
    }

    #[test]
    fn r744_validate_decision_inputs_required_present_ok() {
        let fields = serde_json::json!([
            {"id": "reason", "required": true},
        ]);
        let values = serde_json::json!({"reason": "  hi  "});
        assert!(validate_decision_inputs(&fields, &values).is_ok());
    }

    #[test]
    fn r744_validate_decision_inputs_too_long() {
        let fields = serde_json::json!([
            {"id": "x", "required": false, "maxLength": 3},
        ]);
        let values = serde_json::json!({"x": "abcd"});
        let r = validate_decision_inputs(&fields, &values);
        assert!(matches!(
            r,
            Err(InputValidationError::TooLong { ref id, max_length: 3, .. }) if id == "x"
        ));
    }

    #[test]
    fn r744_validate_decision_inputs_max_length_ok() {
        let fields = serde_json::json!([
            {"id": "x", "required": false, "maxLength": 4},
        ]);
        let values = serde_json::json!({"x": "abc"});
        assert!(validate_decision_inputs(&fields, &values).is_ok());
    }

    #[test]
    fn r744_validate_decision_inputs_no_fields() {
        let fields = serde_json::json!([]);
        let values = serde_json::json!({});
        assert!(validate_decision_inputs(&fields, &values).is_ok());
    }

    #[test]
    fn r744_validate_decision_inputs_skips_empty_id() {
        let fields = serde_json::json!([{"required": true}]);
        let values = serde_json::json!({});
        assert!(validate_decision_inputs(&fields, &values).is_ok());
    }
}
