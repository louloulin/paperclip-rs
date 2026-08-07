//! Workspace env shaping for local and remote execution targets.
//!
//! Rust port of Node `packages/adapter-utils/src/server-utils.ts`:
//! - `sanitizeSshRemoteEnv` (L2311-2317) +
//!   `sanitizeRemoteExecutionEnv` from `remote-execution-env.ts`
//! - `shapePaperclipWorkspaceEnvForExecution` (L2023-2117)
//! - `rewriteWorkspaceCwdEnvVarsForExecution` (L2118-2154)
//! - `refreshPaperclipWorkspaceEnvForExecution` (L2155-2228)
//!
//! `applyPaperclipWorkspaceEnv` (L1988-2022) is already implemented in
//! `build_runtime.rs`; this module covers the remaining shaping helpers
//! and the SSH / remote-target sanitizer.
//!
//! All helpers are pure: no I/O, no async, no global state. Designed
//! for high cohesion — callers opt in to the helpers they need, and
//! every function is independently unit-testable.

use std::collections::BTreeMap;
use std::path::Path;

// ============================================================================
// sanitizeSshRemoteEnv
// ============================================================================

/// Env keys whose values must be re-validated against the inherited host
/// environment when the call site is remote. Mirrors
/// `REMOTE_EXECUTION_ENV_IDENTITY_KEYS` (remote-execution-env.ts L1-16).
pub const REMOTE_EXECUTION_ENV_IDENTITY_KEYS: &[&str] = &[
    "PATH",
    "HOME",
    "PWD",
    "SHELL",
    "USER",
    "LOGNAME",
    "NVM_DIR",
    "TMPDIR",
    "TMP",
    "TEMP",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_RUNTIME_DIR",
];

/// Look up a key in `inherited_env` case-insensitively and return its
/// string value, or `None`. Mirrors `readEnvValueCaseInsensitive`
/// (remote-execution-env.ts L18-26).
pub fn read_env_value_case_insensitive(
    inherited_env: &BTreeMap<String, String>,
    key: &str,
) -> Option<String> {
    if let Some(v) = inherited_env.get(key) {
        return Some(v.clone());
    }
    let upper = key.to_ascii_uppercase();
    for (candidate_key, candidate_value) in inherited_env {
        if candidate_key.to_ascii_uppercase() == upper {
            return Some(candidate_value.clone());
        }
    }
    None
}

/// Sanitize an env block for SSH-remote execution. Keys that are not in
/// the remote-identity list are forwarded verbatim. Identity keys whose
/// value matches the inherited host value are dropped — the caller is
/// expected to re-derive them on the remote side. Mirrors
/// `sanitizeRemoteExecutionEnv` (remote-execution-env.ts L28-44) wrapped
/// by `sanitizeSshRemoteEnv` (server-utils.ts L2311-2317).
pub fn sanitize_ssh_remote_env(
    env: &BTreeMap<String, String>,
    inherited_env: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    sanitize_remote_execution_env(env, inherited_env)
}

/// Inner implementation. Mirrors `sanitizeRemoteExecutionEnv`
/// (remote-execution-env.ts L28-44).
pub fn sanitize_remote_execution_env(
    env: &BTreeMap<String, String>,
    inherited_env: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (key, value) in env {
        let normalized = key.to_ascii_uppercase();
        if !REMOTE_EXECUTION_ENV_IDENTITY_KEYS.contains(&normalized.as_str()) {
            out.insert(key.clone(), value.clone());
            continue;
        }
        let inherited = read_env_value_case_insensitive(inherited_env, key);
        match inherited {
            Some(v) if v == *value => {
                // Same as host → drop; remote will re-derive.
            }
            _ => {
                out.insert(key.clone(), value.clone());
            }
        }
    }
    out
}

// ============================================================================
// shapePaperclipWorkspaceEnvForExecution
// ============================================================================

