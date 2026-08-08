//! `pc-acpx` prepared runtime — a subset of the Node `AcpxPreparedRuntime`
//! interface translated into a Rust data structure.
//!
//! `AcpxPreparedRuntime` in the Node engine is the **output** of the giant
//! `buildRuntime` function; it carries 30+ fields covering the agent
//! identity, the resolved environment, the timeout/wall-clock policy, the
//! staged runtime, the bridges, the skill prompt, the MCP servers, etc. The
//! full port spans multiple rounds. This module captures the **minimum
//! data-only fields** that downstream helpers consume without modification.
//!
//! When the field set is needed by a later round, add it to this struct and
//! to the constructor helpers here. The goal is to grow the surface
//! incrementally without breaking the public API.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::agent_command::BuiltInAgentCommand;
use crate::cache_lifecycle::AsyncCallback;
use crate::startup_metrics::StartupStepMetrics;

// ============================================================================
// Common types
// ============================================================================

/// Normalized session mode. Mirrors `NormalizedMode` from `normalize.rs`,
/// redeclared here so `PreparedRuntime` is self-contained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreparedRuntimeMode {
    Persistent,
    OneShot,
}

impl PreparedRuntimeMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            PreparedRuntimeMode::Persistent => "persistent",
            PreparedRuntimeMode::OneShot => "oneshot",
        }
    }
}

/// Normalized permission mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreparedRuntimePermissionMode {
    ApproveAll,
    ApproveReads,
    DenyAll,
}

impl PreparedRuntimePermissionMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            PreparedRuntimePermissionMode::ApproveAll => "approve-all",
            PreparedRuntimePermissionMode::ApproveReads => "approve-reads",
            PreparedRuntimePermissionMode::DenyAll => "deny-all",
        }
    }
}

// ============================================================================
// Prepared staged runtime (sandbox bridge seam)
// ============================================================================

/// Staged remote runtime descriptor. Returned by `build_runtime` when a
/// remote process session staged the workspace + managed home into a
/// sandbox before the run started. Mirrors the inline
/// `PreparedAdapterExecutionTargetRuntime` shape used by the Node engine's
/// `buildRuntime` for the remote lane.
///
/// The Rust port keeps the type intentionally minimal: the full
/// remote-runtime record lives in the sandbox-utils seam (R375+), so
/// here we only expose the two paths the executor consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedStagedRuntime {
    /// Host-side workspace path (the staging source).
    pub workspace_local_dir: PathBuf,
    /// In-sandbox workspace path (the staging target). `None` on local
    /// runs that never crossed the staging seam.
    pub workspace_remote_dir: Option<PathBuf>,
}

impl PreparedStagedRuntime {
    /// Build a host-only (local) staged runtime descriptor.
    pub fn local(workspace_local_dir: impl Into<PathBuf>) -> Self {
        Self {
            workspace_local_dir: workspace_local_dir.into(),
            workspace_remote_dir: None,
        }
    }

    /// Build a host + remote (sandbox) staged runtime descriptor.
    pub fn remote(
        workspace_local_dir: impl Into<PathBuf>,
        workspace_remote_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            workspace_local_dir: workspace_local_dir.into(),
            workspace_remote_dir: Some(workspace_remote_dir.into()),
        }
    }
}

/// Normalized non-interactive permission policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreparedRuntimeNonInteractivePermissions {
    Deny,
    Fail,
}

impl PreparedRuntimeNonInteractivePermissions {
    pub fn as_str(&self) -> &'static str {
        match self {
            PreparedRuntimeNonInteractivePermissions::Deny => "deny",
            PreparedRuntimeNonInteractivePermissions::Fail => "fail",
        }
    }
}

/// Timeout resolution. Tells the operator where the effective timeout came
/// from so a later timeout is diagnosable from the run log alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeoutResolution {
    /// Effective timeout in seconds. `0` = no timeout.
    pub timeout_sec: u64,
    /// Source of the timeout (e.g. "adapterConfig", "sandbox default", "default").
    pub source: String,
    /// Optional human-readable note (e.g. "(sandbox default; set adapterConfig.timeoutSec to override)").
    pub note: Option<String>,
}

