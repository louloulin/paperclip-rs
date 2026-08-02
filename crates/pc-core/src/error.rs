//! 领域层错误（不变量违反）。与基础设施错误（pc-errors）解耦。

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("invalid identifier: {0}")]
    InvalidId(String),
    #[error("invariant violated on {entity}: {message}")]
    InvariantViolation { entity: &'static str, message: String },
    #[error("empty value not allowed for {field}")]
    EmptyField { field: &'static str },
}

pub type CoreResult<T> = std::result::Result<T, CoreError>;