/// A single workspace hint entry. We keep the inner fields opaque (typed
/// `serde_json::Value`) so we do not have to model Paperclip's full
/// workspace schema; the helper only mutates the `cwd` and (optionally)
/// reads `projectId`.
pub type WorkspaceHint = serde_json::Map<String, serde_json::Value>;

/// Shaped workspace env. Mirrors the return value of
/// `shapePaperclipWorkspaceEnvForExecution` (server-utils.ts L2077-2117).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShapedWorkspaceEnv {
    pub workspace_cwd: Option<String>,
    pub workspace_worktree_path: Option<String>,
    pub workspace_hints: Vec<WorkspaceHint>,
}

/// Inputs for [`shape_paperclip_workspace_env_for_execution`]. Mirrors
/// `shapePaperclipWorkspaceEnvForExecution` (server-utils.ts L2023-2076).
#[derive(Debug, Clone, Default)]
pub struct ShapeWorkspaceEnvInput<'a> {
    pub workspace_cwd: Option<&'a str>,
    pub workspace_worktree_path: Option<&'a str>,
    pub workspace_hints: Vec<WorkspaceHint>,
    pub execution_target_is_remote: bool,
    pub execution_cwd: Option<&'a str>,
    pub staged_project_dirs: BTreeMap<String, String>,
}

/// Shape the workspace env for either local or remote execution. Mirrors
/// `shapePaperclipWorkspaceEnvForExecution` (server-utils.ts L2023-2117).
///
/// Local target: passthrough with trim/null-coercion.
/// Remote target: rewrite `workspaceCwd` to `executionCwd`, null out the
/// worktree path (the remote transport has no host worktree), and rewrite
/// each hint's `cwd`:
/// - If the hint's `cwd` equals the local `workspaceCwd`, repoint to
///   `executionCwd` (or strip it when `executionCwd` is missing).
/// - If the hint carries a `projectId` and `stagedProjectDirs` has an
///   entry, repoint to that staged directory. Otherwise strip the cwd
///   (never expose an unstaged path).
pub fn shape_paperclip_workspace_env_for_execution(
    input: &ShapeWorkspaceEnvInput<'_>,
) -> ShapedWorkspaceEnv {
    let workspace_cwd = input
        .workspace_cwd
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let workspace_worktree_path = input
        .workspace_worktree_path
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let workspace_hints = input.workspace_hints.clone();

    if !input.execution_target_is_remote {
        return ShapedWorkspaceEnv {
            workspace_cwd,
            workspace_worktree_path,
            workspace_hints,
        };
    }

    let execution_cwd = input
        .execution_cwd
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let realized_workspace_cwd = execution_cwd.clone();
    let local_workspace_cwd = workspace_cwd
        .as_deref()
        .map(|p| canonicalize_like_resolve(p));
    let staged_project_dirs = &input.staged_project_dirs;
    let shaped_hints: Vec<WorkspaceHint> = workspace_hints
        .into_iter()
        .map(|mut hint| {
            let hint_cwd = hint
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .unwrap_or("")
                .to_string();
            if hint_cwd.is_empty() {
                return hint;
            }

            if let Some(local) = &local_workspace_cwd {
                if canonicalize_like_resolve(&hint_cwd) == *local {
                    if let Some(realized) = &realized_workspace_cwd {
                        hint.insert(
                            "cwd".to_string(),
                            serde_json::Value::String(realized.clone()),
                        );
                    } else {
                        hint.remove("cwd");
                    }
                    return hint;
                }
            }

            let hint_project_id = hint
                .get("projectId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(staged) = staged_project_dirs.get(&hint_project_id) {
                let trimmed = staged.trim();
                if !trimmed.is_empty() {
                    hint.insert(
                        "cwd".to_string(),
                        serde_json::Value::String(trimmed.to_string()),
                    );
                } else {
                    hint.remove("cwd");
                }
            } else {
                hint.remove("cwd");
            }
            hint
        })
        .collect();

    ShapedWorkspaceEnv {
        workspace_cwd: realized_workspace_cwd,
        workspace_worktree_path: None,
        workspace_hints: shaped_hints,
    }
}

