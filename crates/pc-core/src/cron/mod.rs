//! `cron` 子模块门面：定义公开类型 + 错误 + 入口函数。
//!
//! 下沉自 `pc-cron` crate（原 crate 已删除）。
//! 内部拆分：
//! - `parse` — 表达式解析（token 切分 + 字段解析 + 边界校验）
//! - `tick` — 下次触发时间计算（按粒度跳跃，含搜索窗口保护）
//! - `tests` — 模块私有规则单测

use serde::{Deserialize, Serialize};
use thiserror::Error;

mod parse;
#[cfg(test)]
mod tests;
mod tick;

pub use parse::parse_field as _parse_field;
pub use tick::{advance_to_next_month, find_next, next_tick};

// ============================================================================
// Types
// ============================================================================

/// 一个已解析的 cron 调度。每个字段是该字段所有合法整数值的**有序去重数组**。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedCron {
    pub minutes: Vec<u32>,
    pub hours: Vec<u32>,
    pub days_of_month: Vec<u32>,
    pub months: Vec<u32>,
    pub days_of_week: Vec<u32>,
}

/// 字段边界规格。
#[derive(Debug, Clone, Copy)]
pub struct FieldSpec {
    pub min: u32,
    pub max: u32,
    pub name: &'static str,
}

/// 标准 5 字段 cron 各字段的边界规格（minute / hour / day-of-month / month / day-of-week）。
pub const FIELD_SPECS: [FieldSpec; 5] = [
    FieldSpec {
        min: 0,
        max: 59,
        name: "minute",
    },
    FieldSpec {
        min: 0,
        max: 23,
        name: "hour",
    },
    FieldSpec {
        min: 1,
        max: 31,
        name: "day of month",
    },
    FieldSpec {
        min: 1,
        max: 12,
        name: "month",
    },
    FieldSpec {
        min: 0,
        max: 6,
        name: "day of week",
    },
];

// ============================================================================
// Errors
// ============================================================================

/// cron 解析或调度计算错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CronError {
    #[error("Cron expression must not be empty")]
    Empty,
    #[error("Cron expression must have exactly 5 fields, got {got}: \"{expression}\"")]
    WrongFieldCount { expression: String, got: usize },
    #[error("Empty element in cron {field} field")]
    EmptyElement { field: &'static str },
    #[error("Invalid step \"{step}\" in cron {field} field")]
    InvalidStep { field: &'static str, step: String },
    #[error("Invalid range \"{base}\" in cron {field} field")]
    InvalidRange { field: &'static str, base: String },
    #[error("Invalid start \"{start}\" in cron {field} field")]
    InvalidStart { field: &'static str, start: String },
    #[error("Invalid range {start}-{end} in cron {field} field (start > end)")]
    InvertedRange {
        field: &'static str,
        start: i64,
        end: i64,
    },
    #[error("Invalid value \"{value}\" in cron {field} field")]
    InvalidValue { field: &'static str, value: String },
    #[error("Value {value} out of range [{min}-{max}] for cron {field} field")]
    OutOfRange {
        field: &'static str,
        value: i64,
        min: i64,
        max: i64,
    },
    #[error("Empty result for cron {field} field")]
    EmptyResult { field: &'static str },
}

// ============================================================================
// Public API
// ============================================================================

/// 解析一个 5 字段 cron 表达式。
///
/// @throws [`CronError`] 解析失败。
pub fn parse_cron(expression: &str) -> Result<ParsedCron, CronError> {
    parse::parse_cron(expression)
}

/// 校验一个 cron 表达式是否合法。返回 `None` 表示合法，返回 `Some(err)` 表示非法及错误原因。
pub fn validate_cron(expression: &str) -> Option<CronError> {
    parse_cron(expression).err()
}

/// 解析一个 cron 表达式并计算下次触发时间（默认 `now`）。
///
/// @throws [`CronError`] 表达式非法。
pub fn next_tick_from_expression(
    expression: &str,
    after: chrono::DateTime<chrono::Utc>,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, CronError> {
    let cron = parse_cron(expression)?;
    Ok(tick::next_tick(&cron, after))
}
