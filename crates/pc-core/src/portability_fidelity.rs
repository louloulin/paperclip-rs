//! Portability fidelity: counts + warnings for company export bundles.
//!
//! 对齐 Node `packages/shared/src/portability-fidelity.ts`：
//! - 10 项 `ExportFidelityCountKey` 固定字段名（与 JSON 形状 1:1）
//! - `buildExportFidelityWarnings` 在 `approvals / costEvents / activityLogEntries`
//!   三个未导出分类上有非零计数时产出 warning
//! - `normalizeExportFidelityCounts` 校验非法输入并产出零拷贝的强类型 counts

use serde::{Deserialize, Serialize};

/// Portability fidelity 告警的严重等级。
///
/// 对齐 Node `PortabilityFidelitySeverity`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PortabilityFidelitySeverity {
    Info,
    Warning,
    Blocker,
}

impl PortabilityFidelitySeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Blocker => "blocker",
        }
    }
}

/// 一条 portability fidelity 告警。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortabilityFidelityWarning {
    pub code: String,
    pub severity: PortabilityFidelitySeverity,
    pub message: String,
}

/// 导出保真度报告的 schema 版本常量。
///
/// 对齐 Node `EXPORT_FIDELITY_REPORT_SCHEMA`。
pub const EXPORT_FIDELITY_REPORT_SCHEMA: &str = "paperclip-export-fidelity-v1";

/// 10 个 count 字段的有序键集合。
///
/// 对齐 Node `EXPORT_FIDELITY_COUNT_KEYS`。
pub const EXPORT_FIDELITY_COUNT_KEYS: &[&str] = &[
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

/// 10 项不可变 `i64` 计数值的强类型 record。
///
/// 与 Node `ExportFidelityCounts` 等价；通过 `Default` 构造零值，
/// 允许调用方按字段名一一写入。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportFidelityCounts {
    #[serde(default)]
    pub label_definitions: i64,
    #[serde(default)]
    pub issue_label_references: i64,
    #[serde(default)]
    pub issue_blocker_relations: i64,
    #[serde(default)]
    pub issue_documents: i64,
    #[serde(default)]
    pub issue_work_products: i64,
    #[serde(default)]
    pub issue_attachments: i64,
    #[serde(default)]
    pub approvals: i64,
    #[serde(default)]
    pub cost_events: i64,
    #[serde(default)]
    pub activity_log_entries: i64,
    #[serde(default)]
    pub issue_monitors: i64,
}

impl ExportFidelityCounts {
    /// 全 0 counts（与 Node 测试中 `zeroCounts` 1:1）。
    pub const ZERO: Self = Self {
        label_definitions: 0,
        issue_label_references: 0,
        issue_blocker_relations: 0,
        issue_documents: 0,
        issue_work_products: 0,
        issue_attachments: 0,
        approvals: 0,
        cost_events: 0,
        activity_log_entries: 0,
        issue_monitors: 0,
    };
}

/// 完整的 export fidelity report。
///
/// 对齐 Node `ExportFidelityReport`：schema + companyId + counts + warnings + generatedAt。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportFidelityReport {
    pub schema: String,
    pub company_id: String,
    pub counts: ExportFidelityCounts,
    pub warnings: Vec<PortabilityFidelityWarning>,
    pub generated_at: String,
}

/// 未导出数据告警条目：(code, count 字段名, 单数文案, 复数文案)。
///
/// `count_key` 字段名采用与 `ExportFidelityCounts` JSON 序列化相同的小写 camelCase
/// 形式（与 `EXPORT_FIDELITY_COUNT_KEYS` 一致），便于通过 `field_by_str` 查找。
/// 对齐 Node `UNSUPPORTED_DATA_WARNINGS`。
const UNSUPPORTED_DATA_WARNINGS: &[(&str, &str, &str, &str)] = &[
    (
        "approvals_not_exported",
        "approvals",
        "approval",
        "approvals",
    ),
    (
        "cost_history_not_exported",
        "costEvents",
        "cost event",
        "cost events",
    ),
    (
        "activity_history_not_exported",
        "activityLogEntries",
        "activity log entry",
        "activity log entries",
    ),
];

