#![forbid(unsafe_code)]
//! Environment business service: environments + leases.
mod config;
mod custom_image_runtime;
mod custom_image_setup_session_utils;
mod custom_image_terminal_sessions;
mod runtime_parity;
pub mod validate_environment_driver;
pub mod validate_sandbox_provider;
pub mod run_orchestrator_pure;
pub mod plugin_registry;
pub mod plugin_environment_driver_validate;
pub mod plugin_worker_manager;
pub mod plugin_environment_driver_validate_config;
pub mod probe_environment_driver;
pub mod environment_lease;
pub mod environment_workspace;
pub mod environment_setup;
pub mod environment_template;
pub mod json_schema_secret_refs;
pub mod environment_custom_images_pure;
pub mod plugin_job_scheduler_types;
mod plugin_environment_driver_pure;
pub mod misc_pure;
mod service;
pub use pc_repos::environment::{
    EnvironmentDriver, EnvironmentLeaseRow, EnvironmentRow, EnvironmentStatus, LeasePolicy,
    LeaseStatus, NewEnvironment, NewEnvironmentLease,
};
pub use config::{
    get_sandbox_provider, is_valid_driver_key, is_valid_plugin_sandbox_provider_key,
    normalize_environment_config, normalize_ssh_for_probe, parse_environment_driver_config,
    parse_fake_sandbox_environment_config, parse_plugin_environment_config,
    parse_plugin_sandbox_environment_config, parse_sandbox_environment_config,
    parse_ssh_environment_config, read_ssh_environment_private_key_secret_id,
    strip_sandbox_provider_envelope, ConfigError, ConfigIssue, FakeSandboxEnvironmentConfig,
    NormalizedEnvironmentConfig, ParsedEnvironmentConfig, PluginEnvironmentConfig,
    PluginSandboxEnvironmentConfig, SandboxEnvironmentConfig, SecretRef, SecretRefVersion,
    SshEnvironmentConfig,
};
pub use custom_image_runtime::{
    apply_custom_image_template_to_sandbox_config, classify_environment_custom_image_config_change,
    default_environment_custom_image_runtime_config_binding,
    environment_custom_image_template_from_row, environment_custom_image_template_matches_base_config,
    fingerprint_environment_sandbox_provider_config,
    normalize_environment_custom_image_runtime_config_binding,
    read_environment_custom_image_template_kind,
    resolve_environment_custom_image_runtime_config_binding, stable_stringify,
    ClassifyConfigChangeInput, EnvironmentCustomImageConfigChangeKind,
    EnvironmentCustomImageRuntimeConfigBinding, EnvironmentCustomImageTemplate,
    EnvironmentCustomImageTemplateKind, EnvironmentCustomImageTemplateRow, MatchBaseConfigInput,
    ResolveBindingInput, TemplateBindingInput,
    ENVIRONMENT_CUSTOM_IMAGE_CONFIG_FINGERPRINT_EXCLUDED_PATHS,
    ENVIRONMENT_CUSTOM_IMAGE_RUNTIME_CONFIG_BINDING_METADATA_KEY,
    ENVIRONMENT_CUSTOM_IMAGE_TEMPLATE_KINDS, ENVIRONMENT_CUSTOM_IMAGE_TEMPLATE_SOURCE_FIELDS,
};
pub use custom_image_setup_session_utils::{
    read_custom_image_setup_session_company_id, read_future_date, read_nullable_date,
    require_future_custom_image_setup_expiry, SetupSessionExpiredError,
};
pub use custom_image_terminal_sessions::{
    parse_custom_image_setup_ssh_command, validate_custom_image_setup_ssh_payload,
    CreateTerminalSessionInput, EnvironmentCustomImageTerminalConnectionClose,
    EnvironmentCustomImageTerminalConnectionRegistry,
    EnvironmentCustomImageTerminalPayloadValidationFailureCode,
    EnvironmentCustomImageTerminalPayloadValidationResult,
    EnvironmentCustomImageTerminalSessionRecord,
    EnvironmentCustomImageTerminalSessionStore, MintedEnvironmentCustomImageTerminalSession,
    ParsedCustomImageSetupSshCommand,
    ENVIRONMENT_CUSTOM_IMAGE_TERMINAL_CONNECTION_REGISTRY,
    ENVIRONMENT_CUSTOM_IMAGE_TERMINAL_SESSION_STORE,
    DEFAULT_TERMINAL_SESSION_TOKEN_TTL_MS, TERMINAL_SESSION_TOKEN_BYTES,
};
pub use plugin_environment_driver_pure::{
    plugin_driver_provider_key, resolve_plugin_execute_rpc_timeout_ms,
    PluginEnvironmentDriverKey, DEFAULT_READY_PLUGIN_WORKER_RECOVERY_TIMEOUT_MS,
    RPC_OVERHEAD_BUFFER_MS,
};

pub use runtime_parity::{
    build_environment_lease_context, find_reusable_sandbox_lease_id, EnvironmentLeaseContext,
    ExecutionWorkspaceRef, SandboxConfigRef, SandboxLeaseCandidate,
};
pub use service::{
    EnvironmentError, EnvironmentHook, EnvironmentHookEvent, EnvironmentService,
    NoopEnvironmentHook, RecordingEnvironmentHook,
};
