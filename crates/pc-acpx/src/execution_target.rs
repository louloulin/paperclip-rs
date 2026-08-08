//! `pc-acpx::execution_target` - port of `execution-target.ts` from Node
//! `paperclip/packages/adapter-utils/src/`.
//!
//! Pure helpers for the adapter execution-target router. Async
//! functions (`ensureAdapterExecutionTargetCommandResolvable`,
//! `runAdapterExecutionTargetProcess`,
//! `runAdapterExecutionTargetShellCommand`,
//! `maybeRunSandboxInstallCommand`,
//! `readAdapterExecutionTargetHomeDir`,
//! `ensureAdapterExecutionTargetFile`,
//! `ensureAdapterExecutionTargetDirectory`,
//! `prepareAdapterExecutionTargetRuntime`,
//! `startAdapterExecutionTargetProcessSessionBridge`,
//! `startAdapterExecutionTargetPaperclipBridge`) are deferred -
//! they require real process exec, ssh runtime, sandbox runtime, and
//! HTTP server / WebSocket plumbing. This module ports:
//!
//! - All public types (10 interfaces + 1 type alias)
//! - Constant: `DEFAULT_REMOTE_SANDBOX_ADAPTER_TIMEOUT_SEC`
//! - Small helpers: `parse_object`, `read_string`,
//!   `read_string_meta`, `resolve_host_for_url`,
//!   `resolve_default_paperclip_api_url`,
//!   `is_bridge_debug_enabled`,
//!   `is_adapter_execution_target_instance`
//! - Public sync API:
//!   - `adapter_execution_target_to_remote_spec`
//!   - `adapter_execution_target_is_remote`
//!   - `adapter_execution_target_uses_managed_home`
//!   - `adapter_execution_target_remote_cwd`
//!   - `override_adapter_execution_target_remote_cwd`
//!   - `resolve_adapter_execution_target_cwd`
//!   - `adapter_execution_target_uses_paperclip_bridge`
//!   - `describe_adapter_execution_target`
//!   - `resolve_adapter_execution_target_timeout` /
//!     `resolve_adapter_execution_target_timeout_sec`
//!   - `format_adapter_execution_timeout_error_message`
//!   - `format_adapter_execution_timeout_start_log_line`
//!   - `adapter_execution_target_session_identity` /
//!     `adapter_execution_target_session_matches`
//!   - `parse_adapter_execution_target`
//!   - `adapter_execution_target_from_remote_execution`
//!   - `read_adapter_execution_target`
//!   - `runtime_asset_dir`

use serde::{Deserialize, Serialize};


// =============================================================================
// Constants
// =============================================================================

/// 4-hour wall-clock backstop for sandbox-backed adapter runs. This is a
/// last-resort kill switch, not the primary hang detector. The value
/// intentionally matches the recovery watchdog's
/// ACTIVE_RUN_OUTPUT_CRITICAL_THRESHOLD_MS in
/// `server/src/services/recovery/service.ts` so healthy long runs are
/// never killed by the adapter before the watchdog would even consider
/// them stuck.
pub const DEFAULT_REMOTE_SANDBOX_ADAPTER_TIMEOUT_SEC: u64 = 14_400;

// =============================================================================
// SSH execution spec - inlined here for parity with the Node source. This is
// the same `SshRemoteExecutionSpec` interface used by `./ssh.js`. When the
// pc-acpx ssh module lands in R403, the type + parser will move to
// `pc_acpx::ssh` and re-exported from there.
// =============================================================================

/// Re-exported SSH parser - canonical location is `pc_acpx::ssh`.
pub use crate::ssh::parse_ssh_remote_execution_spec;

/// Canonical SSH remote execution spec - canonical location is
/// `pc_acpx::ssh`. Re-exported here so existing call sites that
/// imported `pc_acpx::execution_target::SshRemoteExecutionSpec`
/// continue to work after the R403 split.
pub use crate::ssh::SshRemoteExecutionSpec;

// =============================================================================
// Internal small helpers
// =============================================================================

/// `value` coerced to a plain object map (returns an empty map when
/// not an object). Mirrors the Node `parseObject` helper.
#[must_use]
pub fn parse_object(value: &serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match value {
        serde_json::Value::Object(map) => map.clone(),
        _ => serde_json::Map::new(),
    }
}

/// Coerce a JSON value to a trimmed, non-empty string. Mirrors Node
/// `readString`.
#[must_use]
pub fn read_string(value: &serde_json::Value) -> Option<String> {
    let s = value.as_str()?;
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Convenience: `parse_object(parent).get(key).map(read_string)`.
/// Mirrors Node `readStringMeta`.
#[must_use]
pub fn read_string_meta(
    parsed: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<String> {
    parsed.get(key).and_then(read_string)
}

/// Normalize a URL host string. Mirrors Node `resolveHostForUrl`.
#[must_use]
pub fn resolve_host_for_url(raw_host: &str) -> String {
    let host = raw_host.trim();
    if host.is_empty() || host == "0.0.0.0" || host == "::" {
        return "localhost".to_string();
    }
    if host.contains(':') && !host.starts_with('[') && !host.ends_with(']') {
        return format!("[{host}]");
    }
    host.to_string()
}

/// Best-effort default Paperclip API URL, derived from environment
/// variables (mocked to localhost when none provided so the helper
/// remains pure). Mirrors Node `resolveDefaultPaperclipApiUrl`.
#[must_use]
pub fn resolve_default_paperclip_api_url_from(
    listen_host: Option<&str>,
    listen_port: Option<&str>,
    fallback_host: Option<&str>,
    fallback_port: Option<&str>,
) -> String {
    let raw = listen_host
        .or(fallback_host)
        .unwrap_or("localhost");
    let host = resolve_host_for_url(raw);
    let port = listen_host
        .map(|_| listen_port.unwrap_or("3100"))
        .or(fallback_port)
        .unwrap_or("3100");
    format!("http://{host}:{port}")
}

/// Check whether the optional PAPERCLIP_BRIDGE_DEBUG env flag is
/// enabled. Mirrors Node `isBridgeDebugEnabled`. Decoupled from
/// `process.env` so the helper stays pure.
#[must_use]
pub fn is_bridge_debug_enabled_from(env_value: Option<&str>) -> bool {
    match env_value.map(|v| v.trim().to_lowercase()) {
        Some(ref v) if v == "1" || v == "true" || v == "yes" => true,
        _ => false,
    }
}

/// Type predicate mirroring Node
/// `isAdapterExecutionTargetInstance`. Returns true when `value`
/// looks like a valid `AdapterExecutionTarget` (local / ssh /
/// sandbox variant).
#[must_use]
pub fn is_adapter_execution_target_instance(value: &serde_json::Value) -> bool {
    let parsed = parse_object(value);
    let kind = read_string_meta(&parsed, "kind").unwrap_or_default();
    if kind == "local" {
        return true;
    }
    if kind != "remote" {
        return false;
    }
    let transport = read_string_meta(&parsed, "transport").unwrap_or_default();
    if transport == "ssh" {
        let spec_value = parsed.get("spec").cloned().map_or_else(
            || serde_json::Value::Object(serde_json::Map::new()),
            |v| match v {
                serde_json::Value::Object(m) => serde_json::Value::Object(m),
                _ => serde_json::Value::Object(serde_json::Map::new()),
            },
        );
        return parse_ssh_remote_execution_spec(&spec_value).is_some();
    }
    if transport != "sandbox" {
        return false;
    }
    read_string_meta(&parsed, "remoteCwd").is_some()
}

// =============================================================================
// Core types - mirrored 1:1 from Node interfaces.
// =============================================================================

/// Workspace realization mode. Mirrors Node
/// `AdapterWorkspaceRealizationMode`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterWorkspaceRealizationMode {
    Copy,
    InPlace,
}

/// One path alias inside a workspace realization. Mirrors Node
/// `AdapterWorkspacePathAlias`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterWorkspacePathAlias {
    pub path: String,
    pub target: String,
}

