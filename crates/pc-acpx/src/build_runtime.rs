//! `pc-acpx` `build_runtime` — top-level assembly that wires every helper in
//! this crate into a single `PreparedRuntime`.
//!
//! This is the Rust port of the giant `buildRuntime` function in Node
//! `acpx-engine/execute.ts` (line 1354). The Node function is ~700 lines
//! because it composes normalization, skill staging, codex-home seeding,
//! Claude settings, fingerprint hashing, and the remote-sandbox staging
//! seam all in one async I/O function.
//!
//! The Rust port is intentionally split into smaller, independently
//! testable units:
//!
//! - [`build_paperclip_env`] mirrors Node `buildPaperclipEnv` (pure).
//! - [`apply_paperclip_workspace_env`] mirrors Node
//!   `applyPaperclipWorkspaceEnv` (pure).
//! - [`BuildRuntimeInput`] captures the inputs `build_runtime` consumes.
//! - [`build_runtime`] is the top-level orchestrator. It is **synchronous
//!   and pure** — every async I/O concern (skill staging, codex-home
//!   seeding, Claude settings, the remote-sandbox staging seam) is left
//!   to the caller, which feeds the resolved results into the input.
//!
//! R375 will lift the async I/O parts (skill runtime, claude settings,
//! remote staging) back into `build_runtime` once the corresponding
//! `SubprocessAcpRuntime` integration lands.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::codex_startup_config::{build_codex_startup_config, CodexStartupConfigInput};
use crate::constants::DEFAULT_ACP_ENGINE_TIMEOUT_SEC;
use crate::error::AcpxError;
use crate::hash::short_hash;
use crate::normalize::{
    normalize_agent, normalize_mode, normalize_non_interactive_permissions,
    normalize_permission_mode, normalize_requested_thinking_effort,
};
use crate::paths::default_state_dir;
use crate::prepared_runtime::{
    PreparedRuntime, PreparedRuntimeMode, PreparedRuntimeNonInteractivePermissions,
    PreparedRuntimePermissionMode, PreparedStagedRuntime, TimeoutResolution,
};

// ============================================================================
// Agent identity
// ============================================================================

/// The minimal agent identity used to build the Paperclip env vars
/// (`PAPERCLIP_AGENT_ID`, `PAPERCLIP_COMPANY_ID`). Mirrors the inline
/// `{ id, companyId }` shape consumed by Node `buildPaperclipEnv`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentIdentity {
    /// Agent id (e.g. `"claude"`, `"codex"`, `"gemini"`).
    pub id: String,
    /// Company id owning the agent.
    pub company_id: String,
}

impl AgentIdentity {
    pub fn new(id: impl Into<String>, company_id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            company_id: company_id.into(),
        }
    }
}

// ============================================================================
// Paperclip env helpers
// ============================================================================

/// Resolve the platform-default Paperclip API URL when no
/// `PAPERCLIP_RUNTIME_API_URL` / `PAPERCLIP_API_URL` env override is set.
/// Mirrors Node `buildPaperclipEnv`'s `runtimeHost` + `runtimePort` +
/// `apiUrl` derivation.
fn resolve_paperclip_api_url(env: &HashMap<String, String>) -> String {
    if let Some(url) = env
        .get("PAPERCLIP_RUNTIME_API_URL")
        .filter(|value| !value.is_empty())
    {
        return url.clone();
    }
    if let Some(url) = env
        .get("PAPERCLIP_API_URL")
        .filter(|value| !value.is_empty())
    {
        return url.clone();
    }
    let host_raw = env
        .get("PAPERCLIP_LISTEN_HOST")
        .filter(|value| !value.is_empty())
        .cloned()
        .or_else(|| env.get("HOST").filter(|value| !value.is_empty()).cloned())
        .unwrap_or_else(|| "localhost".to_string());
    let host = resolve_host_for_url(&host_raw);
    let port = env
        .get("PAPERCLIP_LISTEN_PORT")
        .filter(|value| !value.is_empty())
        .cloned()
        .or_else(|| env.get("PORT").filter(|value| !value.is_empty()).cloned())
        .unwrap_or_else(|| "3100".to_string());
    format!("http://{host}:{port}")
}

fn resolve_host_for_url(raw_host: &str) -> String {
    let host = raw_host.trim();
    if host.is_empty() || host == "0.0.0.0" || host == "::" {
        return "localhost".to_string();
    }
    if host.contains(':') && !host.starts_with('[') && !host.ends_with(']') {
        return format!("[{host}]");
    }
    host.to_string()
}

