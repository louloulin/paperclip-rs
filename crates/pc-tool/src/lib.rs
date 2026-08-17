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

// ---- R779 curated root re-exports ----

pub use descriptor_hash::{descriptor_hash, flatten_keys, stable_hash};
pub use profile_binding::{
    narrowest_scope_bindings, profile_ids_in_binding_order,
    tool_profile_binding_scope_precedence, BindingLike,
    ToolProfileBindingTargetType,
};
pub use policy_validation::{
    iso_date_or_null, rate_limit_rule, trust_rule_config,
    trust_rule_is_active, RateLimitRule, TrustRuleConfig,
};
pub use summarize_redact::{
    summarize_and_redact, RedactionPlan, RedactionResult, RedactionSummary,
};
pub use side_effect_idempotency::{
    audit_outcome, risk_rank, side_effect_idempotency_key,
    AuditOutcome, IdempotencyContext, ToolAccessDecision,
};
pub use argument_condition::{argument_filters_match, read_path, ArgumentFilters};
pub use selector_match::{selector_matches, ToolAccessContext, ToolAccessSelector};
pub use misc_pure::{
    normalize_key, number_value, percent, percentile, schema_has_input_properties,
    CONNECTION_KEY_MAX_LEN, DEFAULT_TOOL_KEY, FK_VIOLATION_CAUSE_DEPTH,
};
pub use tool_invocation_pure::{
    connection_uid, oauth_actor_type,
    normalize_key as invocation_normalize_key, number_value as invocation_number_value,
    ActorBinding, ActorType,
};
pub use tool_validation_pure::{
    is_tool_kind_allowed, is_tool_status_allowed,
    validate_tool_kind, validate_tool_metadata,
    validate_tool_name_non_empty, validate_tool_status,
    ALLOWED_TOOL_KINDS, ALLOWED_TOOL_STATUSES,
};
pub use profile_helpers::{
    pending_new_tools_for_profile, profile_covers_catalog_scope,
    profile_entry_matches_catalog, summarize_profile,
    PendingNewToolItem, ToolProfileSummary,
};
