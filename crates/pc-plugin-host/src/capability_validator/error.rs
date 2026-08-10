//! Validator 错误类型。
//!
//! 高内聚：所有 validator 抛出的错误都在这。
//! 低耦合：只依赖 `thiserror`，零业务依赖。

use thiserror::Error;

/// Capability 检查失败错误（与 Node `forbidden(msg)` 1:1 对齐）。
///
/// `assert_*` 系列方法失败时抛此错。
#[derive(Debug, Error)]
#[error("{message}")]
pub struct ForbiddenError {
    pub message: String,
}

impl ForbiddenError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}
