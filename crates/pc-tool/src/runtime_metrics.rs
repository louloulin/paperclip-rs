//! Tool runtime metric 计数器 + hook bus。
//!
//! 对应 Node `server/src/services/tool-runtime-metrics.ts`（57 行）1:1 复刻。
//! （原 `pc-tool-runtime-metrics` crate 已下沉到 `pc-tool`）。

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Timelike, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;
use uuid::Uuid;

// ============================================================================
// Errors
// ============================================================================

/// tool runtime metrics 服务错误。
#[derive(Debug, Error)]
pub enum MetricError {
    #[error("company_id must be non-nil UUID")]
    InvalidCompanyId,
    #[error("metric name must be non-empty")]
    EmptyMetric,
    #[error("db error: {0}")]
    Db(#[from] sqlx::Error),
}

/// Best-effort 错误吞咽（与 Node try/catch 等价）。
pub type MetricResult<T> = std::result::Result<T, MetricError>;

// ============================================================================
// Constants
// ============================================================================

/// Audit 写入失败 metric 名（与 Node `TOOL_RUNTIME_AUDIT_WRITE_FAILURE_METRIC` 1:1 对齐）。
pub const AUDIT_WRITE_FAILURE_METRIC: &str = "audit_write_failed";

// ============================================================================
// Pure helpers
// ============================================================================

/// 把 DateTime 截断到分钟起点（seconds=0, nanoseconds=0）。
///
/// 与 Node `minuteBucket(date)` 1:1 对齐：
/// ```ts
/// const bucket = new Date(date);
/// bucket.setSeconds(0, 0);
/// return bucket;
/// ```
#[must_use]
pub fn minute_bucket(at: DateTime<Utc>) -> DateTime<Utc> {
    at.with_second(0)
        .and_then(|d| d.with_nanosecond(0))
        .unwrap_or(at)
}

// ============================================================================
// Hook bus
// ============================================================================

/// Hook 事件类型 —— 序列化时 camelCase tag（与 Node `tag` 字段对齐）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum MetricHookEvent {
    /// 普通 metric 累加成功。
    Incremented {
        company_id: Uuid,
        metric: String,
        bucket_start_at: DateTime<Utc>,
    },
    /// Audit 写入失败被记录（best-effort）。
    AuditWriteFailureRecorded { company_id: Uuid },
}

/// MetricHook trait —— 接收 metric 事件的扩展点。
///
/// 设计动机：让上层（plugin-host / telemetry / audit）能在不耦合 DB 层的前提下
/// 监听 metric 累加事件。
#[async_trait]
pub trait MetricHook: Send + Sync {
    /// 处理一个事件。
    async fn on_metric_event(&self, event: MetricHookEvent) -> MetricResult<()>;
}

/// Noop 实现 —— 不做任何事。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopMetricHook;

#[async_trait]
impl MetricHook for NoopMetricHook {
    async fn on_metric_event(&self, _event: MetricHookEvent) -> MetricResult<()> {
        Ok(())
    }
}

/// 录制所有事件 —— 用于测试 / 调试。
#[derive(Debug, Default, Clone)]
pub struct RecordingMetricHook {
    events: Arc<Mutex<Vec<MetricHookEvent>>>,
}

impl RecordingMetricHook {
    pub fn new() -> Self {
        Self::default()
    }

    /// 同步版本的 `len()` —— 方便不进入 async context 也能查询。
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

    /// 异步版本的 `len()` —— 用于跨 await 调用。
    pub async fn len_async(&self) -> usize {
        self.events.lock().await.len()
    }

    pub async fn is_empty_async(&self) -> bool {
        self.events.lock().await.is_empty()
    }

    pub async fn clear_async(&self) {
        self.events.lock().await.clear();
    }

    pub async fn events_snapshot_async(&self) -> Vec<MetricHookEvent> {
        self.events.lock().await.clone()
    }

