use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pc_errors::{internal, Error as PcError, Result as PcResult};
use pc_repos::{tool_runtime_metrics as repo, Db};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

pub use pc_repos::tool_runtime_metrics::TOOL_RUNTIME_AUDIT_WRITE_FAILURE_METRIC as AUDIT_WRITE_FAILURE_METRIC;
pub const MINUTE_BUCKET_INVALID: &str = "at must be a valid UTC timestamp";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum MetricHookEvent {
    Incremented {
        company_id: Uuid,
        metric: String,
        bucket_start_at: DateTime<Utc>,
    },
    AuditWriteFailureRecorded {
        company_id: Uuid,
    },
}

#[async_trait]
pub trait MetricHook: Send + Sync {
    async fn on_metric_event(&self, _event: MetricHookEvent) -> PcResult<()> {
        Ok(())
    }
}

pub struct NoopMetricHook;
#[async_trait]
impl MetricHook for NoopMetricHook {}

#[derive(Default)]
pub struct RecordingMetricHook {
    pub events: std::sync::Mutex<Vec<MetricHookEvent>>,
}
impl RecordingMetricHook {
    pub fn events_snapshot(&self) -> Vec<MetricHookEvent> {
        self.events.lock().expect("mutex").clone()
    }
    pub fn clear(&self) {
        self.events.lock().expect("mutex").clear()
    }
    pub fn len(&self) -> usize {
        self.events.lock().expect("mutex").len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
#[async_trait]
impl MetricHook for RecordingMetricHook {
    async fn on_metric_event(&self, e: MetricHookEvent) -> PcResult<()> {
        self.events.lock().expect("mutex").push(e);
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolRuntimeMetricsError {
    #[error("validation: {0}")]
    Validation(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Pc(#[from] PcError),
}
impl From<pc_repos::RepoError> for ToolRuntimeMetricsError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Pc(internal(e.to_string()))
    }
}
pub type MetricsResult<T> = std::result::Result<T, ToolRuntimeMetricsError>;

fn require_non_nil(id: Uuid, field: &str) -> MetricsResult<()> {
    if id.is_nil() {
        Err(ToolRuntimeMetricsError::Validation(format!(
            "{field} is required"
        )))
    } else {
        Ok(())
    }
}

#[derive(Clone)]
pub struct ToolRuntimeMetricsService {
    db: Db,
    hooks: Vec<Arc<dyn MetricHook>>,
}

impl ToolRuntimeMetricsService {
    pub fn new(db: Db) -> Self {
        Self { db, hooks: vec![] }
    }
    pub fn with_hooks(db: Db, hooks: Vec<Arc<dyn MetricHook>>) -> Self {
        Self { db, hooks }
    }
    pub fn add_hook(mut self, h: Arc<dyn MetricHook>) -> Self {
        self.hooks.push(h);
        self
    }
    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }
    async fn dispatch(&self, e: MetricHookEvent) {
        for h in &self.hooks {
            if let Err(err) = h.on_metric_event(e.clone()).await {
                tracing::warn!(?err, "metric hook failed");
            }
        }
    }

    /// Re-export the pure helper so callers don't need to import the repo module.
    pub fn minute_bucket(at: DateTime<Utc>) -> DateTime<Utc> {
        repo::minute_bucket(at)
    }

    /// Increment a metric counter. Hook fires after the SQL commit succeeds.
    pub async fn increment(
        &self,
        company_id: Uuid,
        metric: &'static str,
        at: Option<DateTime<Utc>>,
    ) -> MetricsResult<()> {
        require_non_nil(company_id, "companyId")?;
        if metric.trim().is_empty() {
            return Err(ToolRuntimeMetricsError::Validation(
                "metric must not be empty".into(),
            ));
        }
        repo::increment_tool_runtime_metric_counter(
            &self.db,
            repo::IncrementMetricInput {
                company_id,
                metric,
                at,
            },
        )
        .await?;
        let bucket = repo::minute_bucket(at.unwrap_or_else(Utc::now));
        self.dispatch(MetricHookEvent::Incremented {
            company_id,
            metric: metric.to_string(),
            bucket_start_at: bucket,
        })
        .await;
        Ok(())
    }

    /// Record an audit-write failure (best-effort, swallows DB errors).
    /// Mirrors Node `recordToolRuntimeAuditWriteFailure` semantics.
    pub async fn record_audit_write_failure(&self, company_id: Uuid) {
        require_non_nil(company_id, "companyId").ok();
        repo::record_tool_runtime_audit_write_failure(&self.db, company_id).await;
        self.dispatch(MetricHookEvent::AuditWriteFailureRecorded { company_id })
            .await;
    }
}
