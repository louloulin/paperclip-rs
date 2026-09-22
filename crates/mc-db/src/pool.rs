//! PostgreSQL 连接池封装。

use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::postgres::{PgPool, PgPoolOptions};

/// Multica 数据库连接句柄。
#[derive(Clone)]
pub struct Db {
    pool: PgPool,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db")
            .field("size", &self.pool.size())
            .field("idle", &self.pool.num_idle())
            .finish()
    }
}

impl Db {
    /// 连接到一个 PostgreSQL 数据库。
    pub async fn connect(
        url: &str,
        max_connections: u32,
        min_connections: u32,
    ) -> Result<Self, crate::DbError> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .min_connections(min_connections)
            .acquire_timeout(Duration::from_secs(5))
            .idle_timeout(Some(Duration::from_secs(60 * 5)))
            .connect(url)
            .await?;
        Ok(Self { pool })
    }

    /// 用于测试 / 占位场景的懒构造 —— 不会立即拨号，但 pool 类型保持一致。
    /// 调用方负责在使用前确保 DB 可达；调用 `pool()` / `stats()` 不会触发网络。
    pub fn connect_lazy(
        url: &str,
        max_connections: u32,
        min_connections: u32,
    ) -> Result<Self, crate::DbError> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .min_connections(min_connections)
            .acquire_timeout(Duration::from_secs(5))
            .idle_timeout(Some(Duration::from_secs(60 * 5)))
            .connect_lazy(url)
            .map_err(crate::DbError::Connect)?;
        Ok(Self { pool })
    }

    /// 测试用占位 Db：返回一个未实际建立连接的 pool。
    /// 仅用于不需要实际查库的 handler（PAT / 内存型 store）。
    /// 调用 `pool().acquire()` 会失败，不要在集成测试里发起实际查询。
    #[cfg(any(test, feature = "test-util"))]
    pub async fn placeholder() -> Self {
        use sqlx::postgres::PgConnectOptions;
        let opts = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(1) // 不可用端口：connect_lazy_with 不会立即连接。
            .database("none");
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .min_connections(0)
            .connect_lazy_with(opts);
        Self { pool }
    }

    /// 用一个已连接的 `PgPool` 构造 Db（供集成测试使用）。
    #[cfg(any(test, feature = "test-util"))]
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// 拿到底层 `sqlx::PgPool`，供 repo 直接调用。
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// 当前连接池统计。
    pub fn stats(&self) -> PoolStats {
        PoolStats {
            size: self.pool.size() as u32,
            idle: self.pool.num_idle() as u32,
            max: self.pool.options().get_max_connections(),
        }
    }

    /// 优雅关闭。
    pub async fn close(&self) {
        self.pool.close().await;
    }
}

/// 连接池统计。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolStats {
    pub size: u32,
    pub idle: u32,
    pub max: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_stats_serializable() {
        let stats = PoolStats {
            size: 5,
            idle: 2,
            max: 20,
        };
        let json = serde_json::to_string(&stats).unwrap();
        assert!(json.contains("\"size\":5"));
    }
}