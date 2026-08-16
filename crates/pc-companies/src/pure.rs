#![forbid(unsafe_code)]

//! Companies pure helpers — 1:1 port of paperclip/server/src/services/companies.ts
//!
//! R714: zero-DB helpers extracted from the companies service. Each function is a
//! small, testable building block.

use chrono::{DateTime, Datelike, TimeZone, Utc};
use serde_json::Value;

/// Fallback prefix used when a company name has no A-Z characters.
pub const ISSUE_PREFIX_FALLBACK: &str = "CMP";

/// PostgreSQL unique-violation SQLSTATE.
pub const PG_UNIQUE_VIOLATION: &str = "23505";

/// Constraint name that protects the unique index on companies.issue_prefix.
pub const ISSUE_PREFIX_CONSTRAINT: &str = "companies_issue_prefix_idx";

/// Derive the base 3-letter issue prefix from a company name.
///
/// Node parity: `name.toUpperCase().replace(/[^A-Z]/g, "").slice(0, 3)`,
/// falling back to `ISSUE_PREFIX_FALLBACK` if the result is empty.
pub fn derive_issue_prefix_base(name: &str) -> String {
    let normalized: String = name
        .to_uppercase()
        .chars()
        .filter(|c| c.is_ascii_uppercase())
        .collect();
    let trimmed: String = normalized.chars().take(3).collect();
    if trimmed.is_empty() {
        ISSUE_PREFIX_FALLBACK.to_string()
    } else {
        trimmed
    }
}

/// Generate the suffix appended to a prefix on retry.
///
/// Node parity: `if (attempt <= 1) return ""; return "A".repeat(attempt - 1)`.
pub fn suffix_for_attempt(attempt: u32) -> String {
    if attempt <= 1 {
        String::new()
    } else {
        "A".repeat((attempt - 1) as usize)
    }
}

/// Compute the [start, end) UTC window for the month containing `now`.
///
/// Node parity: `new Date(Date.UTC(year, month, 1, 0, 0, 0, 0))` and
/// `new Date(Date.UTC(year, month + 1, 1, 0, 0, 0, 0))`.
pub fn current_utc_month_window(now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    let year = now.year();
    let month = now.month();
    let start = Utc
        .with_ymd_and_hms(year, month, 1, 0, 0, 0)
        .single()
        .expect("valid first-of-month UTC");
    let (ny, nm) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    let end = Utc
        .with_ymd_and_hms(ny, nm, 1, 0, 0, 0)
        .single()
        .expect("valid first-of-next-month UTC");
    (start, end)
}

/// Walk the `cause` chain of a Postgres error looking for a unique violation on
/// the issue-prefix index.
///
/// Node parity: tolerates circular `cause` references and accepts either
/// `constraint` or `constraint_name` (Drizzle variants).
pub fn is_issue_prefix_conflict(error: &Value) -> bool {
    // JSON values don't have stable identity across clones, so cap recursion
    // depth (Node tolerates circular refs but Postgres error chains are always
    // shallow).
    const MAX_DEPTH: usize = 32;
    let mut current = Some(error.clone());
    let mut depth = 0usize;
    while let Some(err) = current {
        if depth >= MAX_DEPTH { break; }
        depth += 1;
        if let Some(obj) = err.as_object() {
            let code = obj.get("code").and_then(Value::as_str);
            let constraint = obj
                .get("constraint")
                .and_then(Value::as_str)
                .or_else(|| obj.get("constraint_name").and_then(Value::as_str));
            if code == Some(PG_UNIQUE_VIOLATION) && constraint == Some(ISSUE_PREFIX_CONSTRAINT) {
                return true;
            }
        }
        current = err.get("cause").cloned();
    }
    false
}

#[cfg(test)]
mod internal_tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    #[test]
    fn derive_base_alpha() {
        assert_eq!(derive_issue_prefix_base("Acme"), "ACM");
        assert_eq!(derive_issue_prefix_base("  acme co  "), "ACM");
        assert_eq!(derive_issue_prefix_base("A!B@C#D"), "ABC");
    }

    #[test]
    fn derive_base_truncates_to_three() {
        assert_eq!(derive_issue_prefix_base("Foobar"), "FOO");
        assert_eq!(derive_issue_prefix_base("XYZ"), "XYZ");
    }

    #[test]
    fn derive_base_fallback() {
        assert_eq!(derive_issue_prefix_base(""), "CMP");
        assert_eq!(derive_issue_prefix_base("123"), "CMP");
        assert_eq!(derive_issue_prefix_base("---"), "CMP");
    }

    #[test]
    fn suffix_attempts() {
        assert_eq!(suffix_for_attempt(0), "");
        assert_eq!(suffix_for_attempt(1), "");
        assert_eq!(suffix_for_attempt(2), "A");
        assert_eq!(suffix_for_attempt(3), "AA");
        assert_eq!(suffix_for_attempt(5), "AAAA");
    }

    #[test]
    fn utc_month_window_january() {
        let now = Utc.with_ymd_and_hms(2025, 1, 15, 12, 0, 0).unwrap();
        let (start, end) = current_utc_month_window(now);
        assert_eq!(start.to_rfc3339(), "2025-01-01T00:00:00+00:00");
        assert_eq!(end.to_rfc3339(), "2025-02-01T00:00:00+00:00");
    }

    #[test]
    fn utc_month_window_december_wraps_year() {
        let now = Utc.with_ymd_and_hms(2025, 12, 31, 23, 59, 59).unwrap();
        let (start, end) = current_utc_month_window(now);
        assert_eq!(start.to_rfc3339(), "2025-12-01T00:00:00+00:00");
        assert_eq!(end.to_rfc3339(), "2026-01-01T00:00:00+00:00");
    }

    #[test]
    fn conflict_detect_direct() {
        let err = json!({
            "code": "23505",
            "constraint": "companies_issue_prefix_idx"
        });
        assert!(is_issue_prefix_conflict(&err));
    }

    #[test]
    fn conflict_detect_constraint_name_variant() {
        let err = json!({
            "code": "23505",
            "constraint_name": "companies_issue_prefix_idx"
        });
        assert!(is_issue_prefix_conflict(&err));
    }

    #[test]
    fn conflict_detect_walks_cause_chain() {
        let err = json!({
            "code": "OTHER",
            "cause": {
                "code": "23505",
                "constraint": "companies_issue_prefix_idx"
            }
        });
        assert!(is_issue_prefix_conflict(&err));
    }

    #[test]
    fn conflict_detect_wrong_constraint() {
        let err = json!({
            "code": "23505",
            "constraint": "some_other_idx"
        });
        assert!(!is_issue_prefix_conflict(&err));
    }

    #[test]
    fn conflict_detect_wrong_code() {
        let err = json!({
            "code": "99999",
            "constraint": "companies_issue_prefix_idx"
        });
        assert!(!is_issue_prefix_conflict(&err));
    }

    #[test]
    fn conflict_detect_handles_non_object() {
        assert!(!is_issue_prefix_conflict(&json!("string error")));
        assert!(!is_issue_prefix_conflict(&json!(null)));
        assert!(!is_issue_prefix_conflict(&json!(42)));
    }
}