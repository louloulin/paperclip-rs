#![forbid(unsafe_code)]
#![allow(clippy::doc_markdown)]

//! Portability export fidelity reporting.
//!
//! R547: Direct port of `paperclip/packages/shared/src/portability-fidelity.ts`.
//! Pure functions over counts of supported / unsupported data categories.

/// Fidelity report schema version. Bump if `ExportFidelityReport` shape changes.
pub const EXPORT_FIDELITY_REPORT_SCHEMA: &str = "paperclip-export-fidelity-v1";

/// All numeric count keys tracked by an export fidelity report.
///
/// Order is preserved across the wire — callers that deserialize reports
/// from JSON rely on the array order for stable logging.
pub const EXPORT_FIDELITY_COUNT_KEYS: [&str; 10] = [
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
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PortabilityFidelitySeverity {
    Info,
    Warning,
    Blocker,
}

impl PortabilityFidelitySeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Blocker => "blocker",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortabilityFidelityWarning {
    pub code: String,
    pub severity: PortabilityFidelitySeverity,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExportFidelityCounts {
    pub label_definitions: u64,
    pub issue_label_references: u64,
    pub issue_blocker_relations: u64,
    pub issue_documents: u64,
    pub issue_work_products: u64,
    pub issue_attachments: u64,
    pub approvals: u64,
    pub cost_events: u64,
    pub activity_log_entries: u64,
    pub issue_monitors: u64,
}

impl ExportFidelityCounts {
    pub fn zero() -> Self {
        Self::default()
    }

    fn get_by_key(&self, key: &str) -> u64 {
        match key {
            "labelDefinitions" => self.label_definitions,
            "issueLabelReferences" => self.issue_label_references,
            "issueBlockerRelations" => self.issue_blocker_relations,
            "issueDocuments" => self.issue_documents,
            "issueWorkProducts" => self.issue_work_products,
            "issueAttachments" => self.issue_attachments,
            "approvals" => self.approvals,
            "costEvents" => self.cost_events,
            "activityLogEntries" => self.activity_log_entries,
            "issueMonitors" => self.issue_monitors,
            _ => 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportFidelityReport {
    pub schema: String,
    pub company_id: String,
    pub counts: ExportFidelityCounts,
    pub warnings: Vec<PortabilityFidelityWarning>,
    pub generated_at: String,
}

#[derive(Debug, Clone, Copy)]
struct UnsupportedCategory {
    code: &'static str,
    count_key: &'static str,
    singular: &'static str,
    plural: &'static str,
}

const UNSUPPORTED_CATEGORIES: [UnsupportedCategory; 3] = [
    UnsupportedCategory {
        code: "approvals_not_exported",
        count_key: "approvals",
        singular: "approval",
        plural: "approvals",
    },
    UnsupportedCategory {
        code: "cost_history_not_exported",
        count_key: "costEvents",
        singular: "cost event",
        plural: "cost events",
    },
    UnsupportedCategory {
        code: "activity_history_not_exported",
        count_key: "activityLogEntries",
        singular: "activity log entry",
        plural: "activity log entries",
    },
];

/// Build fidelity warnings for any data category whose count is positive
/// but is not currently carried by the export bundle.
///
/// Mirrors `buildExportFidelityWarnings` from `portability-fidelity.ts`.
pub fn build_export_fidelity_warnings(
    counts: &ExportFidelityCounts,
) -> Vec<PortabilityFidelityWarning> {
    UNSUPPORTED_CATEGORIES
        .iter()
        .filter_map(|cat| {
            let row_count = counts.get_by_key(cat.count_key);
            if row_count == 0 {
                return None;
            }
            let noun = if row_count == 1 {
                cat.singular
            } else {
                cat.plural
            };
            let verb = if row_count == 1 { "is" } else { "are" };
            Some(PortabilityFidelityWarning {
                code: cat.code.to_string(),
                severity: PortabilityFidelitySeverity::Warning,
                message: format!("{row_count} {noun} {verb} not included in the export bundle."),
            })
        })
        .collect()
}

/// Normalize an arbitrary `serde_json::Value`-like map into an `ExportFidelityCounts`.
///
/// Returns `None` if the input is not an object, missing any required key,
/// or contains any non-finite / negative numeric value.
///
/// In the Rust port we accept a generic `&serde_json::Value` via the
/// `JsonValue` trait alias below so callers can plug in any JSON library.
pub fn normalize_export_fidelity_counts(value: &serde_json::Value) -> Option<ExportFidelityCounts> {
    let obj = value.as_object()?;
    let mut counts = ExportFidelityCounts::zero();
    for key in EXPORT_FIDELITY_COUNT_KEYS {
        let raw = obj.get(key)?;
        let n = raw.as_u64()?;
        counts.set_by_key(key, n);
    }
    Some(counts)
}

impl ExportFidelityCounts {
    fn set_by_key(&mut self, key: &str, value: u64) {
        match key {
            "labelDefinitions" => self.label_definitions = value,
            "issueLabelReferences" => self.issue_label_references = value,
            "issueBlockerRelations" => self.issue_blocker_relations = value,
            "issueDocuments" => self.issue_documents = value,
            "issueWorkProducts" => self.issue_work_products = value,
            "issueAttachments" => self.issue_attachments = value,
            "approvals" => self.approvals = value,
            "costEvents" => self.cost_events = value,
            "activityLogEntries" => self.activity_log_entries = value,
            "issueMonitors" => self.issue_monitors = value,
            _ => {}
        }
    }
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    fn zero_counts() -> ExportFidelityCounts {
        ExportFidelityCounts::zero()
    }

    #[test]
    fn warning_zero_returns_empty() {
        assert!(build_export_fidelity_warnings(&zero_counts()).is_empty());
    }

    #[test]
    fn warning_supported_positive_returns_empty() {
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
    fn warning_unsupported_positive_emits_in_declared_order() {
        let counts = ExportFidelityCounts {
            approvals: 5,
            cost_events: 6,
            activity_log_entries: 7,
            ..ExportFidelityCounts::zero()
        };
        let warnings = build_export_fidelity_warnings(&counts);
        assert_eq!(warnings.len(), 3);
        assert_eq!(warnings[0].code, "approvals_not_exported");
        assert_eq!(warnings[1].code, "cost_history_not_exported");
        assert_eq!(warnings[2].code, "activity_history_not_exported");
        for w in &warnings {
            assert_eq!(w.severity, PortabilityFidelitySeverity::Warning);
        }
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
    fn warning_singular_for_count_one() {
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
}