/// How to materialize the adapter workspace before the run. Mirrors
/// Node `AdapterWorkspaceRealization`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterWorkspaceRealization {
    pub mode: AdapterWorkspaceRealizationMode,
    pub authoritative_root: String,
    pub path_aliases: Vec<AdapterWorkspacePathAlias>,
    pub outbound_restore_paths: Vec<String>,
}

/// Common workspace metadata attached to every
/// `AdapterExecutionTarget`. Mirrors Node
/// `AdapterExecutionTargetWorkspaceMetadata`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AdapterExecutionTargetWorkspaceMetadata {
    pub workspace_realization: Option<AdapterWorkspaceRealization>,
}

/// Local (non-remote) execution target. Mirrors Node
/// `AdapterLocalExecutionTarget`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AdapterLocalExecutionTarget {
    pub kind: String, // "local"
    pub environment_id: Option<String>,
    pub lease_id: Option<String>,
    pub workspace_realization: Option<AdapterWorkspaceRealization>,
}

/// SSH-backed remote execution target. Mirrors Node
/// `AdapterSshExecutionTarget`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterSshExecutionTarget {
    pub kind: String, // "remote"
    pub transport: String, // "ssh"
    pub environment_id: Option<String>,
    pub lease_id: Option<String>,
    pub remote_cwd: String,
    pub spec: SshRemoteExecutionSpec,
    pub workspace_realization: Option<AdapterWorkspaceRealization>,
}

/// Sandbox-backed remote execution target. Mirrors Node
/// `AdapterSandboxExecutionTarget`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterSandboxExecutionTarget {
    pub kind: String, // "remote"
    pub transport: String, // "sandbox"
    pub provider_key: Option<String>,
    pub shell_command: Option<String>,
    pub environment_id: Option<String>,
    pub lease_id: Option<String>,
    pub remote_cwd: String,
    pub timeout_ms: Option<u64>,
    pub stream_run_logs: Option<bool>,
    pub workspace_realization: Option<AdapterWorkspaceRealization>,
}

/// The union of all execution targets. Mirrors Node
/// `AdapterExecutionTarget` (which is a TypeScript discriminated
/// union of Local / Ssh / Sandbox).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum AdapterExecutionTarget {
    #[serde(rename = "local")]
    Local(AdapterLocalExecutionTarget),
    #[serde(rename = "remote")]
    Remote(AdapterRemoteExecutionTarget),
}

/// Transport-specific Remote body. The `transport` field discriminates
/// between SSH and Sandbox; callers pattern-match via `match` (or
/// `as_ssh()` / `as_sandbox()` helpers).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "transport")]
pub enum AdapterRemoteExecutionTarget {
    #[serde(rename = "ssh")]
    Ssh(AdapterSshExecutionTarget),
    #[serde(rename = "sandbox")]
    Sandbox(AdapterSandboxExecutionTarget),
}

impl AdapterRemoteExecutionTarget {
    #[must_use]
    pub fn remote_cwd(&self) -> &str {
        match self {
            Self::Ssh(s) => &s.remote_cwd,
            Self::Sandbox(s) => &s.remote_cwd,
        }
    }

    #[must_use]
    pub fn environment_id(&self) -> Option<&str> {
        match self {
            Self::Ssh(s) => s.environment_id.as_deref(),
            Self::Sandbox(s) => s.environment_id.as_deref(),
        }
    }

    #[must_use]
    pub fn lease_id(&self) -> Option<&str> {
        match self {
            Self::Ssh(s) => s.lease_id.as_deref(),
            Self::Sandbox(s) => s.lease_id.as_deref(),
        }
    }

    #[must_use]
    pub fn workspace_realization(&self) -> Option<&AdapterWorkspaceRealization> {
        match self {
            Self::Ssh(s) => s.workspace_realization.as_ref(),
            Self::Sandbox(s) => s.workspace_realization.as_ref(),
        }
    }
}

impl AdapterExecutionTarget {
    /// Convenience accessor for the local payload, when `kind == local`.
    #[must_use]
    pub fn as_local(&self) -> Option<&AdapterLocalExecutionTarget> {
        match self {
            Self::Local(l) => Some(l),
            _ => None,
        }
    }
    /// Convenience accessor for the SSH payload.
    #[must_use]
    pub fn as_ssh(&self) -> Option<&AdapterSshExecutionTarget> {
        match self {
            Self::Remote(AdapterRemoteExecutionTarget::Ssh(s)) => Some(s),
            _ => None,
        }
    }
    /// Convenience accessor for the Sandbox payload.
    #[must_use]
    pub fn as_sandbox(&self) -> Option<&AdapterSandboxExecutionTarget> {
        match self {
            Self::Remote(AdapterRemoteExecutionTarget::Sandbox(s)) => Some(s),
            _ => None,
        }
    }
    /// Convenience accessor for any remote body.
    #[must_use]
    pub fn as_remote(&self) -> Option<&AdapterRemoteExecutionTarget> {
        match self {
            Self::Remote(r) => Some(r),
            _ => None,
        }
    }
    /// Mutation-friendly variant mirroring Node's object-spread idiom
    /// in callers that override `remoteCwd`. Mirrors the shallow
    /// copy used by `overrideAdapterExecutionTargetRemoteCwd`.
    pub fn set_remote_cwd(&mut self, next_remote_cwd: String) {
        match self {
            Self::Remote(AdapterRemoteExecutionTarget::Ssh(s)) => {
                s.remote_cwd = next_remote_cwd.clone();
                s.spec.remote_cwd = next_remote_cwd;
            }
            Self::Remote(AdapterRemoteExecutionTarget::Sandbox(s)) => {
                s.remote_cwd = next_remote_cwd;
            }
            _ => {}
        }
    }
}

/// Type alias for the remote-execution spec carried by an SSH target.
/// Mirrors Node `AdapterRemoteExecutionSpec`.
pub type AdapterRemoteExecutionSpec = SshRemoteExecutionSpec;

/// Adapter-facing managed-runtime asset alias. The full descriptor
/// lives in `command_managed_runtime::CommandManagedRuntimeAsset`. The
/// alias re-exports the type for callers that parameterize on
/// `AdapterManagedRuntimeAsset`. Mirrors Node
/// `AdapterManagedRuntimeAsset`.
/// Adapter-facing managed-runtime asset descriptor. Mirrors Node
/// `AdapterManagedRuntimeAsset` (alias to
/// `CommandManagedRuntimeAsset`). The full asset descriptor lives
/// in the command-managed runtime module; this type is a
/// placeholder carrying the minimum subset pc-acpx needs today and
/// is meant to be replaced with a re-export when
/// `CommandManagedRuntimeAsset` lands in
/// `pc_acpx::command_managed_runtime`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AdapterManagedRuntimeAsset {
    pub key: String,
    pub local_dir: String,
}

// =============================================================================
// Async-call option types. These interfaces are preserved so async
// helpers in pc-core can keep their typed signatures without circular
// deps. Functional methods (e.g. `stop()`) become boolean flags.
// =============================================================================

/// Re-export of `sanitize_remote_execution_env` for parity with Node.
pub use crate::remote_execution_env::sanitize_remote_execution_env as sanitize_remote_execution_env;

/// Top-level descriptor returned by
/// `prepareAdapterExecutionTargetRuntime`. Mirrors Node
/// `PreparedAdapterExecutionTargetRuntime` (minus the async
/// `restoreWorkspace`, mirrored as a `has_restore_workspace` flag).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreparedAdapterExecutionTargetRuntime {
    pub target: AdapterExecutionTarget,
    pub workspace_remote_dir: Option<String>,
    pub runtime_root_dir: Option<String>,
    pub asset_dirs: std::collections::BTreeMap<String, String>,
    pub additional_source_dirs: std::collections::BTreeMap<String, String>,
    pub additional_source_failures: Vec<crate::sandbox_managed_runtime::AdditionalSourceStagingFailure>,
    /// Async builder captured as a flag for parity.
    pub has_restore_workspace: bool,
}