// ============================================================================
// Prepared runtime
// ============================================================================

/// A subset of the Node `AcpxPreparedRuntime` data structure. The fields
/// here are the inputs that downstream helpers (timeout formatting, env
/// rendering, prompt building) consume without needing the bridges and
/// staging seams that live in later rounds.
#[derive(Debug, Clone)]
pub struct PreparedRuntime {
    /// The agent id, e.g. `"claude"`, `"codex"`, `"gemini"`.
    pub acpx_agent: String,
    /// Session mode.
    pub mode: PreparedRuntimeMode,
    /// Working directory for the runtime.
    pub cwd: PathBuf,
    /// Workspace identifier (empty string when no workspace).
    pub workspace_id: String,
    /// Workspace repo URL (empty string when no workspace).
    pub workspace_repo_url: String,
    /// Workspace repo ref (empty string when no workspace).
    pub workspace_repo_ref: String,
    /// Env passed to the runtime. Paths and secrets are resolved by the
    /// caller.
    pub env: BTreeMap<String, String>,
    /// Env actually written to the runtime logs (secrets may be redacted).
    pub logged_env: BTreeMap<String, String>,
    /// State directory for the runtime (per-(company, agent) cache).
    pub state_dir: PathBuf,
    /// Permission mode.
    pub permission_mode: PreparedRuntimePermissionMode,
    /// Non-interactive permission policy.
    pub non_interactive_permissions: PreparedRuntimeNonInteractivePermissions,
    /// Requested model.
    pub requested_model: String,
    /// Requested thinking effort.
    pub requested_thinking_effort: String,
    /// Fast mode toggle.
    pub fast_mode: bool,
    /// Effective wall-clock timeout in seconds. `0` disables the timeout.
    pub timeout_sec: u64,
    /// Timeout resolution source.
    pub timeout_resolution: TimeoutResolution,
    /// Session key (used as the cache key for warm handles).
    pub session_key: String,
    /// Config fingerprint (used to detect incompatible resumes).
    pub fingerprint: String,
    /// Built-in agent command (or `None` when the agent is custom).
    pub agent_command: Option<BuiltInAgentCommand>,
    /// Per-step startup metrics. Empty for local runs.
    pub step_metrics: StartupStepMetrics,
    /// Staged remote runtime (sandbox lane only). `None` for local /
    /// runner-less ACP→CLI fallback runs that never crossed the staging
    /// seam.
    pub staged_runtime: Option<PreparedStagedRuntime>,
    /// Remote execution session identity (SSH 4-tuple or sandbox 5-tuple).
    /// `None` on local runs. Mirrors Node `prepared.remoteExecutionIdentity`
    /// and is serialized into `sessionParams.remoteExecution`.
    pub remote_execution_identity: Option<BTreeMap<String, serde_json::Value>>,
    /// Env delta applied by the staging seam (e.g. `CODEX_HOME` repointed
    /// onto the in-sandbox asset dir). Replayed verbatim on a compatible
    /// resume so a later run reuses the same home.
    pub remote_staging_env_delta: Option<BTreeMap<String, String>>,
    /// Per-run teardown that copies the live in-sandbox auth back onto the
    /// host (e.g. codex `auth.json` copy-back). `None` on local runs.
    pub remote_managed_home_teardown: Option<AsyncCallback>,
    /// One-time staged-temp cleanup. Fired when the staged entry is
    /// dropped (cache eviction / session end). `None` on local runs.
    pub remote_staging_dispose: Option<AsyncCallback>,
}

