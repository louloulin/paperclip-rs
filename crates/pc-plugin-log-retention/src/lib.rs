#![forbid(unsafe_code)]
//! `pc-plugin-log-retention` —— plugin log 清理高级 facade。
//!
//! 对应 Node `server/src/services/plugin-log-retention.ts`（86 行）。
//!
//! 设计目标：1:1 复刻 + Rust 增强
//! - 常量：`DEFAULT_RETENTION_DAYS = 7` / `DELETE_BATCH_SIZE = 5_000` / `MAX_ITERATIONS = 100` / `DEFAULT_INTERVAL_MS = 3_600_000`
//! - [`prune_plugin_logs`] —— 一次性 batch DELETE + 计数（SQL 由 pc-repos 提供）
//! - [`start_plugin_log_retention`] —— 启动 tokio interval + 立即跑一次 + 返回 stop handle
//! - [`PluginLogRetentionHook`] —— 钩子扩展点（sweep-start / sweep-done / iteration-limit-hit / error）
//!
//! 与 pc-repos 的区别：
//! - pc-repos 提供 SQL 实现（`plugin_log_retention` 模块）
//! - 本 crate 在此基础上提供 hook bus + 启动 / 停止生命周期管理

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::task::JoinHandle;
use tracing::{info, warn};

// ============================================================================
// Re-exports from pc-repos
// ============================================================================

pub use pc_repos::plugin_log_retention as repo;

/// 默认保留天数（与 Node `DEFAULT_RETENTION_DAYS = 7` 1:1 对齐）。
pub use repo::DEFAULT_RETENTION_DAYS;

/// 单次 DELETE batch 大小（与 Node `DELETE_BATCH_SIZE = 5_000` 1:1 对齐）。
pub use repo::DELETE_BATCH_SIZE;

/// 单次 sweep 最大迭代次数（与 Node `MAX_ITERATIONS = 100` 1:1 对齐）。
pub use repo::MAX_ITERATIONS;

/// 默认 sweep 间隔（与 Node `60 * 60 * 1_000` 1:1 对齐）。
pub use repo::DEFAULT_INTERVAL_MS;

/// 一次性 batch DELETE（与 Node `prunePluginLogs` 1:1 对齐）。
pub async fn prune_plugin_logs(
    db: &pc_repos::Db,
    retention_days: i64,
) -> Result<u64, RetentionError> {
    repo::prune_plugin_logs(db, retention_days)
        .await
        .map_err(RetentionError::Db)
}

// ============================================================================
// Errors
// ============================================================================

/// Plugin log retention 服务错误。
#[derive(Debug, Error)]
pub enum RetentionError {
    #[error("db error: {0}")]
    Db(sqlx::Error),
    #[error("retention_days must be >= 0")]
    InvalidRetention,
    #[error("interval_ms must be > 0")]
    InvalidInterval,
}

// ============================================================================
// Hook bus
// ============================================================================

/// Hook 事件 —— 序列化时 camelCase tag。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum RetentionHookEvent {
    /// 一次 sweep 开始。
    SweepStarted {
        cutoff: DateTime<Utc>,
        retention_days: i64,
    },
    /// 一次 sweep 结束。
    SweepCompleted {
        deleted: u64,
        iterations: i64,
        retention_days: i64,
    },
    /// sweep 命中迭代上限。
    IterationLimitHit {
        total_deleted: u64,
        iterations: i64,
        cutoff: DateTime<Utc>,
    },
    /// sweep 失败（错误已被吞咽，但可通知上层）。
    SweepFailed {
        retention_days: i64,
        error: String,
    },
}

/// 扩展点 —— 让上层（telemetry / metrics / admin UI）监听 sweep 生命周期事件。
#[async_trait]
pub trait PluginLogRetentionHook: Send + Sync {
    async fn on_retention_event(&self, event: RetentionHookEvent);
}

/// Noop 实现。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopRetentionHook;

#[async_trait]
impl PluginLogRetentionHook for NoopRetentionHook {
    async fn on_retention_event(&self, _event: RetentionHookEvent) {}
}

// ============================================================================
// Service
// ============================================================================

/// 抽象 DB 依赖 —— 方便测试注入 fake。
#[async_trait]
pub trait PruneBackend: Send + Sync {
    async fn prune(&self, retention_days: i64) -> Result<u64, RetentionError>;
}

