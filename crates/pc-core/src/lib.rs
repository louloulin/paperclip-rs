//! Paperclip 领域核心。
//!
//! 高内聚：所有领域类型（实体、值对象、不变量）集中在此。
//! 低耦合：不依赖任何 IO crate（sqlx、tokio 等）。
//! 上层服务（pc-repos、pc-http、pc-heartbeat）依赖本 crate。

pub mod actor;
pub mod actor_runtime;
pub mod adapter_registry_bootstrap;
pub mod agent_eligibility;
pub mod catalog_provenance;
pub mod error;
pub mod execution_allowlist;
pub mod execution_policy_bootstrap;
pub mod execution_workspace_policy;
pub mod workspace_realization;
pub mod feature_catalog;
pub mod hash;
pub mod id;
pub mod managed_config;
pub mod mcp_http;
pub mod money;
pub mod portability_fidelity;
pub mod project_workspace_runtime_config;
pub mod portable_path;
pub mod routable_blocked;
pub mod runtime_skill_selections;
pub mod source_trust;
pub mod timestamp;
pub mod tool_content_guards;
pub mod tool_profile_binding;

pub use actor::Actor;
pub use actor_runtime::{
    spawn_system_actor, ActorKey, ActorRegistry, ActorRegistryError, DomainMessage, MessageOrigin,
    SystemActor,
};
pub use adapter_registry_bootstrap::{
    parse_adapter_registry_env, parse_adapter_registry_json, reconcile_adapter_availability,
    AdapterAvailabilityReconciliation, AdapterRegistryEntry, AdapterRegistryError,
    PAPERCLIP_ADAPTERS, PAPERCLIP_ADAPTERS_FILE,
};
pub use catalog_provenance::{
    read_catalog_string_list, read_portable_catalog_provenance, CatalogProvenance,
    PORTABLE_CATALOG_PROVENANCE_STRING_KEYS,
};
pub use error::{CoreError, CoreResult};
pub use execution_allowlist::{
    evaluate_execution_allowlist, is_execution_forced_to_kubernetes,
    is_kubernetes_sandbox_environment, ExecutionAllowlistDecision, ExecutionEnvironmentCandidate,
    ExecutionMode, ExecutionPolicy, KUBERNETES_PROVIDER_KEY,
};

pub use execution_policy_bootstrap::{
    parse_execution_policy_bootstrap_env, ExecutionMode as ExecutionPolicyMode,
    ExecutionPolicyBootstrap, ExecutionPolicyBootstrapError, KubernetesBackend,
    KubernetesEgressMode, KubernetesEnvironmentConfigInput, PAPERCLIP_EXECUTION_MODE,
    PAPERCLIP_K8S_ADAPTER_TYPE, PAPERCLIP_K8S_BACKEND, PAPERCLIP_K8S_EGRESS_ALLOW_CIDRS,
    PAPERCLIP_K8S_EGRESS_ALLOW_FQDNS, PAPERCLIP_K8S_EGRESS_MODE, PAPERCLIP_K8S_IMAGE_REGISTRY,
    PAPERCLIP_K8S_IN_CLUSTER, PAPERCLIP_K8S_NAMESPACE_PREFIX, PAPERCLIP_K8S_RPC_TIMEOUT_MS,
    PAPERCLIP_K8S_RUNTIME_CLASS_NAME,
};

pub use feature_catalog::{
    is_managed as is_managed_feature_key, tier_of as feature_tier_of, FeatureCatalogEntry,
    FeatureTier, InstanceFeatureKey, INSTANCE_FEATURE_CATALOG, INSTANCE_FEATURE_KEYS,
};

pub use managed_config::{
    clear_managed_config_cache, find_secret_like_config_key, get_managed_instance_config,
    parse_managed_config_env, ManagedConfigEnv, ManagedConfigError, ManagedEnvironmentSpec,
    ManagedInstanceConfig, MANAGED_CONFIG_ENV_KEY, SECRET_LIKE_CONFIG_KEY_PATTERN,
    SECRET_LIKE_CONFIG_KEY_PATTERN_STR, SUPPORTED_MANAGED_CONFIG_VERSION,
};

