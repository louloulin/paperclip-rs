//! Plugin job store 错误类型。

use thiserror::Error;

use pc_repos::RepoError;

#[derive(Debug, Error)]
pub enum PluginJobStoreError {
    #[error("plugin not found: {0}")]
    PluginNotFound(String),

    #[error("repository error: {0}")]
    Repository(#[from] RepoError),

    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
}

pub type PluginJobStoreResult<T> = std::result::Result<T, PluginJobStoreError>;