/// Build the Paperclip-managed env vars (`PAPERCLIP_AGENT_ID`,
/// `PAPERCLIP_COMPANY_ID`, `PAPERCLIP_API_URL`). Mirrors Node
/// `buildPaperclipEnv` from `server-utils.ts`.
pub fn build_paperclip_env(
    agent: &AgentIdentity,
    process_env: &HashMap<String, String>,
) -> BTreeMap<String, String> {
    let mut vars = BTreeMap::new();
    vars.insert("PAPERCLIP_AGENT_ID".to_string(), agent.id.clone());
    vars.insert("PAPERCLIP_COMPANY_ID".to_string(), agent.company_id.clone());
    vars.insert(
        "PAPERCLIP_API_URL".to_string(),
        resolve_paperclip_api_url(process_env),
    );
    vars
}

/// Apply the workspace + agent-home env vars onto an existing env map.
/// Mirrors Node `applyPaperclipWorkspaceEnv`. Empty / null inputs are
/// skipped, exactly like the Node loop.
#[allow(clippy::too_many_arguments)]
pub fn apply_paperclip_workspace_env(
    env: &mut BTreeMap<String, String>,
    workspace_cwd: &str,
    workspace_source: &str,
    workspace_strategy: &str,
    workspace_id: &str,
    workspace_repo_url: &str,
    workspace_repo_ref: &str,
    workspace_branch: &str,
    workspace_worktree_path: &str,
    agent_home: &str,
) {
    let mappings: [(&str, &str); 9] = [
        ("PAPERCLIP_WORKSPACE_CWD", workspace_cwd),
        ("PAPERCLIP_WORKSPACE_SOURCE", workspace_source),
        ("PAPERCLIP_WORKSPACE_STRATEGY", workspace_strategy),
        ("PAPERCLIP_WORKSPACE_ID", workspace_id),
        ("PAPERCLIP_WORKSPACE_REPO_URL", workspace_repo_url),
        ("PAPERCLIP_WORKSPACE_REPO_REF", workspace_repo_ref),
        ("PAPERCLIP_WORKSPACE_BRANCH", workspace_branch),
        ("PAPERCLIP_WORKSPACE_WORKTREE_PATH", workspace_worktree_path),
        ("AGENT_HOME", agent_home),
    ];
    for (key, value) in mappings {
        if !value.is_empty() {
            env.insert(key.to_string(), value.to_string());
        }
    }
}

// ============================================================================
// Wake / issue env extraction
// ============================================================================

/// Pull the per-wake Paperclip env vars out of the adapter context.
/// Mirrors the inline `wakeTaskId` / `wakeReason` / `wakeCommentId` /
/// `approvalId` / `approvalStatus` / `linkedIssueIds` extraction in
/// Node `buildRuntime`. Empty strings collapse to `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WakeContext {
    pub task_id: String,
    pub wake_reason: String,
    pub wake_comment_id: String,
    pub approval_id: String,
    pub approval_status: String,
    pub linked_issue_ids: Vec<String>,
}

impl WakeContext {
    pub fn from_context(context: &Value) -> Self {
        let task_id = first_non_empty_string(&[context.get("taskId"), context.get("issueId")]);
        let wake_reason = string_or_empty(context.get("wakeReason"));
        let wake_comment_id =
            first_non_empty_string(&[context.get("wakeCommentId"), context.get("commentId")]);
        let approval_id = string_or_empty(context.get("approvalId"));
        let approval_status = string_or_empty(context.get("approvalStatus"));
        let linked_issue_ids = match context.get("issueIds") {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(|value| value.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            _ => Vec::new(),
        };
        Self {
            task_id,
            wake_reason,
            wake_comment_id,
            approval_id,
            approval_status,
            linked_issue_ids,
        }
    }

    /// Project this wake context onto an env map. Mirrors the inline
    /// `if (wakeTaskId) env.PAPERCLIP_TASK_ID = ...` block in Node
    /// `buildRuntime`. Empty fields are skipped (matching the Node
    /// behavior, not collapsing to empty-string env vars).
    pub fn apply_to_env(&self, env: &mut BTreeMap<String, String>) {
        if !self.task_id.is_empty() {
            env.insert("PAPERCLIP_TASK_ID".to_string(), self.task_id.clone());
        }
        if !self.wake_reason.is_empty() {
            env.insert(
                "PAPERCLIP_WAKE_REASON".to_string(),
                self.wake_reason.clone(),
            );
        }
        if !self.wake_comment_id.is_empty() {
            env.insert(
                "PAPERCLIP_WAKE_COMMENT_ID".to_string(),
                self.wake_comment_id.clone(),
            );
        }
        if !self.approval_id.is_empty() {
            env.insert(
                "PAPERCLIP_APPROVAL_ID".to_string(),
                self.approval_id.clone(),
            );
        }
        if !self.approval_status.is_empty() {
            env.insert(
                "PAPERCLIP_APPROVAL_STATUS".to_string(),
                self.approval_status.clone(),
            );
        }
        if !self.linked_issue_ids.is_empty() {
            env.insert(
                "PAPERCLIP_LINKED_ISSUE_IDS".to_string(),
                self.linked_issue_ids.join(","),
            );
        }
    }
}

// ============================================================================
// Workspace hint extraction
// ============================================================================

/// Workspace hints (referenced + alternative workspaces). Mirrors the
/// `workspaceHints` array in Node `buildRuntime`. Each entry is the
/// raw JSON object — the executor passes them to the agent via
/// `PAPERCLIP_WORKSPACES_JSON`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceHints {
    pub entries: Vec<Value>,
}

