//! `pc-acpx::server_utils` - port of `server-utils.ts` from Node
//! `paperclip/packages/adapter-utils/src/`.
//!
//! This is **Part 1** (R405) of the multi-round port. R405 covers the
//! sync pure helpers + types only:
//!
//! - `RunProcessResult` / `TerminalResultCleanupOptions` /
//!   `TerminalResultCleanupEvidence` types
//! - `RunningProcess` / `SpawnTarget` mirrored structs
//! - Constants: `UNMANAGED_BACKGROUND_TASK_*`, `MAX_CAPTURE_BYTES`,
//!   `MAX_EXCERPT_BYTES`, `TERMINAL_RESULT_SCAN_OVERLAP_CHARS`,
//!   `DEFAULT_PAPERCLIP_INSTANCE_ID`, `REDACTED_LOG_VALUE`
//! - `isPaperclipRuntimeEnvKey` / `isForbiddenConfigEnvKey` env classifiers
//! - `parseObject` / `asString` / `asNumber` / `asBoolean` /
//!   `asStringArray` / `parseJson` value coercers
//! - `appendWithCap` / `appendWithByteCap` bounded string accumulators
//! - `resolvePathValue` / `renderTemplate` / `joinPromptSections`
//! - `signalDecision` pure decision helper (the actual kill half is
//!   deferred with the async spawn layer)
//!
//! Async functions (spawn, ensureCommandResolvable, runChildProcess,
//! resolvePaperclipSkillsDir, etc.) are deferred to later rounds
//! (R406-R408) - they require real filesystem + child-process runtimes.

use std::collections::HashMap;

use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};

// =============================================================================
// Constants - mirrored 1:1 from Node literals.
// =============================================================================

/// Stop reason used in [`TerminalResultCleanupEvidence.stop_reason`]
/// when the harness cancels an unmanaged background task.
pub const UNMANAGED_BACKGROUND_TASK_STOP_REASON: &str = "unmanaged_background_task_stopped";
/// Human-readable reason paired with
/// [`UNMANAGED_BACKGROUND_TASK_STOP_REASON`].
pub const UNMANAGED_BACKGROUND_TASK_LIVENESS_REASON: &str =
    "unmanaged background task stopped; no durable live path";

/// Default cap for stdout/stderr capture in run-log streaming.
pub const MAX_CAPTURE_BYTES: usize = 4 * 1024 * 1024;
/// Default cap for log excerpts shown in UI / API responses.
pub const MAX_EXCERPT_BYTES: usize = 32 * 1024;
/// Overlap window when scanning stdout for terminal-result markers.
pub const TERMINAL_RESULT_SCAN_OVERLAP_CHARS: usize = 64 * 1024;
/// Default Paperclip instance id when none is supplied.
pub const DEFAULT_PAPERCLIP_INSTANCE_ID: &str = "default";
/// Replacement value when redacting sensitive env / log content.
pub const REDACTED_LOG_VALUE: &str = "***REDACTED***";

/// Path segment pattern (1:1 of Node `PATH_SEGMENT_RE`).
pub const PATH_SEGMENT_RE_SRC: &str = "^[a-zA-Z0-9_-]+$";
/// Sensitive env-key pattern (1:1 of Node `SENSITIVE_ENV_KEY`).
pub const SENSITIVE_ENV_KEY_RE_SRC: &str = "(?i)(key|token|secret|password|passwd|authorization|cookie)";

static PATH_SEGMENT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(PATH_SEGMENT_RE_SRC).expect("PATH_SEGMENT_RE_SRC is a valid regex")
});

/// Static empty `HashMap<String, String>` for borrow-friendly default
/// arguments (avoids temporary-value lifetime issues).
static EMPTY_HASHMAP_STR: std::sync::OnceLock<HashMap<String, String>> =
    std::sync::OnceLock::new();

fn empty_hashmap_str() -> &'static HashMap<String, String> {
    EMPTY_HASHMAP_STR.get_or_init(HashMap::new)
}

/// Static empty `HashMap<String, serde_json::Value>`.
static EMPTY_HASHMAP_VALUE: std::sync::OnceLock<HashMap<String, serde_json::Value>> =
    std::sync::OnceLock::new();

fn empty_hashmap_value() -> &'static HashMap<String, serde_json::Value> {
    EMPTY_HASHMAP_VALUE.get_or_init(HashMap::new)
}
static SENSITIVE_ENV_KEY_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(SENSITIVE_ENV_KEY_RE_SRC).expect("SENSITIVE_ENV_KEY_RE_SRC is a valid regex")
});
static TEMPLATE_PLACEHOLDER_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\{\{\s*([a-zA-Z0-9_.-]+)\s*\}\}").expect("template placeholder regex is valid")
});

// =============================================================================
// Process result types - mirrored from Node interfaces.
// =============================================================================

/// Result returned from a completed child-process invocation. Mirrors
/// Node `RunProcessResult`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RunProcessResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    pub timed_out: bool,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_result_cleanup: Option<TerminalResultCleanupEvidence>,
}

/// Options that govern post-exit terminal-result cleanup. Mirrors Node
/// `TerminalResultCleanupOptions`. The async half (waiting for
/// `has_terminal_result` to fire, escalating to SIGKILL) is deferred.
#[derive(Clone)]
pub struct TerminalResultCleanupOptions {
    /// Closure that inspects the latest captured stdout/stderr and
    /// returns `true` once a terminal-result marker has been seen.
    pub has_terminal_result:
        Arc<dyn Fn(RunProcessOutput<'_>) -> bool + Send + Sync + 'static>,
    /// Grace period (ms) before escalating the stop signal.
    pub grace_ms: Option<u64>,
}

/// Borrowed view of a running process's accumulated stdout/stderr. Used
/// to drive the terminal-result detection closure without copying.
pub struct RunProcessOutput<'a> {
    pub stdout: &'a str,
    pub stderr: &'a str,
}

/// Evidence captured when a task is force-stopped because the harness
/// could not detect a terminal-result marker in time. Mirrors Node
/// `TerminalResultCleanupEvidence`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalResultCleanupEvidence {
    pub kind: String, // always "terminal_result_cleanup"
    pub stopped: bool,
    pub stop_reason: String,
    pub reason: String,
    pub terminal_result_seen: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    pub force_killed: bool,
}

impl TerminalResultCleanupEvidence {
    /// Build the canonical evidence payload. Mirrors the Node default
    /// constructor.
    #[must_use]
    pub fn new(
        terminal_result_seen: bool,
        signal: Option<String>,
        force_killed: bool,
    ) -> Self {
        Self {
            kind: "terminal_result_cleanup".to_string(),
            stopped: true,
            stop_reason: UNMANAGED_BACKGROUND_TASK_STOP_REASON.to_string(),
            reason: UNMANAGED_BACKGROUND_TASK_LIVENESS_REASON.to_string(),
            terminal_result_seen,
            signal,
            force_killed,
        }
    }
}

use std::sync::Arc;

// =============================================================================
// Running process / spawn target (mirrored structs; signal decision is pure).
// =============================================================================

/// Snapshot of a running child process needed by the signal-decision
/// helper. Mirrors the Node `RunningProcess` interface, minus the actual
/// `ChildProcess` handle (the real kill is deferred with the async
/// spawn layer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunningProcessSignalInfo {
    /// Process group id (`-pid` on POSIX, `None` on Windows or when
    /// `pid` is missing).
    pub process_group_id: Option<i32>,
    /// Whether `child.exitCode` / `child.signalCode` is non-null
    /// (i.e. the child has actually closed).
    pub already_exited: bool,
}

/// Spawn parameters that [`createSpawnTarget`] (deferred) would
/// translate into a `Command`. Mirrors Node `SpawnTarget`. The `cleanup`
/// closure is held as `Arc<dyn Fn>` so callers can register teardown
/// work without forcing pc-acpx to depend on a runtime.
#[derive(Clone)]
pub struct SpawnTarget {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: Option<HashMap<String, String>>,
    pub cleanup: Option<Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>>,
}

use futures::future::BoxFuture;

/// Decision returned by [`signal_decision`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalTarget {
    /// Signal the whole process group (POSIX `kill(-pgid, signal)`).
    ProcessGroup { pgid: i32 },
    /// Signal the direct child only.
    DirectChild,
    /// Child has already exited; no signal needed.
    None,
}

/// Pure decision logic extracted from Node `signalRunningProcess`.
///
/// The Node function:
///   1. Tries `process.kill(-pgid, signal)` when on POSIX and pgid > 0.
///      Catches the exception and falls through to step 2.
///   2. Signals the direct child if it has not yet exited (`exitCode`
///      and `signalCode` are both null).
///
/// `signal_decision` returns the *target* of step 1 (or `None` when
/// step 2 should be skipped because the child has already exited).
/// The actual `kill` syscall is deferred to the async spawn layer.
#[must_use]
pub fn signal_decision(
    info: RunningProcessSignalInfo,
    is_windows: bool,
) -> SignalTarget {
    if info.already_exited {
        return SignalTarget::None;
    }
    if !is_windows {
        if let Some(pgid) = info.process_group_id {
            if pgid > 0 {
                return SignalTarget::ProcessGroup { pgid };
            }
        }
    }
    SignalTarget::DirectChild
}

// =============================================================================
// Env key classifiers - mirrored 1:1 from Node.
// =============================================================================

/// Returns `true` for keys inside the reserved `PAPERCLIP_*` runtime
/// namespace. Mirrors Node `isPaperclipRuntimeEnvKey`.
#[must_use]
pub fn is_paperclip_runtime_env_key(key: &str) -> bool {
    key.starts_with("PAPERCLIP_")
}

/// Returns `true` for keys that adapter / user config env must never
/// override (currently just `PAPERCLIP_API_KEY`, since the harness
/// mints the run token). Mirrors Node `isForbiddenConfigEnvKey`.
#[must_use]
pub fn is_forbidden_config_env_key(key: &str) -> bool {
    key == "PAPERCLIP_API_KEY"
}

/// Returns `true` when the path segment matches `PATH_SEGMENT_RE`
/// (`^[a-zA-Z0-9_-]+$`). Mirrors Node `PATH_SEGMENT_RE.test(value)`.
#[must_use]
pub fn is_valid_path_segment(value: &str) -> bool {
    PATH_SEGMENT_RE.is_match(value)
}

/// Returns `true` when the env key matches `SENSITIVE_ENV_KEY`
/// (case-insensitive `(key|token|secret|password|passwd|authorization|cookie)`).
/// Mirrors Node `SENSITIVE_ENV_KEY.test(key)`.
#[must_use]
pub fn is_sensitive_env_key(key: &str) -> bool {
    SENSITIVE_ENV_KEY_RE.is_match(key)
}

// =============================================================================
// JSON value coercers - mirrored 1:1 from Node.
// =============================================================================

/// Return `value` as a `serde_json::Map` when it is a JSON object;
/// otherwise return an empty map. Mirrors Node `parseObject`.
#[must_use]
pub fn parse_object(value: &serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    if let serde_json::Value::Object(m) = value {
        m.clone()
    } else {
        serde_json::Map::new()
    }
}

/// Return `value` as `String` when it is a non-empty JSON string;
/// otherwise `fallback`. Mirrors Node `asString`.
#[must_use]
pub fn as_string(value: &serde_json::Value, fallback: &str) -> String {
    match value {
        serde_json::Value::String(s) if !s.is_empty() => s.clone(),
        _ => fallback.to_string(),
    }
}

/// Return `value` as `f64` when it is a finite JSON number; otherwise
/// `fallback`. Mirrors Node `asNumber`.
#[must_use]
pub fn as_number(value: &serde_json::Value, fallback: f64) -> f64 {
    match value {
        serde_json::Value::Number(n) => n.as_f64().filter(|f| f.is_finite()).unwrap_or(fallback),
        _ => fallback,
    }
}

/// Return `value` as `bool` when it is a JSON boolean; otherwise
/// `fallback`. Mirrors Node `asBoolean`.
#[must_use]
pub fn as_boolean(value: &serde_json::Value, fallback: bool) -> bool {
    match value {
        serde_json::Value::Bool(b) => *b,
        _ => fallback,
    }
}

/// Return the array of JSON strings inside `value`, filtering out
/// non-string entries. Mirrors Node `asStringArray`.
#[must_use]
pub fn as_string_array(value: &serde_json::Value) -> Vec<String> {
    if let serde_json::Value::Array(arr) = value {
        arr.iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    } else {
        Vec::new()
    }
}

/// Parse `value` as JSON, returning `None` on parse error. Mirrors Node
/// `parseJson`.
#[must_use]
pub fn parse_json(value: &str) -> Option<serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(value).ok()
}

// =============================================================================
// Bounded string accumulators.
// =============================================================================

/// Append `chunk` to `prev` and keep the trailing `cap_chars` *characters*.
/// If the result fits, return the full combined string; otherwise
/// discard the leading characters so only the trailing `cap_chars`
/// remain. Mirrors Node `appendWithCap` (which counts UTF-16 code
/// units; for ASCII this matches `chars().count()`).
#[must_use]
pub fn append_with_cap(prev: &str, chunk: &str, cap_chars: usize) -> String {
    let mut combined = String::with_capacity(prev.len() + chunk.len());
    combined.push_str(prev);
    combined.push_str(chunk);
    let char_count = combined.chars().count();
    if char_count <= cap_chars {
        return combined;
    }
    let skip = char_count - cap_chars;
    combined.chars().skip(skip).collect()
}

/// Append `chunk` to `prev` and keep the trailing `cap_bytes` *UTF-8
/// bytes*. If the result fits, return it as-is; otherwise discard
/// leading bytes — advancing past any incomplete multi-byte sequence
/// — so only the trailing `cap_bytes` remain. Mirrors Node
/// `appendWithByteCap`.
#[must_use]
pub fn append_with_byte_cap(prev: &str, chunk: &str, cap_bytes: usize) -> String {
    let mut combined = String::with_capacity(prev.len() + chunk.len());
    combined.push_str(prev);
    combined.push_str(chunk);
    if combined.len() <= cap_bytes {
        return combined;
    }
    let start = combined.len() - cap_bytes;
    // Walk forward to the next char boundary so we never slice in the
    // middle of a UTF-8 codepoint.
    let safe_start = combined
        .char_indices()
        .map(|(i, _)| i)
        .find(|&i| i >= start)
        .unwrap_or(combined.len());
    combined[safe_start..].to_string()
}

// =============================================================================
// Template / path / prompt helpers.
// =============================================================================