impl PreparedRuntime {
    /// Create a builder with sensible defaults. The caller fills in the
    /// agent-specific fields.
    pub fn builder(acpx_agent: impl Into<String>) -> PreparedRuntimeBuilder {
        PreparedRuntimeBuilder {
            acpx_agent: acpx_agent.into(),
            mode: PreparedRuntimeMode::Persistent,
            cwd: PathBuf::new(),
            workspace_id: String::new(),
            workspace_repo_url: String::new(),
            workspace_repo_ref: String::new(),
            env: BTreeMap::new(),
            logged_env: BTreeMap::new(),
            state_dir: PathBuf::new(),
            permission_mode: PreparedRuntimePermissionMode::ApproveAll,
            non_interactive_permissions: PreparedRuntimeNonInteractivePermissions::Deny,
            requested_model: String::new(),
            requested_thinking_effort: String::new(),
            fast_mode: false,
            timeout_sec: 0,
            timeout_resolution: TimeoutResolution {
                timeout_sec: 0,
                source: "default".into(),
                note: None,
            },
            session_key: String::new(),
            fingerprint: String::new(),
            agent_command: None,
            step_metrics: StartupStepMetrics::default(),
            staged_runtime: None,
            remote_execution_identity: None,
            remote_staging_env_delta: None,
            remote_managed_home_teardown: None,
            remote_staging_dispose: None,
        }
    }
}

/// Build a `PreparedRuntime` field-by-field. Used by `buildRuntime` in later
/// rounds to assemble the data structure without boilerplate.
#[derive(Debug, Clone)]
pub struct PreparedRuntimeBuilder {
    pub acpx_agent: String,
    pub mode: PreparedRuntimeMode,
    pub cwd: PathBuf,
    pub workspace_id: String,
    pub workspace_repo_url: String,
    pub workspace_repo_ref: String,
    pub env: BTreeMap<String, String>,
    pub logged_env: BTreeMap<String, String>,
    pub state_dir: PathBuf,
    pub permission_mode: PreparedRuntimePermissionMode,
    pub non_interactive_permissions: PreparedRuntimeNonInteractivePermissions,
    pub requested_model: String,
    pub requested_thinking_effort: String,
    pub fast_mode: bool,
    pub timeout_sec: u64,
    pub timeout_resolution: TimeoutResolution,
    pub session_key: String,
    pub fingerprint: String,
    pub agent_command: Option<BuiltInAgentCommand>,
    pub step_metrics: StartupStepMetrics,
    pub staged_runtime: Option<PreparedStagedRuntime>,
    pub remote_execution_identity: Option<BTreeMap<String, serde_json::Value>>,
    pub remote_staging_env_delta: Option<BTreeMap<String, String>>,
    pub remote_managed_home_teardown: Option<AsyncCallback>,
    pub remote_staging_dispose: Option<AsyncCallback>,
}

impl PreparedRuntimeBuilder {
    pub fn mode(mut self, mode: PreparedRuntimeMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = cwd.into();
        self
    }

    pub fn workspace_id(mut self, value: impl Into<String>) -> Self {
        self.workspace_id = value.into();
        self
    }

    pub fn workspace_repo_url(mut self, value: impl Into<String>) -> Self {
        self.workspace_repo_url = value.into();
        self
    }

    pub fn workspace_repo_ref(mut self, value: impl Into<String>) -> Self {
        self.workspace_repo_ref = value.into();
        self
    }

    pub fn env(mut self, env: BTreeMap<String, String>) -> Self {
        self.env = env;
        self
    }

    pub fn logged_env(mut self, env: BTreeMap<String, String>) -> Self {
        self.logged_env = env;
        self
    }

