#![forbid(unsafe_code)]

//! Routine creation / patch / trigger validation pure helpers — extracted from
//! `pc-routines/src/service.rs` `CreateRoutine::normalize` /
//! `RoutinePatch::validate` / `CreateRoutineTrigger::validate` to make the
//! policy rules independently testable.
//!
//! R746: 与 R744/R745 同模式——核心判断拆为纯函数（不返回 pc_errors，
//! 调用方负责把 `Err(&'static str)` 升级为 `validation()`/`unprocessable()`）。
//!
//! 对齐 `paperclip/server/src/services/routines.ts` 中
//! `routineCreateSchema` / `routineUpdateSchema` / trigger kind 校验。

use uuid::Uuid;

/// priority 取值集合（low / medium / high / urgent）。
pub const ALLOWED_PRIORITIES: &[&str] = &["low", "medium", "high", "urgent"];
/// status 取值集合（draft / active / paused / archived）。
pub const ALLOWED_STATUSES: &[&str] = &["draft", "active", "paused", "archived"];
/// concurrencyPolicy 取值集合（allow / skip / queue）。
pub const ALLOWED_CONCURRENCY: &[&str] = &["allow", "skip", "queue"];
/// catchUpPolicy 取值集合（skip_missed / enqueue_missed_with_cap）。
pub const ALLOWED_CATCHUP: &[&str] = &["skip_missed", "enqueue_missed_with_cap"];
/// activityGatePolicy 取值集合（always / require_external_activity）。
pub const ALLOWED_ACTIVITY_GATE: &[&str] = &["always", "require_external_activity"];
/// trigger.kind 取值集合（schedule / webhook）。
pub const ALLOWED_TRIGGER_KINDS: &[&str] = &["schedule", "webhook"];

/// 默认 priority（"medium"）。
pub const DEFAULT_PRIORITY: &str = "medium";
/// 默认 status（"active"）。
pub const DEFAULT_STATUS: &str = "active";
/// 默认 concurrencyPolicy（"allow"）。
pub const DEFAULT_CONCURRENCY: &str = "allow";
/// 默认 catchUpPolicy（"skip_missed"）。
pub const DEFAULT_CATCHUP: &str = "skip_missed";
/// 默认 activityGatePolicy（"always"）。
pub const DEFAULT_ACTIVITY_GATE: &str = "always";
/// 默认 activityGateScope（"company"）。
pub const DEFAULT_ACTIVITY_GATE_SCOPE: &str = "company";
/// 默认 trigger timezone（"UTC"）。
pub const DEFAULT_TRIGGER_TIMEZONE: &str = "UTC";

// =============================================================================
// 谓词（predicates）
// =============================================================================

pub fn is_priority_allowed(value: &str) -> bool {
    ALLOWED_PRIORITIES.contains(&value)
}

pub fn is_status_allowed(value: &str) -> bool {
    ALLOWED_STATUSES.contains(&value)
}

pub fn is_concurrency_allowed(value: &str) -> bool {
    ALLOWED_CONCURRENCY.contains(&value)
}

pub fn is_catchup_allowed(value: &str) -> bool {
    ALLOWED_CATCHUP.contains(&value)
}

pub fn is_activity_gate_allowed(value: &str) -> bool {
    ALLOWED_ACTIVITY_GATE.contains(&value)
}

pub fn is_trigger_kind_allowed(value: &str) -> bool {
    ALLOWED_TRIGGER_KINDS.contains(&value)
}

// =============================================================================
// 默认值（unwrap_or else 默认值的等价）
// =============================================================================

pub fn default_priority(input: Option<&str>) -> &str {
    input.unwrap_or(DEFAULT_PRIORITY)
}

pub fn default_status(input: Option<&str>) -> &str {
    input.unwrap_or(DEFAULT_STATUS)
}

pub fn default_concurrency(input: Option<&str>) -> &str {
    input.unwrap_or(DEFAULT_CONCURRENCY)
}

pub fn default_catchup(input: Option<&str>) -> &str {
    input.unwrap_or(DEFAULT_CATCHUP)
}

pub fn default_activity_gate(input: Option<&str>) -> &str {
    input.unwrap_or(DEFAULT_ACTIVITY_GATE)
}

pub fn default_activity_gate_scope(input: Option<&str>) -> &str {
    input.unwrap_or(DEFAULT_ACTIVITY_GATE_SCOPE)
}

pub fn default_trigger_timezone(input: Option<&str>) -> &str {
    input.unwrap_or(DEFAULT_TRIGGER_TIMEZONE)
}

// =============================================================================
// 校验（返回 Result<(), &'static str>，调用方 wrap 成 pc_errors）
// =============================================================================

