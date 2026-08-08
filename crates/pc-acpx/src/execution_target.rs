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
                && read_string_meta(&parsed, "remoteCwd").as_deref()
                    == Some(s.spec.remote_cwd.as_str())
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

// =============================================================================
// R486 — paperclip bridge handle 计划
// （对齐 Node `execution-target.ts` L1719-1896
// `startAdapterExecutionTargetPaperclipBridge` 的纯决策部分）
// =============================================================================

/// bridge 代理请求超时（对齐 Node `AbortSignal.timeout(30_000)`）。
pub const BRIDGE_PROXY_REQUEST_TIMEOUT_MS: u64 = 30_000;

/// 选择沙箱 shell（对齐 Node `preferredSandboxShell`：
/// `preferredShellForSandbox(target.shellCommand)`）。
#[must_use]
pub fn preferred_sandbox_shell(target: &AdapterSandboxExecutionTarget) -> &'static str {
    match target.shell_command.as_deref() {
        Some("bash") => "bash",
        _ => "sh",
    }
}

/// 选择可执行 target 的 shell（对齐 Node `adapterExecutionTargetShellCommand`：
/// ssh → `sh`；sandbox → `preferredSandboxShell`）。
#[must_use]
pub fn adapter_execution_target_shell_command(
    target: Option<&AdapterExecutionTarget>,
) -> &'static str {
    match target {
        Some(AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Ssh(_))) => "sh",
        Some(AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Sandbox(s))) => {
            preferred_sandbox_shell(s)
        }
        _ => "sh",
    }
}

/// 解析 bridge 执行超时 ms（对齐 Node `bridgeTimeoutMs`：
/// `timeoutSec > 0 ? trunc(timeoutSec * 1000) : adapterExecutionTargetTimeoutMs(target)`；
/// 后者仅 sandbox 提供 `target.timeoutMs`）。
#[must_use]
pub fn resolve_bridge_timeout_ms(
    timeout_sec: Option<f64>,
    target: Option<&AdapterExecutionTarget>,
) -> Option<u64> {
    if let Some(v) = timeout_sec {
        if v.is_finite() && v > 0.0 {
            return Some(v.trunc() as u64 * 1_000);
        }
    }
    match target {
        Some(AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Sandbox(s))) => {
            s.timeout_ms
        }
        _ => None,
    }
}

/// 归一化 bridge maxBodyBytes（对齐 Node：
/// `> 0 取整，否则 DEFAULT_SANDBOX_CALLBACK_BRIDGE_MAX_BODY_BYTES`）。
#[must_use]
pub fn resolve_bridge_max_body_bytes(max_body_bytes: Option<u64>) -> u64 {
    match max_body_bytes {
        Some(v) if v > 0 => v,
        _ => crate::sandbox_callback_bridge::DEFAULT_SANDBOX_CALLBACK_BRIDGE_MAX_BODY_BYTES,
    }
}

/// bridge 运行时目录（对齐 Node：
/// `bridgeRuntimeDir = join(runtimeRootDir, "paperclip-bridge")`；
/// `queueDir = join(bridgeRuntimeDir, "queue")`；
/// `assetRemoteDir = join(bridgeRuntimeDir, "server")`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeHandlePaths {
    pub bridge_runtime_dir: String,
    pub queue_dir: String,
    pub asset_remote_dir: String,
}

/// 解析 bridge 运行时目录三元组。
#[must_use]
pub fn bridge_handle_paths(runtime_root_dir: &str) -> BridgeHandlePaths {
    let root = runtime_root_dir.trim_end_matches('/');
    let bridge_runtime_dir = if root.is_empty() {
        "paperclip-bridge".to_string()
    } else {
        format!("{root}/paperclip-bridge")
    };
    BridgeHandlePaths {
        queue_dir: format!("{bridge_runtime_dir}/queue"),
        asset_remote_dir: format!("{bridge_runtime_dir}/server"),
        bridge_runtime_dir,
    }
}

/// bridge worker 代理请求计划
/// （对齐 Node `startAdapterExecutionTargetPaperclipBridge` 中
/// `handleRequest` 的决策：method 归一化、headers 装配、forward URL、
/// body 携带规则、30s 超时）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeProxyRequestPlan {
    pub method: String,
    pub url: String,
    pub headers: std::collections::BTreeMap<String, String>,
    /// Some(body) 时随请求发送（GET/HEAD 为 None）。
    pub body: Option<String>,
    pub timeout_ms: u64,
}

/// 组装 bridge 代理请求计划。
///
/// 对齐 Node：
/// ```ts
/// const method = request.method.trim().toUpperCase() || "GET";
/// headers.set("authorization", `Bearer ${hostApiToken}`);
/// headers.set("x-paperclip-run-id", input.runId);
/// fetch(buildBridgeForwardUrl(hostApiUrl, request), {
///   method, headers,
///   ...(GET/HEAD ? {} : { body: request.body }),
///   signal: AbortSignal.timeout(30_000),
/// });
/// ```
#[must_use]
pub fn build_bridge_proxy_request_plan(
    request: &crate::sandbox_callback_bridge::SandboxCallbackBridgeRequest,
    host_api_url: &str,
    host_api_token: &str,
    run_id: &str,
) -> BridgeProxyRequestPlan {
    let method = {
        let trimmed = request.method.trim().to_uppercase();
        if trimmed.is_empty() {
            "GET".to_string()
        } else {
            trimmed
        }
    };
    let mut headers = std::collections::BTreeMap::new();
    for (key, value) in &request.headers {
        if value.trim().is_empty() {
            continue;
        }
        headers.insert(key.clone(), value.clone());
    }
    headers.insert("authorization".to_string(), format!("Bearer {host_api_token}"));
    headers.insert("x-paperclip-run-id".to_string(), run_id.to_string());
    let url = crate::sandbox_callback_bridge::build_bridge_forward_url(
        host_api_url,
        &request.path,
        &request.query,
    );
    let body = if method == "GET" || method == "HEAD" {
        None
    } else {
        Some(request.body.clone())
    };
    BridgeProxyRequestPlan {
        method,
        url,
        headers,
        body,
        timeout_ms: BRIDGE_PROXY_REQUEST_TIMEOUT_MS,
    }
}

/// bridge handle 启动计划
/// （对齐 Node `startAdapterExecutionTargetPaperclipBridge` 决策部分 +
/// handle env 组装）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartPaperclipBridgePlan {
    pub paths: BridgeHandlePaths,
    pub run_id: String,
    pub bridge_token: String,
    pub max_body_bytes: u64,
    pub host_api_url: String,
    pub timeout_ms: Option<u64>,
    pub env: std::collections::BTreeMap<String, String>,
    /// sandbox transport 且 `streamRunLogs != false` 时启用 run log 流。
    pub has_run_log_tail: bool,
}

/// 校验 host API token（对齐 Node：trim 空 → 报错）。
pub fn bridge_host_api_token_or_error(host_api_token: Option<&str>) -> Result<String, String> {
    match host_api_token.map(str::trim).filter(|s| !s.is_empty()) {
        Some(token) => Ok(token.to_string()),
        None => Err(
            "Sandbox bridge mode requires a host-side Paperclip API token.".to_string(),
        ),
    }
}