    pub fn state_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.state_dir = path.into();
        self
    }

    pub fn permission_mode(mut self, mode: PreparedRuntimePermissionMode) -> Self {
        self.permission_mode = mode;
        self
    }

    pub fn non_interactive_permissions(
        mut self,
        policy: PreparedRuntimeNonInteractivePermissions,
    ) -> Self {
        self.non_interactive_permissions = policy;
        self
    }

    pub fn requested_model(mut self, value: impl Into<String>) -> Self {
        self.requested_model = value.into();
        self
    }

    pub fn requested_thinking_effort(mut self, value: impl Into<String>) -> Self {
        self.requested_thinking_effort = value.into();
        self
    }

    pub fn fast_mode(mut self, enabled: bool) -> Self {
        self.fast_mode = enabled;
        self
    }

    pub fn timeout_sec(mut self, value: u64) -> Self {
        self.timeout_sec = value;
        self
    }

    pub fn timeout_resolution(mut self, resolution: TimeoutResolution) -> Self {
        self.timeout_resolution = resolution;
        self
    }

    pub fn session_key(mut self, value: impl Into<String>) -> Self {
        self.session_key = value.into();
        self
    }

    pub fn fingerprint(mut self, value: impl Into<String>) -> Self {
        self.fingerprint = value.into();
        self
    }

    pub fn agent_command(mut self, command: BuiltInAgentCommand) -> Self {
        self.agent_command = Some(command);
        self
    }

    pub fn step_metrics(mut self, metrics: StartupStepMetrics) -> Self {
        self.step_metrics = metrics;
        self
    }

    pub fn staged_runtime(mut self, staged: PreparedStagedRuntime) -> Self {
        self.staged_runtime = Some(staged);
        self
    }

    /// Set the remote execution session identity (R434).
    pub fn remote_execution_identity(
        mut self,
        identity: BTreeMap<String, serde_json::Value>,
    ) -> Self {
        self.remote_execution_identity = Some(identity);
        self
    }

    pub fn remote_staging_env_delta(mut self, delta: BTreeMap<String, String>) -> Self {
        self.remote_staging_env_delta = Some(delta);
        self
    }

    pub fn remote_managed_home_teardown(mut self, callback: AsyncCallback) -> Self {
        self.remote_managed_home_teardown = Some(callback);
        self
    }

    pub fn remote_staging_dispose(mut self, callback: AsyncCallback) -> Self {
        self.remote_staging_dispose = Some(callback);
        self
    }

    pub fn build(self) -> PreparedRuntime {
        PreparedRuntime {
            acpx_agent: self.acpx_agent,
            mode: self.mode,
            cwd: self.cwd,
            workspace_id: self.workspace_id,
            workspace_repo_url: self.workspace_repo_url,
            workspace_repo_ref: self.workspace_repo_ref,
            env: self.env,
            logged_env: self.logged_env,
            state_dir: self.state_dir,
            permission_mode: self.permission_mode,
            non_interactive_permissions: self.non_interactive_permissions,
            requested_model: self.requested_model,
            requested_thinking_effort: self.requested_thinking_effort,
            fast_mode: self.fast_mode,
            timeout_sec: self.timeout_sec,
            timeout_resolution: self.timeout_resolution,
            session_key: self.session_key,
            fingerprint: self.fingerprint,
            agent_command: self.agent_command,
            step_metrics: self.step_metrics,
            staged_runtime: self.staged_runtime,
            remote_execution_identity: self.remote_execution_identity,
            remote_staging_env_delta: self.remote_staging_env_delta,
            remote_managed_home_teardown: self.remote_managed_home_teardown,
            remote_staging_dispose: self.remote_staging_dispose,
        }
    }
}

