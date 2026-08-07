//! `pc-acpx` — Pure helpers and async I/O primitives for the Paperclip
//! ACPX engine. The crate is a Rust port of `packages/adapter-utils/src/
//! acpx-engine/` from the Node `paperclip` monorepo.
//!
//! ## Layers
//!
//! - **Pure helpers** (no I/O): `constants`, `gemini_version`, `session_codec`,
//!   `hash`, `normalize`, `transcript`, `usage`.
//! - **Resolver** (no I/O, but requires a fallback filesystem context):
//!   `settings`.
//! - **Async I/O helpers** (uses `tokio::fs`): `fs_ops`, `bin`.
//! - **Engine wire types** (no I/O): `agent_command`, `startup_metrics`,
//!   `prepared_runtime`.
//!
//! Each module is independent: callers opt in to the helpers they need.

#![deny(rust_2018_idioms)]
#![warn(unused_must_use)]

pub mod acp_runtime;
pub mod adapter_skills;
pub mod acpx_engine_executor;
pub mod agent_command;
pub mod bin;
pub mod build_prompt;
pub mod build_runtime;
pub mod cache;
pub mod cache_lifecycle;
pub mod child_stderr;
pub mod codex_startup_config;
pub mod constants;
pub mod env_helpers;
pub mod error;
pub mod error_classification;
pub mod fs_ops;
pub mod gemini_command_shell;
pub mod gemini_version;
pub mod hash;
pub mod instance_root;
pub mod jsonrpc_wire;
pub mod log_redaction;
pub mod managed_home;
pub mod normalize;
pub mod paperclip_claude_settings;
pub mod paths;
pub mod prepared_runtime;
pub mod prompt_compose;
pub mod reconcile_skills;
pub mod session_codec;
pub mod session_compat;
pub mod session_config_options;
pub mod settings;
pub mod skill_io;
pub mod skill_materialize;
pub mod skill_runtime;
pub mod skill_snapshot;
pub mod skill_sync_preference;
pub mod startup_metrics;
pub mod startup_timing;
pub mod subprocess_acp_runtime;
pub mod subprocess_handle;
pub mod subprocess_signal;
pub mod transcript;
pub mod usage;
pub mod workspace_env;

pub use adapter_skills::AdapterSkillContext;

pub use acp_runtime::{
    AcpRuntime, AcpRuntimeAvailableCommand, AcpRuntimeCancelInput, AcpRuntimeCapabilities,
    AcpRuntimeCloseInput, AcpRuntimeControl, AcpRuntimeDoctorReport, AcpRuntimeEnsureInput,
    AcpRuntimeError, AcpRuntimeEvent, AcpRuntimeEventStream, AcpRuntimeGetCapabilitiesInput,
    AcpRuntimeGetStatusInput, AcpRuntimeHandle, AcpRuntimeMode, AcpRuntimePromptMode,
    AcpRuntimeSessionModels, AcpRuntimeSessionUsage, AcpRuntimeSetConfigOptionInput,
    AcpRuntimeSetModeInput, AcpRuntimeStatus, AcpRuntimeStream, AcpRuntimeToolCallLocation,
    AcpRuntimeTurn, AcpRuntimeTurnAttachment, AcpRuntimeTurnInput, AcpRuntimeTurnResult,
    AcpRuntimeTurnResultError, AcpRuntimeTurnResultFuture, AcpRuntimeTurnResultResolver,
    AcpRuntimeUsageBreakdown, AcpRuntimeUsageCost, McpServerEntry, MockAcpRuntime,
    SessionAgentOptions,
};
pub use agent_command::{
    resolve_built_in_agent_command, shell_quote, BuiltInAgentCommand,
    ResolveBuiltInAgentCommandInput,
};
pub use bin::{find_ancestor_bin, Platform};