/// Best-effort approximation of Node's `path.resolve`. Pure lexical:
/// we resolve a path by joining it against the current working directory
/// when it is relative, then normalize away `.` / `..` segments. We
/// never touch the filesystem (which means symlinks are not resolved —
/// Node's `path.resolve` is also lexical and never resolves symlinks).
///
/// Two callers feeding already-absolute paths get back those paths
/// unchanged, matching Node's `path.resolve("/work/sub") === "/work/sub"`.
fn canonicalize_like_resolve(value: &str) -> std::path::PathBuf {
    let p = Path::new(value);
    let base = if p.is_absolute() {
        std::path::PathBuf::from("/")
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd,
            Err(_) => std::path::PathBuf::from("."),
        }
    };
    lexically_normalize(base.join(value))
}

/// Lexically resolve `.` / `..` segments without touching the filesystem.
fn lexically_normalize(mut path: std::path::PathBuf) -> std::path::PathBuf {
    let mut components: Vec<std::path::Component<'_>> = Vec::new();
    for comp in path.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if let Some(last) = components.last() {
                    if matches!(last, std::path::Component::Normal(_)) {
                        components.pop();
                        continue;
                    }
                }
                components.push(comp);
            }
            other => components.push(other),
        }
    }
    let mut out = std::path::PathBuf::new();
    for comp in components {
        out.push(comp.as_os_str());
    }
    out
}

// ============================================================================
// rewriteWorkspaceCwdEnvVarsForExecution
// ============================================================================

/// Rewrite any `*_WORKSPACE_CWD` env entry that points at the local
/// workspace cwd so it instead points at the remote `executionCwd`.
/// Mirrors `rewriteWorkspaceCwdEnvVarsForExecution` (server-utils.ts
/// L2118-2154). No-op when `executionTargetIsRemote` is false, or when
/// either cwd is missing.
pub fn rewrite_workspace_cwd_env_vars_for_execution(
    env: &BTreeMap<String, String>,
    workspace_cwd: Option<&str>,
    execution_cwd: Option<&str>,
    execution_target_is_remote: bool,
) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = env.clone();
    let local_workspace_cwd = workspace_cwd
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(canonicalize_like_resolve);
    // `executionCwd` is a remote path on the target host; we deliberately
    // do not run `path.resolve` against it because that applies host-Node
    // semantics to a path that lives on the remote shell. Callers always
    // pass absolute remote paths, so we forward the trimmed value verbatim.
    let remote_workspace_cwd = execution_cwd
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    if !execution_target_is_remote
        || local_workspace_cwd.is_none()
        || remote_workspace_cwd.is_none()
    {
        return out;
    }
    let local = local_workspace_cwd.unwrap();
    let remote = remote_workspace_cwd.unwrap();

    for (key, value) in out.clone() {
        if !key.ends_with("_WORKSPACE_CWD") {
            continue;
        }
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        if canonicalize_like_resolve(trimmed) != local {
            continue;
        }
        out.insert(key, remote.clone());
    }
    out
}

// ============================================================================
// refreshPaperclipWorkspaceEnvForExecution
// ============================================================================

/// Inputs for [`refresh_paperclip_workspace_env_for_execution`]. Mirrors
/// `refreshPaperclipWorkspaceEnvForExecution` (server-utils.ts L2155-2228).
#[derive(Debug)]
pub struct RefreshWorkspaceEnvInput<'a> {
    pub env: &'a mut BTreeMap<String, String>,
    pub env_config: Option<&'a BTreeMap<String, String>>,
    pub workspace_cwd: Option<&'a str>,
    pub workspace_source: Option<&'a str>,
    pub workspace_strategy: Option<&'a str>,
    pub workspace_id: Option<&'a str>,
    pub workspace_repo_url: Option<&'a str>,
    pub workspace_repo_ref: Option<&'a str>,
    pub workspace_branch: Option<&'a str>,
    pub workspace_worktree_path: Option<&'a str>,
    pub workspace_hints: Vec<WorkspaceHint>,
    pub agent_home: Option<&'a str>,
    pub execution_target_is_remote: bool,
    pub execution_cwd: Option<&'a str>,
    pub staged_project_dirs: BTreeMap<String, String>,
}

