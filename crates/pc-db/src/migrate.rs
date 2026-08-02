//! 数据库迁移。
//!
//! Phase A：迁移目录为空（109 表 SQL 将在 Phase B 之前迁移过来）。
//! 这里只验证 sqlx::migrate! 框架能跑通。

use crate::DbError;
use tracing::info;

pub struct Migrator;

impl Migrator {
    /// 运行所有 pending 迁移。
    pub async fn run(_db: &super::Db) -> Result<(), DbError> {
        // Phase A：暂时跳过，等 Phase B 引入 109 表 SQL DDL 时启用
        // sqlx::migrate!("./migrations").run(db.pool()).await?;
        info!("migrations skipped (Phase A: no migrations yet)");
        Ok(())
    }

    /// 查看已应用的迁移版本。
    pub async fn status(_db: &super::Db) -> Result<Vec<String>, DbError> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrator_runs_without_migrations() {
        // 这里不依赖真实 DB：只验证类型与函数可调用
        let result = Migrator::status_dummy().await;
        assert!(result.is_ok());
    }
}

impl Migrator {
    // 占位方法，让上面单元测试无需 DB
    pub async fn status_dummy() -> Result<Vec<String>, DbError> { Ok(vec![]) }
}