/// 组装 bridge handle 启动计划。
#[must_use]
pub fn start_adapter_execution_target_paperclip_bridge_plan(
    run_id: &str,
    target: Option<&AdapterExecutionTarget>,
    runtime_root_dir: Option<&str>,
    adapter_key: &str,
    timeout_sec: Option<f64>,
    host_api_token: Option<&str>,
    host_api_url: Option<&str>,
    max_body_bytes: Option<u64>,
) -> Result<StartPaperclipBridgePlan, String> {
    let host_api_token = bridge_host_api_token_or_error(host_api_token)?;
    let remote_cwd = adapter_execution_target_remote_cwd(target, "");
    let runtime_root_dir = match runtime_root_dir.map(str::trim).filter(|s| !s.is_empty()) {
        Some(dir) => dir.to_string(),
        None => format!("{remote_cwd}/.paperclip-runtime/{adapter_key}"),
    };
    let paths = bridge_handle_paths(&runtime_root_dir);
    let bridge_token =
        crate::sandbox_callback_bridge::create_sandbox_callback_bridge_token(None);
    let max_body_bytes = resolve_bridge_max_body_bytes(max_body_bytes);
    let host_api_url = host_api_url
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("http://localhost:3100")
        .to_string();
    let timeout_ms = resolve_bridge_timeout_ms(timeout_sec, target);
    let mut env = std::collections::BTreeMap::new();
    env.insert("PAPERCLIP_API_URL".to_string(), host_api_url.clone());
    env.insert("PAPERCLIP_API_KEY".to_string(), bridge_token.clone());
    env.insert("PAPERCLIP_API_BRIDGE_MODE".to_string(), "queue_v1".to_string());
    env.insert("PAPERCLIP_BRIDGE_QUEUE_DIR".to_string(), paths.queue_dir.clone());
    let has_run_log_tail = match target {
        Some(AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Sandbox(s))) => {
            s.stream_run_logs != Some(false)
        }
        _ => false,
    };
    Ok(StartPaperclipBridgePlan {
        paths,
        run_id: run_id.to_string(),
        bridge_token,
        max_body_bytes,
        host_api_url,
        timeout_ms,
        env,
        has_run_log_tail,
    })
}

/// 主执行流程接入决策（对齐 Node codex/claude `execute.ts` 的
/// `if (executionTargetIsRemote && adapterExecutionTargetUsesPaperclipBridge(...))`
/// 分支）：非远程 → `Ok(None)`；远程且无 host token → `Err`
/// （Node 在 `startAdapterExecutionTargetPaperclipBridge` 内 throw）；
/// 否则组装 bridge handle 计划。
pub fn decide_execution_bridge_plan(
    run_id: &str,
    target: Option<&AdapterExecutionTarget>,
    runtime_root_dir: Option<&str>,
    adapter_key: &str,
    timeout_sec: Option<f64>,
    env_paperclip_api_key: Option<&str>,
    host_api_url: Option<&str>,
) -> Result<Option<StartPaperclipBridgePlan>, String> {
    if !adapter_execution_target_uses_paperclip_bridge(target) {
        return Ok(None);
    }
    let plan = start_adapter_execution_target_paperclip_bridge_plan(
        run_id,
        target,
        runtime_root_dir,
        adapter_key,
        timeout_sec,
        env_paperclip_api_key,
        host_api_url,
        None,
    )?;
    Ok(Some(plan))
}

/// 把 bridge handle env 合并进执行 env（对齐 Node
/// `Object.assign(env, paperclipBridge.env)`：bridge env 覆盖同名键）。
pub fn merge_bridge_handle_env(
    env: &mut std::collections::BTreeMap<String, String>,
    plan: &StartPaperclipBridgePlan,
) {
    for (key, value) in &plan.env {
        env.insert(key.clone(), value.clone());
    }
}

// =============================================================================
// R490 — 执行 env 与 bridge env 合并决策
// （对齐 Node codex execute.ts L891-907 / claude execute.ts L679-692：
// ```ts
// if (executionTargetIsRemote && adapterExecutionTargetUsesPaperclipBridge(runtimeExecutionTarget)) {
//   paperclipBridge = await startAdapterExecutionTargetPaperclipBridge({...});
//   if (paperclipBridge) { Object.assign(env, paperclipBridge.env); }
// }
// ```
// codex/claude 两个 adapter 共用的纯决策：把 bridge handle env 合并进
// 子进程执行 env，并生成 Node 同款启动日志行。不启动真实 bridge
// server / worker（执行器在 `pc-acpx::sandbox_callback_bridge`，后续轮次
// 接入 route 层）。
// =============================================================================

/// 执行 env 与 bridge 合并的输入（codex / claude adapter 共用）。
#[derive(Debug, Clone)]
pub struct MergeExecutionBridgeEnvInput<'a> {
    pub run_id: &'a str,
    /// route 层构建好的基础执行 env（已含 PAPERCLIP_RUN_ID / wake /
    /// workspace / PAPERCLIP_API_KEY 等键）。
    pub base_env: &'a std::collections::BTreeMap<String, String>,
    /// 原始 execution target JSON（adapter context 注入的
    /// `context.execution_target`）。
    pub execution_target: Option<&'a serde_json::Value>,
    /// bridge runtime root dir；None 时回退到
    /// `<remoteCwd>/.paperclip-runtime/<adapterKey>`。
    pub runtime_root_dir: Option<&'a str>,
    /// bridge adapter key（`"codex"` / `"claude"`）。
    pub adapter_key: &'a str,
    /// 超时秒（adapterConfig.timeoutSec，>0 生效；None 回退 target 默认）。
    pub timeout_sec: Option<f64>,
    /// host API URL 显式覆盖（adapter 一般不传，从 base_env 解析）。
    pub host_api_url: Option<&'a str>,
}

/// 合并后的执行 env 计划。
#[derive(Debug, Clone)]
pub struct MergedExecutionEnv {
    /// 最终子进程 env（bridge 合并后）。
    pub env: std::collections::BTreeMap<String, String>,
    /// 有 bridge 时携带的启动计划（含 paths / token / env）。
    pub bridge_plan: Option<StartPaperclipBridgePlan>,
    /// 有 bridge 时的启动日志行（Node
    /// `[paperclip] Starting sandbox callback bridge for <key> in <dir>.`）。
    pub start_log_line: Option<String>,
}

