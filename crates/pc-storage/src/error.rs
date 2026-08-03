//! 存储错误类型。

use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialize error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("invalid argument: {0}")]
    Invalid(String),
    #[error("integrity check failed: expected {expected}, got {actual}")]
    Integrity { expected: String, actual: String },
    #[error("provider not configured: {0}")]
    ProviderUnavailable(String),
    #[error("not implemented: {0}")]
    NotImplemented(String),
    #[error("timeout after {0:?}")]
    Timeout(std::time::Duration),
}

pub type StorageResult<T> = Result<T, StorageError>;
