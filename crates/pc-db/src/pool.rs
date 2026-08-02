//! sqlx `PgPool` 封装。提供带重试的 connect 与共享的 `Db` 句柄。

use crate::DbError;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions, PgPool};
use std::str::FromStr;
use std::time::Duration;
use tracing::{info, warn};

/// 共享的 `PostgreSQL` 连接池句柄。`Clone` 廉价（内部 `Arc`）。
#[derive(Clone)]
pub struct Db {
    pool: PgPool,
}

impl Db {
    /// 建立连接池。带指数退避重试，适配嵌入式 PG 冷启动。
    pub async fn connect(
        url: &str,
        max_connections: u32,
        min_connections: u32,
    ) -> Result<Self, DbError> {
        let opts = PgConnectOptions::from_str(url)
            .map_err(|e| DbError::Pool(format!("invalid url: {e}")))?
            // 屏蔽每次 acquire 的 info 日志（避免刷屏）
            .log_statements(tracing::log::LevelFilter::Debug);

        let mut last_err: Option<DbError> = None;
        for attempt in 1..=5 {
            match PgPoolOptions::new()
                .max_connections(max_connections)
                .min_connections(min_connections)
                .acquire_timeout(Duration::from_secs(5))
                .connect_with(opts.clone())
                .await
            {
                Ok(pool) => {
                    info!(
                        attempt,
                        max = max_connections,
                        min = min_connections,
                        "db connected"
                    );
                    return Ok(Self { pool });
                }
                Err(e) => {
                    warn!(attempt, error = %e, "db connect failed, retrying");
                    last_err = Some(DbError::Connect(e));
                    let delay = Duration::from_millis(200 * (1 << (attempt - 1)));
                    tokio::time::sleep(delay).await;
                }
            }
        }
        Err(last_err.unwrap_or(DbError::Pool("exhausted retries".into())))
    }

    /// 健康检查：执行 `SELECT 1`。
    pub async fn ping(&self) -> Result<(), DbError> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }


    /// 从外部 `PgPool` 包装（用于测试共享池等场景）。
    #[must_use]
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_is_clone_and_send() {
        fn assert_send<T: Send + Sync>() {}
        assert_send::<Db>();
    }
}
