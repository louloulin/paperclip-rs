//! R385 — Integration tests for `workspace_env` + `subprocess_signal`.
//!
//! Mirrors Node parity surface:
//! - `sanitizeSshRemoteEnv` (server-utils.ts L2311-2317) +
//!   `sanitizeRemoteExecutionEnv` (remote-execution-env.ts L28-44)
//! - `shapePaperclipWorkspaceEnvForExecution` (server-utils.ts L2023-2117)
//! - `rewriteWorkspaceCwdEnvVarsForExecution` (server-utils.ts L2118-2154)
//! - `refreshPaperclipWorkspaceEnvForExecution` (server-utils.ts L2155-2228)
//! - `signalRunningProcess` (server-utils.ts L82-112)

use pc_acpx::{
    read_env_value_case_insensitive, refresh_paperclip_workspace_env_for_execution,
    rewrite_workspace_cwd_env_vars_for_execution, sanitize_ssh_remote_env,
    shape_paperclip_workspace_env_for_execution, signal_running_process, RefreshWorkspaceEnvInput,
    ShapeWorkspaceEnvInput, Signal, SignalOutcome, SignalRunningProcessInput, WorkspaceHint,
    REMOTE_EXECUTION_ENV_IDENTITY_KEYS,
};
use serde_json::json;
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// sanitizeSshRemoteEnv
// ---------------------------------------------------------------------------

#[test]
fn sanitize_ssh_remote_env_passes_non_identity_keys() {
    let env: BTreeMap<String, String> = [
        ("FOO".to_string(), "bar".to_string()),
        ("LANG".to_string(), "en_US.UTF-8".to_string()),
    ]
    .into_iter()
    .collect();
    let inherited = BTreeMap::new();
    let out = sanitize_ssh_remote_env(&env, &inherited);
    assert_eq!(out.get("FOO").unwrap(), "bar");
    assert_eq!(out.get("LANG").unwrap(), "en_US.UTF-8");
    assert_eq!(out.len(), 2);
}

#[test]
fn sanitize_ssh_remote_env_drops_identity_keys_matching_inherited() {
    let env: BTreeMap<String, String> = [
        ("PATH".to_string(), "/usr/bin".to_string()),
        ("HOME".to_string(), "/home/alice".to_string()),
        ("USER".to_string(), "alice".to_string()),
        ("LANG".to_string(), "en_US.UTF-8".to_string()),
    ]
    .into_iter()
    .collect();
    let mut inherited = BTreeMap::new();
    inherited.insert("PATH".to_string(), "/usr/bin".to_string());
    inherited.insert("HOME".to_string(), "/home/alice".to_string());
    inherited.insert("USER".to_string(), "alice".to_string());
    let out = sanitize_ssh_remote_env(&env, &inherited);
    // Identity keys that match inherited are dropped.
    assert!(out.get("PATH").is_none());
    assert!(out.get("HOME").is_none());
    assert!(out.get("USER").is_none());
    // Non-identity keys pass through.
    assert_eq!(out.get("LANG").unwrap(), "en_US.UTF-8");
}

#[test]
fn sanitize_ssh_remote_env_keeps_overridden_identity_keys() {
    let env: BTreeMap<String, String> = [
        ("PATH".to_string(), "/custom/bin".to_string()),
        ("TMPDIR".to_string(), "/scratch".to_string()),
    ]
    .into_iter()
    .collect();
    let mut inherited = BTreeMap::new();
    inherited.insert("PATH".to_string(), "/usr/bin".to_string());
    inherited.insert("TMPDIR".to_string(), "/tmp".to_string());
    let out = sanitize_ssh_remote_env(&env, &inherited);
    assert_eq!(out.get("PATH").unwrap(), "/custom/bin");
    assert_eq!(out.get("TMPDIR").unwrap(), "/scratch");
}