/// 校验 priority 字符串。
pub fn validate_priority(value: &str) -> Result<(), &'static str> {
    if is_priority_allowed(value) {
        Ok(())
    } else {
        Err("priority must be one of low/medium/high/urgent")
    }
}

/// 校验 status 字符串。
pub fn validate_status(value: &str) -> Result<(), &'static str> {
    if is_status_allowed(value) {
        Ok(())
    } else {
        Err("status must be one of draft/active/paused/archived")
    }
}

/// 校验 concurrencyPolicy 字符串。
pub fn validate_concurrency_policy(value: &str) -> Result<(), &'static str> {
    if is_concurrency_allowed(value) {
        Ok(())
    } else {
        Err("concurrencyPolicy must be one of allow/skip/queue")
    }
}

/// 校验 catchUpPolicy 字符串。
pub fn validate_catch_up_policy(value: &str) -> Result<(), &'static str> {
    if is_catchup_allowed(value) {
        Ok(())
    } else {
        Err("catchUpPolicy must be one of skip_missed/enqueue_missed_with_cap")
    }
}

/// 校验 activityGatePolicy 字符串。
pub fn validate_activity_gate_policy(value: &str) -> Result<(), &'static str> {
    if is_activity_gate_allowed(value) {
        Ok(())
    } else {
        Err("activityGatePolicy must be one of always/require_external_activity")
    }
}

/// 校验 trigger.kind。
pub fn validate_trigger_kind(value: &str) -> Result<(), &'static str> {
    if is_trigger_kind_allowed(value) {
        Ok(())
    } else {
        Err("trigger kind must be schedule or webhook")
    }
}

/// 校验 title 非空（trim 后）。
pub fn validate_title_non_empty(title: &str) -> Result<(), &'static str> {
    if title.trim().is_empty() {
        Err("title must not be empty")
    } else {
        Ok(())
    }
}

/// 校验 company_id 不是 nil uuid。
pub fn validate_company_id_not_nil(id: Uuid) -> Result<(), &'static str> {
    if id.is_nil() {
        Err("companyId is required")
    } else {
        Ok(())
    }
}

/// 校验 schedule trigger 必填项（cron_expression + 可选 timezone）。
pub fn validate_trigger_schedule_inputs(
    cron_expression: Option<&str>,
    timezone: Option<&str>,
) -> Result<(), &'static str> {
    let cron = match cron_expression {
        Some(s) if !s.trim().is_empty() => s,
        _ => return Err("scheduled triggers require cronExpression"),
    };
    let _ = cron;
    let _ = timezone; // timezone has default UTC, no further check needed
    Ok(())
}

/// 校验 webhook trigger 不携带 cron_expression。
pub fn validate_trigger_webhook_inputs(
    cron_expression: Option<&str>,
) -> Result<(), &'static str> {
    if cron_expression.is_some() {
        Err("webhook triggers must not include cronExpression")
    } else {
        Ok(())
    }
}

/// 校验 trigger patch：cron_expression Some(non-empty) → 通过；
/// Some("") → 拒绝；None → 不动。
pub fn validate_trigger_patch_cron(
    cron_expression: Option<Option<&str>>,
) -> Result<(), &'static str> {
    match cron_expression {
        Some(Some(c)) if !c.trim().is_empty() => Ok(()),
        Some(_) => Err("scheduled triggers require cronExpression"),
        None => Ok(()),
    }
}

/// 校验 trigger patch：timezone Some(non-empty) → 通过；Some("") → 拒绝。
pub fn validate_trigger_patch_timezone(
    timezone: Option<Option<&str>>,
) -> Result<(), &'static str> {
    match timezone {
        Some(Some(t)) if !t.trim().is_empty() => Ok(()),
        Some(_) => Err("scheduled triggers require timezone"),
        None => Ok(()),
    }
}