/// 根据 counts 构造告警列表。
///
/// - `approvals / cost_events / activity_log_entries` 三类在 count > 0 时各产出一条
///   severity=warning 的告警；
/// - 文案复刻 Node：`"<N> <singular|plural> is|are not included in the export bundle."`。
///
/// 对齐 Node `buildExportFidelityWarnings`。
#[must_use]
pub fn build_export_fidelity_warnings(
    counts: &ExportFidelityCounts,
) -> Vec<PortabilityFidelityWarning> {
    let mut warnings = Vec::new();
    for (code, count_key, singular, plural) in UNSUPPORTED_DATA_WARNINGS {
        let value = counts.field_by_str(count_key);
        if value <= 0 {
            continue;
        }
        let noun = if value == 1 { singular } else { plural };
        let verb = if value == 1 { "is" } else { "are" };
        warnings.push(PortabilityFidelityWarning {
            code: (*code).to_string(),
            severity: PortabilityFidelitySeverity::Warning,
            message: format!("{value} {noun} {verb} not included in the export bundle."),
        });
    }
    warnings
}

/// 校验并归一化未知输入为 `ExportFidelityCounts`。
///
/// 拒绝非 object / 数组 / 字符串；要求 10 个 count 字段全部为非负有限数。
/// 对齐 Node `normalizeExportFidelityCounts`。
#[must_use]
pub fn normalize_export_fidelity_counts(value: &serde_json::Value) -> Option<ExportFidelityCounts> {
    let serde_json::Value::Object(record) = value else {
        return None;
    };
    let mut counts = ExportFidelityCounts::default();
    for key in EXPORT_FIDELITY_COUNT_KEYS {
        let raw = record.get(*key)?;
        let parsed = raw.as_i64()?;
        if parsed < 0 {
            return None;
        }
        counts.set_field_by_str(key, parsed);
    }
    Some(counts)
}

