//! 数据库健康检查。

use crate::Db;
use serde::Serialize;
use std::time::Instant;
use tracing::warn;

#[derive(Debug, Serialize)]
pub struct DbHealth {
    pub ok: bool,
    pub latency_ms: u128,
    pub error: Option<String>,
}

pub struct HealthCheck;

impl HealthCheck {
    pub async fn check(db: &Db) -> DbHealth {
        let start = Instant::now();
        match db.ping().await {
            Ok(()) => DbHealth {
                ok: true,
                latency_ms: start.elapsed().as_millis(),
                error: None,
            },
            Err(e) => {
                warn!(error = %e, "db health check failed");
                DbHealth {
                    ok: false,
                    latency_ms: start.elapsed().as_millis(),
                    error: Some(e.to_string()),
                }
            }
        }
    }
}
