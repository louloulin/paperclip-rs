#![forbid(unsafe_code)]
//! Tool runtime metric counters business service.
mod service;
pub use pc_repos::tool_runtime_metrics::{minute_bucket, IncrementMetricInput};
pub use service::{
    MetricHook, MetricHookEvent, NoopMetricHook, RecordingMetricHook, ToolRuntimeMetricsError,
    ToolRuntimeMetricsService, AUDIT_WRITE_FAILURE_METRIC, MINUTE_BUCKET_INVALID,
};
