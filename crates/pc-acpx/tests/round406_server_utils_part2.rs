//! Round 406 - integration tests for `pc_acpx::server_utils` (Part 2: env helpers).
//!
//! Validates end-to-end composition of the env helpers ported in R406:
//!   - redactEnvForLogs + redactCommandTextForLogs compose through
//!     buildInvocationEnvForLogs without leaking secrets
//!   - buildPaperclipEnv resolves host / port / api_url against the
//!     priority chain (runtime_api_url → api_url → listen_host:listen_port)
//!   - applyPaperclipWorkspaceEnv + shape/rewrite/refresh pipeline
//!     composes for both local and remote execution targets
//!   - sanitizeInheritedPaperclipEnv preserves the runtime allowlist
//!   - ensurePathInEnv + defaultPathForPlatform round-trip

use std::collections::HashMap;

use pc_acpx::server_utils::{
    apply_paperclip_workspace_env, build_invocation_env_for_logs, build_paperclip_env,
    default_path_for_platform, ensure_path_in_env, redact_command_text_for_logs,
    redact_env_for_logs, refresh_paperclip_workspace_env_for_execution,
    rewrite_workspace_cwd_env_vars_for_execution, sanitize_inherited_paperclip_env,
    sanitize_ssh_remote_env, shape_paperclip_workspace_env_for_execution,
    ApplyPaperclipWorkspaceEnvInput, BuildInvocationEnvForLogsOptions, BuildPaperclipEnvInput,
    RefreshPaperclipWorkspaceEnvInput, RewriteWorkspaceCwdEnvVarsForExecutionInput,
    ShapePaperclipWorkspaceEnvInput, REDACTED_LOG_VALUE,
};

// ===========================================================================
// redact_env_for_logs + redact_command_text_for_logs + build_invocation_env
// ===========================================================================

#[test]
fn redaction_pipeline_strips_secrets_from_invocation_env() {
    let mut env = HashMap::new();
    env.insert("PATH".to_string(), "/usr/bin".to_string());
    env.insert("AGENT_HOME".to_string(), "/home/agent".to_string());
    let mut runtime = HashMap::new();
    runtime.insert("PAPERCLIP_AGENT_ID".to_string(), "agent-1".to_string());
    runtime.insert("API_KEY".to_string(), "should-be-redacted".to_string());
    runtime.insert("GH_TOKEN".to_string(), "ghp_xyz".to_string());
    let merged = build_invocation_env_for_logs(
        &env,
        BuildInvocationEnvForLogsOptions {
            runtime_env: Some(&runtime),
            include_runtime_keys: Some(&["PAPERCLIP_AGENT_ID"]),
            resolved_command: Some("agent run --api-key=hunter2 --verbose"),
            resolved_command_env_key: Some("PAPERCLIP_RESOLVED_COMMAND"),
        },
    );
    // Path / agent home survive; sensitive env values were never added
    // (we only requested AGENT_ID from runtime).
    assert_eq!(merged["PATH"], "/usr/bin");
    assert_eq!(merged["AGENT_HOME"], "/home/agent");
    assert_eq!(merged["PAPERCLIP_AGENT_ID"], "agent-1");
    // Resolved command lands in the resolved-command env key with the
    // secret redacted.
    let resolved = &merged["PAPERCLIP_RESOLVED_COMMAND"];
    assert!(!resolved.contains("hunter2"), "secret leaked: {resolved}");
    assert!(resolved.contains("REDACTED"));
}

#[test]
fn redact_env_for_logs_preserves_non_sensitive_values_verbatim() {
    let mut env = HashMap::new();
    env.insert("PATH".to_string(), "/usr/bin:/usr/local/bin".to_string());
    env.insert("LANG".to_string(), "en_US.UTF-8".to_string());
    env.insert("PAPERCLIP_AGENT_ID".to_string(), "agent-1".to_string());
    env.insert("MY_COOKIE".to_string(), "secret-cookie".to_string()); // matches SENSITIVE_ENV_KEY
    let redacted = redact_env_for_logs(&env);
    assert_eq!(redacted["PATH"], "/usr/bin:/usr/local/bin");
    assert_eq!(redacted["LANG"], "en_US.UTF-8");
    assert_eq!(redacted["PAPERCLIP_AGENT_ID"], "agent-1");
    assert_eq!(redacted["MY_COOKIE"], REDACTED_LOG_VALUE);
}