struct DbBackend {
    db: pc_repos::Db,
}

#[async_trait]
impl PruneBackend for DbBackend {
    async fn prune(&self, retention_days: i64) -> Result<u64, RetentionError> {
        prune_plugin_logs(&self.db, retention_days).await
    }
}

/// Service —— 暴露 prune 操作 + hook fan-out。
pub struct PluginLogRetentionService<B: PruneBackend> {
    backend: Arc<B>,
    hooks: Vec<Arc<dyn PluginLogRetentionHook>>,
}

impl<B: PruneBackend> PluginLogRetentionService<B> {
    pub fn new(backend: Arc<B>) -> Self {
        Self {
            backend,
            hooks: Vec::new(),
        }
    }

    pub fn with_hooks(backend: Arc<B>, hooks: Vec<Arc<dyn PluginLogRetentionHook>>) -> Self {
        Self {
            backend,
            hooks,
        }
    }

    /// 触发一次 sweep，fan-out 钩子。
    pub async fn sweep(&self, retention_days: i64) -> Result<u64, RetentionError> {
        if retention_days < 0 {
            return Err(RetentionError::InvalidRetention);
        }

        let cutoff = Utc::now() - chrono::Duration::days(retention_days);
        self.fan_out(RetentionHookEvent::SweepStarted {
            cutoff,
            retention_days,
        })
        .await;

        let result = self.backend.prune(retention_days).await;
        match result {
            Ok(deleted) => {
                self.fan_out(RetentionHookEvent::SweepCompleted {
                    deleted,
                    iterations: 1, // 高级 facade 不暴露 iterations（pc-repos 内部 batch）
                    retention_days,
                })
                .await;
                if deleted > 0 {
                    info!(deleted, retention_days, "Pruned expired plugin logs");
                }
                Ok(deleted)
            }
            Err(err) => {
                let err_str = err.to_string();
                self.fan_out(RetentionHookEvent::SweepFailed {
                    retention_days,
                    error: err_str.clone(),
                })
                .await;
                warn!(err = %err_str, "Plugin log retention sweep failed");
                Err(err)
            }
        }
    }

    async fn fan_out(&self, event: RetentionHookEvent) {
        for h in &self.hooks {
            h.on_retention_event(event.clone()).await;
        }
    }
}

impl PluginLogRetentionService<DbBackend> {
    /// 从 `pc_repos::Db` 构造（无 hook）。
    pub fn from_db(db: pc_repos::Db) -> Self {
        Self::new(Arc::new(DbBackend { db }))
    }

    /// 从 `pc_repos::Db` 构造并接入 hooks。
    pub fn from_db_with_hooks(
        db: pc_repos::Db,
        hooks: Vec<Arc<dyn PluginLogRetentionHook>>,
    ) -> Self {
        Self::with_hooks(Arc::new(DbBackend { db }), hooks)
    }
}

// ============================================================================
// Periodic runner
// ============================================================================

/// 周期任务停止句柄。
pub struct PluginLogRetentionHandle {
    cancel: Arc<AtomicBool>,
    #[allow(dead_code)]
    task: JoinHandle<()>,
}

