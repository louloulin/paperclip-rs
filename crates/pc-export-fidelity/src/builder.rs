//! Builder —— Export fidelity report 构造 / 校验 / warning 推断。

use std::collections::BTreeMap;

use serde_json::Value;
use crate::types::UNSUPPORTED_DATA_WARNINGS;

use crate::types::{
    ExportFidelityCounts, ExportFidelityReport, PortabilityFidelitySeverity,
    PortabilityFidelityWarning, EXPORT_FIDELITY_REPORT_SCHEMA,
};

// ============================================================================
// Warnings builder
// ============================================================================

/// 根据 counts 推断 warnings（与 Node `buildExportFidelityWarnings(counts)` 1:1 对齐）。
///
/// 规则：
/// - 遍历 [`UNSUPPORTED_DATA_WARNINGS`]（approvals / costEvents / activityLogEntries）
/// - count > 0 时推一条 warning：
///   - count = 1 → `"{n} {singular} is not included ..."`
///   - count != 1 → `"{n} {plural} are not included ..."`
/// - severity 总是 `warning`
pub fn build_export_fidelity_warnings(counts: &ExportFidelityCounts) -> Vec<PortabilityFidelityWarning> {
    let mut warnings = Vec::new();
    for spec in UNSUPPORTED_DATA_WARNINGS.iter() {
        let row_count = match counts.get(spec.count_key) {
            Some(&c) if c > 0 => c,
            _ => continue,
        };
        let unit = if row_count == 1 { spec.singular } else { spec.plural };
        let verb = if row_count == 1 { "is" } else { "are" };
        warnings.push(PortabilityFidelityWarning {
            code: spec.code.to_string(),
            severity: PortabilityFidelitySeverity::Warning,
            message: format!("{row_count} {unit} {verb} not included in the export bundle."),
        });
    }
    warnings
}

// ============================================================================
// Counts normalization
// ============================================================================

/// 校验 + 归一化外部 `value` 为 `ExportFidelityCounts`（与 Node `normalizeExportFidelityCounts(value)` 1:1 对齐）。
///
/// 规则：
/// - `value` 必须是非空、非数组对象
/// - 每个 `EXPORT_FIDELITY_COUNT_KEYS` 必须存在且为有限非负数字
/// - 任何字段无效 → 返回 `None`
pub fn normalize_export_fidelity_counts(value: &Value) -> Option<ExportFidelityCounts> {
    let Some(obj) = value.as_object() else { return None };
    if obj.is_empty() {
        return None;
    }
    let mut out: ExportFidelityCounts = BTreeMap::new();
    for key in EXPORT_FIDELITY_REPORT_SCHEMA_KEYS {
        let raw = obj.get(*key)?;
        let n = raw.as_f64()?;
        if !n.is_finite() || n < 0.0 {
            return None;
        }
        out.insert((*key).to_string(), n as i64);
    }
    Some(out)
}

// 静态 key 列表（避免每次 `normalize_export_fidelity_counts` 重新分配）
pub(crate) static EXPORT_FIDELITY_REPORT_SCHEMA_KEYS: &[&str] = crate::types::EXPORT_FIDELITY_COUNT_KEYS;

// ============================================================================
// Report builder
// ============================================================================