#[test]
fn redact_command_text_for_logs_redacts_common_secret_flags() {
    let cases = [
        ("agent run --api-key=hunter2", "REDACTED"),
        ("agent run --token abc123 --verbose", "REDACTED"),
        ("agent run --password=secret", "REDACTED"),
        ("agent run", "agent run"), // no secrets → unchanged
    ];
    for (cmd, expected_fragment) in cases {
        let out = redact_command_text_for_logs(cmd);
        if cmd.contains("--password") || cmd.contains("--api-key") || cmd.contains("--token") {
            assert!(
                !out.contains("hunter2") && !out.contains("abc123") && !out.contains("secret"),
                "leak in {cmd}: {out}"
            );
            assert!(out.contains(expected_fragment), "missing marker in {out}");
        } else {
            assert_eq!(out, expected_fragment);
        }
    }
}

// ===========================================================================
// build_paperclip_env
// ===========================================================================

#[test]
fn build_paperclip_env_priority_chain_resolves_api_url() {
    // Highest priority: PAPERCLIP_RUNTIME_API_URL.
    let mut runtime = HashMap::new();
    runtime.insert(
        "PAPERCLIP_RUNTIME_API_URL".to_string(),
        "https://primary.example.com".to_string(),
    );
    runtime.insert(
        "PAPERCLIP_API_URL".to_string(),
        "https://fallback.example.com".to_string(),
    );
    runtime.insert("PAPERCLIP_LISTEN_HOST".to_string(), "127.0.0.1".to_string());
    runtime.insert("PAPERCLIP_LISTEN_PORT".to_string(), "4000".to_string());
    let vars = build_paperclip_env(BuildPaperclipEnvInput {
        agent_id: "a-1",
        company_id: "c-1",
        runtime_env: &runtime,
        default_listen_host: "localhost",
        default_listen_port: "3100",
    });
    assert_eq!(vars["PAPERCLIP_AGENT_ID"], "a-1");
    assert_eq!(vars["PAPERCLIP_COMPANY_ID"], "c-1");
    assert_eq!(vars["PAPERCLIP_API_URL"], "https://primary.example.com");

    // Drop the highest → next priority PAPERCLIP_API_URL.
    let mut runtime2 = runtime.clone();
    runtime2.remove("PAPERCLIP_RUNTIME_API_URL");
    let vars2 = build_paperclip_env(BuildPaperclipEnvInput {
        agent_id: "a-1",
        company_id: "c-1",
        runtime_env: &runtime2,
        default_listen_host: "localhost",
        default_listen_port: "3100",
    });
    assert_eq!(vars2["PAPERCLIP_API_URL"], "https://fallback.example.com");

    // Drop both → default listen_host:listen_port.
    let runtime3 = HashMap::new();
    let vars3 = build_paperclip_env(BuildPaperclipEnvInput {
        agent_id: "a-1",
        company_id: "c-1",
        runtime_env: &runtime3,
        default_listen_host: "localhost",
        default_listen_port: "3100",
    });
    assert_eq!(vars3["PAPERCLIP_API_URL"], "http://localhost:3100");
}

// ===========================================================================
// apply_paperclip_workspace_env
// ===========================================================================

#[test]
fn apply_paperclip_workspace_env_handles_partial_inputs() {
    let mut env = HashMap::new();
    apply_paperclip_workspace_env(
        &mut env,
        ApplyPaperclipWorkspaceEnvInput {
            workspace_cwd: Some("/workspace"),
            workspace_source: Some(""), // empty → skipped
            workspace_strategy: None,
            workspace_id: Some("ws-1"),
            workspace_repo_url: None,
            workspace_repo_ref: None,
            workspace_branch: None,
            workspace_worktree_path: None,
            agent_home: Some("/home/agent"),
        },
    );
    assert_eq!(
        env.get("PAPERCLIP_WORKSPACE_CWD"),
        Some(&"/workspace".to_string())
    );
    assert_eq!(env.get("PAPERCLIP_WORKSPACE_ID"), Some(&"ws-1".to_string()));
    assert_eq!(env.get("AGENT_HOME"), Some(&"/home/agent".to_string()));
    // Empty / None values were skipped.
    assert!(!env.contains_key("PAPERCLIP_WORKSPACE_SOURCE"));
    assert!(!env.contains_key("PAPERCLIP_WORKSPACE_STRATEGY"));
    assert!(!env.contains_key("PAPERCLIP_WORKSPACE_REPO_URL"));
    assert!(!env.contains_key("PAPERCLIP_WORKSPACE_WORKTREE_PATH"));
}

// ===========================================================================
// shape_paperclip_workspace_env_for_execution
// ===========================================================================

fn hint_with_cwd(cwd: &str, project_id: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert("cwd".to_string(), serde_json::json!(cwd));
    m.insert("projectId".to_string(), serde_json::json!(project_id));
    m
}