/// 执行 env 构建决策：远程 + usesBridge 时合并 bridge env。
///
/// 对齐 Node codex execute.ts L891-907 / claude execute.ts L679-692：
/// - 非远程（或远程但 usesBridge 为 false）→ 原样返回 `base_env`，
///   无 bridge、无日志行
/// - 远程 + usesBridge → `PAPERCLIP_API_KEY` 缺失时报错（Node throw）；
///   否则组装 bridge 计划、按 `Object.assign(env, paperclipBridge.env)`
///   覆盖同名键，并生成启动日志行
/// - `host_api_url` 解析：显式覆盖 > base_env 的
///   `PAPERCLIP_RUNTIME_API_URL` > `PAPERCLIP_API_URL` > 默认 URL
///   （对齐 Node `process.env` 解析语义）
pub fn merge_execution_bridge_env(
    input: &MergeExecutionBridgeEnvInput<'_>,
) -> Result<MergedExecutionEnv, String> {
    let target = input
        .execution_target
        .and_then(|value| parse_adapter_execution_target(value));
    if !adapter_execution_target_uses_paperclip_bridge(target.as_ref()) {
        return Ok(MergedExecutionEnv {
            env: input.base_env.clone(),
            bridge_plan: None,
            start_log_line: None,
        });
    }
    let host_api_token = input.base_env.get("PAPERCLIP_API_KEY").map(String::as_str);
    let host_api_url = input
        .host_api_url
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            input
                .base_env
                .get("PAPERCLIP_RUNTIME_API_URL")
                .map(String::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            input
                .base_env
                .get("PAPERCLIP_API_URL")
                .map(String::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .map(str::to_string);
    let plan = decide_execution_bridge_plan(
        input.run_id,
        target.as_ref(),
        input.runtime_root_dir,
        input.adapter_key,
        input.timeout_sec,
        host_api_token,
        host_api_url.as_deref(),
    )?
    .expect("uses paperclip bridge implies plan present");
    let mut env = input.base_env.clone();
    merge_bridge_handle_env(&mut env, &plan);
    let start_log_line = Some(format!(
        "[paperclip] Starting sandbox callback bridge for {} in {}.\n",
        input.adapter_key, plan.paths.bridge_runtime_dir
    ));
    Ok(MergedExecutionEnv {
        env,
        bridge_plan: Some(plan),
        start_log_line,
    })
}

// =============================================================================
// R487 — 进程 session bridge 决策
// （对齐 Node `execution-target.ts` L1266-1735：
// `writeProcessSessionProxyScript` / `syncProcessSessionRemoteScript` /
// `startAdapterExecutionTargetProcessSessionBridge` 纯决策部分）
// =============================================================================

/// 进程 session 代理脚本文件名（对齐 Node `PROCESS_SESSION_PROXY_SCRIPT`）。
pub const PROCESS_SESSION_PROXY_SCRIPT: &str = "paperclip-process-session-proxy.mjs";
/// 进程 session 远端脚本文件名（对齐 Node `PROCESS_SESSION_REMOTE_SCRIPT`）。
pub const PROCESS_SESSION_REMOTE_SCRIPT: &str = "paperclip-process-session-remote.mjs";
/// 进程 session 鉴权超时 ms（对齐 Node `PROCESS_SESSION_AUTH_TIMEOUT_MS`）。
pub const PROCESS_SESSION_AUTH_TIMEOUT_MS: u64 = 5_000;

/// 单行 JSON + 换行（对齐 Node `jsonLine`）。
#[must_use]
pub fn json_line(value: &serde_json::Value) -> String {
    format!("{}\n", serde_json::to_string(value).unwrap_or_else(|_| "null".to_string()))
}

/// 按 `\n` 拆分 JSON 行流（对齐 Node `splitJsonLines`：
/// 最后一段为不完整 rest）。
#[must_use]
pub fn split_json_lines(buffer: &str) -> (Vec<String>, String) {
    let parts: Vec<&str> = buffer.split('\n').collect();
    let lines: Vec<String> = parts[..parts.len().saturating_sub(1)]
        .iter()
        .map(|line| (*line).to_string())
        .collect();
    let rest = parts.last().unwrap_or(&"").to_string();
    (lines, rest)
}

/// 进程 session 代理脚本源码模板
/// （对齐 Node `getProcessSessionProxySource`；port/token 插值，
/// token 以 JSON 字符串字面量嵌入）。
#[must_use]
pub fn get_process_session_proxy_source(port: u16, token: &str) -> String {
    let token_json = serde_json::to_string(token).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        r#"#!/usr/bin/env node
import net from "node:net";

const socket = net.createConnection({{ host: "127.0.0.1", port: {port} }});
const token = {token_json};
let buffer = "";
let exiting = false;

function send(message) {{
  socket.write(JSON.stringify({{ token, ...message }}) + "\n");
}}

socket.on("connect", () => send({{ type: "hello" }}));
process.stdin.on("data", (chunk) => send({{ type: "stdin", data: Buffer.from(chunk).toString("base64") }}));
process.stdin.on("end", () => send({{ type: "stdinEnd" }}));
process.stdin.resume();

socket.setEncoding("utf8");
socket.on("data", (chunk) => {{
  buffer += chunk;
  const parts = buffer.split(/\n/);
  buffer = parts.pop() || "";
  for (const line of parts) {{
    if (!line.trim()) continue;
    const message = JSON.parse(line);
    if (message.type === "data") {{
      const out = Buffer.from(message.data || "", "base64");
      (message.stream === "stderr" ? process.stderr : process.stdout).write(out);
    }} else if (message.type === "error") {{
      process.stderr.write(String(message.message || "Process session bridge failed.") + "\n");
      exiting = true;
      process.exitCode = 1;
      socket.end();
    }} else if (message.type === "exit") {{
      exiting = true;
      process.exitCode = typeof message.code === "number" ? message.code : 1;
      socket.end();
    }}
  }}
}});
socket.on("close", () => {{
  if (!exiting) process.exit(1);
}});
"#
    )
}

/// 进程 session 远端脚本源码模板
/// （对齐 Node `getProcessSessionRemoteSource`）。
#[must_use]
pub fn get_process_session_remote_source() -> String {
    r#"import { spawn } from "node:child_process";
import { promises as fs } from "node:fs";
import path from "node:path";

const sessionDir = process.env.PAPERCLIP_PROCESS_SESSION_DIR;
const commandPayload = process.env.PAPERCLIP_PROCESS_SESSION_COMMAND_B64;
if (!sessionDir || !commandPayload) throw new Error("Missing process session bridge env.");

const stdinDir = path.posix.join(sessionDir, "stdin");
const eventsDir = path.posix.join(sessionDir, "events");
let seq = 0;
let stdinClosed = false;

const config = JSON.parse(Buffer.from(commandPayload, "base64").toString("utf8"));
await fs.mkdir(stdinDir, { recursive: true });
await fs.mkdir(eventsDir, { recursive: true });

let writeChain = Promise.resolve();

function writeEvent(event) {
  seq += 1;
  const file = path.posix.join(eventsDir, String(seq).padStart(12, "0") + ".json");
  const write = writeChain.then(async () => {
    await fs.writeFile(file + ".tmp", JSON.stringify(event) + "\n", "utf8");
    await fs.rename(file + ".tmp", file);
  });
  writeChain = write.catch(() => undefined);
  return write;
}

const child = spawn(config.command, Array.isArray(config.args) ? config.args : [], {
  cwd: config.cwd || process.cwd(),
  env: { ...process.env, ...(config.env || {}) },
  stdio: ["pipe", "pipe", "pipe"],
});

child.stdout.on("data", (chunk) => void writeEvent({ type: "data", stream: "stdout", data: Buffer.from(chunk).toString("base64") }));
child.stderr.on("data", (chunk) => void writeEvent({ type: "data", stream: "stderr", data: Buffer.from(chunk).toString("base64") }));
child.on("error", (error) => void writeEvent({ type: "error", message: error.message }));
// "close" (not "exit") so stdout/stderr fully drain before the exit event;
// the write chain then guarantees the exit file lands after every data file.
child.on("close", (code, signal) => void writeEvent({ type: "exit", code, signal }));

async function pollStdin() {
  while (!stdinClosed) {
    const entries = (await fs.readdir(stdinDir).catch(() => [])).filter((name) => name.endsWith(".json")).sort();
    for (const name of entries) {
      const file = path.posix.join(stdinDir, name);
      const raw = await fs.readFile(file, "utf8").catch(() => null);
      await fs.rm(file, { force: true }).catch(() => undefined);
      if (!raw) continue;
      const message = JSON.parse(raw);
      if (message.type === "stdin" && typeof message.data === "string") {
        child.stdin.write(Buffer.from(message.data, "base64"));
      } else if (message.type === "stdinEnd") {
        stdinClosed = true;
        child.stdin.end();
        break;
      }
    }
    if (!stdinClosed) await new Promise((resolve) => setTimeout(resolve, 50));
  }
}

void pollStdin().catch((error) => void writeEvent({ type: "error", message: error instanceof Error ? error.message : String(error) }));
"#
    .to_string()
}

/// 进程 session 远端脚本同步计划
/// （对齐 Node `syncProcessSessionRemoteScript`：label/action/lockDir 专用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSessionRemoteScriptPlan {
    pub remote_script_path: String,
    pub sha256: String,
    pub uploaded_decision_script: String,
    pub action: String,
    pub label: String,
    pub lock_dir: String,
}

impl ProcessSessionRemoteScriptPlan {
    /// 便捷读取器：期望的远端 sha256（同步脚本门控值）。
    #[must_use]
    pub fn expected_sha(&self) -> &str {
        &self.sha256
    }
}

/// 组装远端脚本同步计划。
#[must_use]
pub fn sync_process_session_remote_script_plan(
    remote_script_dir: &str,
    remote_script_path: &str,
) -> ProcessSessionRemoteScriptPlan {
    let body = get_process_session_remote_source();
    let sha256 = crate::sandbox_callback_bridge::sha256_hex_utf8(&body);
    let lock_dir = format!(
        "{}/.paperclip-process-session-script.lock",
        remote_script_dir.trim_end_matches('/')
    );
    let uploaded_decision_script =
        crate::sandbox_callback_bridge::build_sync_text_file_with_hash_skip_script(
            &crate::sandbox_callback_bridge::SyncTextFileScriptInput {
                remote_dir: remote_script_dir.to_string(),
                remote_path: remote_script_path.to_string(),
                lock_dir: lock_dir.clone(),
                expected_sha: sha256.clone(),
                label: "Process session remote script".to_string(),
            },
        );
    ProcessSessionRemoteScriptPlan {
        remote_script_path: remote_script_path.to_string(),
        sha256,
        uploaded_decision_script,
        action: "sync process session remote script".to_string(),
        label: "Process session remote script".to_string(),
        lock_dir,
    }
}