impl WorkspaceHints {
    pub fn from_context(context: &Value) -> Self {
        let entries = match context.get("paperclipWorkspaces") {
            Some(Value::Array(items)) => items
                .iter()
                .filter(|value| value.is_object())
                .cloned()
                .collect(),
            _ => Vec::new(),
        };
        Self { entries }
    }
}

// ============================================================================
// Build runtime input
// ============================================================================

/// All inputs `build_runtime` consumes. The struct is intentionally
/// explicit (no I/O, no hidden state) so tests can drive every branch
/// deterministically.
///
/// Async I/O outputs the Node `buildRuntime` would compute inline
/// (skill prompt, codex-home seed, claude settings, MCP server
/// resolution) are **out of scope for R374**: the caller feeds the
/// already-resolved values into [`BuildRuntimeInput`] and we wire them
/// onto [`PreparedRuntime`] without re-running the I/O.
#[derive(Debug, Clone)]
pub struct BuildRuntimeInput {
    /// Adapter run id (e.g. `"run_<uuid>"`).
    pub run_id: String,
    /// Agent identity (id + company id).
    pub agent: AgentIdentity,
    /// Raw engine config (`agent`, `mode`, `model`, `cwd`, `stateDir`,
    /// `env`, `agentCommand`, `fastMode`, `timeoutSec`, ...).
    pub config: Value,
    /// Adapter execution context (`paperclipWorkspace`, wake/approval
    /// ids, `runtimeMcp`, ...).
    pub context: Value,
    /// Auth token (PAPERCLIP_API_KEY). `None` when the run is
    /// unauthenticated (e.g. local dev).
    pub auth_token: Option<String>,
    /// Effective working directory for the agent (already resolved by
    /// the caller: workspace > configured > process cwd).
    pub cwd: PathBuf,
    /// Override for the per-(company, agent) state directory. `None`
    /// falls back to `default_state_dir(company_id, agent_id)`.
    pub state_dir: Option<PathBuf>,
    /// Adapter module dir (skills source).
    pub module_dir: PathBuf,
    /// Paperclip package root dir (built-in agent binary lookup).
    pub package_root_dir: PathBuf,
    /// Adapter type (e.g. `"claude_local"`, `"codex_local"`).
    pub adapter_type: String,
    /// Whether the execution target is a remote sandbox.
    pub execution_target_is_remote: bool,
    /// Workspace identity (empty strings collapse to None / no-op).
    pub workspace_id: String,
    pub workspace_repo_url: String,
    pub workspace_repo_ref: String,
    pub workspace_branch: String,
    pub workspace_source: String,
    pub workspace_strategy: String,
    pub workspace_worktree_path: String,
    pub agent_home: String,
    /// Caller-resolved MCP server entries (already serialized into
    /// `AcpRuntime::ensure_session` form). `None` for runs with no MCP.
    pub mcp_servers: Vec<Value>,
    /// Host process env (used to resolve `PAPERCLIP_API_URL`,
    /// `PORT`, `HOST`, ...).
    pub process_env: HashMap<String, String>,
    /// Optional staged runtime (remote sandbox lane only). `None` for
    /// local / runner-less runs.
    pub staged_runtime: Option<PreparedStagedRuntime>,
    /// Whether the resolved MCP server list differs from `mcp_servers`
    /// (set when the caller wants the fingerprint to ignore MCP).
    pub ignore_mcp_in_fingerprint: bool,
}

impl BuildRuntimeInput {
    /// Build a minimal input for unit tests / happy path.
    pub fn for_test(agent_id: &str, company_id: &str, cwd: &Path) -> Self {
        Self {
            run_id: "run_test".to_string(),
            agent: AgentIdentity::new(agent_id, company_id),
            config: Value::Object(Default::default()),
            context: Value::Object(Default::default()),
            auth_token: None,
            cwd: cwd.to_path_buf(),
            state_dir: None,
            module_dir: PathBuf::from("/module"),
            package_root_dir: PathBuf::from("/pkg"),
            adapter_type: format!("{agent_id}_local"),
            execution_target_is_remote: false,
            workspace_id: String::new(),
            workspace_repo_url: String::new(),
            workspace_repo_ref: String::new(),
            workspace_branch: String::new(),
            workspace_source: String::new(),
            workspace_strategy: String::new(),
            workspace_worktree_path: String::new(),
            agent_home: String::new(),
            mcp_servers: Vec::new(),
            process_env: HashMap::new(),
            staged_runtime: None,
            ignore_mcp_in_fingerprint: false,
        }
    }
}

