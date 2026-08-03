//! 备份错误类型。

use thiserror::Error;

#[derive(Debug, Error)]
pub enum BackupError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("pg_dump failed (status {status:?}): {stderr}")]
    PgDump { status: Option<i32>, stderr: String },

    #[error("pg_restore/psql failed (status {status:?}): {stderr}")]
    Restore { status: Option<i32>, stderr: String },

    #[error("invalid backup file: {0}")]
    InvalidBackup(String),

    #[error("environment error: {0}")]
    Env(String),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}