/// 构建进程 session 启动命令 payload
/// （对齐 Node：`base64(JSON.stringify({ command, args,
/// cwd: cwd || target.remoteCwd, env: sanitizeRemoteExecutionEnv(launchEnv) }))`）。
#[must_use]
pub fn build_process_session_command_payload(
    command: &str,
    args: &[String],
    cwd: &str,
    env: &std::collections::BTreeMap<String, String>,
) -> String {
    let sanitized = crate::remote_execution_env::sanitize_remote_execution_env(env, &std::collections::BTreeMap::new());
    let payload = serde_json::json!({
        "command": command,
        "args": args,
        "cwd": cwd,
        "env": sanitized,
    });
    crate::sandbox_callback_bridge::base64_encode_utf8(
        &serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string()),
    )
}

/// 构建进程 session 启动 shell 脚本
/// （对齐 Node `startAdapterExecutionTargetProcessSessionBridge` 的
/// start execute：mkdir stdin/events + nohup node remote + printf pid）。
#[must_use]
pub fn build_process_session_bridge_start_script(
    stdin_dir: &str,
    events_dir: &str,
    session_dir: &str,
    command_payload: &str,
    remote_script_path: &str,
) -> String {
    let quote = crate::sandbox_callback_bridge::shell_quote;
    [
        format!(
            "mkdir -p {} {}",
            quote(stdin_dir),
            quote(events_dir)
        ),
        format!(
            "PAPERCLIP_PROCESS_SESSION_DIR={} PAPERCLIP_PROCESS_SESSION_COMMAND_B64={} nohup node {} >/dev/null 2>&1 < /dev/null &",
            quote(session_dir),
            quote(command_payload),
            quote(remote_script_path)
        ),
        "printf '%s\\n' \"$!\"".to_string(),
    ]
    .join("\n")
}

/// 远端事件 → socket 动作（对齐 Node `writeRemoteEventToSocket`：
/// `exit` → 写完结束 socket；`error` → destroy；其他 → 写行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteEventSocketAction {
    Write,
    End,
    Destroy,
}

/// 决策 socket 动作。
#[must_use]
pub fn remote_event_socket_action(event_type: &str) -> RemoteEventSocketAction {
    match event_type {
        "exit" => RemoteEventSocketAction::End,
        "error" => RemoteEventSocketAction::Destroy,
        _ => RemoteEventSocketAction::Write,
    }
}

/// 进程 session bridge 启动计划
/// （对齐 Node `startAdapterExecutionTargetProcessSessionBridge` 决策部分）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSessionBridgePlan {
    pub bridge_runtime_dir: String,
    pub session_id: String,
    pub session_dir: String,
    pub stdin_dir: String,
    pub events_dir: String,
    pub remote_script_path: String,
    pub command_payload: String,
    pub start_script: String,
    pub proxy_token: String,
    pub timeout_ms: Option<u64>,
}

/// 组装进程 session bridge 启动计划。
///
/// 仅 sandbox 远程 target 返回 `Some`（对齐 Node gate：
/// `kind !== "remote" || transport !== "sandbox"` → null）。
#[must_use]
pub fn start_adapter_execution_target_process_session_bridge_plan(
    session_id: &str,
    target: Option<&AdapterExecutionTarget>,
    runtime_root_dir: Option<&str>,
    adapter_key: &str,
    command: &str,
    args: &[String],
    cwd: &str,
    launch_env: &std::collections::BTreeMap<String, String>,
    timeout_sec: Option<f64>,
) -> Option<ProcessSessionBridgePlan> {
    let sandbox = match target {
        Some(AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Sandbox(s))) => s,
        _ => return None,
    };
    let timeout_ms = match timeout_sec {
        Some(v) if v.is_finite() && v > 0.0 => Some(v.trunc() as u64 * 1_000),
        _ => sandbox.timeout_ms,
    };
    let remote_cwd = sandbox.remote_cwd.trim_end_matches('/');
    let runtime_root_dir = match runtime_root_dir.map(str::trim).filter(|s| !s.is_empty()) {
        Some(dir) => dir.to_string(),
        None => format!("{remote_cwd}/.paperclip-runtime/{adapter_key}"),
    };
    let bridge_runtime_dir = format!(
        "{}/process-sessions",
        runtime_root_dir.trim_end_matches('/')
    );
    let session_dir = format!("{bridge_runtime_dir}/{session_id}");
    let stdin_dir = format!("{session_dir}/stdin");
    let events_dir = format!("{session_dir}/events");
    let remote_script_path = format!("{bridge_runtime_dir}/{PROCESS_SESSION_REMOTE_SCRIPT}");
    let effective_cwd = if cwd.trim().is_empty() {
        sandbox.remote_cwd.clone()
    } else {
        cwd.to_string()
    };
    let command_payload = build_process_session_command_payload(
        command,
        args,
        &effective_cwd,
        launch_env,
    );
    let start_script = build_process_session_bridge_start_script(
        &stdin_dir,
        &events_dir,
        &session_dir,
        &command_payload,
        &remote_script_path,
    );
    let proxy_token = crate::sandbox_callback_bridge::create_sandbox_callback_bridge_token(Some(18));
    Some(ProcessSessionBridgePlan {
        bridge_runtime_dir,
        session_id: session_id.to_string(),
        session_dir,
        stdin_dir,
        events_dir,
        remote_script_path,
        command_payload,
        start_script,
        proxy_token,
        timeout_ms,
    })
}

// =============================================================================
// R488 — 进程 session proxy 连接/事件决策
// （对齐 Node `execution-target.ts` L1479-1565 的 connection handler 与
// `deliverRemoteEvent` / `poll` / `stop` 纯决策部分）
// =============================================================================

/// proxy 事件轮询间隔 ms（对齐 Node `pollTimer = setTimeout(..., 100)`）。
pub const PROXY_POLL_INTERVAL_MS: u64 = 100;

/// stdin 文件序号名（对齐 Node
/// `` `${String(seq).padStart(12, "0")}.json` ``）。
#[must_use]
pub fn proxy_stdin_file_name(seq: u64) -> String {
    format!("{seq:012}.json")
}

/// 连接消息决策结果
/// （对齐 Node connection handler：token 不等或已有活跃 socket 时 destroy；
/// 首次鉴权成功时接管 socket 并 flush；已鉴权连接继续处理）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyConnectionDecision {
    /// 销毁连接（token 不匹配 / 已有活跃 socket 抢占）。
    Reject,
    /// 首次鉴权成功：接管为活跃 socket，flush 缓冲事件。
    Authenticate,
    /// 已鉴权连接：继续处理消息。
    Proceed,
}

/// 决策连接消息动作。
///
/// 对齐 Node：
/// ```js
/// if (message.token !== token) { destroy; return; }
/// if (!authenticated) {
///   if (socket) { destroy; return; }   // 已有活跃 socket 抢占
///   authenticated = true; ...; flushPendingRemoteEvents();
/// }
/// ```
#[must_use]
pub fn decide_proxy_connection_message(
    message_token: Option<&str>,
    expected_token: &str,
    authenticated: bool,
    has_active_socket: bool,
) -> ProxyConnectionDecision {
    if message_token != Some(expected_token) {
        return ProxyConnectionDecision::Reject;
    }
    if !authenticated {
        if has_active_socket {
            return ProxyConnectionDecision::Reject;
        }
        return ProxyConnectionDecision::Authenticate;
    }
    ProxyConnectionDecision::Proceed
}

/// 鉴权超时判定（对齐 Node authTimer：
/// 未鉴权空闲连接在 `PROCESS_SESSION_AUTH_TIMEOUT_MS` 后被 destroy）。
#[must_use]
pub fn proxy_connection_auth_timed_out(authenticated: bool) -> bool {
    !authenticated
}