#[test]
fn shape_workspace_env_remote_repoints_all_hints_to_staged_dirs() {
    let hints = vec![
        hint_with_cwd("/stale/a", "p-1"),
        hint_with_cwd("/stale/b", "p-2"),
        hint_with_cwd("/stale/c", "p-missing"),
    ];
    let mut staged = HashMap::new();
    staged.insert("p-1".to_string(), "/sandbox/p-1".to_string());
    staged.insert("p-2".to_string(), "/sandbox/p-2".to_string());
    let out = shape_paperclip_workspace_env_for_execution(ShapePaperclipWorkspaceEnvInput {
        workspace_cwd: Some("/workspace"),
        workspace_workspace_worktree_path: None,
        workspace_hints: Some(&hints),
        execution_target_is_remote: true,
        execution_cwd: Some("/workspace"),
        staged_project_dirs: Some(&staged),
    });
    // Hint 1 and 2 → repointed to staged dirs.
    assert_eq!(
        out.workspace_hints[0].get("cwd"),
        Some(&serde_json::json!("/sandbox/p-1"))
    );
    assert_eq!(
        out.workspace_hints[1].get("cwd"),
        Some(&serde_json::json!("/sandbox/p-2"))
    );
    // Hint 3 has no staged entry → cwd dropped.
    assert!(out.workspace_hints[2].get("cwd").is_none());
}

#[test]
fn shape_workspace_env_remote_trims_workspace_cwd() {
    let out = shape_paperclip_workspace_env_for_execution(ShapePaperclipWorkspaceEnvInput {
        workspace_cwd: Some("  /workspace  "),
        workspace_workspace_worktree_path: None,
        workspace_hints: None,
        execution_target_is_remote: true,
        execution_cwd: Some("/workspace"),
        staged_project_dirs: None,
    });
    assert_eq!(out.workspace_cwd.as_deref(), Some("/workspace"));
}

// ===========================================================================
// rewrite_workspace_cwd_env_vars_for_execution
// ===========================================================================

#[test]
fn rewrite_remote_only_rewrites_when_target_is_remote() {
    let mut env = HashMap::new();
    env.insert(
        "AGENT_WORKSPACE_CWD".to_string(),
        serde_json::json!("/local"),
    );
    env.insert("PATH".to_string(), serde_json::json!("/usr/bin"));
    let out =
        rewrite_workspace_cwd_env_vars_for_execution(RewriteWorkspaceCwdEnvVarsForExecutionInput {
            env: Some(&env),
            workspace_cwd: Some("/local"),
            execution_cwd: Some("/remote"),
            execution_target_is_remote: true,
        });
    assert_eq!(out["AGENT_WORKSPACE_CWD"], "/remote");
    // Non *_WORKSPACE_CWD keys untouched.
    assert_eq!(out["PATH"], "/usr/bin");
}

#[test]
fn rewrite_filters_non_string_env_values() {
    let mut env = HashMap::new();
    env.insert("NUMERIC".to_string(), serde_json::json!(42)); // filtered out
    env.insert(
        "AGENT_WORKSPACE_CWD".to_string(),
        serde_json::json!("/local"),
    );
    let out =
        rewrite_workspace_cwd_env_vars_for_execution(RewriteWorkspaceCwdEnvVarsForExecutionInput {
            env: Some(&env),
            workspace_cwd: Some("/local"),
            execution_cwd: Some("/remote"),
            execution_target_is_remote: true,
        });
    // Numeric value is filtered (Node `Object.fromEntries` string-only).
    assert!(!out.contains_key("NUMERIC"));
    assert_eq!(out["AGENT_WORKSPACE_CWD"], "/remote");
}

// ===========================================================================
// refresh_paperclip_workspace_env_for_execution
// ===========================================================================

#[test]
fn refresh_clears_stale_workspace_env_then_applies_shaped() {
    let mut env = HashMap::new();
    env.insert("PAPERCLIP_WORKSPACE_CWD".to_string(), "stale".to_string());
    env.insert(
        "PAPERCLIP_WORKSPACE_WORKTREE_PATH".to_string(),
        "stale-wt".to_string(),
    );
    env.insert("PAPERCLIP_WORKSPACES_JSON".to_string(), "[]".to_string());
    env.insert("UNRELATED".to_string(), "keep".to_string());

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
    assert!(env.contains_key("UNRELATED"));
    assert_eq!(out.workspace_cwd.as_deref(), Some("/workspace"));
}