pub use agent_eligibility::{
    get_agent_org_chain_health, get_agent_work_eligibility, is_agent_assignable_to_work,
    is_agent_invokable, is_agent_status_assignable_to_work, is_agent_status_invokable,
    AgentEligibilityAgent, AgentEligibilityLifecycleReason, AgentInvalidOrgChainAncestor,
    AgentOrgChainEntry, AgentOrgChainHealth, AgentOrgChainHealthStatus, AgentOrgChainInvalidReason,
    AgentOrgChainRelation, AgentWorkEligibility,
};
pub use execution_workspace_policy::{
    build_execution_workspace_adapter_config,
    default_issue_execution_workspace_settings_for_project,
    has_reusable_execution_workspace_binding, is_unrunnable_worktree_combo,
    issue_execution_workspace_mode_for_persisted_workspace,
    resolve_effective_workspace_strategy_type, resolve_execution_workspace_environment_id,
    resolve_execution_workspace_mode, resolve_pinned_issue_workspace_strategy_type,
    select_environment_execution_workspace_settings, ExecutionWorkspaceEnvironmentResolution,
    ExecutionWorkspaceStrategy, IssueExecutionWorkspaceSettings, NetworkEgress,
    ParsedExecutionWorkspaceMode, ProjectExecutionWorkspacePolicy, UnrunnableWorktreeIssueRef,
    WORKSPACE_WORKTREE_REQUIRES_PROJECT_CODE, WORKSPACE_WORKTREE_REQUIRES_PROJECT_MESSAGE,
    WORKSPACE_WORKTREE_REQUIRES_PROJECT_REMEDIATION,
};
pub use workspace_realization::{
    build_workspace_realization_record, build_workspace_realization_record_from_driver_input,
    build_workspace_realization_request, read_additional_sources, read_path_aliases,
    read_string, read_string_array, read_workspace_realization_request,
    BuildRecordInput, BuildRequestInput, DriverInput, RealizationRequestError,
    WorkspaceDriverWorkspace,
};
pub use id::Id;
pub use mcp_http::{
    looks_like_json_rpc_message, mcp_http_request_headers, parse_mcp_http_response_body,
    McpHttpParseError, MCP_HTTP_ACCEPT,
};
pub use money::Money;
pub use portability_fidelity::{
    build_export_fidelity_warnings, normalize_export_fidelity_counts, ExportFidelityCounts,
    ExportFidelityReport, PortabilityFidelitySeverity, PortabilityFidelityWarning,
    EXPORT_FIDELITY_COUNT_KEYS, EXPORT_FIDELITY_REPORT_SCHEMA,
};
pub use portable_path::normalize_portable_path;
pub use routable_blocked::{
    deliver_agent_unblock_notification, routable_blocked_rollout_at, AgentWakeupRequest,
    DeliverAgentUnblockNotificationInput, IssueUnblockContextSnapshot, IssueUnblockDescriptor,
    IssueUnblockOwner, IssueUnblockPayload, NotifiedMarker, RoutableBlockedIssue, WakeupNotifier,
};
pub use runtime_skill_selections::{
    skill_version_selection_map, SkillVersionSelectionEntry, SkillVersionSelectionOptions,
};
pub use source_trust::{
    build_low_trust_source_trust, build_promoted_source_trust, is_low_trust_quarantined,
    redact_quarantined_body_for_higher_trust, sanitize_quarantined_comment_for_higher_trust,
    BuildLowTrustSourceTrustInput, BuildPromotedSourceTrustInput, PromotedAt, PromotedByActorType,
    SourceTrustArtifactKind, SourceTrustCommentSanitizable, SourceTrustDisposition,
    SourceTrustMetadata, SourceTrustPromotionSource, SourceTrustRedactable, TrustPreset,
    DEFAULT_TRUST_PRESET, LOW_TRUST_QUARANTINED_BODY, LOW_TRUST_REVIEW_PRESET,
};
pub use timestamp::Timestamp;
pub use tool_profile_binding::{
    narrowest_scope_bindings, profile_ids_in_binding_order, tool_profile_binding_scope_precedence,
    ToolProfileBinding, ToolProfileBindingTargetType, TOOL_PROFILE_BINDING_SCOPE_PRECEDENCE,
};
