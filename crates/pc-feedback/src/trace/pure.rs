#![forbid(unsafe_code)]

//! Feedback trace 业务层的 zero-DB pure helpers。
//!
//! 对应 Node paperclip/server/src/services/feedback-trace.ts 中
//! 跟 DB 解耦的纯函数部分（validation / limit clamping）。

use uuid::Uuid;

const DEFAULT_TRACE_LIMIT: i64 = 100;
const MAX_TRACE_LIMIT: i64 = 500;

/// 校验 trace_id（uuid 非 nil）。
pub fn validate_trace_id(id: Uuid) -> Result<(), String> {
    if id.is_nil() {
        return Err("traceId is required".into());
    }
    Ok(())
}

/// 校验 issue_id（uuid 非 nil）。
pub fn validate_issue_id(id: Uuid) -> Result<(), String> {
    if id.is_nil() {
        return Err("issueId is required".into());
    }
    Ok(())
}

/// 校验 company_id（uuid 非 nil）。
pub fn validate_company_id(id: Uuid) -> Result<(), String> {
    if id.is_nil() {
        return Err("companyId is required".into());
    }
    Ok(())
}

/// 把用户传入的 limit 钳制到 [0, MAX_TRACE_LIMIT]，负数视为 0。
pub fn clamp_trace_limit(requested: i64) -> i64 {
    if requested < 0 {
        return 0;
    }
    requested.min(MAX_TRACE_LIMIT)
}

/// 当 limit == 0 时回退到 DEFAULT_TRACE_LIMIT。
pub fn resolve_trace_limit(requested: i64) -> i64 {
    let clamped = clamp_trace_limit(requested);
    if clamped == 0 {
        DEFAULT_TRACE_LIMIT
    } else {
        clamped
    }
}

/// 合并 hook event 的 trace_id + issue_id 用于 logging。
pub fn format_trace_hook_label(trace_id: Uuid, issue_id: Uuid) -> String {
    format!("trace={} issue={}", trace_id, issue_id)
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    #[test]
    fn validate_trace_id_accepts_real() {
        let id = Uuid::new_v4();
        assert!(validate_trace_id(id).is_ok());
    }

    #[test]
    fn validate_trace_id_rejects_nil() {
        assert!(validate_trace_id(Uuid::nil()).is_err());
    }

    #[test]
    fn validate_issue_id_accepts_real() {
        let id = Uuid::new_v4();
        assert!(validate_issue_id(id).is_ok());
    }

    #[test]
    fn validate_issue_id_rejects_nil() {
        assert!(validate_issue_id(Uuid::nil()).is_err());
    }

    #[test]
    fn validate_company_id_rejects_nil() {
        assert!(validate_company_id(Uuid::nil()).is_err());
    }

    #[test]
    fn clamp_trace_limit_negative_to_zero() {
        assert_eq!(clamp_trace_limit(-5), 0);
    }

    #[test]
    fn clamp_trace_limit_zero() {
        assert_eq!(clamp_trace_limit(0), 0);
    }

    #[test]
    fn clamp_trace_limit_normal() {
        assert_eq!(clamp_trace_limit(50), 50);
    }

    #[test]
    fn clamp_trace_limit_too_large_clamped() {
        assert_eq!(clamp_trace_limit(10_000), MAX_TRACE_LIMIT);
    }

    #[test]
    fn resolve_trace_limit_zero_falls_back() {
        assert_eq!(resolve_trace_limit(0), DEFAULT_TRACE_LIMIT);
    }

    #[test]
    fn resolve_trace_limit_negative_falls_back() {
        assert_eq!(resolve_trace_limit(-1), DEFAULT_TRACE_LIMIT);
    }

    #[test]
    fn resolve_trace_limit_preserves_small_positive() {
        assert_eq!(resolve_trace_limit(20), 20);
    }

    #[test]
    fn format_trace_hook_label_includes_both() {
        let t = Uuid::new_v4();
        let i = Uuid::new_v4();
        let s = format_trace_hook_label(t, i);
        assert!(s.contains(&t.to_string()));
        assert!(s.contains(&i.to_string()));
    }

    #[test]
    fn r755_trace_pure_resolve_trace_limit_at_max_boundary() {
        assert_eq!(resolve_trace_limit(MAX_TRACE_LIMIT), MAX_TRACE_LIMIT);
        assert_eq!(resolve_trace_limit(MAX_TRACE_LIMIT + 1), MAX_TRACE_LIMIT);
    }

    #[test]
    fn r755_trace_pure_validate_company_id_accepts_real() {
        let id = Uuid::new_v4();
        assert!(validate_company_id(id).is_ok());
    }

    #[test]
    fn r755_trace_pure_format_trace_hook_label_uses_prefixed_keys() {
        let t = Uuid::nil();
        let i = Uuid::nil();
        let s = format_trace_hook_label(t, i);
        assert_eq!(s, format!("trace={t} issue={i}"));
    }
}
