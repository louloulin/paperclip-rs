//! 数据库健康检查。

use crate::pool::Db;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// 健康状态。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
}

/// 健康检查结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheck {
    pub status: HealthStatus,
    pub latency_ms: u64,
    pub message: Option<String>,
}

impl HealthCheck {
    pub fn healthy(latency: Duration) -> Self {
        Self {
            status: HealthStatus::Healthy,
            latency_ms: latency.as_millis() as u64,
            message: None,
        }
    }

    pub fn unhealthy(latency: Duration, message: impl Into<String>) -> Self {
        Self {
            status: HealthStatus::Unhealthy,
            latency_ms: latency.as_millis() as u64,
            message: Some(message.into()),
        }
    }

    pub fn degraded(latency: Duration, message: impl Into<String>) -> Self {
        Self {
            status: HealthStatus::Degraded,
            latency_ms: latency.as_millis() as u64,
            message: Some(message.into()),
        }
    }

    pub fn is_healthy(&self) -> bool {
        matches!(self.status, HealthStatus::Healthy)
    }
}

pub async fn check(db: &Db) -> HealthCheck {
    let start = Instant::now();
    match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(db.pool())
        .await
    {
        Ok(1) => HealthCheck::healthy(start.elapsed()),
        Ok(other) => HealthCheck::unhealthy(
            start.elapsed(),
            format!("unexpected response from SELECT 1: {other}"),
        ),
        Err(e) => HealthCheck::unhealthy(start.elapsed(), e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_status_serializes() {
        let h = HealthCheck::healthy(Duration::from_millis(5));
        let json = serde_json::to_string(&h).unwrap();
        assert!(json.contains("\"status\":\"Healthy\""));
        assert_eq!(h.latency_ms, 5);
        assert!(h.is_healthy());
    }
}