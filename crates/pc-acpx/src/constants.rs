//! `pc-acpx` constants — defaults that mirror the Node `acpx-engine/constants.ts`.

// ============================================================================
// Default values for the ACP engine executor
//
// Aligned with Node `packages/adapter-utils/src/acpx-engine/constants.ts` +
// `default*` exports from `index.ts`. These are the public defaults an
// adapter can lean on when the caller does not pin a value.
// ============================================================================

/// Default agent name when the caller does not pin one.
pub const DEFAULT_ACP_ENGINE_AGENT: &str = "claude";

/// Default session mode (`persistent` keeps the session between turns, `oneshot`
/// rebuilds it every turn).
pub const DEFAULT_ACP_ENGINE_MODE: &str = "persistent";

/// Default permission mode for the supported agent runtimes.
pub const DEFAULT_ACP_ENGINE_PERMISSION_MODE: &str = "approve-all";

/// Default non-interactive permission policy — `deny` short-circuits with a
/// fail-closed error instead of trying to grant non-interactive holds.
pub const DEFAULT_ACP_ENGINE_NON_INTERACTIVE_PERMISSIONS: &str = "deny";

/// Default wall-clock timeout, in seconds. `0` disables the timeout.
pub const DEFAULT_ACP_ENGINE_TIMEOUT_SEC: u64 = 0;

/// Default idle window for the warm-handle cache.
pub const DEFAULT_ACP_ENGINE_WARM_HANDLE_IDLE_MS: u64 = 0;

/// Gemini version probe timeout (used in I/O path; declared here so the
/// `gemini_version` tests can assert the constant).
pub const GEMINI_VERSION_PROBE_TIMEOUT_MS: u64 = 2_000;

/// Gemini native `acp` flag minimum version. Versions **at or above** this
/// support `--acp`; earlier versions need `--experimental-acp`.
pub const GEMINI_NATIVE_ACP_FLAG_MIN_VERSION: [u32; 3] = [0, 33, 0];

// ============================================================================
// Adapter type → ACPX agent id mapping (mirrors Node `ACPX_ADAPTER_AGENT_IDS`)
// ============================================================================

/// Stable mapping from `AcpxAdapterType` to its ACPX agent id.
pub const ACPX_ADAPTER_AGENT_IDS: &[(&str, &str)] = &[
    ("claude_local", "claude"),
    ("codex_local", "codex"),
    ("gemini_local", "gemini"),
    ("custom_acp", "custom"),
];

/// Look up the ACPX agent id for a Paperclip adapter type, returning `None`
/// for unknown adapters.
pub fn acpx_agent_id_for_adapter_type(adapter_type: Option<&str>) -> Option<&'static str> {
    let adapter_type = adapter_type?;
    ACPX_ADAPTER_AGENT_IDS
        .iter()
        .find(|(key, _)| *key == adapter_type)
        .map(|(_, value)| *value)
}