/// Options for running a managed CLI process on an
/// `AdapterExecutionTarget`. Mirrors Node
/// `AdapterExecutionTargetProcessOptions`. Async hooks are mirrored
/// as capability flags so pc-core can pass typed handles when
/// present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterExecutionTargetProcessOptions {
    pub cwd: String,
    pub env: std::collections::BTreeMap<String, String>,
    pub stdin: Option<String>,
    pub timeout_sec: u64,
    pub grace_sec: u64,
    pub has_on_log: bool,
    pub has_on_runtime_progress: bool,
    pub has_on_spawn: bool,
    pub has_terminal_result_cleanup: bool,
    pub has_run_log_tail: bool,
    pub has_local_process_sandbox: bool,
}

/// Options for running an arbitrary shell command on an
/// `AdapterExecutionTarget`. Mirrors Node
/// `AdapterExecutionTargetShellOptions`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterExecutionTargetShellOptions {
    pub cwd: String,
    pub env: std::collections::BTreeMap<String, String>,
    pub timeout_sec: Option<u64>,
    pub grace_sec: Option<u64>,
    pub has_on_log: bool,
}

/// Paperclip bridge handle. Mirrors Node
/// `AdapterExecutionTargetPaperclipBridgeHandle` (async `stop`
/// mirrored as a `has_stop` flag).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterExecutionTargetPaperclipBridgeHandle {
    pub env: std::collections::BTreeMap<String, String>,
    pub has_run_log_tail: bool,
    pub has_stop: bool,
}

/// Process-session bridge handle. Mirrors Node
/// `AdapterExecutionTargetProcessSessionBridgeHandle` (async `stop`
/// mirrored as a `has_stop` flag).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterExecutionTargetProcessSessionBridgeHandle {
    pub agent_command: String,
    pub has_stop: bool,
}

// =============================================================================
// Public sync API.
// =============================================================================

/// Reduce the target to its remote-execution spec (SSH only).
/// Returns the SSH spec for an SSH remote target, `None`
/// otherwise. Mirrors Node
/// `adapterExecutionTargetToRemoteSpec`.
#[must_use]
pub fn adapter_execution_target_to_remote_spec(
    target: Option<&AdapterExecutionTarget>,
) -> Option<&SshRemoteExecutionSpec> {
    match target {
        Some(AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Ssh(s))) => {
            Some(&s.spec)
        }
        _ => None,
    }
}

/// `true` for any remote (SSH or Sandbox) target. Mirrors Node
/// `adapterExecutionTargetIsRemote`.
#[must_use]
pub fn adapter_execution_target_is_remote(target: Option<&AdapterExecutionTarget>) -> bool {
    matches!(target, Some(AdapterExecutionTarget::Remote(_)))
}

/// `true` only for Sandbox-backed targets. Mirrors Node
/// `adapterExecutionTargetUsesManagedHome`.
#[must_use]
pub fn adapter_execution_target_uses_managed_home(
    target: Option<&AdapterExecutionTarget>,
) -> bool {
    matches!(
        target,
        Some(AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Sandbox(_)))
    )
}

/// Pick `target.remoteCwd` for remote targets; fall back to the
/// supplied local cwd. Mirrors Node
/// `adapterExecutionTargetRemoteCwd`.
#[must_use]
pub fn adapter_execution_target_remote_cwd(
    target: Option<&AdapterExecutionTarget>,
    local_cwd: &str,
) -> String {
    match target {
        Some(AdapterExecutionTarget::Remote(r)) => r.remote_cwd().to_string(),
        _ => local_cwd.to_string(),
    }
}

/// Return a clone of `target` with its `remoteCwd` overridden.
/// Returns the input target unchanged for non-remote targets or
/// when no new cwd is supplied. Mirrors Node
/// `overrideAdapterExecutionTargetRemoteCwd`.
pub fn override_adapter_execution_target_remote_cwd(
    target: AdapterExecutionTarget,
    remote_cwd: Option<&str>,
) -> AdapterExecutionTarget {
    let trimmed = remote_cwd.map(str::trim).filter(|s| !s.is_empty());
    let (Some(next), Some(AdapterExecutionTarget::Remote(_))) = (trimmed, Some(&target)) else {
        return target;
    };
    let mut out = target.clone();
    match &out {
        AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Ssh(s)) => {
            if s.remote_cwd == next {
                return target;
            }
            out.set_remote_cwd(next.to_string());
        }
        AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Sandbox(s)) => {
            if s.remote_cwd == next {
                return target;
            }
            out.set_remote_cwd(next.to_string());
        }
        _ => {}
    }
    out
}

/// Resolve the effective cwd for a run: prefer the configured cwd
/// when non-empty, otherwise fall back to the target's remote cwd
/// (or the supplied local fallback). Mirrors Node
/// `resolveAdapterExecutionTargetCwd`.
#[must_use]
pub fn resolve_adapter_execution_target_cwd(
    target: Option<&AdapterExecutionTarget>,
    configured_cwd: Option<&str>,
    local_fallback_cwd: &str,
) -> String {
    if let Some(c) = configured_cwd {
        let t = c.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    adapter_execution_target_remote_cwd(target, local_fallback_cwd)
}

/// `true` for any remote target (the Paperclip bridge is
/// exclusively a remote feature in this codebase). Mirrors Node
/// `adapterExecutionTargetUsesPaperclipBridge`.
#[must_use]
pub fn adapter_execution_target_uses_paperclip_bridge(
    target: Option<&AdapterExecutionTarget>,
) -> bool {
    adapter_execution_target_is_remote(target)
}

/// Human-readable description for logs and UI. Mirrors Node
/// `describeAdapterExecutionTarget`.
#[must_use]
pub fn describe_adapter_execution_target(target: Option<&AdapterExecutionTarget>) -> String {
    match target {
        None => "local environment".to_string(),
        Some(AdapterExecutionTarget::Local(_)) => "local environment".to_string(),
        Some(AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Ssh(s))) => {
            format!(
                "SSH environment {}@{}:{}",
                s.spec.username, s.spec.host, s.spec.port
            )
        }
        Some(AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Sandbox(s))) => {
            match s.provider_key.as_deref() {
                Some(pk) => format!("sandbox environment ({pk})"),
                None => "sandbox environment".to_string(),
            }
        }
    }
}

/// Where a resolved timeout comes from. Mirrors Node
/// `AdapterExecutionTargetTimeoutSource`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterExecutionTargetTimeoutSource {
    Configured,
    SandboxDefault,
    Unlimited,
}

/// Resolved wall-clock timeout + its source. Mirrors Node
/// `AdapterExecutionTargetTimeoutResolution`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterExecutionTargetTimeoutResolution {
    pub timeout_sec: f64,
    pub source: AdapterExecutionTargetTimeoutSource,
}

impl AdapterExecutionTargetTimeoutResolution {
    /// True when the wall-clock timeout is non-positive (no
    /// timeout).
    #[must_use]
    pub fn is_disabled(&self) -> bool {
        self.timeout_sec <= 0.0
    }
}

/// Apply the configured `timeoutSec` to the target. Rules mirror
/// Node: positive → "configured"; negative → explicitly disabled
/// "configured"; zero falls through to target defaults; Sandbox
/// targets pick up the `DEFAULT_REMOTE_SANDBOX_ADAPTER_TIMEOUT_SEC`
/// 4h backstop; everything else stays unlimited.
#[must_use]
pub fn resolve_adapter_execution_target_timeout(
    target: Option<&AdapterExecutionTarget>,
    configured_timeout_sec: Option<f64>,
) -> AdapterExecutionTargetTimeoutResolution {
    if let Some(v) = configured_timeout_sec {
        if v.is_finite() {
            if v > 0.0 {
                return AdapterExecutionTargetTimeoutResolution {
                    timeout_sec: v,
                    source: AdapterExecutionTargetTimeoutSource::Configured,
                };
            }
            if v < 0.0 {
                return AdapterExecutionTargetTimeoutResolution {
                    timeout_sec: 0.0,
                    source: AdapterExecutionTargetTimeoutSource::Configured,
                };
            }
        }
    }
    if adapter_execution_target_uses_managed_home(target) {
        return AdapterExecutionTargetTimeoutResolution {
            timeout_sec: DEFAULT_REMOTE_SANDBOX_ADAPTER_TIMEOUT_SEC as f64,
            source: AdapterExecutionTargetTimeoutSource::SandboxDefault,
        };
    }
    AdapterExecutionTargetTimeoutResolution {
        timeout_sec: 0.0,
        source: AdapterExecutionTargetTimeoutSource::Unlimited,
    }
}