/// 把 Cron 字符串与 timezone 规范化（schedule 路径）。
pub fn normalize_trigger_schedule(
    kind: &str,
    cron_expression: Option<String>,
    timezone: Option<String>,
) -> (Option<String>, Option<String>) {
    if kind == "schedule" {
        (
            cron_expression,
            Some(timezone.unwrap_or_else(|| DEFAULT_TRIGGER_TIMEZONE.to_string())),
        )
    } else {
        (None, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r746_priority_predicate() {
        assert!(is_priority_allowed("low"));
        assert!(is_priority_allowed("medium"));
        assert!(is_priority_allowed("high"));
        assert!(is_priority_allowed("urgent"));
        assert!(!is_priority_allowed("critical"));
        assert!(!is_priority_allowed(""));
    }

    #[test]
    fn r746_status_predicate() {
        assert!(is_status_allowed("draft"));
        assert!(is_status_allowed("active"));
        assert!(is_status_allowed("paused"));
        assert!(is_status_allowed("archived"));
        assert!(!is_status_allowed("deleted"));
        assert!(!is_status_allowed(""));
    }

    #[test]
    fn r746_concurrency_predicate() {
        assert!(is_concurrency_allowed("allow"));
        assert!(is_concurrency_allowed("skip"));
        assert!(is_concurrency_allowed("queue"));
        assert!(!is_concurrency_allowed("block"));
    }

    #[test]
    fn r746_catchup_predicate() {
        assert!(is_catchup_allowed("skip_missed"));
        assert!(is_catchup_allowed("enqueue_missed_with_cap"));
        assert!(!is_catchup_allowed("run_all_missed"));
    }

    #[test]
    fn r746_activity_gate_predicate() {
        assert!(is_activity_gate_allowed("always"));
        assert!(is_activity_gate_allowed("require_external_activity"));
        assert!(!is_activity_gate_allowed("never"));
    }

    #[test]
    fn r746_trigger_kind_predicate() {
        assert!(is_trigger_kind_allowed("schedule"));
        assert!(is_trigger_kind_allowed("webhook"));
        assert!(!is_trigger_kind_allowed("manual"));
    }

    #[test]
    fn r746_default_priority() {
        assert_eq!(default_priority(None), DEFAULT_PRIORITY);
        assert_eq!(default_priority(Some("high")), "high");
    }

    #[test]
    fn r746_default_status() {
        assert_eq!(default_status(None), DEFAULT_STATUS);
        assert_eq!(default_status(Some("paused")), "paused");
    }

    #[test]
    fn r746_default_concurrency() {
        assert_eq!(default_concurrency(None), DEFAULT_CONCURRENCY);
        assert_eq!(default_concurrency(Some("queue")), "queue");
    }

    #[test]
    fn r746_default_catchup() {
        assert_eq!(default_catchup(None), DEFAULT_CATCHUP);
        assert_eq!(default_catchup(Some("enqueue_missed_with_cap")), "enqueue_missed_with_cap");
    }

    #[test]
    fn r746_default_activity_gate() {
        assert_eq!(default_activity_gate(None), DEFAULT_ACTIVITY_GATE);
        assert_eq!(
            default_activity_gate(Some("require_external_activity")),
            "require_external_activity"
        );
    }

    #[test]
    fn r746_default_activity_gate_scope() {
        assert_eq!(default_activity_gate_scope(None), DEFAULT_ACTIVITY_GATE_SCOPE);
        assert_eq!(default_activity_gate_scope(Some("project")), "project");
    }

    #[test]
    fn r746_default_trigger_timezone() {
        assert_eq!(default_trigger_timezone(None), DEFAULT_TRIGGER_TIMEZONE);
        assert_eq!(default_trigger_timezone(Some("America/New_York")), "America/New_York");
    }

    #[test]
    fn r746_validate_priority_ok() {
        assert!(validate_priority("low").is_ok());
        assert!(validate_priority("high").is_ok());
        assert!(validate_priority("urgent").is_ok());
    }

    #[test]
    fn r746_validate_priority_invalid() {
        let err = validate_priority("critical").unwrap_err();
        assert!(err.contains("priority"));
    }

    #[test]
    fn r746_validate_status_ok() {
        assert!(validate_status("draft").is_ok());
        assert!(validate_status("archived").is_ok());
    }

    #[test]
    fn r746_validate_status_invalid() {
        let err = validate_status("deleted").unwrap_err();
        assert!(err.contains("status"));
    }

    #[test]
    fn r746_validate_concurrency_ok_and_invalid() {
        assert!(validate_concurrency_policy("allow").is_ok());
        let err = validate_concurrency_policy("block").unwrap_err();
        assert!(err.contains("concurrencyPolicy"));
    }

    #[test]
    fn r746_validate_catchup_ok_and_invalid() {
        assert!(validate_catch_up_policy("skip_missed").is_ok());
        let err = validate_catch_up_policy("run_all").unwrap_err();
        assert!(err.contains("catchUpPolicy"));
    }

    #[test]
    fn r746_validate_activity_gate_ok_and_invalid() {
        assert!(validate_activity_gate_policy("always").is_ok());
        let err = validate_activity_gate_policy("never").unwrap_err();
        assert!(err.contains("activityGatePolicy"));
    }

    #[test]
    fn r746_validate_trigger_kind_ok_and_invalid() {
        assert!(validate_trigger_kind("schedule").is_ok());
        let err = validate_trigger_kind("manual").unwrap_err();
        assert!(err.contains("trigger kind"));
    }

    #[test]
    fn r746_validate_title_non_empty_ok() {
        assert!(validate_title_non_empty("hello").is_ok());
        assert!(validate_title_non_empty("  hello  ").is_ok());
    }

    #[test]
    fn r746_validate_title_non_empty_empty() {
        assert!(validate_title_non_empty("").is_err());
        assert!(validate_title_non_empty("   ").is_err());
    }

    #[test]
    fn r746_validate_company_id_not_nil_ok() {
        assert!(validate_company_id_not_nil(Uuid::new_v4()).is_ok());
    }

    #[test]
    fn r746_validate_company_id_nil() {
        let err = validate_company_id_not_nil(Uuid::nil()).unwrap_err();
        assert!(err.contains("companyId"));
    }

    #[test]
    fn r746_validate_trigger_schedule_with_cron() {
        assert!(validate_trigger_schedule_inputs(Some("0 * * * *"), None).is_ok());
        assert!(validate_trigger_schedule_inputs(Some("0 * * * *"), Some("UTC")).is_ok());
    }

    #[test]
    fn r746_validate_trigger_schedule_missing_cron() {
        let err = validate_trigger_schedule_inputs(None, None).unwrap_err();
        assert!(err.contains("cronExpression"));
    }

    #[test]
    fn r746_validate_trigger_schedule_empty_cron() {
        let err = validate_trigger_schedule_inputs(Some("   "), None).unwrap_err();
        assert!(err.contains("cronExpression"));
    }

    #[test]
    fn r746_validate_trigger_webhook_with_no_cron() {
        assert!(validate_trigger_webhook_inputs(None).is_ok());
    }

    #[test]
    fn r746_validate_trigger_webhook_with_cron_blocked() {
        let err = validate_trigger_webhook_inputs(Some("0 * * * *")).unwrap_err();
        assert!(err.contains("must not include cronExpression"));
    }

    #[test]
    fn r746_validate_trigger_patch_cron_none_passes() {
        assert!(validate_trigger_patch_cron(None).is_ok());
    }

    #[test]
    fn r746_validate_trigger_patch_cron_some_nonempty_passes() {
        assert!(validate_trigger_patch_cron(Some(Some("0 0 * * *"))).is_ok());
    }

    #[test]
    fn r746_validate_trigger_patch_cron_empty_blocked() {
        let err = validate_trigger_patch_cron(Some(Some(""))).unwrap_err();
        assert!(err.contains("cronExpression"));
    }

    #[test]
    fn r746_validate_trigger_patch_cron_whitespace_blocked() {
        let err = validate_trigger_patch_cron(Some(Some("  "))).unwrap_err();
        assert!(err.contains("cronExpression"));
    }

    #[test]
    fn r746_validate_trigger_patch_timezone_none_passes() {
        assert!(validate_trigger_patch_timezone(None).is_ok());
    }

    #[test]
    fn r746_validate_trigger_patch_timezone_some_nonempty_passes() {
        assert!(validate_trigger_patch_timezone(Some(Some("UTC"))).is_ok());
    }

    #[test]
    fn r746_validate_trigger_patch_timezone_empty_blocked() {
        let err = validate_trigger_patch_timezone(Some(Some(""))).unwrap_err();
        assert!(err.contains("timezone"));
    }

    #[test]
    fn r746_normalize_trigger_schedule_keeps_inputs() {
        let (cron, tz) = normalize_trigger_schedule(
            "schedule",
            Some("0 0 * * *".to_string()),
            Some("America/New_York".to_string()),
        );
        assert_eq!(cron.as_deref(), Some("0 0 * * *"));
        assert_eq!(tz.as_deref(), Some("America/New_York"));
    }

    #[test]
    fn r746_normalize_trigger_schedule_default_tz() {
        let (cron, tz) = normalize_trigger_schedule(
            "schedule",
            Some("0 0 * * *".to_string()),
            None,
        );
        assert_eq!(cron.as_deref(), Some("0 0 * * *"));
        assert_eq!(tz.as_deref(), Some("UTC"));
    }

    #[test]
    fn r746_normalize_trigger_webhook_strips_cron_and_tz() {
        let (cron, tz) = normalize_trigger_schedule(
            "webhook",
            Some("0 0 * * *".to_string()),
            Some("America/New_York".to_string()),
        );
        assert!(cron.is_none());
        assert!(tz.is_none());
    }

    #[test]
    fn r746_constants_match_service() {
        assert_eq!(ALLOWED_PRIORITIES.len(), 4);
        assert_eq!(ALLOWED_STATUSES.len(), 4);
        assert_eq!(ALLOWED_CONCURRENCY.len(), 3);
        assert_eq!(ALLOWED_CATCHUP.len(), 2);
        assert_eq!(ALLOWED_ACTIVITY_GATE.len(), 2);
        assert_eq!(ALLOWED_TRIGGER_KINDS.len(), 2);
    }
}