#[test]
fn sanitize_ssh_remote_env_case_insensitive_inherited_lookup() {
    let env: BTreeMap<String, String> = [("PATH".to_string(), "/usr/bin".to_string())]
        .into_iter()
        .collect();
    let mut inherited = BTreeMap::new();
    inherited.insert("path".to_string(), "/usr/bin".to_string());
    let out = sanitize_ssh_remote_env(&env, &inherited);
    assert!(out.is_empty());
}

#[test]
fn remote_execution_env_identity_keys_is_a_known_list() {
    // Lock the surface — adding a new identity key must be deliberate.
    assert!(REMOTE_EXECUTION_ENV_IDENTITY_KEYS.contains(&"PATH"));
    assert!(REMOTE_EXECUTION_ENV_IDENTITY_KEYS.contains(&"HOME"));
    assert!(REMOTE_EXECUTION_ENV_IDENTITY_KEYS.contains(&"USER"));
    assert!(REMOTE_EXECUTION_ENV_IDENTITY_KEYS.contains(&"XDG_CONFIG_HOME"));
    // Sanity: there are at least 10 entries.
    assert!(REMOTE_EXECUTION_ENV_IDENTITY_KEYS.len() >= 10);
}

#[test]
fn read_env_value_case_insensitive_handles_exact_and_fuzzy_match() {
    let mut env = BTreeMap::new();
    env.insert("Path".to_string(), "/usr/bin".to_string());
    // Exact match.
    assert_eq!(
        read_env_value_case_insensitive(&env, "Path").as_deref(),
        Some("/usr/bin")
    );
    // Case-insensitive match.
    assert_eq!(
        read_env_value_case_insensitive(&env, "PATH").as_deref(),
        Some("/usr/bin")
    );
    // Missing key.
    assert!(read_env_value_case_insensitive(&env, "MISSING").is_none());
}

// ---------------------------------------------------------------------------
// shapePaperclipWorkspaceEnvForExecution
// ---------------------------------------------------------------------------