const PAPERCLIP_WORKSPACE_KEYS_TO_DROP: &[&str] = &[
    "PAPERCLIP_WORKSPACE_CWD",
    "PAPERCLIP_WORKSPACE_WORKTREE_PATH",
    "PAPERCLIP_WORKSPACES_JSON",
];

const PAPERCLIP_WORKSPACE_ENV_MAPPINGS: &[&str] = &[
    "PAPERCLIP_WORKSPACE_CWD",
    "PAPERCLIP_WORKSPACE_SOURCE",
    "PAPERCLIP_WORKSPACE_STRATEGY",
    "PAPERCLIP_WORKSPACE_ID",
    "PAPERCLIP_WORKSPACE_REPO_URL",
    "PAPERCLIP_WORKSPACE_REPO_REF",
    "PAPERCLIP_WORKSPACE_BRANCH",
    "PAPERCLIP_WORKSPACE_WORKTREE_PATH",
    "AGENT_HOME",
];

/// Refresh the env block in-place for the current execution target.
/// Mirrors `refreshPaperclipWorkspaceEnvForExecution` (server-utils.ts
/// L2155-2228).
///
/// 1. Shape the workspace env (local/remote) via
///    [`shape_paperclip_workspace_env_for_execution`].
/// 2. Drop the three stale `PAPERCLIP_WORKSPACE_*` keys.
/// 3. Apply the workspace mappings (callers use the same `input.env`
///    as both source and destination, so we mutate it directly).
/// 4. Forward shaped hints via `PAPERCLIP_WORKSPACES_JSON` when present.
/// 5. Apply user-config env, but never override `PAPERCLIP_*` runtime
///    keys already assigned and never accept `PAPERCLIP_API_KEY` from
///    config.
pub fn refresh_paperclip_workspace_env_for_execution(
    input: &mut RefreshWorkspaceEnvInput<'_>,
) -> ShapedWorkspaceEnv {
    let shaped = shape_paperclip_workspace_env_for_execution(&ShapeWorkspaceEnvInput {
        workspace_cwd: input.workspace_cwd,
        workspace_worktree_path: input.workspace_worktree_path,
        workspace_hints: input.workspace_hints.clone(),
        execution_target_is_remote: input.execution_target_is_remote,
        execution_cwd: input.execution_cwd,
        staged_project_dirs: input.staged_project_dirs.clone(),
    });

    for key in PAPERCLIP_WORKSPACE_KEYS_TO_DROP {
        input.env.remove(*key);
    }

    apply_workspace_env_mappings(
        input.env,
        &[
            ("PAPERCLIP_WORKSPACE_CWD", shaped.workspace_cwd.as_deref()),
            ("PAPERCLIP_WORKSPACE_SOURCE", input.workspace_source),
            ("PAPERCLIP_WORKSPACE_STRATEGY", input.workspace_strategy),
            ("PAPERCLIP_WORKSPACE_ID", input.workspace_id),
            ("PAPERCLIP_WORKSPACE_REPO_URL", input.workspace_repo_url),
            ("PAPERCLIP_WORKSPACE_REPO_REF", input.workspace_repo_ref),
            ("PAPERCLIP_WORKSPACE_BRANCH", input.workspace_branch),
            (
                "PAPERCLIP_WORKSPACE_WORKTREE_PATH",
                shaped.workspace_worktree_path.as_deref(),
            ),
            ("AGENT_HOME", input.agent_home),
        ],
    );

    if !shaped.workspace_hints.is_empty() {
        let json =
            serde_json::to_string(&shaped.workspace_hints).expect("workspace hints serialize");
        input
            .env
            .insert("PAPERCLIP_WORKSPACES_JSON".to_string(), json);
    }

    if let Some(env_config) = input.env_config {
        let rewritten = rewrite_workspace_cwd_env_vars_for_execution(
            env_config,
            input.workspace_cwd,
            shaped.workspace_cwd.as_deref(),
            input.execution_target_is_remote,
        );
        for (key, value) in rewritten {
            if is_forbidden_config_env_key(&key) {
                continue;
            }
            if is_paperclip_runtime_env_key(&key) && input.env.contains_key(&key) {
                continue;
            }
            input.env.insert(key, value);
        }
    }

    shaped
}