/// 解析 proxy 连接消息行（对齐 Node `JSON.parse(line)` 失败 → destroy）。
pub fn parse_proxy_message_line(line: &str) -> Result<serde_json::Value, serde_json::Error> {
    serde_json::from_str(line)
}

/// stdin 写入计划（对齐 Node connection handler 的 stdin/stdinEnd 分支）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyStdinWrite {
    pub file_name: String,
    pub body: String,
}

/// 构建 stdin 写入计划：
/// - `stdin` 且 data 为 string → `{seq:012}.json` + `{"type":"stdin","data":...}`
/// - `stdinEnd` → `{seq:012}.json` + `{"type":"stdinEnd"}`
/// - 其他 / stdin 缺 data → None（不写）
#[must_use]
pub fn build_proxy_stdin_write(
    seq: u64,
    message_type: Option<&str>,
    data: Option<&str>,
) -> Option<ProxyStdinWrite> {
    let file_name = proxy_stdin_file_name(seq);
    match message_type {
        Some("stdin") => data.map(|data| ProxyStdinWrite {
            body: json_line(&serde_json::json!({ "type": "stdin", "data": data })),
            file_name,
        }),
        Some("stdinEnd") => Some(ProxyStdinWrite {
            body: json_line(&serde_json::json!({ "type": "stdinEnd" })),
            file_name,
        }),
        _ => None,
    }
}

/// 事件轮询是否停止（对齐 Node poll：
/// `if (parsed.type === "exit" || parsed.type === "error") return;`）。
#[must_use]
pub fn decide_proxy_poll_should_stop(event_type: Option<&str>) -> bool {
    matches!(event_type, Some("exit") | Some("error"))
}

/// stop 时补写的 stdinEnd 文件（对齐 Node stop：
/// `` `${String(stdinSeq + 1).padStart(12, "0")}.json` ``）。
#[must_use]
pub fn build_proxy_stop_stdin_end_write(seq: u64) -> ProxyStdinWrite {
    ProxyStdinWrite {
        file_name: proxy_stdin_file_name(seq + 1),
        body: json_line(&serde_json::json!({ "type": "stdinEnd" })),
    }
}

/// 本地监听端口校验（对齐 Node `waitForLocalServerListen`：
/// 无 TCP 地址 → 明确报错）。
pub fn process_session_listen_port_or_error(port: Option<u16>) -> Result<u16, String> {
    port.ok_or_else(|| "Process session bridge did not expose a TCP port.".to_string())
}

/// 写 socket 的错误消息行（对齐 Node catch 分支：
/// `nextSocket.write(jsonLine({ type: "error", message }))`）。
#[must_use]
pub fn proxy_error_message_line(message: &str) -> String {
    json_line(&serde_json::json!({ "type": "error", "message": message }))
}

/// 远端事件投递决策
/// （对齐 Node `deliverRemoteEvent`：有 socket → 直写（exit 结束 /
/// error 销毁）；无 socket → 入缓冲，exit/error 停止后续轮询）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteEventDeliveryDecision {
    WriteToSocket {
        action: RemoteEventSocketAction,
    },
    QueuePending {
        stop_loop: bool,
    },
}

/// 决策远端事件投递方式。
#[must_use]
pub fn decide_remote_event_delivery(
    has_socket: bool,
    event_type: Option<&str>,
) -> RemoteEventDeliveryDecision {
    if has_socket {
        RemoteEventDeliveryDecision::WriteToSocket {
            action: remote_event_socket_action(event_type.unwrap_or("")),
        }
    } else {
        RemoteEventDeliveryDecision::QueuePending {
            stop_loop: decide_proxy_poll_should_stop(event_type),
        }
    }
}