#[test]
fn shape_local_target_passes_trim_and_null() {
    let input = ShapeWorkspaceEnvInput {
        workspace_cwd: Some("  /work  "),
        workspace_worktree_path: Some(" /wt "),
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
fn shape_local_target_null_for_blank_inputs() {
    let input = ShapeWorkspaceEnvInput {
        workspace_cwd: Some("   "),
        workspace_worktree_path: Some(""),
        workspace_hints: vec![],
        execution_target_is_remote: false,
        execution_cwd: None,
        staged_project_dirs: BTreeMap::new(),
    };
    let out = shape_paperclip_workspace_env_for_execution(&input);
    assert_eq!(out.workspace_cwd, None);
    assert_eq!(out.workspace_worktree_path, None);
}

#[test]
fn shape_remote_target_repoints_and_drops_worktree() {
    let input = ShapeWorkspaceEnvInput {
        workspace_cwd: Some("/local/work"),
        workspace_worktree_path: Some("/local/wt"),
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
fn shape_remote_target_hint_matching_local_workspace_repoints() {
    let input = ShapeWorkspaceEnvInput {
        workspace_cwd: Some("/work"),
        workspace_worktree_path: None,
        workspace_hints: vec![json!({"cwd": "/work"}).as_object().unwrap().clone()],
        execution_target_is_remote: true,
        execution_cwd: Some("/remote/work"),
        staged_project_dirs: BTreeMap::new(),
    };
    let out = shape_paperclip_workspace_env_for_execution(&input);
    assert_eq!(
        out.workspace_hints[0].get("cwd").and_then(|v| v.as_str()),
        Some("/remote/work")
    );
}

#[test]
fn shape_remote_target_hint_with_staged_project_repoints() {
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

#[test]
fn shape_remote_target_hint_blank_cwd_passes_through() {
    let input = ShapeWorkspaceEnvInput {
        workspace_cwd: Some("/work"),
        workspace_worktree_path: None,
        workspace_hints: vec![json!({"cwd": "  "}).as_object().unwrap().clone()],
        execution_target_is_remote: true,
        execution_cwd: Some("/remote/work"),
        staged_project_dirs: BTreeMap::new(),
    };
    let out = shape_paperclip_workspace_env_for_execution(&input);
    // Blank cwd hint is left alone.
    assert_eq!(
        out.workspace_hints[0].get("cwd").and_then(|v| v.as_str()),
        Some("  ")
    );
}

// ---------------------------------------------------------------------------
// rewriteWorkspaceCwdEnvVarsForExecution
// ---------------------------------------------------------------------------

#[test]
fn rewrite_remote_workspace_cwd_substitutes_matching_values() {
    let env: BTreeMap<String, String> = [
        ("PAPERCLIP_WORKSPACE_CWD".to_string(), "/work".to_string()),
        ("FOO_WORKSPACE_CWD".to_string(), "/work".to_string()),
        ("OTHER_WORKSPACE_CWD".to_string(), "/elsewhere".to_string()),
        ("UNRELATED".to_string(), "yes".to_string()),
    ]
    .into_iter()
    .collect();
    let out = rewrite_workspace_cwd_env_vars_for_execution(
        &env,
        Some("/work"),
        Some("/remote/work"),
        true,
    );
    assert_eq!(out.get("PAPERCLIP_WORKSPACE_CWD").unwrap(), "/remote/work");
    assert_eq!(out.get("FOO_WORKSPACE_CWD").unwrap(), "/remote/work");
    assert_eq!(out.get("OTHER_WORKSPACE_CWD").unwrap(), "/elsewhere");
    assert_eq!(out.get("UNRELATED").unwrap(), "yes");
}

#[test]
fn rewrite_local_target_is_no_op() {
    let env: BTreeMap<String, String> =
        [("PAPERCLIP_WORKSPACE_CWD".to_string(), "/work".to_string())]
            .into_iter()
            .collect();
    let out = rewrite_workspace_cwd_env_vars_for_execution(
        &env,
        Some("/work"),
        Some("/remote/work"),
        false,
    );
    assert_eq!(out.get("PAPERCLIP_WORKSPACE_CWD").unwrap(), "/work");
}

#[test]
fn rewrite_skips_when_remote_cwd_missing() {
    let env: BTreeMap<String, String> =
        [("PAPERCLIP_WORKSPACE_CWD".to_string(), "/work".to_string())]
            .into_iter()
            .collect();
    let out = rewrite_workspace_cwd_env_vars_for_execution(&env, Some("/work"), None, true);
    assert_eq!(out.get("PAPERCLIP_WORKSPACE_CWD").unwrap(), "/work");
}

// ---------------------------------------------------------------------------
// refreshPaperclipWorkspaceEnvForExecution
// ---------------------------------------------------------------------------

fn hint_obj(value: serde_json::Value) -> WorkspaceHint {
    value.as_object().unwrap().clone()
}

#[test]
fn refresh_local_applies_mappings_and_drops_stale() {
    let mut env: BTreeMap<String, String> = [
        ("PAPERCLIP_WORKSPACE_CWD".to_string(), "stale".to_string()),
        (
            "PAPERCLIP_WORKSPACE_WORKTREE_PATH".to_string(),
            "stale-wt".to_string(),
        ),
        ("PAPERCLIP_WORKSPACES_JSON".to_string(), "[]".to_string()),
        ("PATH".to_string(), "/usr/bin".to_string()),
    ]
    .into_iter()
    .collect();
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
    assert_eq!(
        input.env.get("PAPERCLIP_WORKSPACE_SOURCE").unwrap(),
        "local"
    );
    assert_eq!(input.env.get("AGENT_HOME").unwrap(), "/home/alice");
    assert_eq!(shaped.workspace_cwd.as_deref(), Some("/work"));
}

#[test]
fn refresh_remote_serializes_hints_and_rewrites_user_config() {
    let mut env: BTreeMap<String, String> = [
        ("PAPERCLIP_WORKSPACE_CWD".to_string(), "stale".to_string()),
        ("PATH".to_string(), "/usr/bin".to_string()),
    ]
    .into_iter()
    .collect();
    let env_config: BTreeMap<String, String> = [
        ("PATH".to_string(), "/custom/bin".to_string()),
        (
            "PAPERCLIP_AGENT_ID".to_string(),
            "ag_from_config".to_string(),
        ),
        ("PAPERCLIP_API_KEY".to_string(), "leaked".to_string()),
        ("USER_EXTRA".to_string(), "yes".to_string()),
    ]
    .into_iter()
    .collect();
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
        workspace_hints: vec![hint_obj(json!({"cwd": "/work"}))],
        agent_home: None,
        execution_target_is_remote: true,
        execution_cwd: Some("/remote/work"),
        staged_project_dirs: BTreeMap::new(),
    };
    input
        .env
        .insert("PAPERCLIP_AGENT_ID".to_string(), "ag_runtime".to_string());

    let shaped = refresh_paperclip_workspace_env_for_execution(&mut input);
    assert_eq!(
        input.env.get("PAPERCLIP_WORKSPACE_CWD").unwrap(),
        "/remote/work"
    );
    assert_eq!(shaped.workspace_cwd.as_deref(), Some("/remote/work"));
    // PATH is non-runtime; user config wins.
    assert_eq!(input.env.get("PATH").unwrap(), "/custom/bin");
    // Runtime var was already set; config does not override.
    assert_eq!(input.env.get("PAPERCLIP_AGENT_ID").unwrap(), "ag_runtime");
    // Forbidden config key never accepted.
    assert!(input.env.get("PAPERCLIP_API_KEY").is_none());
    // Non-runtime user config forwarded.
    assert_eq!(input.env.get("USER_EXTRA").unwrap(), "yes");
    // Hints serialized.
    let json = input.env.get("PAPERCLIP_WORKSPACES_JSON").unwrap();
    assert!(json.contains("/remote/work"));
}

// ---------------------------------------------------------------------------
// signalRunningProcess
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn signal_skips_when_child_already_exited() {
    let mut input = SignalRunningProcessInput::new(
        std::process::id(),
        Some(std::process::id()),
        Signal::SIGTERM,
    );
    input.child_already_exited = true;
    let out = signal_running_process(input);
    assert_eq!(out, SignalOutcome::SkippedAlreadyExited);
}

#[cfg(unix)]
#[test]
fn signal_returns_failed_for_unlikely_high_pid() {
    let input = SignalRunningProcessInput {
        child_pid: 0x7FFFFFFE_u32,
        process_group_id: None,
        signal: Signal::SIGTERM,
        child_already_exited: false,
    };
    let out = signal_running_process(input);
    assert!(matches!(out, SignalOutcome::Failed { .. }), "got {:?}", out);
}

#[cfg(unix)]
#[test]
fn signal_skips_zero_pgid_and_dispatches_direct() {
    // pgid == 0 → group branch skipped (Node parity: `pgid > 0` gate).
    let input = SignalRunningProcessInput {
        child_pid: 0x7FFFFFFE_u32,
        process_group_id: Some(0),
        signal: Signal::SIGTERM,
        child_already_exited: false,
    };
    let out = signal_running_process(input);
    assert!(matches!(out, SignalOutcome::Failed { .. }), "got {:?}", out);
}

#[cfg(unix)]
#[test]
fn signal_returns_failed_for_unlikely_high_pgid() {
    // pgid == unlikely high → group signal fails, fallback to direct
    // (also fails). Both branches return Failed.
    let input = SignalRunningProcessInput {
        child_pid: 0x7FFFFFFE_u32,
        process_group_id: Some(0x7FFFFFFE_u32),
        signal: Signal::SIGTERM,
        child_already_exited: false,
    };
    let out = signal_running_process(input);
    assert!(matches!(out, SignalOutcome::Failed { .. }), "got {:?}", out);
}
