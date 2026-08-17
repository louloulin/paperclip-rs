#![forbid(unsafe_code)]
//! Tool application business service.
pub mod connection;
pub mod connection_health;
pub mod descriptor_hash;
pub mod profile_binding;
pub mod risk;
pub mod policy_validation;
pub mod summarize_redact;
pub mod side_effect_idempotency;
pub mod argument_condition;
pub mod selector_match;
pub mod runtime_metrics;
pub mod misc_pure;
pub mod tool_invocation_pure;
pub mod tool_validation_pure;
pub mod profile_helpers;
mod service;
pub use pc_repos::tool::{ToolApplicationRow, ToolApplicationStatus, ToolApplicationType};
pub use connection_health::{
    sanitize_http_failure, sanitize_runtime_error, sanitize_unknown_failure,
    HttpErrorLike, SanitizedHealthFailure, ToolConnectionHealthStatus,
};
pub use risk::{classify_risk, verb_matches, McpToolAnnotations, McpToolDescriptor, ToolRiskLevel, DESTRUCTIVE_VERBS, WRITE_VERBS};
pub use runtime_metrics::{
    minute_bucket, MetricCounterRepo, MetricError, MetricHook, MetricHookEvent, MetricResult,
    NoopMetricHook, RecordingMetricHook, ToolRuntimeMetricsService, AUDIT_WRITE_FAILURE_METRIC,
};
pub use service::{
    NoopToolHook, RecordingToolHook, ToolApplicationPatch, ToolError, ToolHook, ToolHookEvent,
    ToolService,
};