fn apply_workspace_env_mappings(
    env: &mut BTreeMap<String, String>,
    mappings: &[(&str, Option<&str>)],
) {
    let _ = PAPERCLIP_WORKSPACE_ENV_MAPPINGS; // ensure table is kept for parity audit
    for (key, value) in mappings {
        if let Some(v) = value {
            if !v.is_empty() {
                env.insert((*key).to_string(), (*v).to_string());
            }
        }
    }
}

// Local re-exports so this module does not depend on log_redaction
// (keeps the module independent for testing).

fn is_forbidden_config_env_key(key: &str) -> bool {
    key == "PAPERCLIP_API_KEY"
}

fn is_paperclip_runtime_env_key(key: &str) -> bool {
    key.starts_with("PAPERCLIP_")
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env_from(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    // ----- sanitize_ssh_remote_env -----

    #[test]
    fn sanitize_ssh_remote_env_passes_non_identity_keys() {
        let env = env_from(&[("FOO", "bar"), ("LANG", "en_US.UTF-8")]);
        let inherited = BTreeMap::new();
        let out = sanitize_ssh_remote_env(&env, &inherited);
        assert_eq!(out.get("FOO").unwrap(), "bar");
        assert_eq!(out.get("LANG").unwrap(), "en_US.UTF-8");
    }

    #[test]
    fn sanitize_ssh_remote_env_drops_identity_keys_matching_inherited() {
        let env = env_from(&[("PATH", "/usr/bin"), ("USER", "alice")]);
        let mut inherited = BTreeMap::new();
        inherited.insert("PATH".to_string(), "/usr/bin".to_string());
        inherited.insert("USER".to_string(), "alice".to_string());
        let out = sanitize_ssh_remote_env(&env, &inherited);
        assert!(out.is_empty());
    }

    #[test]
    fn sanitize_ssh_remote_env_keeps_identity_keys_overridden() {
        let env = env_from(&[("PATH", "/custom/bin")]);
        let mut inherited = BTreeMap::new();
        inherited.insert("PATH".to_string(), "/usr/bin".to_string());
        let out = sanitize_ssh_remote_env(&env, &inherited);
        assert_eq!(out.get("PATH").unwrap(), "/custom/bin");
    }

    #[test]
    fn sanitize_ssh_remote_env_case_insensitive_inherited_lookup() {
        let env = env_from(&[("PATH", "/usr/bin")]);
        let mut inherited = BTreeMap::new();
        inherited.insert("path".to_string(), "/usr/bin".to_string());
        let out = sanitize_ssh_remote_env(&env, &inherited);
        assert!(out.is_empty());
    }

    // ----- shape_paperclip_workspace_env_for_execution -----

    #[test]
    fn shape_local_target_passes_through_with_trim() {
        let input = ShapeWorkspaceEnvInput {
            workspace_cwd: Some("  /work  "),
            workspace_worktree_path: Some("/wt"),
            workspace_hints: vec![json!({"cwd": "/work/h1"}).as_object().unwrap().clone()],
            execution_target_is_remote: false,
            execution_cwd: None,
            staged_project_dirs: BTreeMap::new(),
        };
        let out = shape_paperclip_workspace_env_for_execution(&input);
        assert_eq!(out.workspace_cwd.as_deref(), Some("/work"));
        assert_eq!(out.workspace_worktree_path.as_deref(), Some("/wt"));
        assert_eq!(out.workspace_hints.len(), 1);
    }

    #[test]
    fn shape_remote_target_repoints_workspace_cwd_and_nulls_worktree() {
        let input = ShapeWorkspaceEnvInput {
            workspace_cwd: Some("/work"),
            workspace_worktree_path: Some("/wt"),
            workspace_hints: vec![],
            execution_target_is_remote: true,
            execution_cwd: Some("/remote/work"),
            staged_project_dirs: BTreeMap::new(),
        };
        let out = shape_paperclip_workspace_env_for_execution(&input);
        assert_eq!(out.workspace_cwd.as_deref(), Some("/remote/work"));
        assert_eq!(out.workspace_worktree_path, None);
    }

    #[test]
    fn shape_remote_target_hint_cwd_matching_local_repoints() {
        let input = ShapeWorkspaceEnvInput {
            workspace_cwd: Some("/work"),
            workspace_worktree_path: None,
            workspace_hints: vec![json!({"cwd": "/work"}).as_object().unwrap().clone()],
            execution_target_is_remote: true,
            execution_cwd: Some("/remote/work"),
            staged_project_dirs: BTreeMap::new(),
        };
        let out = shape_paperclip_workspace_env_for_execution(&input);
        assert_eq!(out.workspace_hints.len(), 1);
        assert_eq!(
            out.workspace_hints[0].get("cwd").and_then(|v| v.as_str()),
            Some("/remote/work")
        );
    }

    #[test]
    fn shape_remote_target_hint_with_staged_project_id_repoints() {
        let mut staged = BTreeMap::new();
        staged.insert("proj_1".to_string(), "/sandbox/proj-1".to_string());
        let input = ShapeWorkspaceEnvInput {
            workspace_cwd: Some("/work"),
            workspace_worktree_path: None,
            workspace_hints: vec![json!({
                "cwd": "/local/proj-1",
                "projectId": "proj_1",
            })
            .as_object()
            .unwrap()
            .clone()],
            execution_target_is_remote: true,
            execution_cwd: Some("/remote/work"),
            staged_project_dirs: staged,
        };
        let out = shape_paperclip_workspace_env_for_execution(&input);
        assert_eq!(
            out.workspace_hints[0].get("cwd").and_then(|v| v.as_str()),
            Some("/sandbox/proj-1")
        );
    }

    #[test]
    fn shape_remote_target_hint_without_staged_strips_cwd() {
        let input = ShapeWorkspaceEnvInput {
            workspace_cwd: Some("/work"),
            workspace_worktree_path: None,
            workspace_hints: vec![json!({
                "cwd": "/local/proj-x",
                "projectId": "proj_x",
            })
            .as_object()
            .unwrap()
            .clone()],
            execution_target_is_remote: true,
            execution_cwd: Some("/remote/work"),
            staged_project_dirs: BTreeMap::new(),
        };
        let out = shape_paperclip_workspace_env_for_execution(&input);
        // Unstaged hint loses its cwd (fail loud, never expose).
        assert!(out.workspace_hints[0].get("cwd").is_none());
    }

    // ----- rewrite_workspace_cwd_env_vars_for_execution -----

    #[test]
    fn rewrite_remote_workspace_cwd_substitutes_matching_values() {
        let env = env_from(&[
            ("PAPERCLIP_WORKSPACE_CWD", "/work"),
            ("FOO_WORKSPACE_CWD", "/work"),
            ("OTHER_WORKSPACE_CWD", "/elsewhere"),
        ]);
        let out = rewrite_workspace_cwd_env_vars_for_execution(
            &env,
            Some("/work"),
            Some("/remote/work"),
            true,
        );
        assert_eq!(out.get("PAPERCLIP_WORKSPACE_CWD").unwrap(), "/remote/work");
        assert_eq!(out.get("FOO_WORKSPACE_CWD").unwrap(), "/remote/work");
        assert_eq!(out.get("OTHER_WORKSPACE_CWD").unwrap(), "/elsewhere");
    }

    #[test]
    fn rewrite_local_target_is_no_op() {
        let env = env_from(&[("PAPERCLIP_WORKSPACE_CWD", "/work")]);
        let out = rewrite_workspace_cwd_env_vars_for_execution(
            &env,
            Some("/work"),
            Some("/remote/work"),
            false,
        );
        assert_eq!(out.get("PAPERCLIP_WORKSPACE_CWD").unwrap(), "/work");
    }

    // ----- refresh_paperclip_workspace_env_for_execution -----

    #[test]
    fn refresh_local_applies_mappings_and_drops_stale() {
        let mut env = env_from(&[
            ("PAPERCLIP_WORKSPACE_CWD", "stale"),
            ("PAPERCLIP_WORKSPACE_WORKTREE_PATH", "stale-wt"),
            ("PAPERCLIP_WORKSPACES_JSON", "[]"),
            ("PATH", "/usr/bin"),
        ]);
        let mut input = RefreshWorkspaceEnvInput {
            env: &mut env,
            env_config: None,
            workspace_cwd: Some("/work"),
            workspace_source: Some("local"),
            workspace_strategy: Some("worktree"),
            workspace_id: Some("ws_1"),
            workspace_repo_url: None,
            workspace_repo_ref: None,
            workspace_branch: Some("main"),
            workspace_worktree_path: Some("/wt"),
            workspace_hints: vec![],
            agent_home: Some("/home/alice"),
            execution_target_is_remote: false,
            execution_cwd: None,
            staged_project_dirs: BTreeMap::new(),
        };
        let shaped = refresh_paperclip_workspace_env_for_execution(&mut input);
        assert_eq!(input.env.get("PAPERCLIP_WORKSPACE_CWD").unwrap(), "/work");
        assert_eq!(
            input.env.get("PAPERCLIP_WORKSPACE_WORKTREE_PATH").unwrap(),
            "/wt"
        );
        assert!(input.env.get("PAPERCLIP_WORKSPACES_JSON").is_none());
        assert_eq!(shaped.workspace_cwd.as_deref(), Some("/work"));
    }

    #[test]
    fn refresh_remote_forwards_user_config_but_never_overrides_runtime() {
        let mut env = env_from(&[("PAPERCLIP_WORKSPACE_CWD", "stale"), ("PATH", "/usr/bin")]);
        let env_config = env_from(&[
            ("PATH", "/custom/bin"),
            ("PAPERCLIP_AGENT_ID", "ag_from_config"),
            ("PAPERCLIP_API_KEY", "leaked"),
            ("USER_EXTRA", "yes"),
        ]);
        let mut input = RefreshWorkspaceEnvInput {
            env: &mut env,
            env_config: Some(&env_config),
            workspace_cwd: Some("/work"),
            workspace_source: Some("remote"),
            workspace_strategy: Some("container"),
            workspace_id: Some("ws_1"),
            workspace_repo_url: None,
            workspace_repo_ref: None,
            workspace_branch: None,
            workspace_worktree_path: None,
            workspace_hints: vec![],
            agent_home: None,
            execution_target_is_remote: true,
            execution_cwd: Some("/remote/work"),
            staged_project_dirs: BTreeMap::new(),
        };
        // Pre-assign a runtime var to verify config cannot override it.
        input
            .env
            .insert("PAPERCLIP_AGENT_ID".to_string(), "ag_runtime".to_string());

        let shaped = refresh_paperclip_workspace_env_for_execution(&mut input);
        assert_eq!(
            input.env.get("PAPERCLIP_WORKSPACE_CWD").unwrap(),
            "/remote/work"
        );
        assert_eq!(shaped.workspace_cwd.as_deref(), Some("/remote/work"));
        // PATH is not runtime; config wins.
        assert_eq!(input.env.get("PATH").unwrap(), "/custom/bin");
        // Runtime var was already set; config ignored.
        assert_eq!(input.env.get("PAPERCLIP_AGENT_ID").unwrap(), "ag_runtime");
        // Forbidden config key never accepted.
        assert!(input.env.get("PAPERCLIP_API_KEY").is_none());
        // Non-runtime user config forwarded.
        assert_eq!(input.env.get("USER_EXTRA").unwrap(), "yes");
    }
}
