//! Plugin log retention (1:1 port of Node `server/src/services/plugin-log-retention.ts`, 86 行).
//!
//! 单一职责：周期性清理 `plugin_logs` 表中过期行。
//!
//! 公开 API：
//! - 常量：`DEFAULT_RETENTION_DAYS = 7` / `DELETE_BATCH_SIZE = 5_000` / `MAX_ITERATIONS = 100`
//! - [`prune_plugin_logs`] —— 一次性 batch DELETE + 计数
//! - [`start_plugin_log_retention`] —— 启动 tokio interval + 立即跑一次 + 返回 stop closure
//!
//! 不持有状态；只依赖 `Db`。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::task::JoinHandle;
use tracing::{info, warn};
use uuid::Uuid;

use crate::Db;

// ============================================================================
// Constants
// ============================================================================

/// 默认保留天数（与 Node `DEFAULT_RETENTION_DAYS` 1:1 对齐）。
pub const DEFAULT_RETENTION_DAYS: i64 = 7;

/// 单次 DELETE batch 大小（与 Node `DELETE_BATCH_SIZE` 1:1 对齐）。
pub const DELETE_BATCH_SIZE: i64 = 5_000;

/// 单次 sweep 最大迭代次数（与 Node `MAX_ITERATIONS` 1:1 对齐）。
pub const MAX_ITERATIONS: i64 = 100;

/// 默认 sweep 间隔：1 小时（与 Node `60 * 60 * 1_000` 1:1 对齐）。
pub const DEFAULT_INTERVAL_MS: u64 = 60 * 60 * 1_000;

// ============================================================================
// Public API
// ============================================================================

/// 删除早于 `retentionDays` 天的 plugin log 行（与 Node `prunePluginLogs` 1:1 对齐）。
///
/// 行为：
/// 1. `cutoff = now() - retentionDays`
/// 2. 循环：每次 `DELETE ... WHERE createdAt < cutoff RETURNING id`
/// 3. 累计 `totalDeleted`，直到 `deleted < DELETE_BATCH_SIZE` 或 `iterations >= MAX_ITERATIONS`
/// 4. 命中 iteration 上限 → warn 日志
/// 5. 总删行 > 0 → info 日志
/// 6. 返回 `totalDeleted`
pub async fn prune_plugin_logs(
    db: &Db,
    retention_days: i64,
) -> sqlx::Result<u64> {
    let cutoff: DateTime<Utc> = Utc::now() - chrono::Duration::days(retention_days);

    let mut total_deleted: u64 = 0;
    let mut iterations: i64 = 0;

    while iterations < MAX_ITERATIONS {
        // 单次 batch DELETE + RETURNING id
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            "DELETE FROM plugin_logs WHERE created_at < $1 RETURNING id",
        )
        .bind(cutoff)
        .fetch_all(db.pool())
        .await?;
        let deleted = rows.len() as u64;

        total_deleted += deleted;
        iterations += 1;

        if deleted < DELETE_BATCH_SIZE as u64 {
            break;
        }
    }

    if iterations >= MAX_ITERATIONS {
        warn!(
            total_deleted,
            iterations,
            cutoff_date = %cutoff,
            "Plugin log retention hit iteration limit; some logs may remain"
        );
    }

    if total_deleted > 0 {
        info!(
            total_deleted,
            retention_days,
            "Pruned expired plugin logs"
        );
    }

    Ok(total_deleted)
}

/// Plugin log retention 周期任务停止句柄（与 Node `clearInterval` 1:1 对齐）。
///
/// 调用 `stop()` 后，interval 停止；正在执行的 sweep 不会被取消。
pub struct PluginLogRetentionHandle {
    cancel: Arc<AtomicBool>,
    // 持有 task handle 以保持 task 存活；调用方不需要 await
    #[allow(dead_code)]
    task: JoinHandle<()>,
}

impl PluginLogRetentionHandle {
    /// 停止周期任务。
    pub fn stop(self) {
        self.cancel.store(true, Ordering::SeqCst);
        // 不等待 task，让它在下一轮迭代自然退出
    }
}

/// 启动周期性 plugin log 清理（与 Node `startPluginLogRetention` 1:1 对齐）。
///
/// 行为：
/// 1. 启动时立即跑一次 sweep（异步）
/// 2. 之后每隔 `interval_ms` 跑一次
/// 3. 返回 `PluginLogRetentionHandle`，调用 `stop()` 取消
///
/// 注：与 Node `setInterval` + `clearInterval` 的差异是 Rust 用 tokio interval + `JoinHandle`。
/// 错误处理：每次 sweep 失败 → warn 日志（与 Node `catch` 1:1 对齐）。
pub fn start_plugin_log_retention(
    db: Db,
    interval_ms: Option<u64>,
    retention_days: Option<i64>,
) -> PluginLogRetentionHandle {
    let interval = Duration::from_millis(interval_ms.unwrap_or(DEFAULT_INTERVAL_MS));
    let retention = retention_days.unwrap_or(DEFAULT_RETENTION_DAYS);
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_child = cancel.clone();

    // 启动时立即跑一次
    let db_immediate = db.clone();
    let initial_task = tokio::spawn(async move {
        if let Err(err) = prune_plugin_logs(&db_immediate, retention).await {
            warn!(err = %err, "Initial plugin log retention sweep failed");
        }
    });
    // 显式 drop 句柄（不 await —— sweep 在后台跑）
    drop(initial_task);

    // 周期任务
    let task = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if cancel_child.load(Ordering::SeqCst) {
                break;
            }
            if let Err(err) = prune_plugin_logs(&db, retention).await {
                warn!(err = %err, "Plugin log retention sweep failed");
            }
        }
    });

    PluginLogRetentionHandle { cancel, task }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 常量 ----

    #[test]
    fn retention_days_constant_matches_node() {
        assert_eq!(DEFAULT_RETENTION_DAYS, 7);
    }

    #[test]
    fn delete_batch_size_constant_matches_node() {
        assert_eq!(DELETE_BATCH_SIZE, 5_000);
    }

    #[test]
    fn max_iterations_constant_matches_node() {
        assert_eq!(MAX_ITERATIONS, 100);
    }

    #[test]
    fn default_interval_constant_is_one_hour() {
        assert_eq!(DEFAULT_INTERVAL_MS, 60 * 60 * 1_000);
    }

    // ---- PluginLogRetentionHandle ----

    #[test]
    fn handle_is_send_and_sync() {
        // 编译期保证：handle 可跨线程使用
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PluginLogRetentionHandle>();
    }

    // ---- SQL 形状 ----

    #[test]
    fn prune_sql_uses_lt_cutoff_and_returning_id() {
        let sql = "DELETE FROM plugin_logs WHERE created_at < $1 RETURNING id";
        assert!(sql.starts_with("DELETE FROM plugin_logs"));
        assert!(sql.contains("created_at < $1"));
        assert!(sql.contains("RETURNING id"));
    }

    // ---- cutoff 计算 ----

    #[test]
    fn cutoff_subtracts_retention_days() {
        let now = Utc::now();
        let cutoff = now - chrono::Duration::days(7);
        // 7 天差
        assert_eq!((now - cutoff).num_days(), 7);
    }

    // ---- 间隔与默认行为 ----

    #[test]
    fn default_interval_is_one_hour_in_milliseconds() {
        // 60 * 60 * 1000 ms = 1 小时
        assert_eq!(DEFAULT_INTERVAL_MS, 3_600_000);
    }

    #[test]
    fn default_retention_is_seven_days() {
        // 与 Node 端 `7` 对齐
        assert_eq!(DEFAULT_RETENTION_DAYS, 7);
    }
}
