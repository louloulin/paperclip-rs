//! R547 — pc-portability-fidelity 综合测试。

#![allow(clippy::doc_markdown)]

use pc_portability_fidelity::{
    build_export_fidelity_warnings, normalize_export_fidelity_counts, ExportFidelityCounts,
    PortabilityFidelitySeverity, EXPORT_FIDELITY_COUNT_KEYS, EXPORT_FIDELITY_REPORT_SCHEMA,
};
use serde_json::json;

fn zero_counts() -> ExportFidelityCounts {
    ExportFidelityCounts::zero()
}

fn zero_json() -> serde_json::Value {
    json!({
        "labelDefinitions": 0,
        "issueLabelReferences": 0,
        "issueBlockerRelations": 0,
        "issueDocuments": 0,
        "issueWorkProducts": 0,
        "issueAttachments": 0,
        "approvals": 0,
        "costEvents": 0,
        "activityLogEntries": 0,
        "issueMonitors": 0,
    })
}

#[test]
fn r547_schema_constant_is_stable() {
    assert_eq!(
        EXPORT_FIDELITY_REPORT_SCHEMA,
        "paperclip-export-fidelity-v1"
    );
}

#[test]
fn r547_count_keys_in_declared_order() {
    assert_eq!(
        EXPORT_FIDELITY_COUNT_KEYS,
        [
            "labelDefinitions",
            "issueLabelReferences",
            "issueBlockerRelations",
            "issueDocuments",
            "issueWorkProducts",
            "issueAttachments",
            "approvals",
            "costEvents",
            "activityLogEntries",
            "issueMonitors",
        ]
    );
}

#[test]
fn r547_build_warnings_zero_returns_empty() {
    let warnings = build_export_fidelity_warnings(&zero_counts());
    assert!(warnings.is_empty());
}

#[test]
fn r547_build_warnings_supported_positive_returns_empty() {
    let counts = ExportFidelityCounts {
        label_definitions: 2,
        issue_label_references: 3,
        issue_blocker_relations: 2,
        issue_documents: 1,
        issue_work_products: 3,
        issue_attachments: 4,
        issue_monitors: 8,
        ..ExportFidelityCounts::zero()
    };
    assert!(build_export_fidelity_warnings(&counts).is_empty());
}

#[test]
fn r547_build_warnings_unsupported_emits_in_declared_order() {
    let counts = ExportFidelityCounts {
        approvals: 5,
        cost_events: 6,
        activity_log_entries: 7,
        ..ExportFidelityCounts::zero()
    };
    let warnings = build_export_fidelity_warnings(&counts);
    assert_eq!(warnings.len(), 3);
    let codes: Vec<_> = warnings.iter().map(|w| w.code.as_str()).collect();
    assert_eq!(
        codes,
        vec![
            "approvals_not_exported",
            "cost_history_not_exported",
            "activity_history_not_exported",
        ]
    );
    for w in &warnings {
        assert_eq!(w.severity, PortabilityFidelitySeverity::Warning);
    }
}

#[test]
fn r547_build_warnings_plural_messages() {
    let counts = ExportFidelityCounts {
        approvals: 5,
        cost_events: 6,
        activity_log_entries: 7,
        ..ExportFidelityCounts::zero()
    };
    let warnings = build_export_fidelity_warnings(&counts);
    assert_eq!(
        warnings[0].message,
        "5 approvals are not included in the export bundle."
    );
    assert_eq!(
        warnings[1].message,
        "6 cost events are not included in the export bundle."
    );
    assert_eq!(
        warnings[2].message,
        "7 activity log entries are not included in the export bundle."
    );
}

#[test]
fn r547_build_warnings_singular_for_count_one() {
    let counts = ExportFidelityCounts {
        approvals: 1,
        ..ExportFidelityCounts::zero()
    };
    let warnings = build_export_fidelity_warnings(&counts);
    assert_eq!(warnings.len(), 1);
    assert_eq!(
        warnings[0].message,
        "1 approval is not included in the export bundle."
    );
}

#[test]
fn r547_normalize_round_trip_valid() {
    let mut counts = zero_counts();
    counts.issue_attachments = 12;
    let json_value = json!({
        "labelDefinitions": 0,
        "issueLabelReferences": 0,
        "issueBlockerRelations": 0,
        "issueDocuments": 0,
        "issueWorkProducts": 0,
        "issueAttachments": 12,
        "approvals": 0,
        "costEvents": 0,
        "activityLogEntries": 0,
        "issueMonitors": 0,
    });
    let parsed = normalize_export_fidelity_counts(&json_value).unwrap();
    assert_eq!(parsed, counts);
}

#[test]
fn r547_normalize_rejects_null() {
    assert!(normalize_export_fidelity_counts(&json!(null)).is_none());
}

#[test]
fn r547_normalize_rejects_array() {
    assert!(normalize_export_fidelity_counts(&json!([])).is_none());
}

#[test]
fn r547_normalize_rejects_string() {
    assert!(normalize_export_fidelity_counts(&json!("counts")).is_none());
}

#[test]
fn r547_normalize_rejects_missing_keys() {
    let mut v = zero_json();
    v.as_object_mut().unwrap().remove("issueMonitors");
    assert!(normalize_export_fidelity_counts(&v).is_none());
}

#[test]
fn r547_normalize_rejects_negative() {
    let v = json!({
        "labelDefinitions": 0,
        "issueLabelReferences": 0,
        "issueBlockerRelations": 0,
        "issueDocuments": 0,
        "issueWorkProducts": 0,
        "issueAttachments": 0,
        "approvals": -1,
        "costEvents": 0,
        "activityLogEntries": 0,
        "issueMonitors": 0,
    });
    assert!(normalize_export_fidelity_counts(&v).is_none());
}

#[test]
fn r547_normalize_rejects_non_integer_number() {
    let v = json!({
        "labelDefinitions": 0,
        "issueLabelReferences": 0,
        "issueBlockerRelations": 0,
        "issueDocuments": 0,
        "issueWorkProducts": 0,
        "issueAttachments": 0,
        "approvals": 1.5,
        "costEvents": 0,
        "activityLogEntries": 0,
        "issueMonitors": 0,
    });
    assert!(normalize_export_fidelity_counts(&v).is_none());
}

#[test]
fn r547_normalize_accepts_large_u64() {
    let v = json!({
        "labelDefinitions": 0,
        "issueLabelReferences": 0,
        "issueBlockerRelations": 0,
        "issueDocuments": 0,
        "issueWorkProducts": 0,
        "issueAttachments": 0,
        "approvals": u64::MAX,
        "costEvents": 0,
        "activityLogEntries": 0,
        "issueMonitors": 0,
    });
    let parsed = normalize_export_fidelity_counts(&v).unwrap();
    assert_eq!(parsed.approvals, u64::MAX);
}

#[test]
fn r547_severity_as_str() {
    assert_eq!(PortabilityFidelitySeverity::Info.as_str(), "info");
    assert_eq!(PortabilityFidelitySeverity::Warning.as_str(), "warning");
    assert_eq!(PortabilityFidelitySeverity::Blocker.as_str(), "blocker");
}
