//! Multica 数据库层。
//!
//! 单一职责：管理 PostgreSQL 连接池、迁移、健康检查。
//! 上层（mc-repos 等）通过 `Db` 句柄访问。
//!
//! 迁移 runner 与 pc-db 的 `Migrator` 行为等价，但接受 `multica-migrate` 提供的迁移列表。

pub mod health;
pub mod migrate;
pub mod pool;

pub use health::{HealthCheck, HealthStatus};
pub use migrate::{MigrationStatus, MigrationStep, Migrator};
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

pub type Result<T> = std::result::Result<T, DbError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_error_display_includes_cause() {
        let e = DbError::MigrationManifest("bad filename".into());
        assert!(format!("{e}").contains("bad filename"));
    }
}