/// 组装进程 session bridge handle
/// （对齐 Node `startAdapterExecutionTargetProcessSessionBridge` 返回的
/// `{ agentCommand, stop }`；异步 stop 以 `has_stop` 能力位呈现）。
#[must_use]
pub fn build_process_session_bridge_handle(
    agent_command: String,
) -> AdapterExecutionTargetProcessSessionBridgeHandle {
    AdapterExecutionTargetProcessSessionBridgeHandle {
        agent_command,
        has_stop: true,
    }
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

    // ---- R486 — paperclip bridge handle 计划 ----

    #[test]
    fn shell_command_selects_by_transport() {
        assert_eq!(adapter_execution_target_shell_command(Some(&ssh_target())), "sh");
        assert_eq!(
            adapter_execution_target_shell_command(Some(&sandbox_target())),
            "sh"
        );
        assert_eq!(
            adapter_execution_target_shell_command(Some(&local_target())),
            "sh"
        );
        let mut sandbox = match sandbox_target() {
            AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Sandbox(s)) => s,
            other => panic!("unexpected {other:?}"),
        };
        sandbox.shell_command = Some("bash".to_string());
        assert_eq!(
            adapter_execution_target_shell_command(Some(&AdapterExecutionTarget::Remote(
                AdapterRemoteExecutionTarget::Sandbox(sandbox)
            ))),
            "bash"
        );
    }

    #[test]
    fn bridge_timeout_ms_prefers_timeout_sec() {
        // timeoutSec > 0 → sec * 1000。
        assert_eq!(
            resolve_bridge_timeout_ms(Some(45.0), Some(&ssh_target())),
            Some(45_000)
        );
        // 非正/缺失 → sandbox timeoutMs；ssh 无 timeoutMs。
        assert_eq!(
            resolve_bridge_timeout_ms(Some(0.0), Some(&sandbox_target())),
            Some(30_000)
        );
        assert_eq!(
            resolve_bridge_timeout_ms(None, Some(&sandbox_target())),
            Some(30_000)
        );
        assert_eq!(resolve_bridge_timeout_ms(None, Some(&ssh_target())), None);
        assert_eq!(resolve_bridge_timeout_ms(None, None), None);
    }

    #[test]
    fn bridge_max_body_bytes_falls_back_to_default() {
        assert_eq!(resolve_bridge_max_body_bytes(Some(512)), 512);
        assert_eq!(
            resolve_bridge_max_body_bytes(Some(0)),
            crate::sandbox_callback_bridge::DEFAULT_SANDBOX_CALLBACK_BRIDGE_MAX_BODY_BYTES
        );
        assert_eq!(
            resolve_bridge_max_body_bytes(None),
            crate::sandbox_callback_bridge::DEFAULT_SANDBOX_CALLBACK_BRIDGE_MAX_BODY_BYTES
        );
    }

    #[test]
    fn bridge_handle_paths_match_node() {
        let paths = bridge_handle_paths("/remote/.paperclip-runtime/codex");
        assert_eq!(
            paths.bridge_runtime_dir,
            "/remote/.paperclip-runtime/codex/paperclip-bridge"
        );
        assert_eq!(
            paths.queue_dir,
            "/remote/.paperclip-runtime/codex/paperclip-bridge/queue"
        );
        assert_eq!(
            paths.asset_remote_dir,
            "/remote/.paperclip-runtime/codex/paperclip-bridge/server"
        );
    }

    #[test]
    fn proxy_request_plan_matches_node_handle_request() {
        let request = crate::sandbox_callback_bridge::SandboxCallbackBridgeRequest {
            id: "req-1".to_string(),
            method: " post ".to_string(),
            path: "/api/issues/i-1/comments".to_string(),
            query: "?a=1".to_string(),
            headers: {
                let mut h = std::collections::BTreeMap::new();
                h.insert("accept".to_string(), "application/json".to_string());
                h.insert("x-blank".to_string(), "   ".to_string());
                h
            },
            body: r#"{"text":"hi"}"#.to_string(),
            created_at: "ts".to_string(),
        };
        let plan = build_bridge_proxy_request_plan(
            &request,
            "http://host:3100",
            "host-token",
            "run-1",
        );
        assert_eq!(plan.method, "POST");
        assert_eq!(plan.url, "http://host:3100/api/issues/i-1/comments?a=1");
        assert_eq!(plan.headers["accept"], "application/json");
        assert!(!plan.headers.contains_key("x-blank"));
        assert_eq!(plan.headers["authorization"], "Bearer host-token");
        assert_eq!(plan.headers["x-paperclip-run-id"], "run-1");
        assert_eq!(plan.body, Some(r#"{"text":"hi"}"#.to_string()));
        assert_eq!(plan.timeout_ms, BRIDGE_PROXY_REQUEST_TIMEOUT_MS);

        // GET 不带 body；空 method 归一化为 GET。
        let get_request = crate::sandbox_callback_bridge::SandboxCallbackBridgeRequest {
            id: "req-2".to_string(),
            method: "   ".to_string(),
            path: "/api/agents/me".to_string(),
            query: String::new(),
            headers: std::collections::BTreeMap::new(),
            body: String::new(),
            created_at: "ts".to_string(),
        };
        let get_plan = build_bridge_proxy_request_plan(
            &get_request,
            "http://host:3100",
            "host-token",
            "run-1",
        );
        assert_eq!(get_plan.method, "GET");
        assert_eq!(get_plan.url, "http://host:3100/api/agents/me");
        assert_eq!(get_plan.body, None);
    }

    #[test]
    fn bridge_host_token_validation_matches_node() {
        assert_eq!(
            bridge_host_api_token_or_error(Some("  tok  ")),
            Ok("tok".to_string())
        );
        assert_eq!(
            bridge_host_api_token_or_error(Some("   ")),
            Err("Sandbox bridge mode requires a host-side Paperclip API token.".to_string())
        );
        assert_eq!(
            bridge_host_api_token_or_error(None),
            Err("Sandbox bridge mode requires a host-side Paperclip API token.".to_string())
        );
    }

    #[test]
    fn start_bridge_plan_composes_full_handle() {
        let plan = start_adapter_execution_target_paperclip_bridge_plan(
            "run-1",
            Some(&ssh_target()),
            None,
            "codex",
            Some(45.0),
            Some("api-token"),
            None,
            None,
        )
        .expect("token present");
        assert_eq!(
            plan.paths.queue_dir,
            "/workspace/.paperclip-runtime/codex/paperclip-bridge/queue"
        );
        assert_eq!(plan.timeout_ms, Some(45_000));
        assert_eq!(plan.max_body_bytes, 256 * 1024);
        assert_eq!(plan.host_api_url, "http://localhost:3100");
        assert!(!plan.bridge_token.is_empty());
        assert_eq!(plan.env["PAPERCLIP_API_BRIDGE_MODE"], "queue_v1");
        assert_eq!(plan.env["PAPERCLIP_API_KEY"], plan.bridge_token);
        assert_eq!(
            plan.env["PAPERCLIP_BRIDGE_QUEUE_DIR"],
            plan.paths.queue_dir
        );
        assert_eq!(plan.env["PAPERCLIP_API_URL"], plan.host_api_url);
        assert!(!plan.has_run_log_tail, "ssh 无 run log 流");
        assert_eq!(plan.run_id, "run-1");

        // sandbox + streamRunLogs → run log tail；缺 token → Err。
        let sandbox_plan = start_adapter_execution_target_paperclip_bridge_plan(
            "run-1",
            Some(&sandbox_target()),
            Some("/custom/runtime"),
            "codex",
            None,
            Some("api-token"),
            Some("http://host:4310"),
            Some(1024),
        )
        .expect("token present");
        assert!(sandbox_plan.has_run_log_tail);
        assert_eq!(sandbox_plan.timeout_ms, Some(30_000));
        assert_eq!(sandbox_plan.max_body_bytes, 1024);
        assert_eq!(sandbox_plan.host_api_url, "http://host:4310");
        assert_eq!(
            sandbox_plan.paths.queue_dir,
            "/custom/runtime/paperclip-bridge/queue"
        );

        let error = start_adapter_execution_target_paperclip_bridge_plan(
            "run-1",
            Some(&ssh_target()),
            None,
            "codex",
            None,
            None,
            None,
            None,
        );
        assert!(error.is_err());
    }

    #[test]
    fn decide_execution_bridge_plan_gates_on_remote_and_token() {
        // 本地 target → Ok(None)（Node：usesBridge 为 false，不启动）。
        assert_eq!(
            decide_execution_bridge_plan(
                "run-1",
                Some(&local_target()),
                None,
                "codex",
                None,
                Some("tok"),
                None,
            ),
            Ok(None)
        );

        // 远程 + token → Ok(Some(plan))，env 就绪。
        let plan = decide_execution_bridge_plan(
            "run-1",
            Some(&ssh_target()),
            None,
            "codex",
            Some(45.0),
            Some("tok"),
            None,
        )
        .expect("remote ok")
        .expect("plan present");
        assert_eq!(plan.env["PAPERCLIP_API_KEY"], plan.bridge_token);
        assert_eq!(plan.env["PAPERCLIP_API_BRIDGE_MODE"], "queue_v1");

        // 远程但 token 缺失 → Err（Node throw）。
        let error = decide_execution_bridge_plan(
            "run-1",
            Some(&ssh_target()),
            None,
            "codex",
            None,
            None,
            None,
        );
        assert_eq!(
            error,
            Err(
                "Sandbox bridge mode requires a host-side Paperclip API token."
                    .to_string()
            )
        );
    }

    #[test]
    fn merge_bridge_handle_env_overrides_existing_keys() {
        let plan = decide_execution_bridge_plan(
            "run-1",
            Some(&ssh_target()),
            None,
            "codex",
            None,
            Some("tok"),
            None,
        )
        .unwrap()
        .unwrap();
        let mut env = std::collections::BTreeMap::new();
        env.insert("PAPERCLIP_API_URL".to_string(), "http://old".to_string());
        env.insert("KEEP_ME".to_string(), "v".to_string());
        merge_bridge_handle_env(&mut env, &plan);
        assert_eq!(env["PAPERCLIP_API_URL"], plan.host_api_url);
        assert_eq!(env["KEEP_ME"], "v");
        assert_eq!(env["PAPERCLIP_BRIDGE_QUEUE_DIR"], plan.paths.queue_dir);
    }

    // ---- R490 — 执行 env 与 bridge env 合并 ----

    fn target_json(target: &AdapterExecutionTarget) -> serde_json::Value {
        serde_json::to_value(target).expect("serialize target")
    }

    fn base_env_with_token() -> std::collections::BTreeMap<String, String> {
        let mut env = std::collections::BTreeMap::new();
        env.insert("PAPERCLIP_RUN_ID".to_string(), "run-1".to_string());
        env.insert("PAPERCLIP_API_KEY".to_string(), "host-token".to_string());
        env.insert("PAPERCLIP_API_URL".to_string(), "http://host:3100".to_string());
        env.insert("CODEX_HOME".to_string(), "/home/codex".to_string());
        env
    }

    fn merge_input<'a>(
        base_env: &'a std::collections::BTreeMap<String, String>,
        execution_target: Option<&'a serde_json::Value>,
        adapter_key: &'a str,
    ) -> MergeExecutionBridgeEnvInput<'a> {
        MergeExecutionBridgeEnvInput {
            run_id: "run-1",
            base_env,
            execution_target,
            runtime_root_dir: None,
            adapter_key,
            timeout_sec: None,
            host_api_url: None,
        }
    }

    #[test]
    fn merge_execution_bridge_env_local_returns_base_unchanged() {
        let base = base_env_with_token();
        let merged = merge_execution_bridge_env(&merge_input(
            &base,
            Some(&target_json(&local_target())),
            "codex",
        ))
        .expect("local no error");
        assert_eq!(merged.env, base);
        assert!(merged.bridge_plan.is_none());
        assert!(merged.start_log_line.is_none());
    }

    #[test]
    fn merge_execution_bridge_env_none_target_is_local() {
        let base = base_env_with_token();
        let merged = merge_execution_bridge_env(&merge_input(&base, None, "codex"))
            .expect("no error");
        assert!(merged.bridge_plan.is_none());
        assert_eq!(merged.env, base);
    }

    #[test]
    fn merge_execution_bridge_env_remote_merges_four_keys_and_log() {
        let base = base_env_with_token();
        let merged = merge_execution_bridge_env(&merge_input(
            &base,
            Some(&target_json(&ssh_target())),
            "codex",
        ))
        .expect("remote ok");
        let plan = merged.bridge_plan.as_ref().expect("plan present");
        assert_eq!(merged.env["PAPERCLIP_API_URL"], plan.host_api_url);
        assert_eq!(merged.env["PAPERCLIP_API_KEY"], plan.bridge_token);
        assert_eq!(merged.env["PAPERCLIP_API_BRIDGE_MODE"], "queue_v1");
        assert_eq!(merged.env["PAPERCLIP_BRIDGE_QUEUE_DIR"], plan.paths.queue_dir);
        assert_eq!(merged.env["CODEX_HOME"], "/home/codex");
        assert_eq!(merged.env["PAPERCLIP_RUN_ID"], "run-1");
        assert_eq!(plan.host_api_url, "http://host:3100");
        assert_eq!(
            merged.start_log_line.as_deref(),
            Some(
                "[paperclip] Starting sandbox callback bridge for codex in \
                 /workspace/.paperclip-runtime/codex/paperclip-bridge.\n"
                    .trim_start()
            )
        );
    }

    #[test]
    fn merge_execution_bridge_env_prefers_runtime_api_url() {
        let mut base = base_env_with_token();
        base.insert(
            "PAPERCLIP_RUNTIME_API_URL".to_string(),
            "http://runtime:4000".to_string(),
        );
        let merged = merge_execution_bridge_env(&merge_input(
            &base,
            Some(&target_json(&ssh_target())),
            "claude",
        ))
        .expect("remote ok");
        let plan = merged.bridge_plan.as_ref().expect("plan present");
        assert_eq!(plan.host_api_url, "http://runtime:4000");
        assert_eq!(
            merged.start_log_line.as_deref(),
            Some(
                "[paperclip] Starting sandbox callback bridge for claude in \
                 /workspace/.paperclip-runtime/claude/paperclip-bridge.\n"
                    .trim_start()
            )
        );
    }

    #[test]
    fn merge_execution_bridge_env_remote_missing_token_errors() {
        let mut base = base_env_with_token();
        base.remove("PAPERCLIP_API_KEY");
        let error = merge_execution_bridge_env(&merge_input(
            &base,
            Some(&target_json(&ssh_target())),
            "codex",
        ))
        .expect_err("token required");
        assert!(error.contains("Sandbox bridge mode requires"));
    }

    // ---- R487 — 进程 session bridge 决策 ----

    #[test]
    fn json_line_and_split_roundtrip() {
        let line = json_line(&serde_json::json!({"type": "hello"}));
        assert_eq!(line, "{\"type\":\"hello\"}\n");
        let (lines, rest) = split_json_lines("{\"a\":1}\n{\"b\":2}\npartial");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "{\"a\":1}");
        assert_eq!(lines[1], "{\"b\":2}");
        assert_eq!(rest, "partial");
        let (lines, rest) = split_json_lines("complete\n");
        assert_eq!(lines, vec!["complete".to_string()]);
        assert_eq!(rest, "");
    }

    #[test]
    fn proxy_source_matches_node_template() {
        let source = get_process_session_proxy_source(4310, "tok\"x");
        assert!(source.starts_with("#!/usr/bin/env node\n"));
        assert!(source.contains("port: 4310"));
        assert!(source.contains("const token = \"tok\\\"x\";"));
        assert!(source.contains("socket.on(\"connect\", () => send({ type: \"hello\" }));"));
        assert!(source.contains("type: \"stdin\", data: Buffer.from(chunk).toString(\"base64\")"));
        assert!(source.contains("type: \"stdinEnd\""));
        assert!(source.contains("message.stream === \"stderr\" ? process.stderr : process.stdout"));
        assert!(source.contains("process.exitCode = typeof message.code === \"number\" ? message.code : 1;"));
        assert!(source.contains("if (!exiting) process.exit(1);"));
        // 模板插值未残留未转义的大括号。
        assert!(!source.contains("{{"));
        assert!(!source.contains("}}"));
    }

    #[test]
    fn remote_source_matches_node_template() {
        let source = get_process_session_remote_source();
        assert!(source.contains("Missing process session bridge env."));
        assert!(source.contains("String(seq).padStart(12, \"0\") + \".json\""));
        assert!(source.contains("await fs.rename(file + \".tmp\", file);"));
        assert!(source.contains("child.on(\"close\", (code, signal) => void writeEvent({ type: \"exit\", code, signal }));"));
        assert!(source.contains("message.type === \"stdinEnd\""));
        assert!(source.contains("setTimeout(resolve, 50)"));
        assert!(source.contains("pollStdin().catch"));
    }

    #[test]
    fn remote_script_sync_plan_matches_node() {
        let plan = sync_process_session_remote_script_plan(
            "/runtime/process-sessions",
            "/runtime/process-sessions/paperclip-process-session-remote.mjs",
        );
        assert_eq!(plan.action, "sync process session remote script");
        assert_eq!(plan.label, "Process session remote script");
        assert_eq!(
            plan.lock_dir,
            "/runtime/process-sessions/.paperclip-process-session-script.lock"
        );
        assert_eq!(
            plan.sha256,
            crate::sandbox_callback_bridge::sha256_hex_utf8(
                &get_process_session_remote_source()
            )
        );
        assert!(plan
            .uploaded_decision_script
            .contains(plan.expected_sha()));
    }

    #[test]
    fn command_payload_roundtrips_sanitized_env() {
        let mut env = std::collections::BTreeMap::new();
        env.insert("FOO".to_string(), "bar".to_string());
        env.insert("PATH".to_string(), "/bin".to_string());
        let payload = build_process_session_command_payload(
            "claude",
            &["-p".to_string(), "hi".to_string()],
            "/remote/cwd",
            &env,
        );
        let decoded = crate::sandbox_callback_bridge::base64_decode_utf8(&payload)
            .expect("valid payload");
        let value: serde_json::Value = serde_json::from_str(&decoded).unwrap();
        assert_eq!(value["command"], "claude");
        assert_eq!(value["args"], serde_json::json!(["-p", "hi"]));
        assert_eq!(value["cwd"], "/remote/cwd");
        assert_eq!(value["env"]["FOO"], "bar");
        // PATH 属于身份键且 inherited 为空 → 保留（对齐 sanitize 语义）。
        assert_eq!(value["env"]["PATH"], "/bin");
    }

    #[test]
    fn start_script_matches_node_layout() {
        let script = build_process_session_bridge_start_script(
            "/s/stdin",
            "/s/events",
            "/s",
            "cGF5bG9hZA==",
            "/runtime/paperclip-process-session-remote.mjs",
        );
        let lines: Vec<&str> = script.lines().collect();
        assert_eq!(lines[0], "mkdir -p '/s/stdin' '/s/events'");
        assert_eq!(
            lines[1],
            "PAPERCLIP_PROCESS_SESSION_DIR='/s' PAPERCLIP_PROCESS_SESSION_COMMAND_B64='cGF5bG9hZA==' nohup node '/runtime/paperclip-process-session-remote.mjs' >/dev/null 2>&1 < /dev/null &"
        );
        assert_eq!(lines[2], "printf '%s\\n' \"$!\"");
    }

    #[test]
    fn remote_event_socket_action_matches_node() {
        assert_eq!(remote_event_socket_action("exit"), RemoteEventSocketAction::End);
        assert_eq!(
            remote_event_socket_action("error"),
            RemoteEventSocketAction::Destroy
        );
        assert_eq!(remote_event_socket_action("data"), RemoteEventSocketAction::Write);
        assert_eq!(remote_event_socket_action("hello"), RemoteEventSocketAction::Write);
    }

    #[test]
    fn process_session_bridge_plan_gates_on_sandbox() {
        // 本地 / ssh → None。
        assert!(start_adapter_execution_target_process_session_bridge_plan(
            "s1",
            Some(&local_target()),
            None,
            "codex",
            "cmd",
            &[],
            "",
            &std::collections::BTreeMap::new(),
            None,
        )
        .is_none());
        assert!(start_adapter_execution_target_process_session_bridge_plan(
            "s1",
            Some(&ssh_target()),
            None,
            "codex",
            "cmd",
            &[],
            "",
            &std::collections::BTreeMap::new(),
            None,
        )
        .is_none());

        // sandbox → Some；路径与超时对齐 Node。
        let plan = start_adapter_execution_target_process_session_bridge_plan(
            "session-uuid",
            Some(&sandbox_target()),
            None,
            "codex",
            "node",
            &["-v".to_string()],
            "",
            &std::collections::BTreeMap::new(),
            None,
        )
        .expect("sandbox plan");
        assert_eq!(
            plan.bridge_runtime_dir,
            "/workspace/.paperclip-runtime/codex/process-sessions"
        );
        assert_eq!(plan.session_dir, "/workspace/.paperclip-runtime/codex/process-sessions/session-uuid");
        assert_eq!(plan.stdin_dir, "/workspace/.paperclip-runtime/codex/process-sessions/session-uuid/stdin");
        assert_eq!(plan.events_dir, "/workspace/.paperclip-runtime/codex/process-sessions/session-uuid/events");
        assert_eq!(
            plan.remote_script_path,
            "/workspace/.paperclip-runtime/codex/process-sessions/paperclip-process-session-remote.mjs"
        );
        // timeoutSec 缺省 → sandbox timeoutMs。
        assert_eq!(plan.timeout_ms, Some(30_000));
        // proxy token 18 bytes → 24 chars base64url。
        assert_eq!(plan.proxy_token.len(), 24);
        // cwd 为空 → target.remoteCwd。
        let decoded = crate::sandbox_callback_bridge::base64_decode_utf8(&plan.command_payload)
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&decoded).unwrap();
        assert_eq!(value["cwd"], "/workspace");
        assert_eq!(value["command"], "node");
        assert!(plan.start_script.contains("nohup node"));
    }

    #[test]
    fn process_session_bridge_plan_prefers_timeout_sec() {
        let plan = start_adapter_execution_target_process_session_bridge_plan(
            "s1",
            Some(&sandbox_target()),
            Some("/rt"),
            "codex",
            "cmd",
            &[],
            "/cwd",
            &std::collections::BTreeMap::new(),
            Some(120.0),
        )
        .expect("sandbox plan");
        assert_eq!(plan.timeout_ms, Some(120_000));
        assert_eq!(plan.bridge_runtime_dir, "/rt/process-sessions");
    }

    // ---- R488 — proxy 连接/事件决策 ----

    #[test]
    fn proxy_stdin_file_name_pads_to_twelve() {
        assert_eq!(proxy_stdin_file_name(1), "000000000001.json");
        assert_eq!(proxy_stdin_file_name(12), "000000000012.json");
        assert_eq!(proxy_stdin_file_name(123_456_789_012), "123456789012.json");
    }

    #[test]
    fn proxy_connection_message_decision_matches_node() {
        let token = "proxy-tok";
        // token 匹配 + 首次 → Authenticate。
        assert_eq!(
            decide_proxy_connection_message(Some(token), token, false, false),
            ProxyConnectionDecision::Authenticate
        );
        // token 不匹配 / 缺失 → Reject。
        assert_eq!(
            decide_proxy_connection_message(Some("wrong"), token, false, false),
            ProxyConnectionDecision::Reject
        );
        assert_eq!(
            decide_proxy_connection_message(None, token, false, false),
            ProxyConnectionDecision::Reject
        );
        // 已有活跃 socket 抢占 → Reject（连接独占会话）。
        assert_eq!(
            decide_proxy_connection_message(Some(token), token, false, true),
            ProxyConnectionDecision::Reject
        );
        // 已鉴权 → Proceed。
        assert_eq!(
            decide_proxy_connection_message(Some(token), token, true, true),
            ProxyConnectionDecision::Proceed
        );
        assert_eq!(
            decide_proxy_connection_message(Some(token), token, true, false),
            ProxyConnectionDecision::Proceed
        );
    }

    #[test]
    fn proxy_auth_timeout_destroys_unauthenticated() {
        assert!(proxy_connection_auth_timed_out(false));
        assert!(!proxy_connection_auth_timed_out(true));
        assert_eq!(PROCESS_SESSION_AUTH_TIMEOUT_MS, 5_000);
    }

    #[test]
    fn proxy_message_line_parsing() {
        let parsed = parse_proxy_message_line(
            r#"{"token":"t","type":"stdin","data":"aGk="}"#,
        )
        .expect("valid");
        assert_eq!(parsed["type"], "stdin");
        assert!(parse_proxy_message_line("not-json").is_err());
    }

    #[test]
    fn proxy_stdin_write_matches_node_branches() {
        let stdin = build_proxy_stdin_write(3, Some("stdin"), Some("aGk=")).unwrap();
        assert_eq!(stdin.file_name, "000000000003.json");
        let stdin_value: serde_json::Value =
            serde_json::from_str(stdin.body.trim_end()).unwrap();
        assert_eq!(stdin_value["type"], "stdin");
        assert_eq!(stdin_value["data"], "aGk=");

        let end = build_proxy_stdin_write(4, Some("stdinEnd"), None).unwrap();
        assert_eq!(end.file_name, "000000000004.json");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(end.body.trim_end())
                .unwrap()["type"],
            "stdinEnd"
        );

        // 其他 type / stdin 缺 data → 不写。
        assert_eq!(build_proxy_stdin_write(5, Some("hello"), None), None);
        assert_eq!(build_proxy_stdin_write(5, Some("stdin"), None), None);
        assert_eq!(build_proxy_stdin_write(5, None, None), None);
    }

    #[test]
    fn proxy_poll_stop_and_stop_stdin_end() {
        assert!(decide_proxy_poll_should_stop(Some("exit")));
        assert!(decide_proxy_poll_should_stop(Some("error")));
        assert!(!decide_proxy_poll_should_stop(Some("data")));
        assert!(!decide_proxy_poll_should_stop(None));
        assert_eq!(PROXY_POLL_INTERVAL_MS, 100);

        let stop = build_proxy_stop_stdin_end_write(5);
        assert_eq!(stop.file_name, "000000000006.json");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(stop.body.trim_end())
                .unwrap()["type"],
            "stdinEnd"
        );
    }

    #[test]
    fn listen_port_and_error_line() {
        assert_eq!(process_session_listen_port_or_error(Some(4310)), Ok(4310));
        assert_eq!(
            process_session_listen_port_or_error(None),
            Err("Process session bridge did not expose a TCP port.".to_string())
        );
        let error_value: serde_json::Value =
            serde_json::from_str(proxy_error_message_line("boom").trim_end()).unwrap();
        assert_eq!(error_value["type"], "error");
        assert_eq!(error_value["message"], "boom");
    }

    #[test]
    fn remote_event_delivery_matches_node() {
        // 有 socket：exit → End、error → Destroy、data → Write。
        assert_eq!(
            decide_remote_event_delivery(true, Some("exit")),
            RemoteEventDeliveryDecision::WriteToSocket {
                action: RemoteEventSocketAction::End
            }
        );
        assert_eq!(
            decide_remote_event_delivery(true, Some("error")),
            RemoteEventDeliveryDecision::WriteToSocket {
                action: RemoteEventSocketAction::Destroy
            }
        );
        assert_eq!(
            decide_remote_event_delivery(true, Some("data")),
            RemoteEventDeliveryDecision::WriteToSocket {
                action: RemoteEventSocketAction::Write
            }
        );
        // 无 socket：缓冲；exit/error 停止轮询。
        assert_eq!(
            decide_remote_event_delivery(false, Some("exit")),
            RemoteEventDeliveryDecision::QueuePending { stop_loop: true }
        );
        assert_eq!(
            decide_remote_event_delivery(false, Some("data")),
            RemoteEventDeliveryDecision::QueuePending { stop_loop: false }
        );
    }
}