pub use cache::{
    cleanup_idle_with_report, AsyncKeyedLocks, IdleCache, IdleEvictionReport, LastUsed,
};
pub use cache_lifecycle::{
    cleanup_idle_handles, cleanup_idle_staged_runtimes, clear_warm_handle_timer, close_warm_handle,
    discard_staged_runtime, save_staged_runtime_after_clean_turn, schedule_idle_handle_cleanup,
    warm_handle_matches, with_session_staging_lease, AsyncCallback, RuntimeCacheEntry,
    SessionStagingLease, SessionStagingLocks, StagedRuntimeCacheEntry, TokioCleanupHandle,
};
pub use child_stderr::{
    flush_child_stderr, flush_child_stderr_with, read_child_stderr_tail, route_child_stderr,
    route_child_stderr_with, ChildStderrError, ChildStderrState, FlushedStderr, RoutedStderr,
    BENIGN_NES_CLOSE_STDERR,
};

pub use constants::{
    acpx_agent_id_for_adapter_type, ACPX_ADAPTER_AGENT_IDS, DEFAULT_ACP_ENGINE_AGENT,
    DEFAULT_ACP_ENGINE_MODE, DEFAULT_ACP_ENGINE_NON_INTERACTIVE_PERMISSIONS,
    DEFAULT_ACP_ENGINE_PERMISSION_MODE, DEFAULT_ACP_ENGINE_TIMEOUT_SEC,
    DEFAULT_ACP_ENGINE_WARM_HANDLE_IDLE_MS, GEMINI_NATIVE_ACP_FLAG_MIN_VERSION,
    GEMINI_VERSION_PROBE_TIMEOUT_MS,
};
pub use env_helpers::{default_path_for_platform, ensure_path_in_env, resolve_runtime_env};
pub use log_redaction::{
    build_invocation_env_for_logs, expand_home_prefix, is_forbidden_config_env_key,
    is_paperclip_runtime_env_key, is_pid_alive, is_sensitive_env_key, redact_command_text_for_logs,
    redact_env_for_logs, sanitize_inherited_paperclip_env, InvocationEnvOptions,
    DEFAULT_RESOLVED_COMMAND_ENV_KEY, REDACTED_COMMAND_TEXT_VALUE, REDACTED_LOG_VALUE,
};
pub use subprocess_signal::{
    signal_running_process, Signal, SignalOutcome, SignalRunningProcessInput,
};
pub use workspace_env::{
    read_env_value_case_insensitive, refresh_paperclip_workspace_env_for_execution,
    rewrite_workspace_cwd_env_vars_for_execution, sanitize_remote_execution_env,
    sanitize_ssh_remote_env, shape_paperclip_workspace_env_for_execution, RefreshWorkspaceEnvInput,
    ShapeWorkspaceEnvInput, ShapedWorkspaceEnv, WorkspaceHint, REMOTE_EXECUTION_ENV_IDENTITY_KEYS,
};

pub use error::AcpxError;

pub use error_classification::{
    classify_error, describe_error_diagnostics, is_resume_failure, AcpxErrorDiagnostics,
    AcpxExecutionPhase, ClassifiedError,
};

pub use fs_ops::{
    ensure_copied_file, ensure_parent_dir, ensure_symlink, lstat_or_none, path_exists,
    path_is_file, readlink_or_none, remove_path_if_exists, symlink_or_copy_file,
    write_file_atomically, WriteFileAtomicallyInput,
};

pub use gemini_version::{
    gemini_acp_command_tokens, gemini_version_supports_native_acp_flag, parse_gemini_version_parts,
    rewrite_gemini_acp_flag_for_version,
};

pub use hash::{short_hash, stable_json};

pub use normalize::{
    normalize_agent, normalize_mode, normalize_non_interactive_permissions,
    normalize_permission_mode, normalize_requested_thinking_effort, NormalizedMode,
    NormalizedNonInteractivePermissions, NormalizedPermissionMode,
};

pub use managed_home::{
    prepare_managed_codex_home, read_managed_codex_skills_manifest,
    write_managed_codex_skills_manifest, LogStream, ManagedSkillsManifest, OnLogSink,
    PrepareManagedCodexHomeInput, PAPERCLIP_MANAGED_CODEX_SKILLS_MANIFEST,
};

pub use prepared_runtime::{
    format_timeout_start_log_line, PreparedRuntime, PreparedRuntimeBuilder, PreparedRuntimeMode,
    PreparedRuntimeNonInteractivePermissions, PreparedRuntimePermissionMode, PreparedStagedRuntime,
    TimeoutResolution,
};