impl ExportFidelityCounts {
    /// 通过 JSON 字段名（camelCase）查询计数值。
    pub fn field_by_str(&self, key: &str) -> i64 {
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

    /// 通过 JSON 字段名（camelCase）写入计数值。
    fn set_field_by_str(&mut self, key: &str, value: i64) {
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
mod tests {
    use super::*;
    use serde_json::json;
    use serde_json::Value;

    fn zero_counts() -> ExportFidelityCounts {
        ExportFidelityCounts::ZERO
    }

    #[test]
    fn schema_and_keys_match_node() {
        assert_eq!(
            EXPORT_FIDELITY_REPORT_SCHEMA,
            "paperclip-export-fidelity-v1"
        );
        assert_eq!(
            EXPORT_FIDELITY_COUNT_KEYS,
            &[
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
    fn zero_counts_is_all_zeros() {
        let counts = ExportFidelityCounts::ZERO;
        for key in EXPORT_FIDELITY_COUNT_KEYS {
            assert_eq!(counts.field_by_str(key), 0, "field {key} should be 0");
        }
    }

    #[test]
    fn default_zero_counts_match_constant() {
        assert_eq!(ExportFidelityCounts::default(), ExportFidelityCounts::ZERO);
    }

    #[test]
    fn severity_serializes_lowercase() {
        assert_eq!(PortabilityFidelitySeverity::Info.as_str(), "info");
        assert_eq!(PortabilityFidelitySeverity::Warning.as_str(), "warning");
        assert_eq!(PortabilityFidelitySeverity::Blocker.as_str(), "blocker");
        assert_eq!(
            serde_json::to_string(&PortabilityFidelitySeverity::Info).unwrap(),
            "\"info\""
        );
        assert_eq!(
            serde_json::to_string(&PortabilityFidelitySeverity::Warning).unwrap(),
            "\"warning\""
        );
        assert_eq!(
            serde_json::to_string(&PortabilityFidelitySeverity::Blocker).unwrap(),
            "\"blocker\""
        );
    }

    #[test]
    fn warnings_empty_when_all_counts_are_zero() {
        let warnings = build_export_fidelity_warnings(&zero_counts());
        assert!(warnings.is_empty());
    }

    #[test]
    fn warnings_skip_zero_even_when_supported_categories_have_rows() {
        let counts = ExportFidelityCounts {
            label_definitions: 2,
            issue_label_references: 3,
            issue_blocker_relations: 2,
            issue_documents: 1,
            issue_work_products: 3,
            issue_attachments: 4,
            issue_monitors: 8,
            ..ExportFidelityCounts::ZERO
        };
        assert!(build_export_fidelity_warnings(&counts).is_empty());
    }

    #[test]
    fn warnings_emit_for_each_unsupported_category() {
        let counts = ExportFidelityCounts {
            approvals: 5,
            cost_events: 6,
            activity_log_entries: 7,
            ..ExportFidelityCounts::ZERO
        };
        let warnings = build_export_fidelity_warnings(&counts);
        assert_eq!(
            warnings.iter().map(|w| w.code.as_str()).collect::<Vec<_>>(),
            vec![
                "approvals_not_exported",
                "cost_history_not_exported",
                "activity_history_not_exported",
            ]
        );
        for warning in &warnings {
            assert_eq!(warning.severity, PortabilityFidelitySeverity::Warning);
        }
    }

    #[test]
    fn warning_message_singular_vs_plural() {
        let one = ExportFidelityCounts {
            approvals: 1,
            ..ExportFidelityCounts::ZERO
        };
        assert_eq!(
            build_export_fidelity_warnings(&one)[0].message,
            "1 approval is not included in the export bundle."
        );

        let many = ExportFidelityCounts {
            cost_events: 6,
            ..ExportFidelityCounts::ZERO
        };
        assert_eq!(
            build_export_fidelity_warnings(&many)[0].message,
            "6 cost events are not included in the export bundle."
        );
    }

    #[test]
    fn normalize_round_trips_valid_counts() {
        let counts = ExportFidelityCounts {
            issue_attachments: 12,
            ..ExportFidelityCounts::ZERO
        };
        let value = serde_json::to_value(&counts).unwrap();
        assert_eq!(normalize_export_fidelity_counts(&value), Some(counts));
    }

    #[test]
    fn normalize_rejects_non_objects() {
        assert!(normalize_export_fidelity_counts(&Value::Null).is_none());
        assert!(normalize_export_fidelity_counts(&json!([])).is_none());
        assert!(normalize_export_fidelity_counts(&json!("counts")).is_none());
        assert!(normalize_export_fidelity_counts(&json!(42)).is_none());
    }

    #[test]
    fn normalize_rejects_missing_keys() {
        let mut partial = zero_counts();
        partial.issue_monitors = 0;
        let mut value = serde_json::to_value(&partial).unwrap();
        value.as_object_mut().unwrap().remove("issueMonitors");
        assert!(normalize_export_fidelity_counts(&value).is_none());
    }

    #[test]
    fn normalize_rejects_negative_or_non_finite_values() {
        for bad in [
            json!({ "approvals": -1, "labelDefinitions": 0, "issueLabelReferences": 0, "issueBlockerRelations": 0,
                    "issueDocuments": 0, "issueWorkProducts": 0, "issueAttachments": 0, "costEvents": 0,
                    "activityLogEntries": 0, "issueMonitors": 0 }),
            json!({ "approvals": 1.5, "labelDefinitions": 0, "issueLabelReferences": 0, "issueBlockerRelations": 0,
                    "issueDocuments": 0, "issueWorkProducts": 0, "issueAttachments": 0, "costEvents": 0,
                    "activityLogEntries": 0, "issueMonitors": 0 }),
        ] {
            assert!(
                normalize_export_fidelity_counts(&bad).is_none(),
                "should reject {bad}"
            );
        }
    }

    #[test]
    fn counts_round_trip_via_serde_with_camel_case_keys() {
        let counts = ExportFidelityCounts {
            label_definitions: 1,
            issue_label_references: 2,
            issue_blocker_relations: 3,
            issue_documents: 4,
            issue_work_products: 5,
            issue_attachments: 6,
            approvals: 7,
            cost_events: 8,
            activity_log_entries: 9,
            issue_monitors: 10,
        };
        let value = serde_json::to_value(&counts).unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(object["labelDefinitions"], 1);
        assert_eq!(object["issueLabelReferences"], 2);
        assert_eq!(object["approvals"], 7);
        assert_eq!(object["issueMonitors"], 10);
        let parsed: ExportFidelityCounts = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, counts);
    }
}