/// Render the human-readable "Adapter execution timeout:" line. Mirrors the
/// Node `formatAdapterExecutionTimeoutStartLogLine` helper.
pub fn format_timeout_start_log_line(resolution: &TimeoutResolution) -> String {
    if resolution.timeout_sec == 0 {
        return "Adapter execution timeout: none".to_string();
    }
    let note = resolution.note.as_deref().unwrap_or("");
    let note = if note.is_empty() {
        String::new()
    } else {
        format!(" {note}")
    };
    format!(
        "Adapter execution timeout: timeoutSec={}{} ({}).",
        resolution.timeout_sec, note, resolution.source
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_round_trip_preserves_all_fields() {
        let runtime = PreparedRuntime::builder("claude")
            .mode(PreparedRuntimeMode::OneShot)
            .cwd("/repo")
            .permission_mode(PreparedRuntimePermissionMode::DenyAll)
            .non_interactive_permissions(PreparedRuntimeNonInteractivePermissions::Fail)
            .fast_mode(true)
            .timeout_sec(60)
            .build();
        assert_eq!(runtime.acpx_agent, "claude");
        assert_eq!(runtime.mode, PreparedRuntimeMode::OneShot);
        assert_eq!(runtime.cwd, PathBuf::from("/repo"));
        assert_eq!(
            runtime.permission_mode,
            PreparedRuntimePermissionMode::DenyAll
        );
        assert_eq!(
            runtime.non_interactive_permissions,
            PreparedRuntimeNonInteractivePermissions::Fail
        );
        assert!(runtime.fast_mode);
        assert_eq!(runtime.timeout_sec, 60);
    }

    #[test]
    fn timeout_line_for_unlimited_run() {
        let resolution = TimeoutResolution {
            timeout_sec: 0,
            source: "default".into(),
            note: None,
        };
        assert_eq!(
            format_timeout_start_log_line(&resolution),
            "Adapter execution timeout: none"
        );
    }

    #[test]
    fn timeout_line_for_sandbox_default() {
        let resolution = TimeoutResolution {
            timeout_sec: 14400,
            source: "sandbox default".into(),
            note: Some("(sandbox default; set adapterConfig.timeoutSec to override)".into()),
        };
        let line = format_timeout_start_log_line(&resolution);
        assert!(line.contains("timeoutSec=14400"));
        assert!(line.contains("(sandbox default; set adapterConfig.timeoutSec to override)"));
        assert!(line.contains("(sandbox default)"));
    }

    #[test]
    fn builder_default_mode_is_persistent() {
        let runtime = PreparedRuntime::builder("claude").build();
        assert_eq!(runtime.mode, PreparedRuntimeMode::Persistent);
        assert_eq!(
            runtime.permission_mode,
            PreparedRuntimePermissionMode::ApproveAll
        );
        assert_eq!(
            runtime.non_interactive_permissions,
            PreparedRuntimeNonInteractivePermissions::Deny
        );
    }

    #[test]
    fn mode_as_str_matches_node_strings() {
        assert_eq!(PreparedRuntimeMode::Persistent.as_str(), "persistent");
        assert_eq!(PreparedRuntimeMode::OneShot.as_str(), "oneshot");
        assert_eq!(
            PreparedRuntimePermissionMode::ApproveAll.as_str(),
            "approve-all"
        );
        assert_eq!(
            PreparedRuntimePermissionMode::ApproveReads.as_str(),
            "approve-reads"
        );
        assert_eq!(PreparedRuntimePermissionMode::DenyAll.as_str(), "deny-all");
        assert_eq!(
            PreparedRuntimeNonInteractivePermissions::Deny.as_str(),
            "deny"
        );
        assert_eq!(
            PreparedRuntimeNonInteractivePermissions::Fail.as_str(),
            "fail"
        );
    }
}

#[cfg(test)]
mod staged_runtime_tests {
    use super::*;

    #[test]
    fn local_descriptor_has_no_remote_dir() {
        let staged = PreparedStagedRuntime::local("/host/repo");
        assert_eq!(staged.workspace_local_dir, PathBuf::from("/host/repo"));
        assert_eq!(staged.workspace_remote_dir, None);
    }

    #[test]
    fn remote_descriptor_keeps_both_paths() {
        let staged = PreparedStagedRuntime::remote("/host/repo", "/sandbox/workspace");
        assert_eq!(staged.workspace_local_dir, PathBuf::from("/host/repo"));
        assert_eq!(
            staged.workspace_remote_dir,
            Some(PathBuf::from("/sandbox/workspace"))
        );
    }

    #[test]
    fn builder_accepts_staged_runtime() {
        let staged = PreparedStagedRuntime::remote("/host", "/sandbox");
        let runtime = PreparedRuntime::builder("claude")
            .staged_runtime(staged.clone())
            .build();
        assert_eq!(runtime.staged_runtime, Some(staged));
    }

    #[test]
    fn builder_accepts_remote_callbacks_and_env_delta() {
        let teardown = AsyncCallback::new(|| async {});
        let dispose = AsyncCallback::new(|| async {});
        let mut delta = BTreeMap::new();
        delta.insert("CODEX_HOME".to_string(), "/sandbox/home".to_string());

        let runtime = PreparedRuntime::builder("codex")
            .remote_staging_env_delta(delta.clone())
            .remote_managed_home_teardown(teardown)
            .remote_staging_dispose(dispose)
            .build();

        assert_eq!(runtime.remote_staging_env_delta, Some(delta));
        assert!(runtime.remote_managed_home_teardown.is_some());
        assert!(runtime.remote_staging_dispose.is_some());
    }
}