pub use build_prompt::{build_prompt, BuildPromptInput, BuildPromptMetrics, BuildPromptOutput};
pub use build_runtime::{
    apply_paperclip_workspace_env, build_paperclip_env, build_runtime, AgentIdentity,
    BuildRuntimeInput, WakeContext, WorkspaceHints,
};

pub use acpx_engine_executor::{
    system_now_ms, AcpxEngineExecutor, AcpxEngineExecutorDeps, AcpxEngineExecutorState,
    AcpxRuntimeFactory, AdapterExecutionContext, AdapterExecutionResult, AdapterExecutionSink,
    EnsureOutcome, ExecutorLogStream, NoopSink, NowFn,
};

pub use reconcile_skills::{
    reconcile_managed_codex_skills, ReconcileManagedCodexSkillsInput, RevocationPhase,
    RevocationRecord,
};

pub use session_codec::{
    build_session_params, deserialize as session_codec_deserialize,
    get_display_id as session_codec_get_display_id, serialize as session_codec_serialize,
    AcpxSessionParams,
};

pub use skill_io::{
    ensure_paperclip_skill_symlink, ensure_paperclip_skill_symlink_with_linker,
    is_maintainer_only_skill_target, list_paperclip_skill_entries,
    normalize_configured_paperclip_runtime_skills, read_installed_skill_targets,
    read_paperclip_runtime_skill_entries, read_paperclip_skill_markdown,
    remove_maintainer_only_skill_symlinks, resolve_paperclip_skills_dir, SkillSymlinkOutcome,
    PAPERCLIP_SKILL_KEY_PREFIX, PAPERCLIP_SKILL_ROOT_RELATIVE_CANDIDATES,
};

pub use skill_materialize::{
    acquire_materialize_lock, build_skill_set_key, hash_path_contents, hash_skill_directory,
    materialize_paperclip_skill_copy, materialized_skill_fingerprint_matches,
    remove_stale_materialize_lock, MaterializedSkillCopyResult, PaperclipSkillEntry,
    SkillSourceStatus, MATERIALIZED_SKILL_LOCK_OWNER, MATERIALIZED_SKILL_LOCK_STALE_MS,
    MATERIALIZED_SKILL_SENTINEL,
};

pub use skill_runtime::{
    prepare_claude_skill_runtime, prepare_codex_skill_runtime, prepare_gemini_skill_runtime,
    resolve_selected_runtime_skills, PrepareClaudeSkillRuntimeInput, PrepareCodexSkillRuntimeInput,
    PrepareGeminiSkillRuntimeInput, PrepareSkillRuntimeOutput, SkillRuntimeIdentity,
};
pub use skill_snapshot::{
    build_managed_skill_origin, build_persistent_skill_snapshot,
    build_runtime_mounted_skill_snapshot, is_paperclip_skill_source_missing,
    resolve_paperclip_skill_missing_detail, resolve_skill_detail, skill_location_label,
    AdapterDesiredSkillEntry, AdapterSkillEntry, AdapterSkillOrigin, AdapterSkillSnapshot,
    AdapterSkillState, AdapterSkillSyncMode, InstalledSkillTarget, InstalledSkillTargetKind,
    PaperclipSkillSourceStatus, PersistentSkillSnapshotOptions, RuntimeMountedSkillSnapshotOptions,
    SkillDetail,
};

pub use skill_sync_preference::{
    canonicalize_desired_paperclip_skill_reference, read_paperclip_skill_sync_preference,
    resolve_paperclip_desired_skill_names, write_paperclip_skill_sync_preference,
    AvailableSkillEntry, PaperclipDesiredSkillEntry, SkillSyncPreference, SkillSyncPreferenceInput,
};

pub use settings::{resolve_engine_settings, AcpxEngineOptions, AcpxEngineSettings};

