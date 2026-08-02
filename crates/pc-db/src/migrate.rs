//! 数据库迁移。
//!
//! Phase A：迁移目录为空（109 表 SQL 将在 Phase B 之前迁移过来）。
//! 这里只验证 `sqlx::migrate!` 框架能跑通。

use crate::DbError;
use tracing::info;

pub struct Migrator;

impl Migrator {
    /// 运行所有 pending 迁移。
    pub async fn run(_db: &super::Db) -> Result<(), DbError> {
        // Phase A：暂时跳过，等 Phase B 引入 109 表 SQL DDL 时启用
        // sqlx::migrate!("./migrations").run(db.pool()).await?;
        info!("migrations skipped (Phase A: no migrations yet)");
        std::future::ready(Ok(())).await
    }

    /// 查看已应用的迁移版本。
    pub async fn status(_db: &super::Db) -> Result<Vec<String>, DbError> {
        std::future::ready(Ok(Vec::new())).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrator_is_constructible() {
        let migrator = Migrator;
        assert_eq!(std::mem::size_of_val(&migrator), 0);
    }
}