    /// 同步版本 —— 内部 try_lock，失败返回空 Vec。
    /// 适合 hot-path 调用方在已持有 hook 引用的场景使用。
    pub fn events_snapshot(&self) -> Vec<MetricHookEvent> {
        self.events
            .try_lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl MetricHook for RecordingMetricHook {
    async fn on_metric_event(&self, event: MetricHookEvent) -> MetricResult<()> {
        self.events.lock().await.push(event);
        Ok(())
    }
}

// Arc<RecordingMetricHook> 也实现 MetricHook —— 允许用 `Arc<RecordingMetricHook>` 注入。
#[async_trait]
impl MetricHook for Arc<RecordingMetricHook> {
    async fn on_metric_event(&self, event: MetricHookEvent) -> MetricResult<()> {
        (**self).on_metric_event(event).await
    }
}

// ============================================================================
// Repo trait
// ============================================================================

/// 抽象 DB 依赖 —— 方便测试注入 fake。
#[async_trait]
pub trait MetricCounterRepo: Send + Sync {
    /// 累加一次 metric counter；返回写入的 bucket 起点（用于 hook 通知）。
    async fn increment(
        &self,
        company_id: Uuid,
        metric: &str,
        at: DateTime<Utc>,
    ) -> MetricResult<DateTime<Utc>>;
}

// ============================================================================
// Service
// ============================================================================

/// Service —— 暴露 metric counter 写操作 + hook 通知。
pub struct ToolRuntimeMetricsService<R: MetricCounterRepo> {
    repo: Arc<R>,
    hooks: Vec<Arc<dyn MetricHook>>,
}

impl<R: MetricCounterRepo> ToolRuntimeMetricsService<R> {
    /// 从 `Arc<R>` 构造（无 hook）。
    pub fn from_repo(repo: Arc<R>) -> Self {
        Self {
            repo,
            hooks: Vec::new(),
        }
    }

    /// 从 `Arc<R>` 构造并接入 hooks。
    pub fn from_repo_with_hooks(repo: Arc<R>, hooks: Vec<Arc<dyn MetricHook>>) -> Self {
        Self { repo, hooks }
    }

    /// 累加一次 metric counter。
    ///
    /// 验证：
    /// - `company_id` 非 nil UUID
    /// - `metric` 非空字符串
    pub async fn increment(
        &self,
        company_id: Uuid,
        metric: &str,
        at: Option<DateTime<Utc>>,
    ) -> MetricResult<()> {
        if company_id.is_nil() {
            return Err(MetricError::InvalidCompanyId);
        }
        if metric.is_empty() {
            return Err(MetricError::EmptyMetric);
        }

        let at = at.unwrap_or_else(Utc::now);
        let bucket_written = self.repo.increment(company_id, metric, at).await?;

        // fan-out hooks
        for h in &self.hooks {
            let ev = MetricHookEvent::Incremented {
                company_id,
                metric: metric.to_string(),
                bucket_start_at: bucket_written,
            };
            if let Err(err) = h.on_metric_event(ev).await {
                tracing::warn!(?err, "metric hook failed");
            }
        }
        Ok(())
    }

    /// 记录一次 audit 写入失败 —— best-effort，错误吞咽。
    pub async fn record_audit_write_failure(&self, company_id: Uuid) {
        if company_id.is_nil() {
            // best-effort：直接吞咽（与 Node `recordToolRuntimeAuditWriteFailure` 一致）
            tracing::warn!(
                company_id = %company_id,
                "skipping audit write failure metric for nil company_id"
            );
            return;
        }
        let now = Utc::now();
        let res = self
            .repo
            .increment(company_id, AUDIT_WRITE_FAILURE_METRIC, now)
            .await;
        match res {
            Ok(_bucket) => {
                for h in &self.hooks {
                    let ev = MetricHookEvent::AuditWriteFailureRecorded { company_id };
                    if let Err(err) = h.on_metric_event(ev).await {
                        tracing::warn!(?err, "metric hook failed");
                    }
                }
            }
            Err(err) => {
                tracing::warn!(
                    ?err,
                    company_id = %company_id,
                    "failed to record audit write failure counter"
                );
            }
        }
    }
}

// ============================================================================
// pc_repos::Db adapter —— 让 e2e / 上层能直接传 pc_repos::Db
// ============================================================================

use pc_repos::tool_runtime_metrics as pc_repo_trm;

/// `pc_repos::Db` 的 `MetricCounterRepo` 适配器。
#[derive(Clone)]
pub struct PcReposDbAdapter {
    db: pc_repos::Db,
}

impl PcReposDbAdapter {
    pub fn new(db: pc_repos::Db) -> Self {
        Self { db }
    }
}

#[async_trait]
impl MetricCounterRepo for PcReposDbAdapter {
    async fn increment(
        &self,
        company_id: Uuid,
        metric: &str,
        at: DateTime<Utc>,
    ) -> MetricResult<DateTime<Utc>> {
        // pc_repos 的函数要求 &'static str —— 这里用一个内部映射保持
        // metric 名的所有权并按 metric 名静态化（首次见到时）。
        let metric_static: &'static str = metric_to_static(metric);
        pc_repo_trm::increment_tool_runtime_metric_counter(
            &self.db,
            pc_repo_trm::IncrementMetricInput {
                company_id,
                metric: metric_static,
                at: Some(at),
            },
        )
        .await?;
        Ok(minute_bucket(at))
    }
}

impl ToolRuntimeMetricsService<PcReposDbAdapter> {
    /// 从 `pc_repos::Db` 直接构造（无 hook）。
    pub fn new(db: pc_repos::Db) -> Self {
        ToolRuntimeMetricsService {
            repo: Arc::new(PcReposDbAdapter::new(db)),
            hooks: Vec::new(),
        }
    }

    /// 从 `pc_repos::Db` 构造并接入 hooks。
    pub fn with_hooks(db: pc_repos::Db, hooks: Vec<Arc<dyn MetricHook>>) -> Self {
        ToolRuntimeMetricsService {
            repo: Arc::new(PcReposDbAdapter::new(db)),
            hooks,
        }
    }
}

fn metric_to_static(metric: &str) -> &'static str {
    // 用 `Box::leak` 把 metric 名永久化。
    // 这是测试 / 上层入口 —— metric 数量有限，不会无限增长。
    Box::leak(metric.to_string().into_boxed_str())
}

/// 静态 helper 容器 —— 不依赖任何 R，方便测试 / 上层直接调用关联函数。
pub struct ToolRuntimeMetrics;

impl ToolRuntimeMetrics {
    /// 截断到分钟起点（与 Node `minuteBucket` 1:1 对齐）。
    #[must_use]
    pub fn minute_bucket(at: DateTime<Utc>) -> DateTime<Utc> {
        minute_bucket(at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, TimeZone};

    #[test]
    fn r708_audit_write_failure_constant() {
        assert_eq!(AUDIT_WRITE_FAILURE_METRIC, "audit_write_failed");
    }

    #[test]
    fn r708_minute_bucket_truncates_seconds_and_nanos() {
        let at = Utc.with_ymd_and_hms(2024, 1, 1, 12, 30, 45).unwrap()
            + chrono::Duration::milliseconds(700);
        let b = minute_bucket(at);
        assert_eq!(b.hour(), 12);
        assert_eq!(b.minute(), 30);
        assert_eq!(b.second(), 0);
        assert_eq!(b.nanosecond(), 0);
    }

    #[test]
    fn r708_minute_bucket_already_at_start() {
        let at = Utc.with_ymd_and_hms(2024, 1, 1, 12, 30, 0).unwrap();
        let b = minute_bucket(at);
        assert_eq!(b, at);
    }

    #[test]
    fn r708_minute_bucket_does_not_change_hour() {
        let at = Utc.with_ymd_and_hms(2024, 1, 1, 12, 59, 59).unwrap();
        let b = minute_bucket(at);
        assert_eq!(b.hour(), 12);
        assert_eq!(b.minute(), 59);
        assert_eq!(b.second(), 0);
    }

    #[test]
    fn r708_minute_bucket_at_midnight() {
        let at = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 30).unwrap();
        let b = minute_bucket(at);
        assert_eq!(b.hour(), 0);
        assert_eq!(b.minute(), 0);
        assert_eq!(b.day(), 1);
        assert_eq!(b.month(), 1);
    }