/// 构造完整 report（与 Node `buildExportFidelityReport(companyId, counts)` 1:1 对齐）。
pub fn build_export_fidelity_report(
    company_id: &str,
    counts: ExportFidelityCounts,
    generated_at: Option<chrono::DateTime<chrono::Utc>>,
) -> ExportFidelityReport {
    ExportFidelityReport {
        schema: EXPORT_FIDELITY_REPORT_SCHEMA.to_string(),
        company_id: company_id.to_string(),
        warnings: build_export_fidelity_warnings(&counts),
        counts,
        generated_at: generated_at
            .unwrap_or_else(chrono::Utc::now)
            .to_rfc3339(),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::UNSUPPORTED_DATA_WARNINGS;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn zero_counts() -> ExportFidelityCounts {
        let mut m = BTreeMap::new();
        for k in EXPORT_FIDELITY_REPORT_SCHEMA_KEYS {
            m.insert((*k).to_string(), 0);
        }
        m
    }

    #[test]
    fn r682_no_warnings_when_zero_counts() {
        let counts = zero_counts();
        let ws = build_export_fidelity_warnings(&counts);
        assert!(ws.is_empty(), "all zero should produce no warnings");
    }

    #[test]
    fn r682_warnings_when_supported_counts_positive() {
        let mut counts = zero_counts();
        counts.insert("approvals".to_string(), 1);
        counts.insert("costEvents".to_string(), 1);
        counts.insert("activityLogEntries".to_string(), 1);
        let ws = build_export_fidelity_warnings(&counts);
        assert_eq!(ws.len(), 3);
        assert_eq!(ws[0].code, "approvals_not_exported");
        assert_eq!(ws[0].severity, PortabilityFidelitySeverity::Warning);
        // singular, "1 approval is not included..."
        assert!(ws[0].message.contains("1 approval is"));
        // costEvents → "cost event is"
        assert!(ws[1].message.contains("1 cost event is"));
        // activityLogEntries → "activity log entry is"
        assert!(ws[2].message.contains("1 activity log entry is"));
    }

    #[test]
    fn r682_warnings_pluralize_when_count_greater_than_one() {
        let mut counts = zero_counts();
        counts.insert("approvals".to_string(), 7);
        let ws = build_export_fidelity_warnings(&counts);
        assert_eq!(ws.len(), 1);
        assert!(ws[0].message.contains("7 approvals are"));
        assert!(!ws[0].message.contains("is "));
    }

    #[test]
    fn r682_warnings_skip_non_supported_keys() {
        // 所有非 approvals/costEvents/activityLogEntries 的字段都被忽略
        let mut counts = zero_counts();
        counts.insert("labelDefinitions".to_string(), 100);
        let ws = build_export_fidelity_warnings(&counts);
        assert!(ws.is_empty());
    }

    #[test]
    fn r682_warning_specs_match_node_codes() {
        let codes: Vec<&str> = UNSUPPORTED_DATA_WARNINGS.iter().map(|s| s.code).collect();
        assert_eq!(
            codes,
            vec![
                "approvals_not_exported",
                "cost_history_not_exported",
                "activity_history_not_exported"
            ]
        );
    }

    #[test]
    fn r682_normalize_counts_accepts_valid_object() {
        let v = json!({
            "labelDefinitions": 5,
            "issueLabelReferences": 0,
            "issueBlockerRelations": 2,
            "issueDocuments": 1,
            "issueWorkProducts": 0,
            "issueAttachments": 7,
            "approvals": 0,
            "costEvents": 0,
            "activityLogEntries": 0,
            "issueMonitors": 1
        });
        let counts = normalize_export_fidelity_counts(&v).expect("normalize");
        assert_eq!(counts.get("labelDefinitions"), Some(&5));
        assert_eq!(counts.get("issueAttachments"), Some(&7));
        assert_eq!(counts.get("issueMonitors"), Some(&1));
    }

    #[test]
    fn r682_normalize_counts_rejects_non_object() {
        assert!(normalize_export_fidelity_counts(&json!(null)).is_none());
        assert!(normalize_export_fidelity_counts(&json!("string")).is_none());
        assert!(normalize_export_fidelity_counts(&json!(123)).is_none());
        assert!(normalize_export_fidelity_counts(&json!([1, 2, 3])).is_none());
    }

    #[test]
    fn r682_normalize_counts_rejects_missing_key() {
        let v = json!({
            "labelDefinitions": 5,
            "issueLabelReferences": 0,
            // 缺 issueBlockerRelations
            "issueDocuments": 1,
            "issueWorkProducts": 0,
            "issueAttachments": 7,
            "approvals": 0,
            "costEvents": 0,
            "activityLogEntries": 0,
            "issueMonitors": 1
        });
        assert!(normalize_export_fidelity_counts(&v).is_none());
    }

    #[test]
    fn r682_normalize_counts_rejects_negative() {
        let v = json!({
            "labelDefinitions": -1,
            "issueLabelReferences": 0,
            "issueBlockerRelations": 0,
            "issueDocuments": 0,
            "issueWorkProducts": 0,
            "issueAttachments": 0,
            "approvals": 0,
            "costEvents": 0,
            "activityLogEntries": 0,
            "issueMonitors": 0
        });
        assert!(normalize_export_fidelity_counts(&v).is_none());
    }

    #[test]
    fn r682_normalize_counts_rejects_non_finite() {
        // JSON 不允许 NaN/Infinity —— 改测类型错（"abc" 而非 number）
        let v = json!({
            "labelDefinitions": "abc",
            "issueLabelReferences": 0,
            "issueBlockerRelations": 0,
            "issueDocuments": 0,
            "issueWorkProducts": 0,
            "issueAttachments": 0,
            "approvals": 0,
            "costEvents": 0,
            "activityLogEntries": 0,
            "issueMonitors": 0
        });
        assert!(normalize_export_fidelity_counts(&v).is_none());
    }

    #[test]
    fn r682_build_report_includes_schema_and_warnings() {
        let mut counts = zero_counts();
        counts.insert("approvals".to_string(), 1);
        let report = build_export_fidelity_report(
            "company-1",
            counts.clone(),
            Some(chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z").unwrap().with_timezone(&chrono::Utc)),
        );
        assert_eq!(report.schema, EXPORT_FIDELITY_REPORT_SCHEMA);
        assert_eq!(report.company_id, "company-1");
        assert_eq!(report.counts.get("labelDefinitions"), Some(&0));
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(report.generated_at, "2026-01-01T00:00:00+00:00");
    }

    #[test]
    fn r682_build_report_uses_now_when_no_timestamp() {
        let counts = zero_counts();
        let before = chrono::Utc::now();
        let report = build_export_fidelity_report("c-1", counts, None);
        let after = chrono::Utc::now();
        // generated_at 应在 [before, after] 之间
        let gen: chrono::DateTime<chrono::Utc> =
            chrono::DateTime::parse_from_rfc3339(&report.generated_at).unwrap().with_timezone(&chrono::Utc);
        assert!(gen >= before && gen <= after);
    }

    #[test]
    fn r682_report_json_roundtrip() {
        let mut counts = zero_counts();
        counts.insert("approvals".to_string(), 3);
        let report = build_export_fidelity_report(
            "company-json",
            counts,
            Some(chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z").unwrap().with_timezone(&chrono::Utc)),
        );
        let s = serde_json::to_string(&report).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&s).expect("parse");
        assert_eq!(v["schema"], json!("paperclip-export-fidelity-v1"));
        assert_eq!(v["companyId"], json!("company-json"));
        assert_eq!(v["counts"]["approvals"], json!(3));
        assert!(v["warnings"].is_array());
        assert!(v["warnings"].as_array().unwrap().len() == 1);
    }
}