// ============================================================================
// build_runtime (top-level assembly)
// ============================================================================

/// Top-level assembly. Pure, synchronous, and side-effect-free: every
/// I/O concern (skill staging, codex-home seeding, claude settings,
/// remote-sandbox staging) is left to the caller, which feeds the
/// resolved values into [`BuildRuntimeInput`].
///
/// Returns a [`PreparedRuntime`] that downstream helpers (the
/// `AcpRuntime::ensure_session` driver, the cache layer, the executor)
/// consume.
pub fn build_runtime(input: &BuildRuntimeInput) -> Result<PreparedRuntime, AcpxError> {
    let acpx_agent = normalize_agent(&input.config);
    let mode = match normalize_mode(&input.config) {
        crate::normalize::NormalizedMode::OneShot => PreparedRuntimeMode::OneShot,
        crate::normalize::NormalizedMode::Persistent => PreparedRuntimeMode::Persistent,
    };
    let permission_mode = match normalize_permission_mode(&input.config) {
        crate::normalize::NormalizedPermissionMode::ApproveAll => {
            PreparedRuntimePermissionMode::ApproveAll
        }
        crate::normalize::NormalizedPermissionMode::ApproveReads => {
            PreparedRuntimePermissionMode::ApproveReads
        }
        crate::normalize::NormalizedPermissionMode::DenyAll => {
            PreparedRuntimePermissionMode::DenyAll
        }
    };
    let non_interactive_permissions = match normalize_non_interactive_permissions(&input.config) {
        crate::normalize::NormalizedNonInteractivePermissions::Deny => {
            PreparedRuntimeNonInteractivePermissions::Deny
        }
        crate::normalize::NormalizedNonInteractivePermissions::Fail => {
            PreparedRuntimeNonInteractivePermissions::Fail
        }
    };
    let requested_thinking_effort =
        normalize_requested_thinking_effort(&input.config).unwrap_or_default();
    let requested_model = string_or_empty(input.config.get("model"))
        .trim()
        .to_string();
    let fast_mode = acpx_agent == "codex"
        && input
            .config
            .get("fastMode")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

    // ---- Workspace + env construction ----
    let mut env = build_paperclip_env(&input.agent, &input.process_env);
    env.insert("PAPERCLIP_RUN_ID".to_string(), input.run_id.clone());
    WakeContext::from_context(&input.context).apply_to_env(&mut env);
    apply_paperclip_workspace_env(
        &mut env,
        &input.cwd.to_string_lossy(),
        &input.workspace_source,
        &input.workspace_strategy,
        &input.workspace_id,
        &input.workspace_repo_url,
        &input.workspace_repo_ref,
        &input.workspace_branch,
        &input.workspace_worktree_path,
        &input.agent_home,
    );

    // ---- Auth token ----
    if let Some(token) = input.auth_token.as_ref().filter(|t| !t.is_empty()) {
        env.insert("PAPERCLIP_API_KEY".to_string(), token.clone());
    }

    // ---- Codex startup config ----
    if acpx_agent == "codex" {
        let startup = build_codex_startup_config(CodexStartupConfigInput {
            existing_config: if input.config.get("CODEX_CONFIG").is_some() {
                Some(
                    string_or_empty(input.config.get("CODEX_CONFIG"))
                        .trim()
                        .to_string(),
                )
            } else {
                None
            },
            requested_model: requested_model.clone(),
            requested_thinking_effort: requested_thinking_effort.clone(),
            fast_mode,
        });
        if let Some(value) = startup.value {
            env.insert("CODEX_CONFIG".to_string(), value);
        }
    }

    // ---- Claude ANTHROPIC_MODEL pre-set ----
    if !requested_model.is_empty() && acpx_agent == "claude" {
        env.entry("ANTHROPIC_MODEL".to_string())
            .or_insert(requested_model.clone());
    }

    // ---- State dir ----
    let state_dir = input
        .state_dir
        .clone()
        .unwrap_or_else(|| default_state_dir(&input.agent.company_id, &input.agent.id));

    // ---- Timeout resolution ----
    let requested_timeout = input
        .config
        .get("timeoutSec")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_ACP_ENGINE_TIMEOUT_SEC);
    let timeout_resolution = if input.execution_target_is_remote {
        TimeoutResolution {
            timeout_sec: requested_timeout,
            source: "adapterConfig".to_string(),
            note: Some("(sandbox lane; set adapterConfig.timeoutSec to override)".to_string()),
        }
    } else if requested_timeout == DEFAULT_ACP_ENGINE_TIMEOUT_SEC {
        TimeoutResolution {
            timeout_sec: requested_timeout,
            source: "default".to_string(),
            note: None,
        }
    } else {
        TimeoutResolution {
            timeout_sec: requested_timeout,
            source: "adapterConfig".to_string(),
            note: None,
        }
    };

    // ---- Fingerprint ----
    let mcp_fingerprint = if input.ignore_mcp_in_fingerprint {
        Vec::new()
    } else {
        input.mcp_servers.clone()
    };
    let fingerprint_input = serde_json::json!({
        "acpxAgent": acpx_agent,
        "cwd": input.cwd.to_string_lossy(),
        "mode": mode.as_str(),
        "permissionMode": permission_mode.as_str(),
        "nonInteractivePermissions": non_interactive_permissions.as_str(),
        "requestedModel": requested_model,
        "requestedThinkingEffort": requested_thinking_effort,
        "fastMode": fast_mode,
        "executionTargetIsRemote": input.execution_target_is_remote,
        "mcpServers": mcp_fingerprint,
    });
    let fingerprint = short_hash(&fingerprint_input);

    // ---- Session key ----
    let task_key = string_or_empty(input.context.get("taskKey"))
        .trim()
        .to_string();
    let task_key = if task_key.is_empty() {
        string_or_empty(input.context.get("taskId"))
            .trim()
            .to_string()
    } else {
        task_key
    };
    let task_key = if task_key.is_empty() {
        input.workspace_id.clone()
    } else {
        task_key
    };
    let task_key = if task_key.is_empty() {
        "default".to_string()
    } else {
        task_key
    };
    let session_key = format!(
        "paperclip:{}:{}:{}:{}",
        input.agent.company_id, input.agent.id, task_key, fingerprint
    );

    // ---- logged env (secrets redacted in callers; for now we mirror env) ----
    let logged_env = env.clone();

    // ---- Build ----
    let mut builder = PreparedRuntime::builder(&acpx_agent)
        .mode(mode)
        .cwd(input.cwd.clone())
        .workspace_id(input.workspace_id.clone())
        .workspace_repo_url(input.workspace_repo_url.clone())
        .workspace_repo_ref(input.workspace_repo_ref.clone())
        .env(env)
        .logged_env(logged_env)
        .state_dir(state_dir)
        .permission_mode(permission_mode)
        .non_interactive_permissions(non_interactive_permissions)
        .requested_model(requested_model)
        .requested_thinking_effort(requested_thinking_effort)
        .fast_mode(fast_mode)
        .timeout_sec(timeout_resolution.timeout_sec)
        .timeout_resolution(timeout_resolution)
        .session_key(session_key)
        .fingerprint(fingerprint);
    if let Some(staged) = &input.staged_runtime {
        builder = builder.staged_runtime(staged.clone());
    }
    Ok(builder.build())
}