/// Convenience: just the seconds value of the resolution. Mirrors
/// Node `resolveAdapterExecutionTargetTimeoutSec`.
#[must_use]
pub fn resolve_adapter_execution_target_timeout_sec(
    target: Option<&AdapterExecutionTarget>,
    configured_timeout_sec: Option<f64>,
) -> f64 {
    resolve_adapter_execution_target_timeout(target, configured_timeout_sec).timeout_sec
}

fn describe_adapter_execution_timeout_source(
    source: &AdapterExecutionTargetTimeoutSource,
) -> &'static str {
    match source {
        AdapterExecutionTargetTimeoutSource::Configured => "configured via adapterConfig.timeoutSec",
        AdapterExecutionTargetTimeoutSource::SandboxDefault => "sandbox default",
        AdapterExecutionTargetTimeoutSource::Unlimited => "no adapter wall-clock timeout",
    }
}

/// Self-describing error message for when the adapter wall-clock
/// execution timeout kills a run. Names the timer that fired and
/// the knob that controls it so run failures never surface as a
/// bare "Timed out". Mirrors Node
/// `formatAdapterExecutionTimeoutErrorMessage`.
#[must_use]
pub fn format_adapter_execution_timeout_error_message(
    resolution: &AdapterExecutionTargetTimeoutResolution,
) -> String {
    format!(
        "Run exceeded the adapter execution timeout (timeoutSec={}, {}). Set adapterConfig.timeoutSec to raise it.",
        resolution.timeout_sec,
        describe_adapter_execution_timeout_source(&resolution.source)
    )
}

/// One-line start-of-run statement of the effective wall-clock
/// timeout and its source. Mirrors Node
/// `formatAdapterExecutionTimeoutStartLogLine`.
#[must_use]
pub fn format_adapter_execution_timeout_start_log_line(
    resolution: &AdapterExecutionTargetTimeoutResolution,
) -> String {
    if resolution.is_disabled() {
        return match resolution.source {
            AdapterExecutionTargetTimeoutSource::Configured => {
                "Adapter execution timeout: none (explicitly disabled via adapterConfig.timeoutSec; set it to a positive value to add one)."
                    .to_string()
            }
            _ => {
                "Adapter execution timeout: none (no adapter wall-clock timeout for this target; set adapterConfig.timeoutSec to add one)."
                    .to_string()
            }
        };
    }
    format!(
        "Adapter execution timeout: timeoutSec={} ({}; set adapterConfig.timeoutSec to override).",
        resolution.timeout_sec,
        describe_adapter_execution_timeout_source(&resolution.source)
    )
}

/// Compute the session-identity payload for a target. Mirrors Node
/// `adapterExecutionTargetSessionIdentity`. Returns the SSH session
/// identity (`buildRemoteExecutionSessionIdentity`) for SSH
/// targets, a 5-tuple hash for Sandbox targets, or `None` for
/// local / null targets.
#[must_use]
pub fn adapter_execution_target_session_identity(
    target: Option<&AdapterExecutionTarget>,
) -> Option<AdapterExecutionTargetSessionIdentity> {
    match target {
        None => None,
        Some(AdapterExecutionTarget::Local(_)) => None,
        Some(AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Ssh(s))) => {
            Some(AdapterExecutionTargetSessionIdentity::Ssh(SshSessionIdentity {
                transport: "ssh".to_string(),
                host: s.spec.host.clone(),
                username: s.spec.username.clone(),
                port: s.spec.port,
            }))
        }
        Some(AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Sandbox(s))) => {
            Some(AdapterExecutionTargetSessionIdentity::Sandbox(
                SandboxSessionIdentity {
                    transport: "sandbox".to_string(),
                    provider_key: s.provider_key.clone(),
                    environment_id: s.environment_id.clone(),
                    lease_id: s.lease_id.clone(),
                    remote_cwd: s.remote_cwd.clone(),
                },
            ))
        }
    }
}

/// Either the SSH session identity (delegated to
/// `remote_managed_runtime`) or a self-contained Sandbox session
/// 5-tuple.
/// Self-contained session identity emitted by
/// `adapter_execution_target_session_identity`. Either an SSH
/// 4-tuple or a Sandbox 5-tuple.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AdapterExecutionTargetSessionIdentity {
    /// SSH 4-tuple - mirrors `RemoteExecutionSessionIdentity` but
    /// kept inline to avoid a pc-acpx crate-level dependency loop.
    Ssh(SshSessionIdentity),
    Sandbox(SandboxSessionIdentity),
}

/// SSH session identity - mirrors the Node
/// `buildRemoteExecutionSessionIdentity` payload (transport /
/// host / username / port). Mirrors Node `RemoteExecutionSessionIdentity`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SshSessionIdentity {
    pub transport: String,
    pub host: String,
    pub username: String,
    pub port: u16,
}

/// Self-contained sandbox session 5-tuple. Mirrors the inner
/// object literal returned by Node
/// `adapterExecutionTargetSessionIdentity` for a Sandbox target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxSessionIdentity {
    pub transport: String,
    pub provider_key: Option<String>,
    pub environment_id: Option<String>,
    pub lease_id: Option<String>,
    pub remote_cwd: String,
}

/// Compare a previously-saved session row against a current
/// target. Returns `true` only when every session-identity field
/// matches. Mirrors Node
/// `adapterExecutionTargetSessionMatches`.
#[must_use]
pub fn adapter_execution_target_session_matches(
    saved: &serde_json::Value,
    target: Option<&AdapterExecutionTarget>,
) -> bool {
    let parsed = parse_object(saved);
    match target {
        None => parsed.is_empty(),
        Some(AdapterExecutionTarget::Local(_)) => parsed.is_empty(),
        Some(AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Ssh(s))) => {
            let parsed = parse_object(saved);
            let Some(current_id) =
                adapter_execution_target_session_identity(target)
            else {
                return false;
            };
            let AdapterExecutionTargetSessionIdentity::Ssh(current_id) = current_id else {
                return false;
            };
            read_string_meta(&parsed, "transport").as_deref()
                == Some(current_id.transport.as_str())
                && read_string_meta(&parsed, "host").as_deref() == Some(current_id.host.as_str())
                && read_string_meta(&parsed, "username").as_deref()
                    == Some(current_id.username.as_str())
                && parsed
                    .get("port")
                    .and_then(serde_json::Value::as_u64)
                    == Some(u64::from(current_id.port))
        }
        Some(AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Sandbox(s))) => {
            let Some(current) = adapter_execution_target_session_identity(target) else {
                return false;
            };
            let AdapterExecutionTargetSessionIdentity::Sandbox(current) = current else {
                return false;
            };
            read_string_meta(&parsed, "transport").as_deref() == Some(current.transport.as_str())
                && read_string_meta(&parsed, "providerKey").as_deref()
                    == current.provider_key.as_deref()
                && read_string_meta(&parsed, "environmentId").as_deref()
                    == current.environment_id.as_deref()
                && read_string_meta(&parsed, "leaseId").as_deref() == current.lease_id.as_deref()
                && read_string_meta(&parsed, "remoteCwd").as_deref()
                    == Some(current.remote_cwd.as_str())
        }
    }
}

