//! Paperclip 数据库层。
//!
//! 单一职责：管理 PostgreSQL 连接池、迁移、健康检查。
//! 上层（pc-repos 等）通过 `Db` 句柄访问。

pub mod pool;
pub mod migrate;
pub mod health;

pub use pool::Db;
pub use migrate::Migrator;
pub use health::HealthCheck;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database connection error: {0}")]
    Connect(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("connection pool error: {0}")]
    Pool(String),
}
