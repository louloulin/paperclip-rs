//! Plugin job store 错误类型。
//!
//! 设计：service 层错误统一表达，避免暴露 sqlx 原始类型给上层。
//! 通过 `From<pc_repos::RepoError>` 与 `From<sqlx::Error>` 自动转换。

use thiserror::Error;

use pc_repos::RepoError;

/// Plugin job store 错误（与 Node throw / 仓储错误 1:1 对齐）。
#[derive(Debug, Error)]
pub enum PluginJobStoreError {
    /// Plugin 不存在（Node `notFound` 错误）。
    #[error("plugin not found: {0}")]
    PluginNotFound(String),

    /// 仓储层错误（sqlx / not_found / json / core 等）。
    #[error("repository error: {0}")]
    Repository(#[from] RepoError),

    /// sqlx 错误（直接透传；当仓储方法返回 `sqlx::Result` 而非 `RepoResult` 时使用）。
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
}

/// `Result<T, PluginJobStoreError>` 的简写别名。
pub type PluginJobStoreResult<T> = std::result::Result<T, PluginJobStoreError>;
