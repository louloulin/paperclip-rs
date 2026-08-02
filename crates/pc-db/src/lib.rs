//! Paperclip 数据库层。
//!
//! 单一职责：管理 `PostgreSQL` 连接池、迁移、健康检查。
//! 上层（pc-repos 等）通过 `Db` 句柄访问。

pub mod health;
pub mod migrate;
pub mod pool;

pub use health::HealthCheck;
pub use migrate::{MigrationStatus, Migrator};
pub use pool::Db;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database connection error: {0}")]
    Connect(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("invalid migration manifest: {0}")]
    MigrationManifest(String),
    #[error("connection pool error: {0}")]
    Pool(String),
}
