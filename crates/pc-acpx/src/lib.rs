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
pub mod constants;
pub mod error;
pub mod fs_ops;
pub mod gemini_version;
pub mod hash;
pub mod normalize;
pub mod prepared_runtime;
pub mod session_codec;
pub mod settings;
pub mod startup_metrics;
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
pub use constants::{
    acpx_agent_id_for_adapter_type, ACPX_ADAPTER_AGENT_IDS, DEFAULT_ACP_ENGINE_AGENT,
    DEFAULT_ACP_ENGINE_MODE, DEFAULT_ACP_ENGINE_NON_INTERACTIVE_PERMISSIONS,
    DEFAULT_ACP_ENGINE_PERMISSION_MODE, DEFAULT_ACP_ENGINE_TIMEOUT_SEC,
    DEFAULT_ACP_ENGINE_WARM_HANDLE_IDLE_MS, GEMINI_NATIVE_ACP_FLAG_MIN_VERSION,
    GEMINI_VERSION_PROBE_TIMEOUT_MS,
};

pub use error::AcpxError;

pub use fs_ops::{
    ensure_parent_dir, path_exists, path_is_file, write_file_atomically, WriteFileAtomicallyInput,
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

pub use prepared_runtime::{
    format_timeout_start_log_line, PreparedRuntime, PreparedRuntimeBuilder, PreparedRuntimeMode,
    PreparedRuntimeNonInteractivePermissions, PreparedRuntimePermissionMode, TimeoutResolution,
};

pub use session_codec::{
    deserialize as session_codec_deserialize, get_display_id as session_codec_get_display_id,
    serialize as session_codec_serialize, AcpxSessionParams,
};

pub use settings::{resolve_engine_settings, AcpxEngineOptions, AcpxEngineSettings};

pub use startup_metrics::{build_startup_step_metrics, StartupMetricsSource, StartupStepMetrics};

pub use transcript::{
    parse_acpx_stdout_line, summarize_tool_call, ToolCallSummary, TranscriptEntry,
};

pub use usage::{
    summarize_acpx_turn_usage, summarize_from_value, AcpxRuntimeStatusView, AcpxRuntimeUsageView,
    AcpxTurnUsageBreakdown, AcpxTurnUsageCost, SummarizeAcpxTurnUsageInput,
    SummarizeAcpxTurnUsageOutput, UsageSummary,
};