// ============================================================================
// Helpers
// ============================================================================

fn string_or_empty(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

fn first_non_empty_string(sources: &[Option<&Value>]) -> String {
    for source in sources {
        if let Some(Value::String(s)) = source {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    String::new()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn agent() -> AgentIdentity {
        AgentIdentity::new("claude", "company_1")
    }

    #[test]
    fn build_paperclip_env_sets_identity_and_default_api_url() {
        let mut process_env = HashMap::new();
        let env = build_paperclip_env(&agent(), &process_env);
        assert_eq!(env.get("PAPERCLIP_AGENT_ID"), Some(&"claude".to_string()));
        assert_eq!(
            env.get("PAPERCLIP_COMPANY_ID"),
            Some(&"company_1".to_string())
        );
        assert_eq!(
            env.get("PAPERCLIP_API_URL"),
            Some(&"http://localhost:3100".to_string())
        );
        // No PAPERCLIP_LISTEN_PORT in env → defaults to 3100.
        process_env.insert("PAPERCLIP_LISTEN_PORT".into(), "9999".into());
        let env = build_paperclip_env(&agent(), &process_env);
        assert_eq!(
            env.get("PAPERCLIP_API_URL"),
            Some(&"http://localhost:9999".to_string())
        );
    }

    #[test]
    fn build_paperclip_env_respects_explicit_api_url_override() {
        let mut process_env = HashMap::new();
        process_env.insert(
            "PAPERCLIP_RUNTIME_API_URL".into(),
            "https://paperclip.example.com".into(),
        );
        let env = build_paperclip_env(&agent(), &process_env);
        assert_eq!(
            env.get("PAPERCLIP_API_URL"),
            Some(&"https://paperclip.example.com".to_string())
        );
    }

    #[test]
    fn resolve_host_for_url_ipv6_brackets() {
        assert_eq!(resolve_host_for_url("::"), "localhost");
        assert_eq!(resolve_host_for_url("0.0.0.0"), "localhost");
        assert_eq!(resolve_host_for_url(""), "localhost");
        assert_eq!(resolve_host_for_url("localhost"), "localhost");
        assert_eq!(resolve_host_for_url("fe80::1"), "[fe80::1]");
        assert_eq!(resolve_host_for_url("[fe80::1]"), "[fe80::1]");
    }

    #[test]
    fn apply_paperclip_workspace_env_skips_empty_fields() {
        let mut env = BTreeMap::new();
        apply_paperclip_workspace_env(
            &mut env,
            "/repo",
            "realized",
            "worktree",
            "ws_1",
            "git@github.com:foo/bar.git",
            "refs/heads/main",
            "main",
            "/worktree",
            "/home",
        );
        assert_eq!(
            env.get("PAPERCLIP_WORKSPACE_CWD"),
            Some(&"/repo".to_string())
        );
        assert_eq!(env.get("PAPERCLIP_WORKSPACE_ID"), Some(&"ws_1".to_string()));
        assert_eq!(env.get("AGENT_HOME"), Some(&"/home".to_string()));

        let mut env2 = BTreeMap::new();
        apply_paperclip_workspace_env(&mut env2, "", "", "", "", "", "", "", "", "");
        assert!(env2.is_empty());
    }

    #[test]
    fn wake_context_prefers_task_id_over_issue_id() {
        let ctx = serde_json::json!({
            "issueId": "issue_1",
            "taskId": "task_42",
        });
        let wake = WakeContext::from_context(&ctx);
        assert_eq!(wake.task_id, "task_42");
    }

    #[test]
    fn wake_context_falls_back_to_issue_id() {
        let ctx = serde_json::json!({ "issueId": "  issue_3  " });
        let wake = WakeContext::from_context(&ctx);
        assert_eq!(wake.task_id, "issue_3");
    }

    #[test]
    fn wake_context_apply_skips_empty_fields() {
        let mut env = BTreeMap::new();
        WakeContext {
            task_id: "task_1".into(),
            wake_reason: "heartbeat".into(),
            wake_comment_id: String::new(),
            approval_id: String::new(),
            approval_status: "approved".into(),
            linked_issue_ids: vec!["i1".into(), "i2".into()],
        }
        .apply_to_env(&mut env);
        assert_eq!(env.get("PAPERCLIP_TASK_ID"), Some(&"task_1".to_string()));
        assert_eq!(
            env.get("PAPERCLIP_WAKE_REASON"),
            Some(&"heartbeat".to_string())
        );
        assert_eq!(
            env.get("PAPERCLIP_APPROVAL_STATUS"),
            Some(&"approved".to_string())
        );
        assert_eq!(
            env.get("PAPERCLIP_LINKED_ISSUE_IDS"),
            Some(&"i1,i2".to_string())
        );
        assert!(env.get("PAPERCLIP_WAKE_COMMENT_ID").is_none());
        assert!(env.get("PAPERCLIP_APPROVAL_ID").is_none());
    }

    #[test]
    fn workspace_hints_filters_non_object_entries() {
        let ctx = serde_json::json!({
            "paperclipWorkspaces": [
                { "projectId": "p1" },
                "not_an_object",
                { "projectId": "p2" },
            ],
        });
        let hints = WorkspaceHints::from_context(&ctx);
        assert_eq!(hints.entries.len(), 2);
    }

    #[test]
    fn build_runtime_assembles_minimal_claude_input() {
        let input = BuildRuntimeInput::for_test("claude", "co_1", Path::new("/repo"));
        let runtime = build_runtime(&input).expect("claude runtime");
        assert_eq!(runtime.acpx_agent, "claude");
        assert_eq!(runtime.cwd, PathBuf::from("/repo"));
        assert_eq!(runtime.mode, PreparedRuntimeMode::Persistent);
        assert_eq!(
            runtime.permission_mode,
            PreparedRuntimePermissionMode::ApproveAll
        );
        assert_eq!(
            runtime.non_interactive_permissions,
            PreparedRuntimeNonInteractivePermissions::Deny
        );
        assert!(runtime.fast_mode == false);
        assert_eq!(runtime.requested_model, "");
        assert!(runtime.session_key.starts_with("paperclip:co_1:claude:"));
        assert!(!runtime.fingerprint.is_empty());
        assert!(runtime.staged_runtime.is_none());
        assert!(runtime.remote_staging_env_delta.is_none());
    }

    #[test]
    fn build_runtime_honors_oneshot_and_deny_all() {
        let mut input = BuildRuntimeInput::for_test("codex", "co_2", Path::new("/repo"));
        input.config = serde_json::json!({
            "agent": "codex",
            "mode": "oneshot",
            "permissionMode": "deny-all",
            "nonInteractivePermissions": "fail",
            "model": "gpt-5",
        });
        let runtime = build_runtime(&input).expect("codex runtime");
        assert_eq!(runtime.acpx_agent, "codex");
        assert_eq!(runtime.mode, PreparedRuntimeMode::OneShot);
        assert_eq!(
            runtime.permission_mode,
            PreparedRuntimePermissionMode::DenyAll
        );
        assert_eq!(
            runtime.non_interactive_permissions,
            PreparedRuntimeNonInteractivePermissions::Fail
        );
        assert_eq!(runtime.requested_model, "gpt-5");
        assert!(runtime
            .env
            .get("CODEX_CONFIG")
            .map(|s| !s.is_empty())
            .unwrap_or(false));
    }

    #[test]
    fn build_runtime_applies_wake_context_and_auth_token() {
        let mut input = BuildRuntimeInput::for_test("claude", "co_3", Path::new("/repo"));
        input.context = serde_json::json!({
            "taskId": "task_7",
            "wakeReason": "approval",
            "approvalId": "apr_1",
            "approvalStatus": "approved",
        });
        input.auth_token = Some("token_xyz".into());
        let runtime = build_runtime(&input).expect("runtime");
        assert_eq!(
            runtime.env.get("PAPERCLIP_TASK_ID"),
            Some(&"task_7".to_string())
        );
        assert_eq!(
            runtime.env.get("PAPERCLIP_WAKE_REASON"),
            Some(&"approval".to_string())
        );
        assert_eq!(
            runtime.env.get("PAPERCLIP_APPROVAL_ID"),
            Some(&"apr_1".to_string())
        );
        assert_eq!(
            runtime.env.get("PAPERCLIP_APPROVAL_STATUS"),
            Some(&"approved".to_string())
        );
        assert_eq!(
            runtime.env.get("PAPERCLIP_API_KEY"),
            Some(&"token_xyz".to_string())
        );
    }

    #[test]
    fn build_runtime_skips_auth_token_when_empty() {
        let mut input = BuildRuntimeInput::for_test("claude", "co_3", Path::new("/repo"));
        input.auth_token = Some(String::new());
        let runtime = build_runtime(&input).expect("runtime");
        assert!(runtime.env.get("PAPERCLIP_API_KEY").is_none());
    }

    #[test]
    fn build_runtime_prefers_anthropic_model_for_claude() {
        let mut input = BuildRuntimeInput::for_test("claude", "co_1", Path::new("/repo"));
        input.config = serde_json::json!({ "model": "claude-opus-4-7" });
        let runtime = build_runtime(&input).expect("runtime");
        assert_eq!(
            runtime.env.get("ANTHROPIC_MODEL"),
            Some(&"claude-opus-4-7".to_string())
        );
    }

    #[test]
    fn build_runtime_does_not_set_anthropic_model_for_other_agents() {
        let mut input = BuildRuntimeInput::for_test("codex", "co_1", Path::new("/repo"));
        input.config = serde_json::json!({ "agent": "codex", "model": "gpt-5" });
        let runtime = build_runtime(&input).expect("runtime");
        assert!(runtime.env.get("ANTHROPIC_MODEL").is_none());
    }

    #[test]
    fn build_runtime_resolves_state_dir_default() {
        let input = BuildRuntimeInput::for_test("claude", "co_x", Path::new("/repo"));
        let runtime = build_runtime(&input).expect("runtime");
        let default = default_state_dir("co_x", "claude");
        assert_eq!(runtime.state_dir, default);
    }

    #[test]
    fn build_runtime_honors_state_dir_override() {
        let mut input = BuildRuntimeInput::for_test("claude", "co_x", Path::new("/repo"));
        input.state_dir = Some(PathBuf::from("/custom/state"));
        let runtime = build_runtime(&input).expect("runtime");
        assert_eq!(runtime.state_dir, PathBuf::from("/custom/state"));
    }

    #[test]
    fn build_runtime_records_remote_target_in_timeout_resolution() {
        let mut input = BuildRuntimeInput::for_test("claude", "co_x", Path::new("/repo"));
        input.execution_target_is_remote = true;
        let runtime = build_runtime(&input).expect("runtime");
        assert!(runtime
            .timeout_resolution
            .note
            .as_deref()
            .unwrap_or("")
            .contains("sandbox"));
    }

    #[test]
    fn build_runtime_records_staged_runtime() {
        let mut input = BuildRuntimeInput::for_test("claude", "co_x", Path::new("/repo"));
        input.staged_runtime = Some(PreparedStagedRuntime::remote(
            "/host/repo",
            "/sandbox/workspace",
        ));
        let runtime = build_runtime(&input).expect("runtime");
        assert_eq!(
            runtime
                .staged_runtime
                .as_ref()
                .map(|s| s.workspace_local_dir.clone()),
            Some(PathBuf::from("/host/repo"))
        );
        assert_eq!(
            runtime
                .staged_runtime
                .as_ref()
                .and_then(|s| s.workspace_remote_dir.clone()),
            Some(PathBuf::from("/sandbox/workspace"))
        );
    }

    #[test]
    fn build_runtime_session_key_changes_when_fingerprint_changes() {
        let mut input_a = BuildRuntimeInput::for_test("claude", "co_x", Path::new("/repo/a"));
        let mut input_b = BuildRuntimeInput::for_test("claude", "co_x", Path::new("/repo/b"));
        input_a.context = serde_json::json!({ "taskKey": "task_1" });
        input_b.context = serde_json::json!({ "taskKey": "task_1" });
        let runtime_a = build_runtime(&input_a).expect("runtime a");
        let runtime_b = build_runtime(&input_b).expect("runtime b");
        assert_ne!(runtime_a.session_key, runtime_b.session_key);
    }

    #[test]
    fn build_runtime_session_key_uses_default_when_no_task_or_workspace() {
        let input = BuildRuntimeInput::for_test("claude", "co_x", Path::new("/repo"));
        let runtime = build_runtime(&input).expect("runtime");
        // session_key shape: "paperclip:<company>:<agent>:<task_key>:<fingerprint>"
        let segments: Vec<&str> = runtime.session_key.split(':').collect();
        assert_eq!(segments[0], "paperclip");
        assert_eq!(segments[1], "co_x");
        assert_eq!(segments[2], "claude");
        assert_eq!(segments[3], "default"); // taskKey fallback chain
    }

    #[test]
    fn build_runtime_session_key_uses_workspace_id_when_no_task() {
        let mut input = BuildRuntimeInput::for_test("claude", "co_x", Path::new("/repo"));
        input.workspace_id = "ws_42".into();
        let runtime = build_runtime(&input).expect("runtime");
        let segments: Vec<&str> = runtime.session_key.split(':').collect();
        assert_eq!(segments[3], "ws_42");
    }

    #[test]
    fn build_runtime_session_key_prefers_task_key() {
        let mut input = BuildRuntimeInput::for_test("claude", "co_x", Path::new("/repo"));
        input.workspace_id = "ws_42".into();
        input.context = serde_json::json!({ "taskKey": "task_priority" });
        let runtime = build_runtime(&input).expect("runtime");
        let segments: Vec<&str> = runtime.session_key.split(':').collect();
        assert_eq!(segments[3], "task_priority");
    }

    #[test]
    fn build_runtime_ignores_mcp_in_fingerprint_when_requested() {
        let mut input = BuildRuntimeInput::for_test("claude", "co_x", Path::new("/repo"));
        let mcp = vec![serde_json::json!({ "name": "x", "url": "http://x" })];
        input.mcp_servers = mcp.clone();
        input.ignore_mcp_in_fingerprint = true;
        let runtime_without = build_runtime(&input).expect("runtime without");

        let mut input2 = input.clone();
        input2.ignore_mcp_in_fingerprint = false;
        let runtime_with = build_runtime(&input2).expect("runtime with");

        // Fingerprint is different because MCP was included vs. excluded.
        assert_ne!(runtime_without.fingerprint, runtime_with.fingerprint);
    }

    #[test]
    fn build_runtime_records_codex_fast_mode() {
        let mut input = BuildRuntimeInput::for_test("codex", "co_x", Path::new("/repo"));
        input.config = serde_json::json!({ "agent": "codex", "fastMode": true });
        let runtime = build_runtime(&input).expect("runtime");
        assert!(runtime.fast_mode);
    }

    #[test]
    fn build_runtime_does_not_set_fast_mode_for_non_codex() {
        let mut input = BuildRuntimeInput::for_test("claude", "co_x", Path::new("/repo"));
        input.config = serde_json::json!({ "agent": "claude", "fastMode": true });
        let runtime = build_runtime(&input).expect("runtime");
        assert!(!runtime.fast_mode);
    }

    #[test]
    fn build_runtime_applies_workspace_env_block() {
        let mut input = BuildRuntimeInput::for_test("claude", "co_x", Path::new("/repo"));
        input.workspace_id = "ws_1".into();
        input.workspace_repo_url = "git@github.com:foo/bar.git".into();
        input.workspace_branch = "main".into();
        input.agent_home = "/home/agent".into();
        let runtime = build_runtime(&input).expect("runtime");
        assert_eq!(
            runtime.env.get("PAPERCLIP_WORKSPACE_ID"),
            Some(&"ws_1".to_string())
        );
        assert_eq!(
            runtime.env.get("PAPERCLIP_WORKSPACE_REPO_URL"),
            Some(&"git@github.com:foo/bar.git".to_string())
        );
        assert_eq!(
            runtime.env.get("PAPERCLIP_WORKSPACE_BRANCH"),
            Some(&"main".to_string())
        );
        assert_eq!(
            runtime.env.get("AGENT_HOME"),
            Some(&"/home/agent".to_string())
        );
    }
}