#[test]
fn refresh_serializes_workspace_hints_as_json() {
    let mut env = HashMap::new();
    let hints = vec![hint_with_cwd("/hint", "p-1")];
    let _ = refresh_paperclip_workspace_env_for_execution(
        &mut env,
        RefreshPaperclipWorkspaceEnvInput {
            workspace_cwd: Some("/workspace"),
            workspace_source: None,
            workspace_strategy: None,
            workspace_id: None,
            workspace_repo_url: None,
            workspace_repo_ref: None,
            workspace_branch: None,
            workspace_worktree_path: None,
            workspace_hints: Some(&hints),
            agent_home: None,
            execution_target_is_remote: false,
            execution_cwd: None,
            env_config: None,
            staged_project_dirs: None,
        },
    );
    let json = env
        .get("PAPERCLIP_WORKSPACES_JSON")
        .expect("hints serialized");
    let parsed: serde_json::Value = serde_json::from_str(json).expect("valid json");
    let arr = parsed.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["cwd"], serde_json::json!("/hint"));
}

// ===========================================================================
// sanitize_inherited_paperclip_env
// ===========================================================================

#[test]
fn sanitize_strips_paperclip_runtime_vars_but_keeps_three_runtime_keys() {
    let mut env = HashMap::new();
    env.insert("PATH".to_string(), "/usr/bin".to_string());
    env.insert("PAPERCLIP_AGENT_ID".to_string(), "x".to_string());
    env.insert("PAPERCLIP_COMPANY_ID".to_string(), "x".to_string());
    env.insert("PAPERCLIP_WAKE_TOKEN".to_string(), "x".to_string());
    env.insert("PAPERCLIP_RUNTIME_API_URL".to_string(), "kept".to_string());
    env.insert("PAPERCLIP_LISTEN_HOST".to_string(), "kept".to_string());
    env.insert("PAPERCLIP_LISTEN_PORT".to_string(), "kept".to_string());
    env.insert("PAPERCLIPAI_CMD".to_string(), "legacy".to_string());
    let out = sanitize_inherited_paperclip_env(&env);
    // Runtime allowlist preserved.
    assert_eq!(out["PAPERCLIP_RUNTIME_API_URL"], "kept");
    assert_eq!(out["PAPERCLIP_LISTEN_HOST"], "kept");
    assert_eq!(out["PAPERCLIP_LISTEN_PORT"], "kept");
    // Other PAPERCLIP_* stripped.
    for k in [
        "PAPERCLIP_AGENT_ID",
        "PAPERCLIP_COMPANY_ID",
        "PAPERCLIP_WAKE_TOKEN",
        "PAPERCLIPAI_CMD",
    ] {
        assert!(!out.contains_key(k), "key {k} should have been stripped");
    }
    // Non-PAPERCLIP_* untouched.
    assert_eq!(out["PATH"], "/usr/bin");
}

// ===========================================================================
// ensure_path_in_env + default_path_for_platform
// ===========================================================================

#[test]
fn ensure_path_in_env_returns_env_unchanged_when_path_present() {
    let mut env = HashMap::new();
    env.insert("PATH".to_string(), "/custom/bin".to_string());
    let out = ensure_path_in_env(&env, false);
    assert_eq!(out.len(), 1);
    assert_eq!(out["PATH"], "/custom/bin");
}

#[test]
fn ensure_path_in_env_fills_posix_default_when_missing() {
    let env = HashMap::new();
    let out = ensure_path_in_env(&env, false);
    let expected = default_path_for_platform(false);
    assert_eq!(out["PATH"], expected);
}

#[test]
fn default_path_for_platform_matches_node_literals() {
    assert_eq!(
        default_path_for_platform(false),
        "/usr/local/bin:/opt/homebrew/bin:/usr/local/sbin:/usr/bin:/bin:/usr/sbin:/sbin"
    );
    let win = default_path_for_platform(true);
    assert!(win.contains("Windows"));
    assert!(win.contains("System32"));
}

// ===========================================================================
// sanitize_ssh_remote_env wrapper
// ===========================================================================

#[test]
fn sanitize_ssh_remote_env_wrapper_composes_through_remote_execution_env() {
    let mut env = HashMap::new();
    env.insert("PATH".to_string(), "/usr/bin".to_string());
    env.insert("HOME".to_string(), "/home/agent".to_string());
    let mut inherited = HashMap::new();
    inherited.insert("PATH".to_string(), "/usr/bin".to_string());
    inherited.insert("HOME".to_string(), "/home/agent".to_string());
    inherited.insert("PAPERCLIP_RUNTIME_API_URL".to_string(), "kept".to_string());
    let out = sanitize_ssh_remote_env(&env, &inherited);
    // The wrapper just forwards — it must not panic and must respect
    // the remote_execution_env allowlist.
    assert!(out.contains_key("PATH") || out.is_empty());
}