impl PluginLogRetentionHandle {
    /// 停止周期任务（不等待当前 sweep 完成）。
    pub fn stop(self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

/// 启动周期性 plugin log 清理（与 Node `startPluginLogRetention` 1:1 对齐）。
///
/// 行为：
/// 1. 启动时立即跑一次 sweep（异步 fire-and-forget）
/// 2. 之后每隔 `interval_ms` 跑一次 sweep
/// 3. 返回 [`PluginLogRetentionHandle`]，调用 `stop()` 取消
///
/// 错误处理：每次 sweep 失败 → warn 日志 + 不抛错（与 Node `catch` 1:1 对齐）。
pub fn start_plugin_log_retention(
    db: pc_repos::Db,
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
// Recording hook (测试辅助)
// ============================================================================

/// 录制所有 hook 事件 —— 测试用。
#[derive(Debug, Default, Clone)]
pub struct RecordingRetentionHook {
    events: Arc<tokio::sync::Mutex<Vec<RetentionHookEvent>>>,
}

impl RecordingRetentionHook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.events.try_lock().map(|g| g.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        if let Ok(mut g) = self.events.try_lock() {
            g.clear();
        }
    }

    pub async fn events_snapshot_async(&self) -> Vec<RetentionHookEvent> {
        self.events.lock().await.clone()
    }

    pub fn events_snapshot(&self) -> Vec<RetentionHookEvent> {
        self.events.try_lock().map(|g| g.clone()).unwrap_or_default()
    }
}

#[async_trait]
impl PluginLogRetentionHook for RecordingRetentionHook {
    async fn on_retention_event(&self, event: RetentionHookEvent) {
        self.events.lock().await.push(event);
    }
}

#[async_trait]
impl PluginLogRetentionHook for Arc<RecordingRetentionHook> {
    async fn on_retention_event(&self, event: RetentionHookEvent) {
        (**self).on_retention_event(event).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r709_constants_match_node() {
        assert_eq!(DEFAULT_RETENTION_DAYS, 7);
        assert_eq!(DELETE_BATCH_SIZE, 5_000);
        assert_eq!(MAX_ITERATIONS, 100);
        assert_eq!(DEFAULT_INTERVAL_MS, 3_600_000);
    }

    #[test]
    fn r709_retention_hook_event_tag_is_camel_case() {
        let v = serde_json::to_value(RetentionHookEvent::SweepStarted {
            cutoff: Utc::now(),
            retention_days: 7,
        })
        .unwrap();
        assert_eq!(v["type"], "sweepStarted");
    }

    #[test]
    fn r709_handle_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PluginLogRetentionHandle>();
        assert_send_sync::<NoopRetentionHook>();
        assert_send_sync::<RecordingRetentionHook>();
        assert_send_sync::<RetentionHookEvent>();
    }

    #[tokio::test]
    async fn r709_noop_hook_is_quiet() {
        let h = NoopRetentionHook;
        h.on_retention_event(RetentionHookEvent::SweepStarted {
            cutoff: Utc::now(),
            retention_days: 7,
        })
        .await;
    }

    #[tokio::test]
    async fn r709_recorder_captures_event() {
        let h = RecordingRetentionHook::default();
        h.on_retention_event(RetentionHookEvent::SweepCompleted {
            deleted: 5,
            iterations: 1,
            retention_days: 7,
        })
        .await;
        assert_eq!(h.len(), 1);
        h.clear();
        assert!(h.is_empty());
    }

    #[tokio::test]
    async fn r709_service_invalid_retention() {
        // 用一个 fake backend 测验证路径
        struct Fake;
        #[async_trait]
        impl PruneBackend for Fake {
            async fn prune(&self, _: i64) -> Result<u64, RetentionError> {
                Ok(0)
            }
        }
        let svc = PluginLogRetentionService::new(Arc::new(Fake));
        assert!(svc.sweep(-1).await.is_err());
    }

    #[tokio::test]
    async fn r709_service_sweep_emits_hooks() {
        struct FakeOk;
        #[async_trait]
        impl PruneBackend for FakeOk {
            async fn prune(&self, _: i64) -> Result<u64, RetentionError> {
                Ok(42)
            }
        }
        let recorder = Arc::new(RecordingRetentionHook::default());
        let svc = PluginLogRetentionService::with_hooks(
            Arc::new(FakeOk),
            vec![recorder.clone() as Arc<dyn PluginLogRetentionHook>],
        );
        let deleted = svc.sweep(7).await.unwrap();
        assert_eq!(deleted, 42);
        let events = recorder.events_snapshot_async().await;
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0],
            RetentionHookEvent::SweepStarted { .. }
        ));
        assert!(matches!(
            events[1],
            RetentionHookEvent::SweepCompleted { .. }
        ));
    }

    #[tokio::test]
    async fn r709_service_sweep_failed_emits_event() {
        struct FakeErr;
        #[async_trait]
        impl PruneBackend for FakeErr {
            async fn prune(&self, _: i64) -> Result<u64, RetentionError> {
                Err(RetentionError::Db(sqlx::Error::PoolClosed))
            }
        }
        let recorder = Arc::new(RecordingRetentionHook::default());
        let svc = PluginLogRetentionService::with_hooks(
            Arc::new(FakeErr),
            vec![recorder.clone() as Arc<dyn PluginLogRetentionHook>],
        );
        assert!(svc.sweep(7).await.is_err());
        let events = recorder.events_snapshot_async().await;
        assert!(events.iter().any(|e| matches!(
            e,
            RetentionHookEvent::SweepFailed { .. }
        )));
    }
}