/// Parse a JSON-ish value into an `AdapterExecutionTarget`.
/// Returns `None` for unrecognized shapes. Mirrors Node
/// `parseAdapterExecutionTarget`.
#[must_use]
pub fn parse_adapter_execution_target(value: &serde_json::Value) -> Option<AdapterExecutionTarget> {
    let parsed = parse_object(value);
    let kind = read_string_meta(&parsed, "kind")?;
    if kind == "local" {
        return Some(AdapterExecutionTarget::Local(AdapterLocalExecutionTarget {
            kind: "local".to_string(),
            environment_id: read_string_meta(&parsed, "environmentId"),
            lease_id: read_string_meta(&parsed, "leaseId"),
            workspace_realization: None,
        }));
    }
    if kind != "remote" {
        return None;
    }
    let transport = read_string_meta(&parsed, "transport")?;
    let remote_cwd = read_string_meta(&parsed, "remoteCwd").unwrap_or_default();
    match transport.as_str() {
        "ssh" => {
            let spec_obj = parsed.get("spec").cloned().unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
            let spec = parse_ssh_remote_execution_spec(&spec_obj)?;
            Some(AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Ssh(
                AdapterSshExecutionTarget {
                    kind: "remote".to_string(),
                    transport: "ssh".to_string(),
                    environment_id: read_string_meta(&parsed, "environmentId"),
                    lease_id: read_string_meta(&parsed, "leaseId"),
                    remote_cwd: spec.remote_cwd.clone(),
                    spec,
                    workspace_realization: None,
                },
            )))
        }
        "sandbox" => {
            if remote_cwd.is_empty() {
                return None;
            }
            let timeout_ms = match parsed.get("timeoutMs") {
                Some(serde_json::Value::Number(n)) => n.as_u64(),
                _ => None,
            };
            let stream_run_logs = parsed.get("streamRunLogs").and_then(serde_json::Value::as_bool);
            Some(AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Sandbox(
                AdapterSandboxExecutionTarget {
                    kind: "remote".to_string(),
                    transport: "sandbox".to_string(),
                    provider_key: read_string_meta(&parsed, "providerKey"),
                    shell_command: read_string_meta(&parsed, "shellCommand"),
                    environment_id: read_string_meta(&parsed, "environmentId"),
                    lease_id: read_string_meta(&parsed, "leaseId"),
                    remote_cwd,
                    timeout_ms,
                    stream_run_logs,
                    workspace_realization: None,
                },
            )))
        }
        _ => None,
    }
}

/// Build an `AdapterExecutionTarget` from a legacy
/// `remoteExecution` payload. Only SSH is supported in this path.
/// Mirrors Node
/// `adapterExecutionTargetFromRemoteExecution`.
#[must_use]
pub fn adapter_execution_target_from_remote_execution(
    remote_execution: &serde_json::Value,
    metadata: Option<AdapterLocalExecutionTargetMetadata>,
) -> Option<AdapterExecutionTarget> {
    let parsed = parse_object(remote_execution);
    let ssh = parse_ssh_remote_execution_spec(&serde_json::Value::Object(parsed.clone()))?;
    Some(AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Ssh(
        AdapterSshExecutionTarget {
            kind: "remote".to_string(),
            transport: "ssh".to_string(),
            environment_id: metadata
                .as_ref()
                .and_then(|m| m.environment_id.clone()),
            lease_id: metadata.as_ref().and_then(|m| m.lease_id.clone()),
            remote_cwd: ssh.remote_cwd.clone(),
            spec: ssh,
            workspace_realization: None,
        },
    )))
}

/// Tiny shape carrying the metadata `adapterExecutionTargetFromRemoteExecution` accepts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterLocalExecutionTargetMetadata {
    pub environment_id: Option<String>,
    pub lease_id: Option<String>,
}

/// Pick the strongest execution-target source from a payload: the
/// already-typed `executionTarget` when valid, otherwise the
/// parsed one, otherwise a target derived from the legacy
/// `remoteExecution` field. Mirrors Node
/// `readAdapterExecutionTarget`.
#[must_use]
pub fn read_adapter_execution_target(
    execution_target: Option<&serde_json::Value>,
    legacy_remote_execution: Option<&serde_json::Value>,
) -> Option<AdapterExecutionTarget> {
    if let Some(v) = execution_target {
        if is_adapter_execution_target_instance(v) {
            return parse_adapter_execution_target(v);
        }
    }
    if let Some(v) = execution_target {
        if let Some(p) = parse_adapter_execution_target(v) {
            return Some(p);
        }
    }
    if let Some(legacy) = legacy_remote_execution {
        return adapter_execution_target_from_remote_execution(legacy, None);
    }
    None
}

/// Resolve the runtime asset directory for an asset key:
/// precomputed map value when present, otherwise
/// `<fallback>/.paperclip-runtime/<key>`. Mirrors Node
/// `runtimeAssetDir`.
#[must_use]
pub trait PreparedAdapterExecutionTargetRuntimeLike {
    fn asset_dirs(&self) -> &std::collections::BTreeMap<String, String>;
}

pub fn runtime_asset_dir(
    prepared: &dyn PreparedAdapterExecutionTargetRuntimeLike,
    key: &str,
    fallback_remote_cwd: &str,
) -> String {
    if let Some(d) = prepared.asset_dirs().get(key) {
        return d.clone();
    }
    let mut out = String::new();
    if !fallback_remote_cwd.is_empty() {
        out.push_str(fallback_remote_cwd.trim_end_matches('/'));
        out.push('/');
    }
    out.push_str(".paperclip-runtime/");
    out.push_str(key);
    out
}

/// Helper trait so `runtime_asset_dir` can accept either the
/// concrete `PreparedAdapterExecutionTargetRuntime` or any caller
/// shape that exposes a `&BTreeMap<key, dir>` view.
impl PreparedAdapterExecutionTargetRuntimeLike for PreparedAdapterExecutionTargetRuntime {
    fn asset_dirs(&self) -> &std::collections::BTreeMap<String, String> {
        &self.asset_dirs
    }
}

// ============================================================================
// R435 — Remote execution runtime helpers (effective_execution_cwd +
// remote_codex_home) — 复刻 Node codex execute.ts L716-738。
// ============================================================================

/// 计算运行实际使用的 cwd，对齐 Node：
///
/// ```text
/// effectiveExecutionCwd = targetWorkspaceRealization?.mode === "in_place"
///   ? targetWorkspaceRealization.authoritativeRoot
///   : adapterExecutionTargetRemoteCwd(executionTarget, cwd);
/// ```
///
/// - 当 workspace_realization 存在且 mode == "in_place" → 返回
///   `authoritative_root`（远程端已经在该路径准备好工作目录）；
/// - 否则 → 返回 remote target 自带的 `remote_cwd`，无 remote target 时
///   退回到 `local_cwd`。
#[must_use]
pub fn effective_execution_cwd(
    workspace_realization: Option<&AdapterWorkspaceRealization>,
    target: Option<&AdapterExecutionTarget>,
    local_cwd: &str,
) -> String {
    if let Some(realization) = workspace_realization {
        if realization.mode == AdapterWorkspaceRealizationMode::InPlace {
            return realization.authoritative_root.clone();
        }
    }
    adapter_execution_target_remote_cwd(target, local_cwd)
}

/// 解析远程 codex 的 home 目录（仅在 `executionTargetIsRemote` 时调用）。
/// 对齐 Node codex execute.ts：
///
/// ```text
/// const remoteCodexHome = executionTargetIsRemote
///   ? preparedExecutionTargetRuntime?.assetDirs.home ??
///     path.posix.join(effectiveExecutionCwd, ".paperclip-runtime", "codex", "home")
///   : null;
/// ```
///
/// 本助手只覆盖默认路径段；当 `prepared.asset_dirs.home` 已就绪时由调用者
/// 优先使用。这里返回 `Option<&str>`：本地时为 `None`，远程时为
/// `<effective_cwd>/.paperclip-runtime/codex/home`。
#[must_use]
pub fn default_remote_codex_home_path(effective_execution_cwd: &str) -> String {
    let trimmed = effective_execution_cwd.trim_end_matches('/');
    if trimmed.is_empty() {
        ".paperclip-runtime/codex/home".to_string()
    } else {
        format!("{trimmed}/.paperclip-runtime/codex/home")
    }
}