pub use startup_timing::{
    build_step_event, measure_startup_step, normalize_provider_family, NoopSpanContext,
    NoopStartupSpan, NoopStartupTraceContext, NoopStartupTracer, RuntimeStartupStepEvent,
    StartupSpan, StartupSpanAttribute, StartupSpanContextAny, StartupSpanStatus,
    StartupStepContext, StartupStepMeasureOptions, StartupTraceContext, StartupTracer,
    BUILT_IN_PROVIDER_FAMILIES, PLUGIN_PROVIDER_FAMILY, RUN_STARTUP_STEP_EVENT_TYPE,
    SPAN_STATUS_CODE_ERROR,
};

pub use codex_startup_config::{
    build_codex_startup_config, CodexStartupConfigInput, CodexStartupConfigOutput,
};
pub use gemini_command_shell::{
    normalize_gemini_acp_command_shell, normalize_gemini_acp_command_shell_with_env,
};
pub use instance_root::{
    default_resolve_paperclip_instance_root_for_adapter, is_valid_paperclip_instance_id,
    resolve_paperclip_instance_root_for_adapter, ResolvePaperclipInstanceRootError,
    ResolvePaperclipInstanceRootInput, DEFAULT_PAPERCLIP_HOME_SUFFIX,
    DEFAULT_PAPERCLIP_INSTANCE_ID, INSTANCES_DIR_NAME, PAPERCLIP_HOME_ENV,
    PAPERCLIP_INSTANCE_ID_ENV,
};
pub use jsonrpc_wire::{
    decode_jsonrpc_frame, encode_jsonrpc_error, encode_jsonrpc_notification,
    encode_jsonrpc_request, encode_jsonrpc_response, jsonrpc_error_from_value, next_jsonrpc_id,
    parse_jsonrpc_line, JsonRpcErrorBody, JsonRpcFrame, JsonRpcIdAllocator, JsonRpcNotification,
    JsonRpcRequest, JsonRpcResponse, JSONRPC_VERSION,
};
pub use paperclip_claude_settings::{
    paperclip_claude_settings_write_with, referenced_source_content_signature,
    ClaudeSettingsWriteInput, PaperclipClaudeSettingsResult,
};
pub use paths::{
    default_paperclip_instance_dir, default_state_dir, resolve_managed_codex_home_dir,
    resolve_paperclip_instance_root,
};
pub use session_compat::{is_compatible_session, unique_sorted, AcpxPreparedRuntimeLite};
pub use session_config_options::{
    render_api_access_note, render_paperclip_env_note, result_error_message,
    session_config_options, usage_breakdowns_equal, SessionConfigOption,
};
pub use startup_metrics::{build_startup_step_metrics, StartupMetricsSource, StartupStepMetrics};
pub use subprocess_acp_runtime::{SubprocessAcpRuntime, SubprocessAcpRuntimeSpec};
pub use subprocess_handle::{SpawnAcpxInput, SubprocessHandle, SubprocessTermination};

pub use prompt_compose::normalize_paperclip_wake_payload;
pub use prompt_compose::{
    is_assignment_shaped_paperclip_wake_reason, is_paperclip_recovery_wake_payload,
    join_prompt_sections, join_prompt_sections_with_separator, render_paperclip_wake_prompt,
    render_template, select_paperclip_task_markdown, NormalizedPaperclipWake,
    PaperclipWakeAgentMessage, PaperclipWakeCheckboxOption, PaperclipWakeCheckboxSelection,
    PaperclipWakeChildIssueSummary, PaperclipWakeComment, PaperclipWakeExecutionStage,
    PaperclipWakeExecutionWorkspace, PaperclipWakeIssue, PaperclipWakeOriginalAssignee,
    PaperclipWakeRecovery, RenderWakePromptOptions, SelectTaskMarkdownOptions,
    ASSIGNMENT_SHAPED_PAPERCLIP_WAKE_REASONS,
};
pub use transcript::{
    parse_acpx_stdout_line, summarize_tool_call, ToolCallSummary, TranscriptEntry,
};

pub use usage::{
    summarize_acpx_turn_usage, summarize_from_value, AcpxRuntimeStatusView, AcpxRuntimeUsageView,
    AcpxTurnUsageBreakdown, AcpxTurnUsageCost, SummarizeAcpxTurnUsageInput,
    SummarizeAcpxTurnUsageOutput, UsageSummary,
};
