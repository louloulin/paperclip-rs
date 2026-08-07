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
pub mod agent_command;
pub mod bin;
pub mod cache;
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
pub mod jsonrpc_wire;
pub mod managed_home;
pub mod normalize;
pub mod paperclip_claude_settings;
pub mod paths;
pub mod prepared_runtime;
pub mod reconcile_skills;
pub mod session_codec;
pub mod session_compat;
pub mod session_config_options;
pub mod settings;
pub mod skill_materialize;
pub mod skill_runtime;
pub mod startup_metrics;
pub mod startup_timing;
pub mod subprocess_acp_runtime;
pub mod subprocess_handle;
pub mod transcript;
pub mod usage;

pub use acp_runtime::{
    AcpRuntime, AcpRuntimeAvailableCommand, AcpRuntimeCancelInput, AcpRuntimeCapabilities,
    AcpRuntimeCloseInput, AcpRuntimeControl, AcpRuntimeDoctorReport, AcpRuntimeEnsureInput,
    AcpRuntimeError, AcpRuntimeEvent, AcpRuntimeEventStream, AcpRuntimeGetCapabilitiesInput,
    AcpRuntimeGetStatusInput, AcpRuntimeHandle, AcpRuntimeMode, AcpRuntimePromptMode,
    AcpRuntimeSessionModels, AcpRuntimeSessionUsage, AcpRuntimeSetConfigOptionInput,
    AcpRuntimeSetModeInput, AcpRuntimeStatus, AcpRuntimeStream, AcpRuntimeToolCallLocation,
    AcpRuntimeTurn, AcpRuntimeTurnAttachment, AcpRuntimeTurnInput, AcpRuntimeTurnResult,
    AcpRuntimeTurnResultError, AcpRuntimeTurnResultFuture, AcpRuntimeTurnResultResolver,
    AcpRuntimeUsageBreakdown, AcpRuntimeUsageCost, McpServerEntry, SessionAgentOptions,
};
pub use agent_command::{
    resolve_built_in_agent_command, shell_quote, BuiltInAgentCommand,
    ResolveBuiltInAgentCommandInput,
};
pub use bin::{find_ancestor_bin, Platform};

pub use cache::{
    cleanup_idle_with_report, AsyncKeyedLocks, IdleCache, IdleEvictionReport, LastUsed,
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
    PreparedRuntimeNonInteractivePermissions, PreparedRuntimePermissionMode, TimeoutResolution,
};

pub use reconcile_skills::{
    reconcile_managed_codex_skills, ReconcileManagedCodexSkillsInput, RevocationPhase,
    RevocationRecord,
};

pub use session_codec::{
    deserialize as session_codec_deserialize, get_display_id as session_codec_get_display_id,
    serialize as session_codec_serialize, AcpxSessionParams,
};

pub use skill_materialize::{
    build_skill_set_key, hash_path_contents, materialize_paperclip_skill_copy,
    MaterializedSkillCopyResult, PaperclipSkillEntry, SkillSourceStatus,
};

pub use skill_runtime::{
    prepare_claude_skill_runtime, prepare_codex_skill_runtime, prepare_gemini_skill_runtime,
    resolve_selected_runtime_skills, PrepareClaudeSkillRuntimeInput, PrepareCodexSkillRuntimeInput,
    PrepareGeminiSkillRuntimeInput, PrepareSkillRuntimeOutput, SkillRuntimeIdentity,
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
    default_paperclip_instance_dir, default_state_dir, expand_home_prefix,
    resolve_managed_codex_home_dir, resolve_paperclip_instance_root,
};
pub use session_compat::{is_compatible_session, unique_sorted, AcpxPreparedRuntimeLite};
pub use session_config_options::{
    render_api_access_note, render_paperclip_env_note, result_error_message,
    session_config_options, usage_breakdowns_equal, SessionConfigOption,
};
pub use startup_metrics::{build_startup_step_metrics, StartupMetricsSource, StartupStepMetrics};
pub use subprocess_acp_runtime::{SubprocessAcpRuntime, SubprocessAcpRuntimeSpec};
pub use subprocess_handle::{SpawnAcpxInput, SubprocessHandle, SubprocessTermination};

pub use transcript::{
    parse_acpx_stdout_line, summarize_tool_call, ToolCallSummary, TranscriptEntry,
};

pub use usage::{
    summarize_acpx_turn_usage, summarize_from_value, AcpxRuntimeStatusView, AcpxRuntimeUsageView,
    AcpxTurnUsageBreakdown, AcpxTurnUsageCost, SummarizeAcpxTurnUsageInput,
    SummarizeAcpxTurnUsageOutput, UsageSummary,
};