/// 综合便捷函数：当 target 是远程时返回默认 codex home 路径，本地返回 None。
#[must_use]
pub fn resolve_remote_codex_home(
    target: Option<&AdapterExecutionTarget>,
    effective_execution_cwd: &str,
) -> Option<String> {
    if !adapter_execution_target_is_remote(target) {
        return None;
    }
    Some(default_remote_codex_home_path(effective_execution_cwd))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn local_target() -> AdapterExecutionTarget {
        AdapterExecutionTarget::Local(AdapterLocalExecutionTarget {
            kind: "local".to_string(),
            environment_id: Some("env-1".to_string()),
            lease_id: Some("lease-1".to_string()),
            workspace_realization: None,
        })
    }

    fn ssh_target() -> AdapterExecutionTarget {
        AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Ssh(
            AdapterSshExecutionTarget {
                kind: "remote".to_string(),
                transport: "ssh".to_string(),
                environment_id: None,
                lease_id: None,
                remote_cwd: "/workspace".to_string(),
                spec: SshRemoteExecutionSpec {
                    host: "host".to_string(),
                    port: 22,
                    username: "u".to_string(),
                    remote_cwd: "/workspace".to_string(),
                    remote_workspace_path: "/workspace".to_string(),
                    private_key: None,
                    known_hosts: None,
                    strict_host_key_checking: true,
                },
                workspace_realization: None,
            },
        ))
    }

    fn sandbox_target() -> AdapterExecutionTarget {
        AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Sandbox(
            AdapterSandboxExecutionTarget {
                kind: "remote".to_string(),
                transport: "sandbox".to_string(),
                provider_key: Some("e2b".to_string()),
                shell_command: None,
                environment_id: Some("env-1".to_string()),
                lease_id: Some("lease-1".to_string()),
                remote_cwd: "/workspace".to_string(),
                timeout_ms: Some(30_000),
                stream_run_logs: Some(true),
                workspace_realization: None,
            },
        ))
    }

    // ---- SSH parser ----

    #[test]
    fn ssh_parser_accepts_valid_spec() {
        let v = json!({
            "host": "h",
            "username": "u",
            "remoteCwd": "/w",
            "port": 2222,
        });
        let s = parse_ssh_remote_execution_spec(&v).expect("must parse");
        assert_eq!(s.host, "h");
        assert_eq!(s.port, 2222);
        assert_eq!(s.username, "u");
        assert_eq!(s.remote_cwd, "/w");
        assert!(s.strict_host_key_checking);
        assert!(s.private_key.is_none());
    }

    #[test]
    fn ssh_parser_rejects_invalid_port() {
        let v = json!({"host": "h", "username": "u", "remoteCwd": "/w", "port": 0});
        assert!(parse_ssh_remote_execution_spec(&v).is_none());
        let v = json!({"host": "h", "username": "u", "remoteCwd": "/w", "port": 70000});
        assert!(parse_ssh_remote_execution_spec(&v).is_none());
    }

    #[test]
    fn ssh_parser_rejects_missing_required_fields() {
        let v = json!({"host": "h", "port": 22});
        assert!(parse_ssh_remote_execution_spec(&v).is_none());
    }

    #[test]
    fn ssh_parser_defaults_remote_workspace_path_to_remote_cwd() {
        let v = json!({"host": "h", "username": "u", "remoteCwd": "/w", "port": 22});
        let s = parse_ssh_remote_execution_spec(&v).expect("must parse");
        assert_eq!(s.remote_workspace_path, "/w");
    }

    // ---- helpers ----

    #[test]
    fn parse_object_returns_object_map() {
        let v = json!({"a": 1});
        let m = parse_object(&v);
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn parse_object_returns_empty_for_non_object() {
        assert!(parse_object(&json!(null)).is_empty());
        assert!(parse_object(&json!("x")).is_empty());
        assert!(parse_object(&json!(42)).is_empty());
        assert!(parse_object(&json!([1, 2, 3])).is_empty());
    }

    #[test]
    fn read_string_returns_trimmed_non_empty() {
        assert_eq!(read_string(&json!("hi")), Some("hi".to_string()));
        assert_eq!(read_string(&json!("  spaced  ")), Some("spaced".to_string()));
        assert_eq!(read_string(&json!("")), None);
        assert_eq!(read_string(&json!("   ")), None);
        assert_eq!(read_string(&json!(42)), None);
    }

    #[test]
    fn read_string_meta_returns_field_trimmed() {
        let parsed = parse_object(&json!({"k": "  v  ", "j": ""}));
        assert_eq!(read_string_meta(&parsed, "k"), Some("v".to_string()));
        assert_eq!(read_string_meta(&parsed, "j"), None);
        assert_eq!(read_string_meta(&parsed, "missing"), None);
    }

    #[test]
    fn resolve_host_for_url_normalizes_wildcards() {
        assert_eq!(resolve_host_for_url(""), "localhost");
        assert_eq!(resolve_host_for_url("0.0.0.0"), "localhost");
        assert_eq!(resolve_host_for_url("::"), "localhost");
        assert_eq!(resolve_host_for_url("host"), "host");
        assert_eq!(resolve_host_for_url("1.2.3.4"), "1.2.3.4");
        assert_eq!(resolve_host_for_url("::1"), "[::1]");
        assert_eq!(resolve_host_for_url("[::1]"), "[::1]");
    }

    #[test]
    fn resolve_default_paperclip_api_url_uses_environment_supplied() {
        assert_eq!(
            resolve_default_paperclip_api_url_from(Some("h"), Some("4000"), None, None),
            "http://h:4000"
        );
    }

    #[test]
    fn resolve_default_paperclip_api_url_falls_back_to_localhost_3100() {
        assert_eq!(
            resolve_default_paperclip_api_url_from(None, None, None, None),
            "http://localhost:3100"
        );
    }

    #[test]
    fn is_bridge_debug_enabled_recognizes_known_truthy_values() {
        assert!(is_bridge_debug_enabled_from(Some("1")));
        assert!(is_bridge_debug_enabled_from(Some("true")));
        assert!(is_bridge_debug_enabled_from(Some("YES")));
        assert!(!is_bridge_debug_enabled_from(Some("0")));
        assert!(!is_bridge_debug_enabled_from(Some("off")));
        assert!(!is_bridge_debug_enabled_from(None));
    }

    #[test]
    fn is_adapter_execution_target_instance_accepts_valid_shapes() {
        assert!(is_adapter_execution_target_instance(&json!({"kind": "local"})));
        assert!(is_adapter_execution_target_instance(
            &json!({"kind": "remote", "transport": "ssh", "spec": {"host": "h", "username": "u", "remoteCwd": "/w", "port": 22}})
        ));
        assert!(is_adapter_execution_target_instance(
            &json!({"kind": "remote", "transport": "sandbox", "remoteCwd": "/w"})
        ));
        assert!(!is_adapter_execution_target_instance(&json!({"kind": "remote"})));
        assert!(!is_adapter_execution_target_instance(&json!({"kind": "alien"})));
    }

    // ---- to_remote_spec / is_remote / uses_managed_home ----

    #[test]
    fn to_remote_spec_returns_ssh_spec_for_ssh_target() {
        let t = ssh_target();
        let s = adapter_execution_target_to_remote_spec(Some(&t)).expect("must be ssh");
        assert_eq!(s.host, "host");
    }

    #[test]
    fn to_remote_spec_returns_none_for_sandbox_target() {
        let t = sandbox_target();
        assert!(adapter_execution_target_to_remote_spec(Some(&t)).is_none());
    }

    #[test]
    fn to_remote_spec_returns_none_for_local_target() {
        let t = local_target();
        assert!(adapter_execution_target_to_remote_spec(Some(&t)).is_none());
    }

    #[test]
    fn is_remote_classifies_correctly() {
        assert!(!adapter_execution_target_is_remote(Some(&local_target())));
        assert!(adapter_execution_target_is_remote(Some(&ssh_target())));
        assert!(adapter_execution_target_is_remote(Some(&sandbox_target())));
        assert!(!adapter_execution_target_is_remote(None));
    }

    #[test]
    fn uses_managed_home_only_sandbox() {
        assert!(!adapter_execution_target_uses_managed_home(Some(&ssh_target())));
        assert!(adapter_execution_target_uses_managed_home(Some(&sandbox_target())));
        assert!(!adapter_execution_target_uses_managed_home(Some(&local_target())));
    }

    #[test]
    fn remote_cwd_uses_target_for_remote_falls_back_for_local() {
        assert_eq!(
            adapter_execution_target_remote_cwd(Some(&ssh_target()), "/local"),
            "/workspace"
        );
        assert_eq!(
            adapter_execution_target_remote_cwd(Some(&local_target()), "/local"),
            "/local"
        );
    }

    // ---- override ----

    #[test]
    fn override_remote_cwd_changes_ssh_target() {
        let t = ssh_target();
        let next = override_adapter_execution_target_remote_cwd(t.clone(), Some("/new"));
        assert_eq!(
            adapter_execution_target_remote_cwd(Some(&next), "/local"),
            "/new"
        );
        // SSH spec is also updated in lockstep
        let s = adapter_execution_target_to_remote_spec(Some(&next)).expect("ssh");
        assert_eq!(s.remote_cwd, "/new");
    }

    #[test]
    fn override_remote_cwd_noop_for_local_target() {
        let t = local_target();
        let next = override_adapter_execution_target_remote_cwd(t.clone(), Some("/new"));
        // Local target should be unchanged
        assert!(matches!(next, AdapterExecutionTarget::Local(_)));
    }

    #[test]
    fn override_remote_cwd_with_empty_returns_input() {
        let t = ssh_target();
        let next = override_adapter_execution_target_remote_cwd(t.clone(), Some("   "));
        assert_eq!(
            adapter_execution_target_remote_cwd(Some(&next), "/local"),
            adapter_execution_target_remote_cwd(Some(&t), "/local")
        );
    }

    // ---- resolve_cwd / uses_paperclip_bridge / describe ----

    #[test]
    fn resolve_cwd_prefers_configured() {
        assert_eq!(
            resolve_adapter_execution_target_cwd(Some(&ssh_target()), Some("/cfg"), "/local"),
            "/cfg"
        );
    }

    #[test]
    fn resolve_cwd_falls_back_to_target_remote_then_local() {
        assert_eq!(
            resolve_adapter_execution_target_cwd(Some(&ssh_target()), None, "/local"),
            "/workspace"
        );
        assert_eq!(
            resolve_adapter_execution_target_cwd(Some(&local_target()), None, "/local"),
            "/local"
        );
    }

    #[test]
    fn uses_paperclip_bridge_is_remote_alias() {
        assert!(!adapter_execution_target_uses_paperclip_bridge(Some(&local_target())));
        assert!(adapter_execution_target_uses_paperclip_bridge(Some(&ssh_target())));
    }

    #[test]
    fn describe_returns_human_readable_strings() {
        assert_eq!(describe_adapter_execution_target(None), "local environment");
        assert_eq!(describe_adapter_execution_target(Some(&local_target())), "local environment");
        assert_eq!(
            describe_adapter_execution_target(Some(&ssh_target())),
            "SSH environment u@host:22"
        );
        assert_eq!(
            describe_adapter_execution_target(Some(&sandbox_target())),
            "sandbox environment (e2b)"
        );
    }

    // ---- timeout ----

    #[test]
    fn resolve_timeout_positive_configured() {
        let r = resolve_adapter_execution_target_timeout(Some(&local_target()), Some(60.0));
        assert_eq!(r.source, AdapterExecutionTargetTimeoutSource::Configured);
        assert_eq!(r.timeout_sec, 60.0);
    }

    #[test]
    fn resolve_timeout_negative_disabled_configured() {
        let r = resolve_adapter_execution_target_timeout(Some(&ssh_target()), Some(-1.0));
        assert_eq!(r.source, AdapterExecutionTargetTimeoutSource::Configured);
        assert_eq!(r.timeout_sec, 0.0);
    }

    #[test]
    fn resolve_timeout_zero_falls_to_sandbox_default() {
        let r = resolve_adapter_execution_target_timeout(Some(&sandbox_target()), Some(0.0));
        assert_eq!(r.source, AdapterExecutionTargetTimeoutSource::SandboxDefault);
        assert_eq!(r.timeout_sec as u64, DEFAULT_REMOTE_SANDBOX_ADAPTER_TIMEOUT_SEC);
    }

    #[test]
    fn resolve_timeout_zero_falls_to_unlimited_for_local() {
        let r = resolve_adapter_execution_target_timeout(Some(&local_target()), Some(0.0));
        assert_eq!(r.source, AdapterExecutionTargetTimeoutSource::Unlimited);
        assert_eq!(r.timeout_sec, 0.0);
    }

    #[test]
    fn resolve_timeout_sec_returns_just_seconds() {
        let sec = resolve_adapter_execution_target_timeout_sec(Some(&sandbox_target()), None);
        assert_eq!(sec as u64, DEFAULT_REMOTE_SANDBOX_ADAPTER_TIMEOUT_SEC);
    }

    #[test]
    fn timeout_error_message_includes_source_and_value() {
        let r = AdapterExecutionTargetTimeoutResolution {
            timeout_sec: 30.0,
            source: AdapterExecutionTargetTimeoutSource::Configured,
        };
        let msg = format_adapter_execution_timeout_error_message(&r);
        assert!(msg.contains("timeoutSec=30"));
        assert!(msg.contains("configured via adapterConfig.timeoutSec"));
    }

    #[test]
    fn timeout_start_log_line_when_disabled_no_knob() {
        let r = AdapterExecutionTargetTimeoutResolution {
            timeout_sec: 0.0,
            source: AdapterExecutionTargetTimeoutSource::Unlimited,
        };
        let line = format_adapter_execution_timeout_start_log_line(&r);
        assert!(line.contains("none"));
        assert!(line.contains("no adapter wall-clock timeout"));
    }

    #[test]
    fn timeout_start_log_line_when_enabled_lists_knob() {
        let r = AdapterExecutionTargetTimeoutResolution {
            timeout_sec: 60.0,
            source: AdapterExecutionTargetTimeoutSource::Configured,
        };
        let line = format_adapter_execution_timeout_start_log_line(&r);
        assert!(line.contains("timeoutSec=60"));
        assert!(line.contains("configured via"));
    }

    // ---- parseAdapterExecutionTarget ----

    #[test]
    fn parse_local_target() {
        let v = json!({"kind": "local", "environmentId": "env-1", "leaseId": "lease-1"});
        let t = parse_adapter_execution_target(&v).expect("must parse");
        assert!(matches!(t, AdapterExecutionTarget::Local(_)));
    }

    #[test]
    fn parse_ssh_target() {
        let v = json!({
            "kind": "remote",
            "transport": "ssh",
            "remoteCwd": "/w",
            "spec": {
                "host": "h", "username": "u", "remoteCwd": "/w", "port": 22,
            },
        });
        let t = parse_adapter_execution_target(&v).expect("must parse");
        assert!(matches!(
            t,
            AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Ssh(_))
        ));
    }

    #[test]
    fn parse_sandbox_target() {
        let v = json!({
            "kind": "remote",
            "transport": "sandbox",
            "remoteCwd": "/w",
            "providerKey": "e2b",
            "timeoutMs": 30000,
        });
        let t = parse_adapter_execution_target(&v).expect("must parse");
        assert!(matches!(
            t,
            AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Sandbox(_))
        ));
    }

    #[test]
    fn parse_unknown_kind_returns_none() {
        let v = json!({"kind": "alien"});
        assert!(parse_adapter_execution_target(&v).is_none());
    }

    #[test]
    fn parse_remote_with_missing_transport_returns_none() {
        let v = json!({"kind": "remote"});
        assert!(parse_adapter_execution_target(&v).is_none());
    }

    // ---- session identity ----

    #[test]
    fn session_identity_none_for_local_target() {
        let id = adapter_execution_target_session_identity(Some(&local_target()));
        assert!(id.is_none());
    }

    #[test]
    fn session_identity_for_ssh_delegates() {
        let id = adapter_execution_target_session_identity(Some(&ssh_target())).expect("ssh id");
        // SSH variant carries the parsed RemoteExecutionSessionIdentity
        assert!(matches!(id, AdapterExecutionTargetSessionIdentity::Ssh(_)));
    }

    #[test]
    fn session_identity_for_sandbox_carries_5tuple() {
        let id = adapter_execution_target_session_identity(Some(&sandbox_target())).expect("sb id");
        match id {
            AdapterExecutionTargetSessionIdentity::Sandbox(s) => {
                assert_eq!(s.transport, "sandbox");
                assert_eq!(s.provider_key.as_deref(), Some("e2b"));
                assert_eq!(s.environment_id.as_deref(), Some("env-1"));
                assert_eq!(s.lease_id.as_deref(), Some("lease-1"));
                assert_eq!(s.remote_cwd, "/workspace");
            }
            _ => panic!("expected sandbox variant"),
        }
    }

    #[test]
    fn session_matches_sandbox_round_trip() {
        let t = sandbox_target();
        let saved = json!({
            "transport": "sandbox",
            "providerKey": "e2b",
            "environmentId": "env-1",
            "leaseId": "lease-1",
            "remoteCwd": "/workspace",
            "ignored": "junk",
        });
        assert!(adapter_execution_target_session_matches(&saved, Some(&t)));
    }

    #[test]
    fn session_mismatch_on_sandbox_field() {
        let t = sandbox_target();
        let saved = json!({
            "transport": "sandbox",
            "providerKey": "other",
            "environmentId": "env-1",
            "leaseId": "lease-1",
            "remoteCwd": "/workspace",
        });
        assert!(!adapter_execution_target_session_matches(&saved, Some(&t)));
    }

    #[test]
    fn session_match_local_with_empty_saved() {
        assert!(adapter_execution_target_session_matches(&json!({}), Some(&local_target())));
        assert!(!adapter_execution_target_session_matches(&json!({"x": 1}), Some(&local_target())));
    }

    // ---- fromRemoteExecution / readAdapterExecutionTarget ----

    #[test]
    fn from_remote_execution_ssh() {
        let v = json!({
            "host": "h",
            "username": "u",
            "remoteCwd": "/w",
            "port": 22,
        });
        let t = adapter_execution_target_from_remote_execution(
            &v,
            Some(AdapterLocalExecutionTargetMetadata {
                environment_id: Some("env-1".to_string()),
                lease_id: None,
            }),
        )
        .expect("must build");
        assert!(matches!(
            t,
            AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Ssh(_))
        ));
    }

    #[test]
    fn read_adapter_execution_target_prefers_typed() {
        let typed = json!({"kind": "local"});
        let legacy = json!({"host": "h", "port": 22, "username": "u", "remoteCwd": "/w"});
        let t = read_adapter_execution_target(Some(&typed), Some(&legacy)).expect("typed wins");
        assert!(matches!(t, AdapterExecutionTarget::Local(_)));
    }

    #[test]
    fn read_adapter_execution_target_falls_back_to_legacy() {
        let legacy = json!({
            "host": "h",
            "username": "u",
            "remoteCwd": "/w",
            "port": 22,
        });
        let t = read_adapter_execution_target(None, Some(&legacy)).expect("legacy");
        assert!(matches!(
            t,
            AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Ssh(_))
        ));
    }

    // ---- runtimeAssetDir ----

    #[test]
    fn runtime_asset_dir_uses_map_when_present() {
        let mut dirs = std::collections::BTreeMap::new();
        dirs.insert("skill-1".to_string(), "/sandbox/runtime/skill-1".to_string());
        let p = PreparedAdapterExecutionTargetRuntime {
            target: local_target(),
            workspace_remote_dir: None,
            runtime_root_dir: None,
            asset_dirs: dirs,
            additional_source_dirs: std::collections::BTreeMap::new(),
            additional_source_failures: vec![],
            has_restore_workspace: false,
        };
        assert_eq!(
            runtime_asset_dir(&p, "skill-1", "/fallback"),
            "/sandbox/runtime/skill-1"
        );
    }

    #[test]
    fn runtime_asset_dir_falls_back_to_well_known_path() {
        let p = PreparedAdapterExecutionTargetRuntime {
            target: local_target(),
            workspace_remote_dir: None,
            runtime_root_dir: None,
            asset_dirs: std::collections::BTreeMap::new(),
            additional_source_dirs: std::collections::BTreeMap::new(),
            additional_source_failures: vec![],
            has_restore_workspace: false,
        };
        assert_eq!(
            runtime_asset_dir(&p, "skill-1", "/workspace"),
            "/workspace/.paperclip-runtime/skill-1"
        );
    }

    #[test]
    fn runtime_asset_dir_trims_trailing_slash() {
        let p = PreparedAdapterExecutionTargetRuntime {
            target: local_target(),
            workspace_remote_dir: None,
            runtime_root_dir: None,
            asset_dirs: std::collections::BTreeMap::new(),
            additional_source_dirs: std::collections::BTreeMap::new(),
            additional_source_failures: vec![],
            has_restore_workspace: false,
        };
        assert_eq!(
            runtime_asset_dir(&p, "skill-1", "/workspace/"),
            "/workspace/.paperclip-runtime/skill-1"
        );
    }

    // ---- R435 effective_execution_cwd / remote_codex_home ----

    fn workspace_realization_in_place(root: &str) -> AdapterWorkspaceRealization {
        AdapterWorkspaceRealization {
            mode: AdapterWorkspaceRealizationMode::InPlace,
            authoritative_root: root.to_string(),
            path_aliases: vec![],
            outbound_restore_paths: vec![],
        }
    }

    fn workspace_realization_copy() -> AdapterWorkspaceRealization {
        AdapterWorkspaceRealization {
            mode: AdapterWorkspaceRealizationMode::Copy,
            authoritative_root: "/copy-target".to_string(),
            path_aliases: vec![],
            outbound_restore_paths: vec![],
        }
    }

    #[test]
    fn effective_execution_cwd_uses_in_place_authoritative_root() {
        let realization = workspace_realization_in_place("/remote/in_place");
        let cwd = effective_execution_cwd(
            Some(&realization),
            Some(&ssh_target()),
            "/local/cwd",
        );
        assert_eq!(cwd, "/remote/in_place");
    }

    #[test]
    fn effective_execution_cwd_falls_back_to_remote_cwd_when_copy() {
        let realization = workspace_realization_copy();
        let cwd = effective_execution_cwd(
            Some(&realization),
            Some(&ssh_target()),
            "/local/cwd",
        );
        // copy mode → 不使用 authoritative_root，回退到 target.remote_cwd
        assert_eq!(cwd, "/workspace");
    }

    #[test]
    fn effective_execution_cwd_falls_back_to_local_when_no_realization() {
        let cwd = effective_execution_cwd(
            None,
            Some(&ssh_target()),
            "/local/cwd",
        );
        assert_eq!(cwd, "/workspace");
    }

    #[test]
    fn effective_execution_cwd_falls_back_to_local_for_local_target() {
        let cwd = effective_execution_cwd(None, Some(&local_target()), "/local/cwd");
        assert_eq!(cwd, "/local/cwd");
    }

    #[test]
    fn default_remote_codex_home_path_appends_dot_paperclip_runtime() {
        assert_eq!(
            default_remote_codex_home_path("/remote/cwd"),
            "/remote/cwd/.paperclip-runtime/codex/home"
        );
    }

    #[test]
    fn default_remote_codex_home_path_trims_trailing_slash() {
        assert_eq!(
            default_remote_codex_home_path("/remote/cwd/"),
            "/remote/cwd/.paperclip-runtime/codex/home"
        );
    }

    #[test]
    fn default_remote_codex_home_path_handles_empty_cwd() {
        assert_eq!(
            default_remote_codex_home_path(""),
            ".paperclip-runtime/codex/home"
        );
    }

    #[test]
    fn resolve_remote_codex_home_returns_none_for_local() {
        let home = resolve_remote_codex_home(Some(&local_target()), "/local/cwd");
        assert!(home.is_none());
    }

    #[test]
    fn resolve_remote_codex_home_returns_path_for_remote() {
        // 调用方负责把 effective_execution_cwd 传给本助手；这里覆盖
        // ssh_target 配合相同 cwd 的常见场景。
        let home = resolve_remote_codex_home(Some(&ssh_target()), "/workspace");
        assert_eq!(
            home.as_deref(),
            Some("/workspace/.paperclip-runtime/codex/home")
        );
    }

    #[test]
    fn resolve_remote_codex_home_returns_none_when_no_target() {
        let home = resolve_remote_codex_home(None, "/remote/cwd");
        assert!(home.is_none());
    }
}