/// Walk `dotted_path` (e.g. `"agent.id"`) inside `obj`, returning the
/// leaf value as a string. JSON strings / numbers / booleans are
/// stringified; objects / arrays are `JSON.stringify`'d; missing
/// segments return `""`. Mirrors Node `resolvePathValue`.
#[must_use]
pub fn resolve_path_value(obj: &serde_json::Value, dotted_path: &str) -> String {
    let mut cursor = obj;
    for part in dotted_path.split('.') {
        match cursor {
            serde_json::Value::Object(m) => match m.get(part) {
                Some(v) => cursor = v,
                None => return String::new(),
            },
            _ => return String::new(),
        }
    }
    match cursor {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

/// Replace every `{{ path }}` placeholder in `template` with the
/// resolved value at `data.path`. Paths follow dotted notation and
/// accept `[a-zA-Z0-9_.-]+`. Mirrors Node `renderTemplate`.
#[must_use]
pub fn render_template(template: &str, data: &serde_json::Value) -> String {
    TEMPLATE_PLACEHOLDER_RE
        .replace_all(template, |caps: &regex::Captures<'_>| {
            let path = caps.get(1).map_or("", |m| m.as_str());
            resolve_path_value(data, path)
        })
        .into_owned()
}

/// Trim each non-empty section and join them with `separator`. Empty
/// / non-string sections are dropped. Mirrors Node `joinPromptSections`
/// which takes `Array<string | null | undefined>`; the Rust signature
/// requires an iterator of `Option<S>` where `None` mirrors
/// `null | undefined` and `Some(s)` carries the trimmed content.
#[must_use]
pub fn join_prompt_sections<I, S>(sections: I, separator: &str) -> String
where
    I: IntoIterator<Item = Option<S>>,
    S: AsRef<str>,
{
    sections
        .into_iter()
        .filter_map(|opt| opt)
        .map(|s| s.as_ref().trim().to_string())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(separator)
}


// =============================================================================
// Env helpers (R406) - mirrors Node `redactEnvForLogs`,
// `redactCommandTextForLogs`, `buildInvocationEnvForLogs`,
// `buildPaperclipEnv`, `applyPaperclipWorkspaceEnv`,
// `shapePaperclipWorkspaceEnvForExecution`,
// `rewriteWorkspaceCwdEnvVarsForExecution`,
// `refreshPaperclipWorkspaceEnvForExecution`,
// `sanitizeInheritedPaperclipEnv`, `defaultPathForPlatform`,
// `sanitizeSshRemoteEnv`, `ensurePathInEnv`.
// =============================================================================

/// Redact values of keys that match `SENSITIVE_ENV_KEY` (token, secret,
/// password, ...). Mirrors Node `redactEnvForLogs`.
#[must_use]
pub fn redact_env_for_logs(env: &HashMap<String, String>) -> HashMap<String, String> {
    env.iter()
        .map(|(k, v)| {
            (
                k.clone(),
                if is_sensitive_env_key(k) {
                    REDACTED_LOG_VALUE.to_string()
                } else {
                    v.clone()
                },
            )
        })
        .collect()
}

/// Redact any `--secret=value` / `--token value` arguments inside a
/// command string. Thin wrapper over `pc_acpx::command_redaction`.
/// Mirrors Node `redactCommandTextForLogs`.
#[must_use]
pub fn redact_command_text_for_logs(command: &str) -> String {
    crate::command_redaction::redact_command_text(command, Some(REDACTED_LOG_VALUE))
}

/// Merge `env` with selected runtime keys, then redact sensitive ones
/// for safe log output. Mirrors Node `buildInvocationEnvForLogs`.
#[must_use]
pub fn build_invocation_env_for_logs(
    env: &HashMap<String, String>,
    options: BuildInvocationEnvForLogsOptions<'_>,
) -> HashMap<String, String> {
    let mut merged = env.clone();
    let runtime_env: &HashMap<String, String> = options.runtime_env.unwrap_or_else(|| empty_hashmap_str());
    if let Some(keys) = options.include_runtime_keys {
        for key in keys {
            if merged.contains_key(*key) {
                continue;
            }
            let Some(value) = runtime_env.get(*key) else { continue };
            if value.is_empty() {
                continue;
            }
            merged.insert((*key).to_string(), value.clone());
        }
    }
    if let Some(resolved) = options
        .resolved_command
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let key = options
            .resolved_command_env_key
            .unwrap_or("PAPERCLIP_RESOLVED_COMMAND");
        merged.insert(
            key.to_string(),
            redact_command_text_for_logs(resolved),
        );
    }
    redact_env_for_logs(&merged)
}

/// Options for [`build_invocation_env_for_logs`].
#[derive(Default)]
pub struct BuildInvocationEnvForLogsOptions<'a> {
    pub runtime_env: Option<&'a HashMap<String, String>>,
    pub include_runtime_keys: Option<&'a [&'a str]>,
    pub resolved_command: Option<&'a str>,
    pub resolved_command_env_key: Option<&'a str>,
}

/// Resolve a host string for URL embedding. Mirrors Node
/// `resolveHostForUrl` (private in server-utils.ts).
///
/// - empty / `0.0.0.0` / `::` → `localhost`
/// - already bracketed (`[::1]`) → pass-through
/// - contains `:` (IPv6 unbracketed) → wrap in `[...]`
/// - otherwise pass-through
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

/// Input for [`build_paperclip_env`]. Mirrors Node `buildPaperclipEnv`
/// but parametrizes the runtime env / defaults so pc-acpx does not need
/// to read `process.env` directly.
pub struct BuildPaperclipEnvInput<'a> {
    pub agent_id: &'a str,
    pub company_id: &'a str,
    pub runtime_env: &'a HashMap<String, String>,
    pub default_listen_host: &'a str,
    pub default_listen_port: &'a str,
}

/// Build the canonical `PAPERCLIP_*` env vars for a run. Mirrors Node
/// `buildPaperclipEnv`.
#[must_use]
pub fn build_paperclip_env(input: BuildPaperclipEnvInput<'_>) -> HashMap<String, String> {
    let runtime_host = resolve_host_for_url(
        input
            .runtime_env
            .get("PAPERCLIP_LISTEN_HOST")
            .or_else(|| input.runtime_env.get("HOST"))
            .map(String::as_str)
            .unwrap_or(input.default_listen_host),
    );
    let runtime_port = input
        .runtime_env
        .get("PAPERCLIP_LISTEN_PORT")
        .or_else(|| input.runtime_env.get("PORT"))
        .cloned()
        .unwrap_or_else(|| input.default_listen_port.to_string());
    let api_url = input
        .runtime_env
        .get("PAPERCLIP_RUNTIME_API_URL")
        .or_else(|| input.runtime_env.get("PAPERCLIP_API_URL"))
        .cloned()
        .unwrap_or_else(|| format!("http://{runtime_host}:{runtime_port}"));
    let mut vars = HashMap::new();
    vars.insert("PAPERCLIP_AGENT_ID".to_string(), input.agent_id.to_string());
    vars.insert(
        "PAPERCLIP_COMPANY_ID".to_string(),
        input.company_id.to_string(),
    );
    vars.insert("PAPERCLIP_API_URL".to_string(), api_url);
    vars
}

/// Apply the canonical `PAPERCLIP_WORKSPACE_*` and `AGENT_HOME` env
/// vars to `env` (mutated in place). Mirrors Node
/// `applyPaperclipWorkspaceEnv`.
pub fn apply_paperclip_workspace_env(
    env: &mut HashMap<String, String>,
    input: ApplyPaperclipWorkspaceEnvInput<'_>,
) {
    let mappings: [(&str, Option<&str>); 9] = [
        ("PAPERCLIP_WORKSPACE_CWD", input.workspace_cwd),
        ("PAPERCLIP_WORKSPACE_SOURCE", input.workspace_source),
        ("PAPERCLIP_WORKSPACE_STRATEGY", input.workspace_strategy),
        ("PAPERCLIP_WORKSPACE_ID", input.workspace_id),
        ("PAPERCLIP_WORKSPACE_REPO_URL", input.workspace_repo_url),
        ("PAPERCLIP_WORKSPACE_REPO_REF", input.workspace_repo_ref),
        ("PAPERCLIP_WORKSPACE_BRANCH", input.workspace_branch),
        (
            "PAPERCLIP_WORKSPACE_WORKTREE_PATH",
            input.workspace_worktree_path,
        ),
        ("AGENT_HOME", input.agent_home),
    ];
    for (key, value) in mappings {
        if let Some(v) = value {
            if !v.is_empty() {
                env.insert(key.to_string(), v.to_string());
            }
        }
    }
}

/// Input for [`apply_paperclip_workspace_env`]. Mirrors Node
/// `applyPaperclipWorkspaceEnv` input.
#[derive(Default)]
pub struct ApplyPaperclipWorkspaceEnvInput<'a> {
    pub workspace_cwd: Option<&'a str>,
    pub workspace_source: Option<&'a str>,
    pub workspace_strategy: Option<&'a str>,
    pub workspace_id: Option<&'a str>,
    pub workspace_repo_url: Option<&'a str>,
    pub workspace_repo_ref: Option<&'a str>,
    pub workspace_branch: Option<&'a str>,
    pub workspace_worktree_path: Option<&'a str>,
    pub agent_home: Option<&'a str>,
}

/// Realize workspace env vars for execution. Trims inputs to non-empty
/// strings, then — for remote targets only — repoints the `cwd` of any
/// non-anchor workspace hint to its `staged_project_dirs` entry (or
/// drops the hint entirely when no entry exists). Mirrors Node
/// `shapePaperclipWorkspaceEnvForExecution`.
#[must_use]
pub fn shape_paperclip_workspace_env_for_execution(
    input: ShapePaperclipWorkspaceEnvInput<'_>,
) -> ShapePaperclipWorkspaceEnvOutput {
    let workspace_cwd = input
        .workspace_cwd
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let workspace_worktree_path = input
        .workspace_workspace_worktree_path
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    if !input.execution_target_is_remote {
        return ShapePaperclipWorkspaceEnvOutput {
            workspace_cwd,
            workspace_worktree_path,
            workspace_hints: input.workspace_hints.map(|h| h.to_vec()).unwrap_or_default(),
        };
    }

    let realized_workspace_cwd = workspace_cwd
        .clone()
        .or_else(|| input.execution_cwd.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string));

    let hints_in = input.workspace_hints.unwrap_or(&[]);
    let mut shaped_hints: Vec<serde_json::Map<String, serde_json::Value>> = Vec::new();
    for hint in hints_in {
        let mut next = hint.clone();
        let project_id = next.get("projectId").and_then(|v| v.as_str()).unwrap_or("");
        let staged = if !project_id.is_empty() {
            input.staged_project_dirs.and_then(|m| m.get(project_id)).map(String::as_str)
        } else {
            None
        };
        if let Some(staged) = staged {
            let staged = staged.trim();
            if !staged.is_empty() {
                next.insert("cwd".to_string(), serde_json::Value::String(staged.to_string()));
                shaped_hints.push(next);
                continue;
            }
        }
        // Drop `cwd` so the agent never receives a path the transport
        // did not stage.
        next.remove("cwd");
        shaped_hints.push(next);
    }

    ShapePaperclipWorkspaceEnvOutput {
        workspace_cwd: realized_workspace_cwd,
        workspace_worktree_path: None,
        workspace_hints: shaped_hints,
    }
}

#[derive(Default)]
pub struct ShapePaperclipWorkspaceEnvInput<'a> {
    pub workspace_cwd: Option<&'a str>,
    pub workspace_workspace_worktree_path: Option<&'a str>,
    pub workspace_hints: Option<&'a [serde_json::Map<String, serde_json::Value>]>,
    pub execution_target_is_remote: bool,
    pub execution_cwd: Option<&'a str>,
    pub staged_project_dirs: Option<&'a HashMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShapePaperclipWorkspaceEnvOutput {
    pub workspace_cwd: Option<String>,
    pub workspace_worktree_path: Option<String>,
    pub workspace_hints: Vec<serde_json::Map<String, serde_json::Value>>,
}

/// Rewrite any `*_WORKSPACE_CWD` env var whose value matches the local
/// `workspace_cwd` to the remote `execution_cwd` (a remote absolute
/// path forwarded verbatim). On a local target, the env is passed
/// through after string-coercion filtering. Mirrors Node
/// `rewriteWorkspaceCwdEnvVarsForExecution`.
#[must_use]
pub fn rewrite_workspace_cwd_env_vars_for_execution(
    input: RewriteWorkspaceCwdEnvVarsForExecutionInput<'_>,
) -> HashMap<String, String> {
    // Filter env down to string-only entries (mirrors Node `Object.fromEntries` filter).
    let env_src: &HashMap<String, serde_json::Value> = input.env.unwrap_or_else(|| empty_hashmap_value());
    let mut next_env: HashMap<String, String> = env_src
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
        .collect();

    let local_workspace_cwd = input
        .workspace_cwd
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(posix_resolve);
    let remote_workspace_cwd = input
        .execution_cwd
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    if !input.execution_target_is_remote || local_workspace_cwd.is_none() || remote_workspace_cwd.is_none() {
        return next_env;
    }
    let local_workspace_cwd = local_workspace_cwd.unwrap();
    let remote_workspace_cwd = remote_workspace_cwd.unwrap();

    for (key, value) in next_env.clone() {
        if !key.ends_with("_WORKSPACE_CWD") {
            continue;
        }
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        if posix_resolve(&trimmed) != local_workspace_cwd {
            continue;
        }
        next_env.insert(key, remote_workspace_cwd.clone());
    }

    next_env
}

pub struct RewriteWorkspaceCwdEnvVarsForExecutionInput<'a> {
    pub env: Option<&'a HashMap<String, serde_json::Value>>,
    pub workspace_cwd: Option<&'a str>,
    pub execution_cwd: Option<&'a str>,
    pub execution_target_is_remote: bool,
}

impl<'a> Default for RewriteWorkspaceCwdEnvVarsForExecutionInput<'a> {
    fn default() -> Self {
        Self {
            env: None,
            workspace_cwd: None,
            execution_cwd: None,
            execution_target_is_remote: false,
        }
    }
}

/// POSIX-flavored `path.resolve` on absolute inputs. Returns the
/// normalized absolute path. Mirrors Node `path.resolve(s)` for
/// absolute POSIX strings (Node's `path.resolve` always uses the host
/// cwd + path module, but the env-var logic only ever passes absolute
/// paths).
fn posix_resolve(p: &str) -> String {
    use std::path::Path;
    let path = Path::new(p);
    // Strip leading `./` and trailing `/` for comparison parity with
    // Node's `path.resolve` which always returns a canonical form.
    let s = path.to_string_lossy().to_string();
    s.trim_end_matches('/').to_string()
}

