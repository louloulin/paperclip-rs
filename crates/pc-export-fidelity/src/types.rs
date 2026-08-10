//! Types —— Export fidelity DTOs、常量、错误码。
//!
//! 与 Node `shared/portability-fidelity.ts` 1:1 对齐。

use serde::{Deserialize, Serialize};

// ============================================================================
// Constants
// ============================================================================

/// Schema 版本字符串（与 Node `EXPORT_FIDELITY_REPORT_SCHEMA` 1:1 对齐）。
pub const EXPORT_FIDELITY_REPORT_SCHEMA: &str = "paperclip-export-fidelity-v1";

/// Count key 元组（与 Node `EXPORT_FIDELITY_COUNT_KEYS` 1:1 对齐）。
///
/// 按 Node 顺序固定，方便下游稳定解析 / debug 输出。
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

// ============================================================================
// Types
// ============================================================================

/// 警告严重级。
///
/// 与 Node `PortabilityFidelitySeverity = "info" | "warning" | "blocker"` 1:1 对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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

/// 单条警告。
///
/// 与 Node `PortabilityFidelityWarning` 1:1 对齐。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortabilityFidelityWarning {
    pub code: String,
    pub severity: PortabilityFidelitySeverity,
    pub message: String,
}

/// Counts 字典：每个 key 是非负整数。
///
/// 与 Node `ExportFidelityCounts` 1:1 对齐。
pub type ExportFidelityCounts = std::collections::BTreeMap<String, i64>;

/// 完整报告。
///
/// 与 Node `ExportFidelityReport` 1:1 对齐。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportFidelityReport {
    pub schema: String,
    pub company_id: String,
    pub counts: ExportFidelityCounts,
    pub warnings: Vec<PortabilityFidelityWarning>,
    pub generated_at: String,
}

/// 不可导出数据警告规格。
///
/// 与 Node `UNSUPPORTED_DATA_WARNINGS`（被 `buildExportFidelityWarnings` 引用）1:1 对齐。
#[derive(Debug, Clone, Copy)]
pub struct UnsupportedDataWarningSpec {
    /// 警告代码（snake_case 短码）。
    pub code: &'static str,
    /// 对应的 count key（必须存在于 `ExportFidelityCounts`）。
    pub count_key: &'static str,
    /// 单数描述（用当 count = 1 时）。
    pub singular: &'static str,
    /// 复数描述（用当 count != 1 时）。
    pub plural: &'static str,
}

/// 默认的不可导出数据警告规格（与 Node 1:1 对齐）。
///
/// - `approvals_not_exported` (approvals)
/// - `cost_history_not_exported` (costEvents)
/// - `activity_history_not_exported` (activityLogEntries)
pub const UNSUPPORTED_DATA_WARNINGS: &[UnsupportedDataWarningSpec] = &[
    UnsupportedDataWarningSpec { code: "approvals_not_exported", count_key: "approvals", singular: "approval", plural: "approvals" },
    UnsupportedDataWarningSpec { code: "cost_history_not_exported", count_key: "costEvents", singular: "cost event", plural: "cost events" },
    UnsupportedDataWarningSpec { code: "activity_history_not_exported", count_key: "activityLogEntries", singular: "activity log entry", plural: "activity log entries" },
];

// ============================================================================
// Error codes (for parser failure messages)
// ============================================================================

/// Error code constants（暴露给 caller 用于诊断 / log）。
///
/// 注：Node 端导出函数不抛异常（`normalizeExportFidelityCounts` 返回 `null`）。
/// 这里只是为 Rust API 提供的错误码文档说明。
pub mod codes {
    /// `counts` 非对象或 null 时（Node `!value || typeof !== "object"`）。
    pub const INVALID_NOT_OBJECT: &str = "export_fidelity_invalid_not_object";
    /// 缺某个必需 key 或 key 类型错误。
    pub const INVALID_MISSING_KEY: &str = "export_fidelity_invalid_missing_key";
    /// 数字字段非有限值或负值。
    pub const INVALID_NUMBER: &str = "export_fidelity_invalid_number";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r682_schema_constant_matches_node() {
        assert_eq!(EXPORT_FIDELITY_REPORT_SCHEMA, "paperclip-export-fidelity-v1");
    }

    #[test]
    fn r682_count_keys_match_node() {
        assert_eq!(EXPORT_FIDELITY_COUNT_KEYS.len(), 10);
        assert_eq!(EXPORT_FIDELITY_COUNT_KEYS[0], "labelDefinitions");
        assert_eq!(EXPORT_FIDELITY_COUNT_KEYS[9], "issueMonitors");
    }

    #[test]
    fn r682_unsupported_warning_specs_match_node() {
        assert_eq!(UNSUPPORTED_DATA_WARNINGS.len(), 3);
        assert_eq!(UNSUPPORTED_DATA_WARNINGS[0].code, "approvals_not_exported");
        assert_eq!(UNSUPPORTED_DATA_WARNINGS[1].code, "cost_history_not_exported");
        assert_eq!(UNSUPPORTED_DATA_WARNINGS[2].code, "activity_history_not_exported");
    }

    #[test]
    fn r682_severity_as_str() {
        assert_eq!(PortabilityFidelitySeverity::Info.as_str(), "info");
        assert_eq!(PortabilityFidelitySeverity::Warning.as_str(), "warning");
        assert_eq!(PortabilityFidelitySeverity::Blocker.as_str(), "blocker");
    }
}