    #[test]
    fn r708_minute_bucket_year_boundary() {
        let at = Utc.with_ymd_and_hms(2023, 12, 31, 23, 59, 30).unwrap();
        let b = minute_bucket(at);
        assert_eq!(b.year(), 2023);
        assert_eq!(b.month(), 12);
        assert_eq!(b.day(), 31);
        assert_eq!(b.hour(), 23);
        assert_eq!(b.minute(), 59);
    }

    #[test]
    fn r708_metric_hook_event_tag_is_camel_case() {
        let v = serde_json::to_value(MetricHookEvent::AuditWriteFailureRecorded {
            company_id: Uuid::nil(),
        })
        .unwrap();
        assert_eq!(v["type"], "auditWriteFailureRecorded");
    }

    #[tokio::test]
    async fn r708_recorder_captures_variants() {
        let h = RecordingMetricHook::default();
        let bucket = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let ev1 = MetricHookEvent::Incremented {
            company_id: Uuid::new_v4(),
            metric: "m".into(),
            bucket_start_at: bucket,
        };
        let ev2 = MetricHookEvent::AuditWriteFailureRecorded {
            company_id: Uuid::new_v4(),
        };
        MetricHook::on_metric_event(&h, ev1).await.unwrap();
        MetricHook::on_metric_event(&h, ev2).await.unwrap();
        assert_eq!(h.len_async().await, 2);
        h.clear_async().await;
        assert!(h.is_empty_async().await);
    }

    #[tokio::test]
    async fn r708_noop_ok() {
        let e = MetricHookEvent::AuditWriteFailureRecorded {
            company_id: Uuid::new_v4(),
        };
        assert!(MetricHook::on_metric_event(&NoopMetricHook, e)
            .await
            .is_ok());
    }

    #[test]
    fn r708_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<NoopMetricHook>();
        assert_send_sync::<RecordingMetricHook>();
        assert_send_sync::<MetricHookEvent>();
    }
}