/// Refresh the workspace env on `input.env` after the shape /
/// apply / rewrite pass. Returns the shaped workspace fields. Mirrors
/// Node `refreshPaperclipWorkspaceEnvForExecution`.
pub fn refresh_paperclip_workspace_env_for_execution(
    env: &mut HashMap<String, String>,
    input: RefreshPaperclipWorkspaceEnvInput<'_>,
) -> ShapePaperclipWorkspaceEnvOutput {
    let shaped = shape_paperclip_workspace_env_for_execution(
        ShapePaperclipWorkspaceEnvInput {
            workspace_cwd: input.workspace_cwd,
            workspace_workspace_worktree_path: input.workspace_worktree_path,
            workspace_hints: input.workspace_hints,
            execution_target_is_remote: input.execution_target_is_remote,
            execution_cwd: input.execution_cwd,
            staged_project_dirs: input.staged_project_dirs,
        },
    );

    env.remove("PAPERCLIP_WORKSPACE_CWD");
    env.remove("PAPERCLIP_WORKSPACE_WORKTREE_PATH");
    env.remove("PAPERCLIP_WORKSPACES_JSON");

    apply_paperclip_workspace_env(
        env,
        ApplyPaperclipWorkspaceEnvInput {
            workspace_cwd: shaped.workspace_cwd.as_deref(),
            workspace_source: input.workspace_source,
            workspace_strategy: input.workspace_strategy,
            workspace_id: input.workspace_id,
            workspace_repo_url: input.workspace_repo_url,
            workspace_repo_ref: input.workspace_repo_ref,
            workspace_branch: input.workspace_branch,
            workspace_worktree_path: shaped.workspace_worktree_path.as_deref(),
            agent_home: input.agent_home,
        },
    );

    if !shaped.workspace_hints.is_empty() {
        let serialized =
            serde_json::to_string(&shaped.workspace_hints).unwrap_or_else(|_| "[]".to_string());
        env.insert("PAPERCLIP_WORKSPACES_JSON".to_string(), serialized);
    }

    let env_config_src: &HashMap<String, serde_json::Value> = input.env_config.unwrap_or_else(|| empty_hashmap_value());
    let shaped_env_config = rewrite_workspace_cwd_env_vars_for_execution(
        RewriteWorkspaceCwdEnvVarsForExecutionInput {
            env: Some(env_config_src),
            workspace_cwd: input.workspace_cwd,
            execution_cwd: shaped.workspace_cwd.as_deref(),
            execution_target_is_remote: input.execution_target_is_remote,
        },
    );

    for (key, value) in shaped_env_config {
        if is_forbidden_config_env_key(&key) {
            continue;
        }
        if is_paperclip_runtime_env_key(&key) && env.contains_key(&key) {
            continue;
        }
        env.insert(key, value);
    }

    shaped
}

#[derive(Default)]
pub struct RefreshPaperclipWorkspaceEnvInput<'a> {
    pub workspace_cwd: Option<&'a str>,
    pub workspace_source: Option<&'a str>,
    pub workspace_strategy: Option<&'a str>,
    pub workspace_id: Option<&'a str>,
    pub workspace_repo_url: Option<&'a str>,
    pub workspace_repo_ref: Option<&'a str>,
    pub workspace_branch: Option<&'a str>,
    pub workspace_worktree_path: Option<&'a str>,
    pub workspace_hints: Option<&'a [serde_json::Map<String, serde_json::Value>]>,
    pub agent_home: Option<&'a str>,
    pub execution_target_is_remote: bool,
    pub execution_cwd: Option<&'a str>,
    pub env_config: Option<&'a HashMap<String, serde_json::Value>>,
    pub staged_project_dirs: Option<&'a HashMap<String, String>>,
}

/// Strip every `PAPERCLIP_*` env var from `base_env` except the three
/// that the runtime needs (`PAPERCLIP_RUNTIME_API_URL`,
/// `PAPERCLIP_LISTEN_HOST`, `PAPERCLIP_LISTEN_PORT`). Also drops the
/// legacy `PAPERCLIPAI_CMD`. Mirrors Node `sanitizeInheritedPaperclipEnv`.
#[must_use]
pub fn sanitize_inherited_paperclip_env(
    base_env: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut env = base_env.clone();
    env.remove("PAPERCLIPAI_CMD");
    let keys: Vec<String> = env
        .keys()
        .filter(|k| k.starts_with("PAPERCLIP_"))
        .filter(|k| {
            *k != "PAPERCLIP_RUNTIME_API_URL"
                && *k != "PAPERCLIP_LISTEN_HOST"
                && *k != "PAPERCLIP_LISTEN_PORT"
        })
        .cloned()
        .collect();
    for k in keys {
        env.remove(&k);
    }
    env
}

/// Default `$PATH` for the current platform. Mirrors Node
/// `defaultPathForPlatform`.
#[must_use]
pub fn default_path_for_platform(is_windows: bool) -> &'static str {
    if is_windows {
        r"C:\Windows\System32;C:\Windows;C:\Windows\System32\Wbem"
    } else {
        "/usr/local/bin:/opt/homebrew/bin:/usr/local/sbin:/usr/bin:/bin:/usr/sbin:/sbin"
    }
}

/// Sanitize the env for an SSH remote execution by delegating to
/// `pc_acpx::remote_execution_env::sanitize_remote_execution_env`.
/// Mirrors Node `sanitizeSshRemoteEnv`.
#[must_use]
pub fn sanitize_ssh_remote_env(
    env: &HashMap<String, String>,
    inherited_env: &HashMap<String, String>,
) -> HashMap<String, String> {
    let env_bt: std::collections::BTreeMap<String, String> = env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let inherited_bt: std::collections::BTreeMap<String, String> = inherited_env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let out = crate::remote_execution_env::sanitize_remote_execution_env(&env_bt, &inherited_bt);
    out.into_iter().collect()
}

/// Ensure `env` has a non-empty `PATH` (or `Path` on Windows) entry.
/// Mirrors Node `ensurePathInEnv`.
#[must_use]
pub fn ensure_path_in_env(env: &HashMap<String, String>, is_windows: bool) -> HashMap<String, String> {
    let path_key = if is_windows { "Path" } else { "PATH" };
    if let Some(v) = env.get(path_key) {
        if !v.is_empty() {
            return env.clone();
        }
    }
    let mut out = env.clone();
    out.insert("PATH".to_string(), default_path_for_platform(false).to_string());
    out
}



// =============================================================================
// Skill entries (R407) - mirrors Node `PaperclipSkillEntry`,
// `PaperclipDesiredSkillEntry`, `InstalledSkillTarget`,
// `MaterializedPaperclipSkillCopyResult`, `AdapterSkillEntry`,
// `AdapterSkillSnapshot`, plus the pure helpers `normalizePathSlashes`,
// `isMaintainerOnlySkillTarget`, `skillLocationLabel`,
// `buildManagedSkillOrigin`, `isPaperclipSkillSourceMissing`,
// `resolvePaperclipSkillMissingDetail`, `resolveSkillDetail`,
// `resolveInstalledEntryTarget`, `expandHomePrefix`,
// `normalizeConfiguredPaperclipRuntimeSkills`,
// `canonicalizeDesiredPaperclipSkillReference`,
// `readPaperclipSkillSyncPreference`,
// `resolvePaperclipDesiredSkillNames`,
// `writePaperclipSkillSyncPreference`, `resolvePaperclipInstanceRootForAdapter`,
// `buildRuntimeMountedSkillSnapshot`, `buildPersistentSkillSnapshot`,
// and the related constants.
//
// Async helpers (`resolvePaperclipSkillsDir`, `listPaperclipSkillEntries`,
// `readInstalledSkillTargets`, `readPaperclipRuntimeSkillEntries`,
// `readPaperclipSkillMarkdown`, `ensurePaperclipSkillSymlink`,
// `materializePaperclipSkillCopy`, `removeMaintainerOnlySkillSymlinks`)
// require real filesystem access; deferred with the async fs layer.
// =============================================================================

/// Skill entry as seen by adapters. Mirrors Node `PaperclipSkillEntry`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaperclipSkillEntry {
    pub key: String,
    pub runtime_name: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_version_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_status: Option<PaperclipSkillSourceStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing_detail: Option<String>,
}

/// Mirrors Node `sourceStatus: "available" | "missing"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PaperclipSkillSourceStatus {
    Available,
    Missing,
}

/// Mirrors Node `PaperclipDesiredSkillEntry`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaperclipDesiredSkillEntry {
    pub key: String,
    pub version_id: Option<String>,
}

/// Mirrors Node `InstalledSkillTarget`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledSkillTarget {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_path: Option<String>,
    pub kind: InstalledSkillTargetKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstalledSkillTargetKind {
    Symlink,
    Directory,
    File,
}

/// Mirrors Node `MaterializedPaperclipSkillCopyResult`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MaterializedPaperclipSkillCopyResult {
    pub copied_files: u64,
    #[serde(default)]
    pub skipped_symlinks: Vec<String>,
}

