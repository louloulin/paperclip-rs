#![forbid(unsafe_code)]
//! `pc-export-fidelity` —— Export fidelity report。
//!
//! 对应 Node `server/src/services/export-fidelity.ts`（83 行） + shared `portability-fidelity.ts`。
//!
//! ## 设计目标
//!
//! - **`ExportFidelityCounts`**：公司维度各表行数（labels / issue_labels / relations / documents /
//!   work_products / attachments / approvals / cost_events / activity_log / monitors）。
//! - **`ExportFidelityWarning`**：导出包不含的字段，给出 warning（如 `approvals_not_exported`）。
//! - **报告结构**：`{ schema, companyId, counts, warnings, generatedAt }`。
//!
//! ## 公共 API
//!
//! - [`collect_export_fidelity_counts`] —— DB 聚合查询（10 个 COUNT）
//! - [`build_export_fidelity_warnings`] —— 纯函数：根据 counts 推断 warnings
//! - [`build_export_fidelity_report`] —— 顶层：counts + warnings + schema 包装
//! - [`normalize_export_fidelity_counts`] —— 输入校验（防止外部 JSON 输入）
//! - [`EXPORT_FIDELITY_REPORT_SCHEMA`] —— schema 版本字符串
//! - [`EXPORT_FIDELITY_COUNT_KEYS`] —— count key 元组
//! - [`UnsupportedDataWarningSpec`] —— 不可导出数据警告规格
//!
//! ## 设计原则
//!
//! - **高内聚**：counts / warnings / report 集中在本 crate。
//! - **低耦合**：pure functions 与 DB query 可独立测试。
//! - **可测**：纯函数单测 + 真实 DB e2e 测试。

mod builder;
mod collector;
mod types;

pub use builder::{
    build_export_fidelity_report, build_export_fidelity_warnings, normalize_export_fidelity_counts
};
pub use collector::collect_export_fidelity_counts;
pub use types::{
    codes, ExportFidelityCounts, ExportFidelityReport, PortabilityFidelitySeverity,
    PortabilityFidelityWarning, EXPORT_FIDELITY_COUNT_KEYS, EXPORT_FIDELITY_REPORT_SCHEMA,
    UnsupportedDataWarningSpec,
};