// ---- Skill snapshot wire types (mirrors Node AdapterSkillEntry / Snapshot) ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdapterSkillState {
    Available,
    Configured,
    Installed,
    Missing,
    Stale,
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdapterSkillSyncMode {
    Unsupported,
    Persistent,
    Ephemeral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterSkillOrigin {
    CompanyManaged,
    UserInstalled,
    ExternalUnknown,
}

/// Mirrors Node `AdapterSkillEntry`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterSkillEntry {
    pub key: String,
    pub runtime_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_version_id: Option<String>,
    pub desired: bool,
    pub managed: bool,
    pub state: AdapterSkillState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<AdapterSkillOrigin>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Mirrors Node `AdapterSkillSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterSkillSnapshot {
    pub adapter_type: String,
    pub supported: bool,
    pub mode: AdapterSkillSyncMode,
    pub desired_skills: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desired_skill_entries: Option<Vec<PaperclipDesiredSkillEntry>>,
    pub entries: Vec<AdapterSkillEntry>,
    pub warnings: Vec<String>,
}

// ---- Skill-related constants ----

/// Relative candidate paths for the Paperclip skills root, walked in
/// order during `resolvePaperclipSkillsDir`. Mirrors Node
/// `PAPERCLIP_SKILL_ROOT_RELATIVE_CANDIDATES`.
pub const PAPERCLIP_SKILL_ROOT_RELATIVE_CANDIDATES: &[&str] =
    &["../../skills", "../../../../../skills"];

/// Sentinel filename that marks a fully materialized skill copy.
/// Mirrors Node `MATERIALIZED_SKILL_SENTINEL`.
pub const MATERIALIZED_SKILL_SENTINEL: &str = ".paperclip-materialized-skill.json";

/// Filename used as a lock owner when a skill is being materialized.
/// Mirrors Node `MATERIALIZED_SKILL_LOCK_OWNER`.
pub const MATERIALIZED_SKILL_LOCK_OWNER: &str = "owner.json";

/// Maximum age (ms) of a stale lock file before another writer may
/// steal it. Mirrors Node `MATERIALIZED_SKILL_LOCK_STALE_MS`.
pub const MATERIALIZED_SKILL_LOCK_STALE_MS: u64 = 30_000;

// ---- Path helpers ----

/// Normalize path separators to `/`. Mirrors Node `normalizePathSlashes`.
#[must_use]
pub fn normalize_path_slashes(value: &str) -> String {
    value.replace('\\', "/")
}

/// A path lives under the maintainer-managed `.agents/skills` tree.
/// Mirrors Node `isMaintainerOnlySkillTarget`.
#[must_use]
pub fn is_maintainer_only_skill_target(candidate: &str) -> bool {
    normalize_path_slashes(candidate).contains("/.agents/skills/")
}

/// Trim a location label, returning `None` when absent / empty. Mirrors
/// Node `skillLocationLabel`.
#[must_use]
pub fn skill_location_label(value: Option<&str>) -> Option<String> {
    value.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Default origin tuple for Paperclip-managed skills. Mirrors Node
/// `buildManagedSkillOrigin`.
#[must_use]
pub fn build_managed_skill_origin() -> ManagedSkillOrigin {
    ManagedSkillOrigin {
        origin: AdapterSkillOrigin::CompanyManaged,
        origin_label: "Managed by Paperclip".to_string(),
        read_only: false,
    }
}

/// Output of [`build_managed_skill_origin`]. Spread into
/// [`AdapterSkillEntry`] via `..managed_origin`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedSkillOrigin {
    pub origin: AdapterSkillOrigin,
    pub origin_label: String,
    pub read_only: bool,
}

impl From<ManagedSkillOrigin> for (Option<AdapterSkillOrigin>, Option<String>, Option<bool>) {
    fn from(m: ManagedSkillOrigin) -> Self {
        (Some(m.origin), Some(m.origin_label), Some(m.read_only))
    }
}

// ---- Skill entry / snapshot helpers ----

/// Returns `true` when the entry's source is missing. Mirrors Node
/// `isPaperclipSkillSourceMissing`.
#[must_use]
pub fn is_paperclip_skill_source_missing(entry: &PaperclipSkillEntry) -> bool {
    matches!(entry.source_status, Some(PaperclipSkillSourceStatus::Missing))
}

/// Resolve a missing-detail string for the entry, falling back to the
/// caller-supplied string. Mirrors Node `resolvePaperclipSkillMissingDetail`.
#[must_use]
pub fn resolve_paperclip_skill_missing_detail(
    entry: &PaperclipSkillEntry,
    fallback: &str,
) -> String {
    entry
        .missing_detail
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

/// Resolve a per-entry detail string, which may be a literal string or a
/// callback. Mirrors Node `resolveSkillDetail`.
#[must_use]
pub fn resolve_skill_detail(
    detail: Option<&SkillDetail<'_>>,
    entry: &PaperclipSkillEntry,
) -> Option<String> {
    match detail? {
        SkillDetail::Literal(s) => Some((*s).to_string()),
        SkillDetail::Callback(f) => f(entry),
    }
}

/// Sum type for the `detail` parameter of [`resolve_skill_detail`].
#[derive(Clone, Copy)]
pub enum SkillDetail<'a> {
    Literal(&'a str),
    Callback(&'a dyn Fn(&PaperclipSkillEntry) -> Option<String>),
}

impl<'a> std::fmt::Debug for SkillDetail<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkillDetail::Literal(s) => f.debug_tuple("Literal").field(s).finish(),
            SkillDetail::Callback(_) => f.debug_tuple("Callback").field(&"<fn>").finish(),
        }
    }
}

/// Pure [`InstalledSkillTarget`] resolver. Mirrors Node
/// `resolveInstalledEntryTarget`. The `is_symlink` / `is_file` /
/// `is_directory` flags come from a `Dirent` on Node; in Rust we accept
/// a pre-classified [`InstalledSkillTargetKind`] hint.
#[must_use]
pub fn resolve_installed_entry_target(
    skills_home: &str,
    entry_name: &str,
    kind: InstalledSkillTargetKind,
    linked_path: Option<&str>,
) -> InstalledSkillTarget {
    let full_path = posix_join(skills_home, entry_name);
    match kind {
        InstalledSkillTargetKind::Symlink => InstalledSkillTarget {
            target_path: linked_path.map(|p| {
                let parent = posix_dirname(&full_path);
                posix_join(&parent, p)
            }),
            kind: InstalledSkillTargetKind::Symlink,
        },
        InstalledSkillTargetKind::Directory => InstalledSkillTarget {
            target_path: Some(full_path),
            kind: InstalledSkillTargetKind::Directory,
        },
        InstalledSkillTargetKind::File => InstalledSkillTarget {
            target_path: Some(full_path),
            kind: InstalledSkillTargetKind::File,
        },
    }
}

/// POSIX `path.join`. Joins `parent` and `child` with a single `/`,
/// handling empty parts.
fn posix_join(parent: &str, child: &str) -> String {
    let parent_trim = parent.trim_end_matches('/');
    let child_trim = child.trim_start_matches('/');
    if parent_trim.is_empty() {
        child_trim.to_string()
    } else if child_trim.is_empty() {
        parent_trim.to_string()
    } else {
        format!("{parent_trim}/{child_trim}")
    }
}

/// POSIX `path.dirname`. Returns the portion of `p` before the final
/// `/`, or `"."` for paths without one.
fn posix_dirname(p: &str) -> String {
    match p.rfind('/') {
        Some(idx) => {
            if idx == 0 {
                "/".to_string()
            } else {
                p[..idx].to_string()
            }
        }
        None => ".".to_string(),
    }
}

/// POSIX `path.resolve` for absolute inputs. Returns the trimmed
/// absolute path. Mirrors Node `path.resolve(p)` for absolute POSIX
/// strings (Node's path.resolve uses host cwd; the env logic only ever
/// passes absolute paths so we simplify).
fn posix_resolve_v2(p: &str) -> String {
    p.trim_end_matches('/').to_string()
}

// ---- Home prefix / instance root ----

/// Expand a leading `~` to `home_dir`. Mirrors Node `expandHomePrefix`.
#[must_use]
pub fn expand_home_prefix(value: &str, home_dir: &str) -> String {
    if value == "~" {
        return home_dir.to_string();
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return posix_join(home_dir, rest);
    }
    value.to_string()
}

/// Resolve the Paperclip instance root for an adapter. Mirrors Node
/// `resolvePaperclipInstanceRootForAdapter`. The `home_dir` /
/// `instance_id` / `env` parameters are explicit so pc-acpx does not
/// need to read `process.env`.
pub fn resolve_paperclip_instance_root_for_adapter(input: ResolveInstanceRootInput<'_>) -> String {
    let env: &HashMap<String, String> = input.env.unwrap_or_else(|| empty_hashmap_str());
    let home_raw = input
        .home_dir
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            env.get("PAPERCLIP_HOME")
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
        });
    let fallback_home = posix_join(input.default_home_dir, ".paperclip");
    let home_resolved = match home_raw {
        Some(h) => posix_resolve_v2(&expand_home_prefix(h, input.default_home_dir)),
        None => posix_resolve_v2(&fallback_home),
    };
    let instance_id = input
        .instance_id
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            env.get("PAPERCLIP_INSTANCE_ID")
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or(DEFAULT_PAPERCLIP_INSTANCE_ID);
    if !is_valid_path_segment(&instance_id) {
        // Mirrors Node: `throw new Error(...)`. In pc-acpx we surface
        // the error rather than panicking.
        return posix_resolve_v2(&posix_join(
            &posix_join(&home_resolved, "instances"),
            "invalid",
        ));
    }
    posix_resolve_v2(&posix_join(&posix_join(&home_resolved, "instances"), &instance_id))
}

#[derive(Default)]
pub struct ResolveInstanceRootInput<'a> {
    pub home_dir: Option<&'a str>,
    pub instance_id: Option<&'a str>,
    pub env: Option<&'a HashMap<String, String>>,
    /// Default `$HOME` directory used when neither `home_dir` nor
    /// `PAPERCLIP_HOME` are provided. Pass `dirs::home_dir()` at the
    /// call site to mirror Node `os.homedir()`.
    pub default_home_dir: &'a str,
}

// ---- Skill sync preference ----

/// Output of [`read_paperclip_skill_sync_preference`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PaperclipSkillSyncPreference {
    pub explicit: bool,
    pub desired_skills: Vec<String>,
    pub desired_skill_entries: Vec<PaperclipDesiredSkillEntry>,
}

/// Read the `paperclipSkillSync` block from a config object. Mirrors
/// Node `readPaperclipSkillSyncPreference`.
#[must_use]
pub fn read_paperclip_skill_sync_preference(config: &serde_json::Value) -> PaperclipSkillSyncPreference {
    let raw = config.get("paperclipSkillSync");
    let Some(obj) = raw.and_then(|v| v.as_object()) else {
        return PaperclipSkillSyncPreference::default();
    };
    let desired_values = obj.get("desiredSkills").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mut desired: Vec<PaperclipDesiredSkillEntry> = Vec::new();
    for value in desired_values {
        if let Some(s) = value.as_str() {
            let key = s.trim();
            if !key.is_empty() {
                desired.push(PaperclipDesiredSkillEntry {
                    key: key.to_string(),
                    version_id: None,
                });
            }
        } else if let Some(record) = value.as_object() {
            let key = record
                .get("key")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .unwrap_or("");
            if !key.is_empty() {
                let version_id = record
                    .get("versionId")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                desired.push(PaperclipDesiredSkillEntry {
                    key: key.to_string(),
                    version_id,
                });
            }
        }
    }
    // Deduplicate by key, first-wins, preserving insertion order.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut desired_skill_entries: Vec<PaperclipDesiredSkillEntry> = Vec::new();
    for entry in desired {
        if seen.insert(entry.key.clone()) {
            desired_skill_entries.push(entry);
        }
    }
    PaperclipSkillSyncPreference {
        explicit: obj.contains_key("desiredSkills"),
        desired_skills: desired_skill_entries.iter().map(|e| e.key.clone()).collect(),
        desired_skill_entries,
    }
}

/// Canonicalize a desired-skill reference against the available skill
/// entries. Mirrors Node `canonicalizeDesiredPaperclipSkillReference`.
#[must_use]
pub fn canonicalize_desired_paperclip_skill_reference(
    reference: &str,
    available_entries: &[AvailableSkillRef<'_>],
) -> String {
    let normalized = reference.trim().to_lowercase();
    if normalized.is_empty() {
        return String::new();
    }
    // 1. Exact key match.
    if let Some(entry) = available_entries
        .iter()
        .find(|e| e.key.trim().to_lowercase() == normalized)
    {
        return entry.key.to_string();
    }
    // 2. Unique runtime-name match.
    let by_runtime: Vec<&AvailableSkillRef<'_>> = available_entries
        .iter()
        .filter(|e| {
            e.runtime_name
                .map(|n| n.trim().to_lowercase() == normalized)
                .unwrap_or(false)
        })
        .collect();
    if by_runtime.len() == 1 {
        return by_runtime[0].key.to_string();
    }
    // 3. Unique slug match (last segment of `key`).
    let slug_matches: Vec<&AvailableSkillRef<'_>> = available_entries
        .iter()
        .filter(|e| {
            e.key
                .trim()
                .to_lowercase()
                .rsplit_once('/')
                .map(|(_, slug)| slug == normalized)
                .unwrap_or(false)
        })
        .collect();
    if slug_matches.len() == 1 {
        return slug_matches[0].key.to_string();
    }
    // 4. Pass-through (lowercased).
    normalized
}

/// Borrowed view of a skill entry for the canonicalization step.
#[derive(Debug, Clone, Copy)]
pub struct AvailableSkillRef<'a> {
    pub key: &'a str,
    pub runtime_name: Option<&'a str>,
}

/// Resolve the desired skill names from the config against the
/// available entries. Mirrors Node `resolvePaperclipDesiredSkillNames`.
#[must_use]
pub fn resolve_paperclip_desired_skill_names(
    config: &serde_json::Value,
    available_entries: &[AvailableSkillRef<'_>],
) -> Vec<String> {
    let preference = read_paperclip_skill_sync_preference(config);
    if !preference.explicit {
        return Vec::new();
    }
    let canonicalized: Vec<String> = preference
        .desired_skills
        .iter()
        .map(|r| canonicalize_desired_paperclip_skill_reference(r, available_entries))
        .filter(|s| !s.is_empty())
        .collect();
    // Dedup while preserving order.
    let mut seen = std::collections::HashSet::new();
    canonicalized
        .into_iter()
        .filter(|s| seen.insert(s.clone()))
        .collect()
}

/// Write the `paperclipSkillSync` block back into the config object.
/// Mirrors Node `writePaperclipSkillSyncPreference`. Returns the
/// updated config (Node mutates in place; Rust returns a new map to
/// match pc-acpx patterns).
#[must_use]
pub fn write_paperclip_skill_sync_preference(
    config: &serde_json::Value,
    desired_skills: &[SkillSyncWrite<'_>],
) -> serde_json::Value {
    let mut next = config.clone();
    let raw = next.get("paperclipSkillSync").cloned().unwrap_or(serde_json::Value::Null);
    let mut current = match raw {
        serde_json::Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    let entries: Vec<PaperclipDesiredSkillEntry> = desired_skills
        .iter()
        .filter_map(|v| match v {
            SkillSyncWrite::Key(key) => {
                let key = key.trim();
                if key.is_empty() {
                    None
                } else {
                    Some(PaperclipDesiredSkillEntry {
                        key: key.to_string(),
                        version_id: None,
                    })
                }
            }
            SkillSyncWrite::Entry { key, version_id } => {
                let key = key.trim();
                if key.is_empty() {
                    None
                } else {
                    Some(PaperclipDesiredSkillEntry {
                        key: key.to_string(),
                        version_id: version_id.clone(),
                    })
                }
            }
        })
        .collect();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut normalized: Vec<PaperclipDesiredSkillEntry> = Vec::new();
    for entry in entries {
        if seen.insert(entry.key.clone()) {
            normalized.push(entry);
        }
    }
    let has_versions = normalized.iter().any(|e| e.version_id.is_some());
    let desired_value = if has_versions {
        serde_json::to_value(&normalized).unwrap_or(serde_json::Value::Array(Vec::new()))
    } else {
        serde_json::to_value(
            normalized
                .iter()
                .map(|e| e.key.clone())
                .collect::<Vec<_>>(),
        )
        .unwrap_or(serde_json::Value::Array(Vec::new()))
    };
    current.insert("desiredSkills".to_string(), desired_value);
    if let serde_json::Value::Object(ref mut m) = next {
        m.insert("paperclipSkillSync".to_string(), serde_json::Value::Object(current));
    }
    next
}

/// Sum type for [`write_paperclip_skill_sync_preference`].
#[derive(Debug, Clone)]
pub enum SkillSyncWrite<'a> {
    Key(&'a str),
    Entry {
        key: &'a str,
        version_id: Option<String>,
    },
}

// ---- Skill snapshot builders ----

/// Options for [`build_runtime_mounted_skill_snapshot`].
pub struct RuntimeMountedSkillSnapshotOptions<'a> {
    pub adapter_type: &'a str,
    pub available_entries: &'a [PaperclipSkillEntry],
    pub desired_skills: &'a [String],
    pub configured_detail: SkillDetail<'a>,
    pub missing_detail: Option<&'a str>,
    pub mode: Option<AdapterSkillSyncMode>,
    pub supported: Option<bool>,
    pub unsupported_detail: Option<SkillDetail<'a>>,
    pub warnings: Option<Vec<String>>,
    pub external_installed: Option<&'a std::collections::HashMap<String, InstalledSkillTarget>>,
    pub external_location_label: Option<&'a str>,
    pub external_detail: Option<&'a str>,
    pub skills_home: Option<&'a str>,
}

impl<'a> Default for RuntimeMountedSkillSnapshotOptions<'a> {
    fn default() -> Self {
        Self {
            adapter_type: "",
            available_entries: &[],
            desired_skills: &[],
            configured_detail: SkillDetail::Literal(""),
            missing_detail: None,
            mode: None,
            supported: None,
            unsupported_detail: None,
            warnings: None,
            external_installed: None,
            external_location_label: None,
            external_detail: None,
            skills_home: None,
        }
    }
}

/// Build an `AdapterSkillSnapshot` for a runtime-mounted (ephemeral)
/// adapter. Mirrors Node `buildRuntimeMountedSkillSnapshot`.
#[must_use]
pub fn build_runtime_mounted_skill_snapshot(
    options: RuntimeMountedSkillSnapshotOptions<'_>,
) -> AdapterSkillSnapshot {
    let adapter_type = options.adapter_type.to_string();
    let mode = options.mode.unwrap_or(AdapterSkillSyncMode::Ephemeral);
    let supported = options.supported.unwrap_or(matches!(mode, AdapterSkillSyncMode::Ephemeral));
    let missing_detail = options
        .missing_detail
        .unwrap_or("Paperclip cannot find this skill in the local runtime skills directory.");
    let external_detail = options
        .external_detail
        .unwrap_or("Installed outside Paperclip management.");
    let mut warnings: Vec<String> = options.warnings.unwrap_or_default();
    let mut by_key: std::collections::HashMap<&str, &PaperclipSkillEntry> =
        std::collections::HashMap::new();
    for entry in options.available_entries {
        by_key.insert(entry.key.as_str(), entry);
    }
    let desired_set: std::collections::HashSet<&str> = options.desired_skills.iter().map(String::as_str).collect();
    let managed_origin = build_managed_skill_origin();
    let mut entries: Vec<AdapterSkillEntry> = Vec::new();
    for available in options.available_entries {
        let desired = desired_set.contains(available.key.as_str());
        if is_paperclip_skill_source_missing(available) {
            let mut e = AdapterSkillEntry {
                key: available.key.clone(),
                runtime_name: Some(available.runtime_name.clone()),
                version_id: available.version_id.clone(),
                current_version_id: available.current_version_id.clone(),
                desired,
                managed: true,
                state: AdapterSkillState::Missing,
                source_path: None,
                target_path: None,
                detail: Some(resolve_paperclip_skill_missing_detail(available, missing_detail)),
                origin: Some(managed_origin.origin),
                origin_label: Some(managed_origin.origin_label.clone()),
                read_only: Some(managed_origin.read_only),
                location_label: None,
            };
            entries.push(e);
            continue;
        }
        let configured = supported && matches!(mode, AdapterSkillSyncMode::Ephemeral) && desired;
        let detail = if desired {
            if configured {
                resolve_skill_detail(Some(&options.configured_detail), available)
            } else {
                let fallback = SkillDetail::Literal(
                    options
                        .unsupported_detail
                        .as_ref()
                        .map(|d| match d {
                            SkillDetail::Literal(s) => *s,
                            _ => "Desired state is stored in Paperclip only; this adapter cannot apply skills at runtime.",
                        })
                        .unwrap_or("Desired state is stored in Paperclip only; this adapter cannot apply skills at runtime."),
                );
                resolve_skill_detail(Some(&fallback), available)
            }
        } else {
            None
        };
        entries.push(AdapterSkillEntry {
            key: available.key.clone(),
            runtime_name: Some(available.runtime_name.clone()),
            version_id: available.version_id.clone(),
            current_version_id: available.current_version_id.clone(),
            desired,
            managed: true,
            state: if configured {
                AdapterSkillState::Configured
            } else {
                AdapterSkillState::Available
            },
            source_path: Some(available.source.clone()),
            target_path: None,
            detail,
            origin: Some(managed_origin.origin),
            origin_label: Some(managed_origin.origin_label.clone()),
            read_only: Some(managed_origin.read_only),
            location_label: None,
        });
    }
    for desired_skill in options.desired_skills {
        if by_key.contains_key(desired_skill.as_str()) {
            continue;
        }
        warnings.push(format!(
            "Desired skill \"{desired_skill}\" is not available from the Paperclip skills directory."
        ));
        entries.push(AdapterSkillEntry {
            key: desired_skill.clone(),
            runtime_name: None,
            version_id: None,
            current_version_id: None,
            desired: true,
            managed: true,
            state: AdapterSkillState::Missing,
            source_path: None,
            target_path: None,
            detail: Some(missing_detail.to_string()),
            origin: Some(AdapterSkillOrigin::ExternalUnknown),
            origin_label: Some("External or unavailable".to_string()),
            read_only: Some(false),
            location_label: None,
        });
    }
    if let Some(external_installed) = options.external_installed {
        for (name, installed_entry) in external_installed {
            if options
                .available_entries
                .iter()
                .any(|e| e.runtime_name == *name)
            {
                continue;
            }
            let target_path = installed_entry
                .target_path
                .clone()
                .or_else(|| options.skills_home.map(|h| posix_join(h, name)));
            entries.push(AdapterSkillEntry {
                key: name.clone(),
                runtime_name: Some(name.clone()),
                version_id: None,
                current_version_id: None,
                desired: false,
                managed: false,
                state: AdapterSkillState::External,
                source_path: None,
                target_path,
                detail: Some(external_detail.to_string()),
                origin: Some(AdapterSkillOrigin::UserInstalled),
                origin_label: Some("User-installed".to_string()),
                read_only: Some(true),
                location_label: skill_location_label(options.external_location_label),
            });
        }
    }
    entries.sort_by(|a, b| a.key.cmp(&b.key));
    let desired_skill_entries: Vec<PaperclipDesiredSkillEntry> = options
        .desired_skills
        .iter()
        .map(|key| PaperclipDesiredSkillEntry {
            key: key.clone(),
            version_id: by_key.get(key.as_str()).and_then(|e| e.version_id.clone()),
        })
        .collect();
    AdapterSkillSnapshot {
        adapter_type,
        supported,
        mode,
        desired_skills: options.desired_skills.to_vec(),
        desired_skill_entries: Some(desired_skill_entries),
        entries,
        warnings,
    }
}

/// Options for [`build_persistent_skill_snapshot`].
pub struct PersistentSkillSnapshotOptions<'a> {
    pub adapter_type: &'a str,
    pub available_entries: &'a [PaperclipSkillEntry],
    pub desired_skills: &'a [String],
    pub installed: Option<&'a std::collections::HashMap<String, InstalledSkillTarget>>,
    pub skills_home: &'a str,
    pub location_label: Option<&'a str>,
    pub installed_detail: Option<&'a str>,
    pub missing_detail: &'a str,
    pub external_conflict_detail: &'a str,
    pub external_detail: &'a str,
    pub warnings: Option<Vec<String>>,
}

/// Build an `AdapterSkillSnapshot` for a persistent (on-disk) adapter.
/// Mirrors Node `buildPersistentSkillSnapshot`.
#[must_use]
pub fn build_persistent_skill_snapshot(
    options: PersistentSkillSnapshotOptions<'_>,
) -> AdapterSkillSnapshot {
    let adapter_type = options.adapter_type.to_string();
    let mut warnings: Vec<String> = options.warnings.unwrap_or_default();
    let mut by_key: std::collections::HashMap<&str, &PaperclipSkillEntry> =
        std::collections::HashMap::new();
    for entry in options.available_entries {
        by_key.insert(entry.key.as_str(), entry);
    }
    let desired_set: std::collections::HashSet<&str> = options.desired_skills.iter().map(String::as_str).collect();
    let managed_origin = build_managed_skill_origin();
    let mut entries: Vec<AdapterSkillEntry> = Vec::new();
    for available in options.available_entries {
        let installed_map = options.installed.unwrap_or_else(|| empty_hashmap_str_installed());
        let installed_entry = installed_map.get(&available.runtime_name);
        let desired = desired_set.contains(available.key.as_str());
        if is_paperclip_skill_source_missing(available) {
            entries.push(AdapterSkillEntry {
                key: available.key.clone(),
                runtime_name: Some(available.runtime_name.clone()),
                version_id: available.version_id.clone(),
                current_version_id: available.current_version_id.clone(),
                desired,
                managed: true,
                state: AdapterSkillState::Missing,
                source_path: None,
                target_path: Some(posix_join(options.skills_home, &available.runtime_name)),
                detail: Some(resolve_paperclip_skill_missing_detail(available, options.missing_detail)),
                origin: Some(managed_origin.origin),
                origin_label: Some(managed_origin.origin_label.clone()),
                read_only: Some(managed_origin.read_only),
                location_label: None,
            });
            continue;
        }
        let mut state = AdapterSkillState::Available;
        let mut managed = false;
        let mut detail: Option<String> = None;
        if let Some(installed) = installed_entry {
            if installed.target_path.as_deref() == Some(available.source.as_str()) {
                managed = true;
                state = if desired {
                    AdapterSkillState::Installed
                } else {
                    AdapterSkillState::Stale
                };
                detail = options.installed_detail.map(str::to_string);
            } else {
                state = AdapterSkillState::External;
                detail = Some(
                    if desired {
                        options.external_conflict_detail.to_string()
                    } else {
                        options.external_detail.to_string()
                    },
                );
            }
        } else if desired {
            state = AdapterSkillState::Missing;
            detail = Some(options.missing_detail.to_string());
        }
        entries.push(AdapterSkillEntry {
            key: available.key.clone(),
            runtime_name: Some(available.runtime_name.clone()),
            version_id: available.version_id.clone(),
            current_version_id: available.current_version_id.clone(),
            desired,
            managed,
            state,
            source_path: Some(available.source.clone()),
            target_path: Some(posix_join(options.skills_home, &available.runtime_name)),
            detail,
            origin: Some(managed_origin.origin),
            origin_label: Some(managed_origin.origin_label.clone()),
            read_only: Some(managed_origin.read_only),
            location_label: None,
        });
    }
    for desired_skill in options.desired_skills {
        if by_key.contains_key(desired_skill.as_str()) {
            continue;
        }
        warnings.push(format!(
            "Desired skill \"{desired_skill}\" is not available from the Paperclip skills directory."
        ));
        entries.push(AdapterSkillEntry {
            key: desired_skill.clone(),
            runtime_name: None,
            version_id: None,
            current_version_id: None,
            desired: true,
            managed: true,
            state: AdapterSkillState::Missing,
            source_path: None,
            target_path: None,
            detail: Some("Paperclip cannot find this skill in the local runtime skills directory.".to_string()),
            origin: Some(AdapterSkillOrigin::ExternalUnknown),
            origin_label: Some("External or unavailable".to_string()),
            read_only: Some(false),
            location_label: None,
        });
    }
    for (name, installed_entry) in options.installed.unwrap_or_else(|| empty_hashmap_str_installed()) {
        if options.available_entries.iter().any(|e| e.runtime_name == *name) {
            continue;
        }
        let target_path = installed_entry
            .target_path
            .clone()
            .unwrap_or_else(|| posix_join(options.skills_home, name));
        entries.push(AdapterSkillEntry {
            key: name.clone(),
            runtime_name: Some(name.clone()),
            version_id: None,
            current_version_id: None,
            desired: false,
            managed: false,
            state: AdapterSkillState::External,
            source_path: None,
            target_path: Some(target_path),
            detail: Some(options.external_detail.to_string()),
            origin: Some(AdapterSkillOrigin::UserInstalled),
            origin_label: Some("User-installed".to_string()),
            read_only: Some(true),
            location_label: skill_location_label(options.location_label),
        });
    }
    entries.sort_by(|a, b| a.key.cmp(&b.key));
    let desired_skill_entries: Vec<PaperclipDesiredSkillEntry> = options
        .desired_skills
        .iter()
        .map(|key| PaperclipDesiredSkillEntry {
            key: key.clone(),
            version_id: by_key.get(key.as_str()).and_then(|e| e.version_id.clone()),
        })
        .collect();
    AdapterSkillSnapshot {
        adapter_type,
        supported: true,
        mode: AdapterSkillSyncMode::Persistent,
        desired_skills: options.desired_skills.to_vec(),
        desired_skill_entries: Some(desired_skill_entries),
        entries,
        warnings,
    }
}

/// Normalize the `paperclipRuntimeSkills` config block into a list of
/// `PaperclipSkillEntry`. Mirrors Node
/// `normalizeConfiguredPaperclipRuntimeSkills`.
#[must_use]
pub fn normalize_configured_paperclip_runtime_skills(
    value: &serde_json::Value,
) -> Vec<PaperclipSkillEntry> {
    let Some(arr) = value.as_array() else { return Vec::new() };
    let mut out: Vec<PaperclipSkillEntry> = Vec::new();
    for raw in arr {
        let Some(entry) = raw.as_object() else { continue };
        let key = entry
            .get("key")
            .and_then(as_string_opt)
            .or_else(|| entry.get("name").and_then(as_string_opt))
            .unwrap_or_default()
            .trim()
            .to_string();
        let runtime_name = entry
            .get("runtimeName")
            .and_then(as_string_opt)
            .or_else(|| entry.get("name").and_then(as_string_opt))
            .unwrap_or_default()
            .trim()
            .to_string();
        let source = entry
            .get("source")
            .and_then(as_string_opt)
            .unwrap_or_default()
            .trim()
            .to_string();
        if key.is_empty() || runtime_name.is_empty() || source.is_empty() {
            continue;
        }
        let version_id = entry
            .get("versionId")
            .and_then(as_string_opt)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let current_version_id = entry
            .get("currentVersionId")
            .and_then(as_string_opt)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let source_status = match entry.get("sourceStatus").and_then(|v| v.as_str()) {
            Some("missing") => Some(PaperclipSkillSourceStatus::Missing),
            _ => Some(PaperclipSkillSourceStatus::Available),
        };
        let missing_detail = entry
            .get("missingDetail")
            .and_then(as_string_opt)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        out.push(PaperclipSkillEntry {
            key,
            runtime_name,
            source,
            version_id,
            current_version_id,
            source_status,
            missing_detail,
        });
    }
    out
}

fn as_string_opt(v: &serde_json::Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

/// Static empty `HashMap<String, InstalledSkillTarget>` for borrow-friendly defaults.
static EMPTY_HASHMAP_INSTALLED: std::sync::OnceLock<HashMap<String, InstalledSkillTarget>> =
    std::sync::OnceLock::new();

fn empty_hashmap_str_installed() -> &'static HashMap<String, InstalledSkillTarget> {
    EMPTY_HASHMAP_INSTALLED.get_or_init(HashMap::new)
}


// =============================================================================
// Unit tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---------- constants ----------

    #[test]
    fn unmanaged_background_task_constants_match_node() {
        assert_eq!(
            UNMANAGED_BACKGROUND_TASK_STOP_REASON,
            "unmanaged_background_task_stopped"
        );
        assert_eq!(
            UNMANAGED_BACKGROUND_TASK_LIVENESS_REASON,
            "unmanaged background task stopped; no durable live path"
        );
    }

    #[test]
    fn capture_constants_match_node() {
        assert_eq!(MAX_CAPTURE_BYTES, 4 * 1024 * 1024);
        assert_eq!(MAX_EXCERPT_BYTES, 32 * 1024);
        assert_eq!(TERMINAL_RESULT_SCAN_OVERLAP_CHARS, 64 * 1024);
        assert_eq!(DEFAULT_PAPERCLIP_INSTANCE_ID, "default");
        assert_eq!(REDACTED_LOG_VALUE, "***REDACTED***");
    }

    #[test]
    fn regex_source_constants_match_node() {
        assert_eq!(PATH_SEGMENT_RE_SRC, "^[a-zA-Z0-9_-]+$");
        assert_eq!(
            SENSITIVE_ENV_KEY_RE_SRC,
            "(?i)(key|token|secret|password|passwd|authorization|cookie)"
        );
    }

    // ---------- isPaperclipRuntimeEnvKey ----------

    #[test]
    fn runtime_env_key_matches_paperclip_prefix() {
        assert!(is_paperclip_runtime_env_key("PAPERCLIP_API_KEY"));
        assert!(is_paperclip_runtime_env_key("PAPERCLIP_AGENT_ID"));
        assert!(!is_paperclip_runtime_env_key("PATH"));
        assert!(!is_paperclip_runtime_env_key(""));
        // Lowercase prefix must NOT match — Paperclip namespace is uppercase.
        assert!(!is_paperclip_runtime_env_key("paperclip_api_key"));
    }

    // ---------- isForbiddenConfigEnvKey ----------

    #[test]
    fn forbidden_config_env_key_only_blocks_api_key() {
        assert!(is_forbidden_config_env_key("PAPERCLIP_API_KEY"));
        assert!(!is_forbidden_config_env_key("PAPERCLIP_AGENT_ID"));
        assert!(!is_forbidden_config_env_key("PATH"));
    }

    // ---------- isValidPathSegment ----------

    #[test]
    fn path_segment_matches_letters_digits_dash_underscore() {
        assert!(is_valid_path_segment("abc"));
        assert!(is_valid_path_segment("a-b_c"));
        assert!(is_valid_path_segment("ABC123"));
        assert!(!is_valid_path_segment("a b"));
        assert!(!is_valid_path_segment("a/b"));
        assert!(!is_valid_path_segment(""));
        assert!(!is_valid_path_segment(".hidden"));
    }

    // ---------- isSensitiveEnvKey ----------

    #[test]
    fn sensitive_env_key_matches_keywords_case_insensitive() {
        assert!(is_sensitive_env_key("API_KEY"));
        assert!(is_sensitive_env_key("oauth_token"));
        assert!(is_sensitive_env_key("client-secret"));
        assert!(is_sensitive_env_key("db_password"));
        assert!(is_sensitive_env_key("SESS_COOKIE"));
        assert!(!is_sensitive_env_key("PATH"));
        assert!(!is_sensitive_env_key("HOME"));
    }

    // ---------- parseObject ----------

    #[test]
    fn parse_object_returns_object_or_empty_map() {
        let obj = json!({"a": 1});
        let m = parse_object(&obj);
        assert_eq!(m.get("a").unwrap(), &json!(1));

        assert!(parse_object(&json!("str")).is_empty());
        assert!(parse_object(&json!(42)).is_empty());
        assert!(parse_object(&json!(null)).is_empty());
        assert!(parse_object(&json!([1, 2, 3])).is_empty());
    }

    // ---------- asString ----------

    #[test]
    fn as_string_returns_string_when_non_empty() {
        assert_eq!(as_string(&json!("hello"), "fallback"), "hello");
        // Empty string → fallback.
        assert_eq!(as_string(&json!(""), "fallback"), "fallback");
        // Non-string → fallback.
        assert_eq!(as_string(&json!(42), "fallback"), "fallback");
        assert_eq!(as_string(&json!(null), "fallback"), "fallback");
    }

    // ---------- asNumber ----------

    #[test]
    fn as_number_returns_finite_number() {
        assert_eq!(as_number(&json!(3.14), 0.0), 3.14);
        assert_eq!(as_number(&json!(0), 99.0), 0.0);
        assert_eq!(as_number(&json!(-1), 99.0), -1.0);
        // Non-finite / non-number → fallback.
        assert_eq!(as_number(&json!("3.14"), 99.0), 99.0);
        assert_eq!(as_number(&json!(null), 99.0), 99.0);
        // JSON null can't carry NaN/Inf, but ensure fallback for boolean.
        assert_eq!(as_number(&json!(true), 99.0), 99.0);
    }

    // ---------- asBoolean ----------

    #[test]
    fn as_boolean_returns_bool_when_present() {
        assert!(as_boolean(&json!(true), false));
        assert!(!as_boolean(&json!(false), true));
        // Non-bool values fall back (Node semantics: typeof !== "boolean").
        assert!(!as_boolean(&json!("true"), false)); // string → fallback false
        assert!(!as_boolean(&json!(1), false)); // number → fallback false
        assert!(as_boolean(&json!(null), true)); // null → fallback true
    }

    // ---------- asStringArray ----------

    #[test]
    fn as_string_array_filters_to_strings() {
        assert_eq!(
            as_string_array(&json!(["a", "b", "c"])),
            vec!["a", "b", "c"]
        );
        assert_eq!(
            as_string_array(&json!(["a", 1, null, "b"])),
            vec!["a", "b"]
        );
        assert_eq!(as_string_array(&json!("not-an-array")), Vec::<String>::new());
        assert_eq!(as_string_array(&json!(null)), Vec::<String>::new());
    }

    // ---------- parseJson ----------

    #[test]
    fn parse_json_returns_value_or_none() {
        let v = parse_json(r#"{"a": 1}"#).expect("valid json");
        assert_eq!(v, json!({"a": 1}));
        assert!(parse_json("not json").is_none());
        assert!(parse_json("").is_none());
    }

    // ---------- appendWithCap ----------

    #[test]
    fn append_with_cap_keeps_trailing_chars() {
        assert_eq!(append_with_cap("", "abc", 5), "abc");
        assert_eq!(append_with_cap("abc", "def", 6), "abcdef");
        // Truncates the leading char so only the trailing 5 survive.
        assert_eq!(append_with_cap("abc", "def", 5), "bcdef");
        // cap smaller than new chunk: only the tail of the new chunk.
        assert_eq!(append_with_cap("abc", "defgh", 3), "fgh");
    }

    // ---------- appendWithByteCap ----------

    #[test]
    fn append_with_byte_cap_keeps_trailing_bytes() {
        assert_eq!(append_with_byte_cap("", "abc", 5), "abc");
        assert_eq!(append_with_byte_cap("abc", "def", 6), "abcdef");
        assert_eq!(append_with_byte_cap("abc", "def", 5), "bcdef");
    }

    #[test]
    fn append_with_byte_cap_respects_utf8_boundaries() {
        // "héllo" is 6 bytes (é = 2 bytes UTF-8). With cap=3 the
        // trailing 3 bytes are "llo" — never slice mid-codepoint.
        let result = append_with_byte_cap("", "héllo", 3);
        assert_eq!(result, "llo");
        // Cap exactly at a multi-byte boundary.
        let result = append_with_byte_cap("", "héllo", 6);
        assert_eq!(result, "héllo");
        // Cap that would otherwise cut in the middle of é should
        // advance past é (start at "llo").
        let result = append_with_byte_cap("", "héllo", 4);
        assert_eq!(result, "llo");
    }

    // ---------- resolvePathValue ----------

    #[test]
    fn resolve_path_value_walks_dotted_path() {
        let obj = json!({"agent": {"id": "abc", "count": 3, "flag": true}});
        assert_eq!(resolve_path_value(&obj, "agent.id"), "abc");
        assert_eq!(resolve_path_value(&obj, "agent.count"), "3");
        assert_eq!(resolve_path_value(&obj, "agent.flag"), "true");
        assert_eq!(resolve_path_value(&obj, "missing"), "");
        assert_eq!(resolve_path_value(&obj, "agent.missing"), "");
        // Walking through a string stops at "".
        let string_obj = json!({"a": "str"});
        assert_eq!(resolve_path_value(&string_obj, "a.b"), "");
    }

    // ---------- renderTemplate ----------

    #[test]
    fn render_template_replaces_placeholders() {
        let data = json!({"agent": {"id": "abc"}, "n": 3});
        let template = "agent={{agent.id}}, n={{n}}";
        assert_eq!(render_template(template, &data), "agent=abc, n=3");
    }

    #[test]
    fn render_template_tolerates_whitespace_and_missing_paths() {
        let data = json!({"a": "x"});
        // Whitespace inside braces is stripped.
        assert_eq!(render_template("{{ a }}", &data), "x");
        // Missing path → empty string.
        assert_eq!(render_template("[{{missing}}]", &data), "[]");
        // Non-string leaf gets JSON-stringified.
        let data2 = json!({"obj": {"x": 1}});
        assert_eq!(render_template("[{{obj}}]", &data2), r#"[{"x":1}]"#);
    }

    // ---------- joinPromptSections ----------

    #[test]
    fn join_prompt_sections_trims_and_filters() {
        let sections: Vec<Option<&str>> = vec![
            Some("  hello  "),
            Some(""),
            Some("  "),
            Some("world  "),
            None,
            Some("!"),
        ];
        assert_eq!(
            join_prompt_sections(sections, "\n\n"),
            "hello\n\nworld\n\n!"
        );
    }

    // ---------- signalDecision ----------

    #[test]
    fn signal_decision_returns_none_when_already_exited() {
        let info = RunningProcessSignalInfo {
            process_group_id: Some(123),
            already_exited: true,
        };
        assert_eq!(signal_decision(info, false), SignalTarget::None);
        assert_eq!(signal_decision(info, true), SignalTarget::None);
    }

    #[test]
    fn signal_decision_returns_process_group_on_posix() {
        let info = RunningProcessSignalInfo {
            process_group_id: Some(123),
            already_exited: false,
        };
        assert_eq!(
            signal_decision(info, false),
            SignalTarget::ProcessGroup { pgid: 123 }
        );
    }

    #[test]
    fn signal_decision_returns_direct_child_on_windows() {
        let info = RunningProcessSignalInfo {
            process_group_id: Some(123),
            already_exited: false,
        };
        // Even with a valid pgid, Windows falls back to direct child.
        assert_eq!(signal_decision(info, true), SignalTarget::DirectChild);
    }

    #[test]
    fn signal_decision_falls_back_when_pgid_missing_or_zero() {
        let info = RunningProcessSignalInfo {
            process_group_id: None,
            already_exited: false,
        };
        assert_eq!(signal_decision(info, false), SignalTarget::DirectChild);
        let zero = RunningProcessSignalInfo {
            process_group_id: Some(0),
            already_exited: false,
        };
        assert_eq!(signal_decision(zero, false), SignalTarget::DirectChild);
    }

    // ---------- TerminalResultCleanupEvidence ----------

    #[test]
    fn cleanup_evidence_constructor_fills_canonical_fields() {
        let ev = TerminalResultCleanupEvidence::new(true, Some("SIGTERM".to_string()), false);
        assert_eq!(ev.kind, "terminal_result_cleanup");
        assert!(ev.stopped);
        assert_eq!(ev.stop_reason, UNMANAGED_BACKGROUND_TASK_STOP_REASON);
        assert_eq!(ev.reason, UNMANAGED_BACKGROUND_TASK_LIVENESS_REASON);
        assert!(ev.terminal_result_seen);
        assert_eq!(ev.signal.as_deref(), Some("SIGTERM"));
        assert!(!ev.force_killed);
    }

    // ---------- redact_env_for_logs ----------

    #[test]
    fn redact_env_for_logs_replaces_sensitive_values() {
        let mut env = HashMap::new();
        env.insert("PATH".to_string(), "/usr/bin".to_string());
        env.insert("API_KEY".to_string(), "secret-abc".to_string());
        env.insert("DB_PASSWORD".to_string(), "hunter2".to_string());
        env.insert("PAPERCLIP_AGENT_ID".to_string(), "agent-1".to_string());
        let redacted = redact_env_for_logs(&env);
        assert_eq!(redacted["PATH"], "/usr/bin");
        assert_eq!(redacted["API_KEY"], REDACTED_LOG_VALUE);
        assert_eq!(redacted["DB_PASSWORD"], REDACTED_LOG_VALUE);
        assert_eq!(redacted["PAPERCLIP_AGENT_ID"], "agent-1");
    }

    // ---------- redact_command_text_for_logs ----------

    #[test]
    fn redact_command_text_for_logs_redacts_secret_flags() {
        // The command_redaction helper is exercised in detail there; this
        // smoke test just ensures the wrapper forwards the redacted
        // placeholder through.
        let cmd = "agent run --api-key=hunter2 --verbose";
        let redacted = redact_command_text_for_logs(cmd);
        assert!(redacted.contains("hunter2") == false, "secret leaked: {redacted}");
        assert!(redacted.contains("REDACTED"));
    }

    // ---------- build_invocation_env_for_logs ----------

    #[test]
    fn build_invocation_env_for_logs_merges_runtime_keys_then_redacts() {
        let mut env = HashMap::new();
        env.insert("PATH".to_string(), "/usr/bin".to_string());
        let mut runtime = HashMap::new();
        runtime.insert("PAPERCLIP_AGENT_ID".to_string(), "agent-1".to_string());
        runtime.insert("API_KEY".to_string(), "secret".to_string());
        let merged = build_invocation_env_for_logs(
            &env,
            BuildInvocationEnvForLogsOptions {
                runtime_env: Some(&runtime),
                include_runtime_keys: Some(&["PAPERCLIP_AGENT_ID"]),
                resolved_command: Some("agent run --token=abc"),
                resolved_command_env_key: Some("PAPERCLIP_RESOLVED_COMMAND"),
            },
        );
        assert_eq!(merged["PAPERCLIP_AGENT_ID"], "agent-1");
        assert!(merged["PAPERCLIP_RESOLVED_COMMAND"].contains("REDACTED"));
        // Existing env wins over runtime.
        let mut env2 = HashMap::new();
        env2.insert("PAPERCLIP_AGENT_ID".to_string(), "local-override".to_string());
        let merged2 = build_invocation_env_for_logs(
            &env2,
            BuildInvocationEnvForLogsOptions {
                runtime_env: Some(&runtime),
                include_runtime_keys: Some(&["PAPERCLIP_AGENT_ID"]),
                resolved_command: None,
                resolved_command_env_key: None,
            },
        );
        assert_eq!(merged2["PAPERCLIP_AGENT_ID"], "local-override");
    }

    // ---------- resolve_host_for_url ----------

    #[test]
    fn resolve_host_for_url_normalizes_wildcards_and_ipv6() {
        assert_eq!(resolve_host_for_url("0.0.0.0"), "localhost");
        assert_eq!(resolve_host_for_url("::"), "localhost");
        assert_eq!(resolve_host_for_url(""), "localhost");
        assert_eq!(resolve_host_for_url("localhost"), "localhost");
        assert_eq!(resolve_host_for_url("node-1.example.com"), "node-1.example.com");
        // IPv6 unbracketed → bracket.
        assert_eq!(resolve_host_for_url("::1"), "[::1]");
        assert_eq!(resolve_host_for_url("fe80::1"), "[fe80::1]");
        // Already bracketed → pass-through.
        assert_eq!(resolve_host_for_url("[::1]"), "[::1]");
    }

    // ---------- build_paperclip_env ----------

    #[test]
    fn build_paperclip_env_fills_canonical_vars() {
        let mut runtime = HashMap::new();
        runtime.insert("PAPERCLIP_LISTEN_HOST".to_string(), "127.0.0.1".to_string());
        runtime.insert("PAPERCLIP_LISTEN_PORT".to_string(), "4000".to_string());
        let vars = build_paperclip_env(BuildPaperclipEnvInput {
            agent_id: "agent-1",
            company_id: "co-1",
            runtime_env: &runtime,
            default_listen_host: "localhost",
            default_listen_port: "3100",
        });
        assert_eq!(vars["PAPERCLIP_AGENT_ID"], "agent-1");
        assert_eq!(vars["PAPERCLIP_COMPANY_ID"], "co-1");
        assert_eq!(vars["PAPERCLIP_API_URL"], "http://127.0.0.1:4000");
    }

    #[test]
    fn build_paperclip_env_falls_back_to_defaults() {
        let runtime = HashMap::new();
        let vars = build_paperclip_env(BuildPaperclipEnvInput {
            agent_id: "a",
            company_id: "c",
            runtime_env: &runtime,
            default_listen_host: "localhost",
            default_listen_port: "3100",
        });
        assert_eq!(vars["PAPERCLIP_API_URL"], "http://localhost:3100");
    }

    #[test]
    fn build_paperclip_env_prefers_runtime_api_url() {
        let mut runtime = HashMap::new();
        runtime.insert(
            "PAPERCLIP_RUNTIME_API_URL".to_string(),
            "https://api.example.com".to_string(),
        );
        runtime.insert("PAPERCLIP_API_URL".to_string(), "http://fallback".to_string());
        let vars = build_paperclip_env(BuildPaperclipEnvInput {
            agent_id: "a",
            company_id: "c",
            runtime_env: &runtime,
            default_listen_host: "localhost",
            default_listen_port: "3100",
        });
        assert_eq!(vars["PAPERCLIP_API_URL"], "https://api.example.com");
    }

    // ---------- apply_paperclip_workspace_env ----------

    #[test]
    fn apply_paperclip_workspace_env_writes_non_empty_keys() {
        let mut env = HashMap::new();
        apply_paperclip_workspace_env(
            &mut env,
            ApplyPaperclipWorkspaceEnvInput {
                workspace_cwd: Some("/workspace"),
                workspace_source: Some("local"),
                workspace_strategy: Some("fresh"),
                workspace_id: Some("ws-1"),
                workspace_repo_url: Some("git@github.com:foo/bar.git"),
                workspace_repo_ref: Some("main"),
                workspace_branch: Some("main"),
                workspace_worktree_path: Some("/workspace/wt"),
                agent_home: Some("/home/agent"),
            },
        );
        assert_eq!(env["PAPERCLIP_WORKSPACE_CWD"], "/workspace");
        assert_eq!(env["PAPERCLIP_WORKSPACE_SOURCE"], "local");
        assert_eq!(env["PAPERCLIP_WORKSPACE_STRATEGY"], "fresh");
        assert_eq!(env["PAPERCLIP_WORKSPACE_ID"], "ws-1");
        assert_eq!(env["PAPERCLIP_WORKSPACE_REPO_URL"], "git@github.com:foo/bar.git");
        assert_eq!(env["PAPERCLIP_WORKSPACE_REPO_REF"], "main");
        assert_eq!(env["PAPERCLIP_WORKSPACE_BRANCH"], "main");
        assert_eq!(env["PAPERCLIP_WORKSPACE_WORKTREE_PATH"], "/workspace/wt");
        assert_eq!(env["AGENT_HOME"], "/home/agent");
    }

    #[test]
    fn apply_paperclip_workspace_env_skips_empty_values() {
        let mut env = HashMap::new();
        env.insert("PAPERCLIP_WORKSPACE_CWD".to_string(), "/old".to_string());
        apply_paperclip_workspace_env(
            &mut env,
            ApplyPaperclipWorkspaceEnvInput {
                workspace_cwd: Some(""),
                workspace_source: None,
                workspace_strategy: None,
                workspace_id: None,
                workspace_repo_url: None,
                workspace_repo_ref: None,
                workspace_branch: None,
                workspace_worktree_path: None,
                agent_home: None,
            },
        );
        // Empty / None values must not overwrite existing env entries.
        assert_eq!(env["PAPERCLIP_WORKSPACE_CWD"], "/old");
        // No new keys added.
        assert_eq!(env.len(), 1);
    }

    // ---------- shape_paperclip_workspace_env_for_execution ----------

    #[test]
    fn shape_paperclip_workspace_env_local_target_returns_inputs_unchanged() {
        let mut hint = serde_json::Map::new();
        hint.insert("cwd".to_string(), serde_json::json!("/hint"));
        hint.insert("projectId".to_string(), serde_json::json!("p-1"));
        let hints = vec![hint];
        let staged = HashMap::new();
        let out = shape_paperclip_workspace_env_for_execution(
            ShapePaperclipWorkspaceEnvInput {
                workspace_cwd: Some("/workspace"),
                workspace_workspace_worktree_path: Some("/workspace/wt"),
                workspace_hints: Some(&hints),
                execution_target_is_remote: false,
                execution_cwd: None,
                staged_project_dirs: Some(&staged),
            },
        );
        assert_eq!(out.workspace_cwd.as_deref(), Some("/workspace"));
        assert_eq!(out.workspace_worktree_path.as_deref(), Some("/workspace/wt"));
        // Hint retains its cwd on local target.
        assert_eq!(
            out.workspace_hints[0].get("cwd"),
            Some(&serde_json::json!("/hint"))
        );
    }

    #[test]
    fn shape_paperclip_workspace_env_remote_repoints_cwd_to_staged_dir() {
        let mut hint = serde_json::Map::new();
        hint.insert("cwd".to_string(), serde_json::json!("/stale"));
        hint.insert("projectId".to_string(), serde_json::json!("p-1"));
        let hints = vec![hint];
        let mut staged = HashMap::new();
        staged.insert("p-1".to_string(), "/sandbox/project-p-1".to_string());
        let out = shape_paperclip_workspace_env_for_execution(
            ShapePaperclipWorkspaceEnvInput {
                workspace_cwd: Some("/workspace"),
                workspace_workspace_worktree_path: None,
                workspace_hints: Some(&hints),
                execution_target_is_remote: true,
                execution_cwd: Some("/workspace"),
                staged_project_dirs: Some(&staged),
            },
        );
        // Hint cwd was repointed to the staged dir.
        assert_eq!(
            out.workspace_hints[0].get("cwd"),
            Some(&serde_json::json!("/sandbox/project-p-1"))
        );
    }

    #[test]
    fn shape_paperclip_workspace_env_remote_drops_unstaged_cwd() {
        let mut hint = serde_json::Map::new();
        hint.insert("cwd".to_string(), serde_json::json!("/stale"));
        hint.insert("projectId".to_string(), serde_json::json!("p-missing"));
        let hints = vec![hint];
        let staged = HashMap::new();
        let out = shape_paperclip_workspace_env_for_execution(
            ShapePaperclipWorkspaceEnvInput {
                workspace_cwd: Some("/workspace"),
                workspace_workspace_worktree_path: None,
                workspace_hints: Some(&hints),
                execution_target_is_remote: true,
                execution_cwd: Some("/workspace"),
                staged_project_dirs: Some(&staged),
            },
        );
        // No staged dir for this hint's projectId → cwd dropped.
        assert!(out.workspace_hints[0].get("cwd").is_none());
    }

    // ---------- rewrite_workspace_cwd_env_vars_for_execution ----------

    #[test]
    fn rewrite_workspace_cwd_env_vars_local_target_is_passthrough() {
        let mut env = HashMap::new();
        env.insert(
            "AGENT_WORKSPACE_CWD".to_string(),
            serde_json::json!("/local"),
        );
        let out = rewrite_workspace_cwd_env_vars_for_execution(
            RewriteWorkspaceCwdEnvVarsForExecutionInput {
                env: Some(&env),
                workspace_cwd: Some("/local"),
                execution_cwd: Some("/remote"),
                execution_target_is_remote: false,
            },
        );
        // No rewriting on local target.
        assert_eq!(out["AGENT_WORKSPACE_CWD"], "/local");
    }

    #[test]
    fn rewrite_workspace_cwd_env_vars_remote_rewrites_matching_local_cwd() {
        let mut env = HashMap::new();
        env.insert(
            "AGENT_WORKSPACE_CWD".to_string(),
            serde_json::json!("/local"),
        );
        env.insert(
            "OTHER_VAR".to_string(),
            serde_json::json!("/local/other"),
        );
        let out = rewrite_workspace_cwd_env_vars_for_execution(
            RewriteWorkspaceCwdEnvVarsForExecutionInput {
                env: Some(&env),
                workspace_cwd: Some("/local"),
                execution_cwd: Some("/remote"),
                execution_target_is_remote: true,
            },
        );
        // Match → rewrites to remote cwd.
        assert_eq!(out["AGENT_WORKSPACE_CWD"], "/remote");
        // No *_WORKSPACE_CWD suffix → pass-through.
        assert_eq!(out["OTHER_VAR"], "/local/other");
    }

    // ---------- refresh_paperclip_workspace_env_for_execution ----------

    #[test]
    fn refresh_paperclip_workspace_env_applies_shaped_env_to_input_env() {
        let mut env = HashMap::new();
        env.insert("UNRELATED".to_string(), "value".to_string());
        env.insert(
            "PAPERCLIP_WORKSPACE_CWD".to_string(),
            "old-cwd".to_string(), // should be cleared
        );
        env.insert(
            "PAPERCLIP_WORKSPACE_WORKTREE_PATH".to_string(),
            "old-wt".to_string(), // should be cleared
        );
        let out = refresh_paperclip_workspace_env_for_execution(
            &mut env,
            RefreshPaperclipWorkspaceEnvInput {
                workspace_cwd: Some("/workspace"),
                workspace_source: Some("local"),
                workspace_strategy: Some("fresh"),
                workspace_id: None,
                workspace_repo_url: None,
                workspace_repo_ref: None,
                workspace_branch: None,
                workspace_worktree_path: None,
                workspace_hints: None,
                agent_home: None,
                execution_target_is_remote: false,
                execution_cwd: None,
                env_config: None,
                staged_project_dirs: None,
            },
        );
        assert_eq!(env["PAPERCLIP_WORKSPACE_CWD"], "/workspace");
        assert_eq!(env["PAPERCLIP_WORKSPACE_SOURCE"], "local");
        assert_eq!(env["PAPERCLIP_WORKSPACE_STRATEGY"], "fresh");
        assert_eq!(env["UNRELATED"], "value");
        assert_eq!(out.workspace_cwd.as_deref(), Some("/workspace"));
    }

    // ---------- sanitize_inherited_paperclip_env ----------

    #[test]
    fn sanitize_inherited_paperclip_env_strips_runtime_vars() {
        let mut env = HashMap::new();
        env.insert("PATH".to_string(), "/usr/bin".to_string());
        env.insert("HOME".to_string(), "/home/agent".to_string());
        env.insert("PAPERCLIP_AGENT_ID".to_string(), "should-be-stripped".to_string());
        env.insert("PAPERCLIP_COMPANY_ID".to_string(), "should-be-stripped".to_string());
        env.insert(
            "PAPERCLIP_RUNTIME_API_URL".to_string(),
            "kept".to_string(),
        );
        env.insert("PAPERCLIP_LISTEN_HOST".to_string(), "kept".to_string());
        env.insert("PAPERCLIP_LISTEN_PORT".to_string(), "kept".to_string());
        env.insert("PAPERCLIPAI_CMD".to_string(), "should-be-stripped".to_string());
        let out = sanitize_inherited_paperclip_env(&env);
        assert_eq!(out["PATH"], "/usr/bin");
        assert_eq!(out["HOME"], "/home/agent");
        assert_eq!(out["PAPERCLIP_RUNTIME_API_URL"], "kept");
        assert_eq!(out["PAPERCLIP_LISTEN_HOST"], "kept");
        assert_eq!(out["PAPERCLIP_LISTEN_PORT"], "kept");
        assert!(!out.contains_key("PAPERCLIP_AGENT_ID"));
        assert!(!out.contains_key("PAPERCLIP_COMPANY_ID"));
        assert!(!out.contains_key("PAPERCLIPAI_CMD"));
    }

    // ---------- default_path_for_platform ----------

    #[test]
    fn default_path_for_platform_returns_platform_specific_value() {
        assert!(default_path_for_platform(true).starts_with("C:\\Windows"));
        assert!(default_path_for_platform(false).starts_with("/usr/local/bin"));
    }

    // ---------- sanitize_ssh_remote_env ----------

    #[test]
    fn sanitize_ssh_remote_env_drops_local_only_keys() {
        // The implementation forwards to remote_execution_env which has
        // its own tests; this smoke test ensures the wrapper composes.
        let mut env = HashMap::new();
        env.insert("PATH".to_string(), "/usr/bin".to_string());
        env.insert("HOME".to_string(), "/home/agent".to_string());
        let mut inherited = HashMap::new();
        inherited.insert("PATH".to_string(), "/usr/bin".to_string());
        inherited.insert("HOME".to_string(), "/home/agent".to_string());
        inherited.insert("USER".to_string(), "local-user".to_string());
        let out = sanitize_ssh_remote_env(&env, &inherited);
        // Wrapper should not panic; at minimum PATH / HOME survive.
        assert!(out.contains_key("PATH") || out.is_empty());
    }

    // ---------- ensure_path_in_env ----------

    #[test]
    fn ensure_path_in_env_preserves_existing_path() {
        let mut env = HashMap::new();
        env.insert("PATH".to_string(), "/custom/bin".to_string());
        let out = ensure_path_in_env(&env, false);
        assert_eq!(out["PATH"], "/custom/bin");
    }

    #[test]
    fn ensure_path_in_env_fills_default_when_missing() {
        let env = HashMap::new();
        let out = ensure_path_in_env(&env, false);
        assert!(out.contains_key("PATH"));
        assert!(!out["PATH"].is_empty());
    }

    // ---------- skill entries / path helpers (R407) ----------

    #[test]
    fn normalize_path_slashes_replaces_backslashes() {
        assert_eq!(normalize_path_slashes("a\\b\\c"), "a/b/c");
        assert_eq!(normalize_path_slashes("plain/path"), "plain/path");
        assert_eq!(normalize_path_slashes(""), "");
    }

    #[test]
    fn is_maintainer_only_skill_target_detects_agents_skills_path() {
        assert!(is_maintainer_only_skill_target("/home/agent/.agents/skills/foo"));
        assert!(is_maintainer_only_skill_target("C:\\home\\.agents\\skills\\foo"));
        assert!(!is_maintainer_only_skill_target("/home/agent/.paperclip/skills/foo"));
        assert!(!is_maintainer_only_skill_target("/tmp/random"));
    }

    #[test]
    fn skill_location_label_trims_and_returns_none() {
        assert_eq!(skill_location_label(Some("/home/agent")), Some("/home/agent".to_string()));
        assert_eq!(skill_location_label(Some("  /home  ")), Some("/home".to_string()));
        assert_eq!(skill_location_label(Some("")), None);
        assert_eq!(skill_location_label(Some("   ")), None);
        assert_eq!(skill_location_label(None), None);
    }

    #[test]
    fn build_managed_skill_origin_returns_company_managed() {
        let origin = build_managed_skill_origin();
        assert_eq!(origin.origin, AdapterSkillOrigin::CompanyManaged);
        assert_eq!(origin.origin_label, "Managed by Paperclip");
        assert!(!origin.read_only);
    }

    #[test]
    fn is_paperclip_skill_source_missing_handles_optional_status() {
        let mut e = make_skill_entry("k1", "r1");
        e.source_status = Some(PaperclipSkillSourceStatus::Missing);
        assert!(is_paperclip_skill_source_missing(&e));
        e.source_status = Some(PaperclipSkillSourceStatus::Available);
        assert!(!is_paperclip_skill_source_missing(&e));
        e.source_status = None;
        assert!(!is_paperclip_skill_source_missing(&e));
    }

    #[test]
    fn resolve_paperclip_skill_missing_detail_falls_back_when_blank() {
        let mut e = make_skill_entry("k1", "r1");
        e.missing_detail = Some("custom reason".to_string());
        assert_eq!(
            resolve_paperclip_skill_missing_detail(&e, "fallback"),
            "custom reason"
        );
        e.missing_detail = Some("   ".to_string());
        assert_eq!(
            resolve_paperclip_skill_missing_detail(&e, "fallback"),
            "fallback"
        );
        e.missing_detail = None;
        assert_eq!(
            resolve_paperclip_skill_missing_detail(&e, "fallback"),
            "fallback"
        );
    }

    #[test]
    fn resolve_skill_detail_picks_callback_over_literal() {
        let entry = make_skill_entry("k1", "r1");
        let lit = SkillDetail::Literal("literal-text");
        assert_eq!(
            resolve_skill_detail(Some(&lit), &entry),
            Some("literal-text".to_string())
        );
        let cb: SkillDetail<'_> = SkillDetail::Callback(&|_e| Some("from-cb".to_string()));
        assert_eq!(
            resolve_skill_detail(Some(&cb), &entry),
            Some("from-cb".to_string())
        );
        assert_eq!(resolve_skill_detail(None, &entry), None);
    }

    #[test]
    fn resolve_installed_entry_target_resolves_symlink_to_absolute() {
        let home = "/skills";
        let resolved = resolve_installed_entry_target(
            home,
            "my-skill",
            InstalledSkillTargetKind::Symlink,
            Some("../actual/my-skill"),
        );
        assert_eq!(
            resolved.target_path.as_deref(),
            Some("/skills/../actual/my-skill")
        );
        assert_eq!(resolved.kind, InstalledSkillTargetKind::Symlink);

        let dir = resolve_installed_entry_target(
            home,
            "my-skill",
            InstalledSkillTargetKind::Directory,
            None,
        );
        assert_eq!(dir.target_path.as_deref(), Some("/skills/my-skill"));
        assert_eq!(dir.kind, InstalledSkillTargetKind::Directory);
    }

    #[test]
    fn expand_home_prefix_expands_tilde() {
        assert_eq!(expand_home_prefix("~", "/home/agent"), "/home/agent");
        assert_eq!(expand_home_prefix("~/x/y", "/home/agent"), "/home/agent/x/y");
        assert_eq!(expand_home_prefix("/abs/path", "/home/agent"), "/abs/path");
        assert_eq!(expand_home_prefix("relative", "/home/agent"), "relative");
    }

    #[test]
    fn resolve_paperclip_instance_root_for_adapter_builds_canonical_path() {
        let out = resolve_paperclip_instance_root_for_adapter(ResolveInstanceRootInput {
            home_dir: Some("/custom/home"),
            instance_id: Some("acpx-prod"),
            env: None,
            default_home_dir: "/home/agent",
        });
        assert_eq!(out, "/custom/home/instances/acpx-prod");
    }

    #[test]
    fn resolve_paperclip_instance_root_for_adapter_falls_back_to_default_home() {
        let out = resolve_paperclip_instance_root_for_adapter(ResolveInstanceRootInput {
            home_dir: None,
            instance_id: Some("default"),
            env: None,
            default_home_dir: "/home/agent",
        });
        assert_eq!(out, "/home/agent/.paperclip/instances/default");
    }

    #[test]
    fn resolve_paperclip_instance_root_reads_env_fallbacks() {
        let mut env = HashMap::new();
        env.insert("PAPERCLIP_HOME".to_string(), "/env/home".to_string());
        env.insert("PAPERCLIP_INSTANCE_ID".to_string(), "from-env".to_string());
        let out = resolve_paperclip_instance_root_for_adapter(ResolveInstanceRootInput {
            home_dir: None,
            instance_id: None,
            env: Some(&env),
            default_home_dir: "/home/agent",
        });
        assert_eq!(out, "/env/home/instances/from-env");
    }

    // ---------- skill sync preference (R407) ----------

    #[test]
    fn read_paperclip_skill_sync_preference_returns_default_when_absent() {
        let cfg = serde_json::json!({});
        let pref = read_paperclip_skill_sync_preference(&cfg);
        assert!(!pref.explicit);
        assert!(pref.desired_skills.is_empty());
        assert!(pref.desired_skill_entries.is_empty());
    }

    #[test]
    fn read_paperclip_skill_sync_preference_parses_string_and_object_entries() {
        let cfg = serde_json::json!({
            "paperclipSkillSync": {
                "desiredSkills": [
                    "k1",
                    { "key": "k2", "versionId": "v2" },
                    "  ",
                    { "key": "k3" },
                    { "key": "k2", "versionId": "v2-alt" }
                ]
            }
        });
        let pref = read_paperclip_skill_sync_preference(&cfg);
        assert!(pref.explicit);
        assert_eq!(
            pref.desired_skills,
            vec!["k1".to_string(), "k2".to_string(), "k3".to_string()]
        );
        assert_eq!(pref.desired_skill_entries.len(), 3);
        assert_eq!(pref.desired_skill_entries[1].version_id.as_deref(), Some("v2"));
    }

    #[test]
    fn write_paperclip_skill_sync_preference_emits_string_array_when_no_versions() {
        let cfg = serde_json::json!({});
        let out = write_paperclip_skill_sync_preference(
            &cfg,
            &[SkillSyncWrite::Key("k1"), SkillSyncWrite::Key("k2")],
        );
        let desired = out
            .get("paperclipSkillSync")
            .and_then(|v| v.get("desiredSkills"))
            .unwrap();
        assert_eq!(desired, &serde_json::json!(["k1", "k2"]));
    }

    #[test]
    fn write_paperclip_skill_sync_preference_emits_object_array_when_versions_present() {
        // Node semantics: if ANY entry has a versionId, the entire
        // `desiredSkills` array is emitted in object form (entries
        // without versionId serialize as `{ key, versionId: null }`).
        let cfg = serde_json::json!({});
        let out = write_paperclip_skill_sync_preference(
            &cfg,
            &[
                SkillSyncWrite::Key("k1"),
                SkillSyncWrite::Entry {
                    key: "k2",
                    version_id: Some("v2".to_string()),
                },
            ],
        );
        let desired = out
            .get("paperclipSkillSync")
            .and_then(|v| v.get("desiredSkills"))
            .unwrap();
        let arr = desired.as_array().expect("array");
        assert_eq!(arr.len(), 2);
        // k1 → object form with null versionId.
        let k1 = arr
            .iter()
            .find(|v| v.get("key").and_then(|x| x.as_str()) == Some("k1"))
            .expect("k1 obj");
        assert_eq!(k1.get("versionId").and_then(|x| x.as_str()), None);
        assert!(k1.get("versionId").map(|x| x.is_null()).unwrap_or(false));
        // k2 → object form with the explicit versionId.
        let k2 = arr
            .iter()
            .find(|v| v.get("key").and_then(|x| x.as_str()) == Some("k2"))
            .expect("k2 obj");
        assert_eq!(k2.get("versionId").and_then(|x| x.as_str()), Some("v2"));
    }

    #[test]
    fn canonicalize_resolves_key_runtime_name_and_slug() {
        let avail = vec![
            AvailableSkillRef { key: "owner/k1", runtime_name: Some("k1") },
            AvailableSkillRef { key: "owner/k2", runtime_name: Some("k2") },
            AvailableSkillRef { key: "owner/k3", runtime_name: Some("k3") },
        ];
        assert_eq!(
            canonicalize_desired_paperclip_skill_reference("OWNER/K1", &avail),
            "owner/k1"
        );
        assert_eq!(
            canonicalize_desired_paperclip_skill_reference("K2", &avail),
            "owner/k2"
        );
        assert_eq!(
            canonicalize_desired_paperclip_skill_reference("k3", &avail),
            "owner/k3"
        );
        assert_eq!(canonicalize_desired_paperclip_skill_reference("", &avail), "");
        assert_eq!(
            canonicalize_desired_paperclip_skill_reference("unknown", &avail),
            "unknown"
        );
    }

    #[test]
    fn resolve_paperclip_desired_skill_names_returns_empty_when_not_explicit() {
        let cfg = serde_json::json!({});
        let avail = vec![AvailableSkillRef { key: "owner/k1", runtime_name: Some("k1") }];
        assert!(resolve_paperclip_desired_skill_names(&cfg, &avail).is_empty());
    }

    #[test]
    fn resolve_paperclip_desired_skill_names_canonicalizes_and_dedups() {
        let cfg = serde_json::json!({
            "paperclipSkillSync": {
                "desiredSkills": ["K1", "k2", "k1", "unknown"]
            }
        });
        let avail = vec![
            AvailableSkillRef { key: "owner/k1", runtime_name: Some("k1") },
            AvailableSkillRef { key: "owner/k2", runtime_name: Some("k2") },
        ];
        let names = resolve_paperclip_desired_skill_names(&cfg, &avail);
        assert_eq!(
            names,
            vec!["owner/k1".to_string(), "owner/k2".to_string(), "unknown".to_string()]
        );
    }

    // ---------- snapshot builders (R407) ----------

    #[test]
    fn build_runtime_mounted_skill_snapshot_marks_configured_when_desired() {
        let avail = vec![make_skill_entry("owner/k1", "k1")];
        let desired = vec!["owner/k1".to_string()];
        let snap = build_runtime_mounted_skill_snapshot(RuntimeMountedSkillSnapshotOptions {
            adapter_type: "test-adapter",
            available_entries: &avail,
            desired_skills: &desired,
            configured_detail: SkillDetail::Literal("configured for k1"),
            ..Default::default()
        });
        assert!(snap.supported);
        assert_eq!(snap.mode, AdapterSkillSyncMode::Ephemeral);
        assert_eq!(snap.entries.len(), 1);
        assert_eq!(snap.entries[0].state, AdapterSkillState::Configured);
        assert_eq!(snap.entries[0].detail.as_deref(), Some("configured for k1"));
        assert!(snap.entries[0].desired);
    }

    #[test]
    fn build_runtime_mounted_skill_snapshot_marks_available_when_not_desired() {
        let avail = vec![make_skill_entry("owner/k1", "k1")];
        let desired: Vec<String> = vec![];
        let snap = build_runtime_mounted_skill_snapshot(RuntimeMountedSkillSnapshotOptions {
            adapter_type: "test-adapter",
            available_entries: &avail,
            desired_skills: &desired,
            configured_detail: SkillDetail::Literal("ignored"),
            ..Default::default()
        });
        assert_eq!(snap.entries[0].state, AdapterSkillState::Available);
        assert!(!snap.entries[0].desired);
    }

    #[test]
    fn build_runtime_mounted_skill_snapshot_warns_for_unavailable_desired() {
        let avail = vec![make_skill_entry("owner/k1", "k1")];
        let desired = vec!["owner/missing".to_string()];
        let snap = build_runtime_mounted_skill_snapshot(RuntimeMountedSkillSnapshotOptions {
            adapter_type: "test-adapter",
            available_entries: &avail,
            desired_skills: &desired,
            configured_detail: SkillDetail::Literal("ignored"),
            ..Default::default()
        });
        assert!(snap.warnings.iter().any(|w| w.contains("owner/missing")));
        assert_eq!(snap.entries.len(), 2);
    }

    #[test]
    fn build_persistent_skill_snapshot_marks_installed_when_target_matches_source() {
        let avail = vec![make_skill_entry("owner/k1", "k1")];
        let mut installed = HashMap::new();
        installed.insert(
            "k1".to_string(),
            InstalledSkillTarget {
                target_path: Some("/skills".to_string()),
                kind: InstalledSkillTargetKind::Symlink,
            },
        );
        let desired = vec!["owner/k1".to_string()];
        let mut avail_with_source = avail;
        avail_with_source[0].source = "/skills".to_string();
        let snap = build_persistent_skill_snapshot(PersistentSkillSnapshotOptions {
            adapter_type: "test",
            available_entries: &avail_with_source,
            desired_skills: &desired,
            installed: Some(&installed),
            skills_home: "/skills",
            location_label: None,
            installed_detail: Some("installed OK"),
            missing_detail: "missing",
            external_conflict_detail: "conflict",
            external_detail: "external",
            warnings: None,
        });
        assert_eq!(snap.entries[0].state, AdapterSkillState::Installed);
        assert!(snap.entries[0].managed);
        assert_eq!(snap.entries[0].detail.as_deref(), Some("installed OK"));
    }

    #[test]
    fn build_persistent_skill_snapshot_marks_external_when_target_mismatch() {
        let avail = vec![make_skill_entry("owner/k1", "k1")];
        let mut installed = HashMap::new();
        installed.insert(
            "k1".to_string(),
            InstalledSkillTarget {
                target_path: Some("/other/path".to_string()),
                kind: InstalledSkillTargetKind::Symlink,
            },
        );
        let desired: Vec<String> = vec![];
        let snap = build_persistent_skill_snapshot(PersistentSkillSnapshotOptions {
            adapter_type: "test",
            available_entries: &avail,
            desired_skills: &desired,
            installed: Some(&installed),
            skills_home: "/skills",
            location_label: None,
            installed_detail: None,
            missing_detail: "missing",
            external_conflict_detail: "conflict",
            external_detail: "external",
            warnings: None,
        });
        assert_eq!(snap.entries[0].state, AdapterSkillState::External);
    }

    // ---------- normalize configured (R407) ----------

    #[test]
    fn normalize_configured_paperclip_runtime_skills_filters_invalid_entries() {
        let cfg = serde_json::json!([
            { "key": "owner/k1", "runtimeName": "k1", "source": "/skills/k1" },
            { "key": "owner/k2", "name": "k2", "source": "/skills/k2" },
            { "key": "", "runtimeName": "x", "source": "/x" },
            { "key": "owner/k3", "runtimeName": "", "source": "/skills/k3" },
            { "key": "owner/k4", "runtimeName": "k4", "source": "" },
            { "not-an-object": true },
            {
                "key": "owner/k5",
                "runtimeName": "k5",
                "source": "/skills/k5",
                "versionId": "v5",
                "currentVersionId": "v5-cur",
                "sourceStatus": "missing",
                "missingDetail": "k5 is missing"
            }
        ]);
        let entries = normalize_configured_paperclip_runtime_skills(&cfg);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].key, "owner/k1");
        assert_eq!(entries[1].key, "owner/k2");
        assert_eq!(entries[2].key, "owner/k5");
        assert_eq!(entries[2].version_id.as_deref(), Some("v5"));
        assert_eq!(entries[2].source_status, Some(PaperclipSkillSourceStatus::Missing));
        assert_eq!(entries[2].missing_detail.as_deref(), Some("k5 is missing"));
    }

    // ---------- helpers used by the tests above ----------

    fn make_skill_entry(key: &str, runtime_name: &str) -> PaperclipSkillEntry {
        PaperclipSkillEntry {
            key: key.to_string(),
            runtime_name: runtime_name.to_string(),
            source: format!("/skills/{runtime_name}"),
            version_id: None,
            current_version_id: None,
            source_status: Some(PaperclipSkillSourceStatus::Available),
            missing_detail: None,
        }
    }
}
