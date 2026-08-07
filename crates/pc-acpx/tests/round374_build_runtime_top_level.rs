//! R374 集成测试 — `pc-acpx` `build_runtime` 顶层组装。
//!
//! 覆盖:
//! - 与已有 helpers(`normalize_*`、`build_paperclip_env`、
//!   `apply_paperclip_workspace_env`、`build_codex_startup_config`、
//!   `short_hash`、`default_state_dir`)的协同
//! - 端到端 happy path(claude/codex/gemini 三种 agent)
//! - 各种 normalize 分支(mode、permission、model、thinking effort)
//! - workspace 标识注入(env vars + session_key segments)
//! - 唤醒上下文(wake task / approval / linked issues)
//! - 远程 sandbox lane 字段(staged_runtime + env delta + callbacks)
//! - fingerprint 在配置变更时正确翻转

use pc_acpx::{
    apply_paperclip_workspace_env, build_paperclip_env, build_runtime, AgentIdentity,
    BuildRuntimeInput, PreparedRuntime, PreparedRuntimeMode,
    PreparedRuntimeNonInteractivePermissions, PreparedRuntimePermissionMode, PreparedStagedRuntime,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn unique_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "pc-acpx-r374-{label}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ))
}

fn minimal_input(agent_id: &str, company_id: &str, cwd: &Path) -> BuildRuntimeInput {
    BuildRuntimeInput::for_test(agent_id, company_id, cwd)
}

#[test]
fn happy_path_claude_agent_assembles_full_prepared_runtime() {
    let input = minimal_input("claude", "co_h", Path::new("/repo/h"));
    let runtime = build_runtime(&input).expect("claude runtime");

    // 1) Identity
    assert_eq!(runtime.acpx_agent, "claude");

    // 2) Env block
    assert_eq!(
        runtime.env.get("PAPERCLIP_AGENT_ID"),
        Some(&"claude".to_string())
    );
    assert_eq!(
        runtime.env.get("PAPERCLIP_COMPANY_ID"),
        Some(&"co_h".to_string())
    );
    assert_eq!(
        runtime.env.get("PAPERCLIP_API_URL"),
        Some(&"http://localhost:3100".to_string())
    );
    assert_eq!(
        runtime.env.get("PAPERCLIP_RUN_ID"),
        Some(&"run_test".to_string())
    );

    // 3) State dir
    assert!(runtime.state_dir.to_string_lossy().contains("claude"));

    // 4) Session key shape
    let segments: Vec<&str> = runtime.session_key.split(':').collect();
    assert_eq!(segments[0], "paperclip");
    assert_eq!(segments[1], "co_h");
    assert_eq!(segments[2], "claude");
    assert!(!runtime.fingerprint.is_empty());

    // 5) Remote lane fields default to None
    assert!(runtime.staged_runtime.is_none());
    assert!(runtime.remote_staging_env_delta.is_none());
    assert!(runtime.remote_managed_home_teardown.is_none());
    assert!(runtime.remote_staging_dispose.is_none());
}

#[test]
fn happy_path_codex_agent_applies_startup_config_and_fast_mode() {
    let mut input = minimal_input("codex", "co_c", Path::new("/repo/c"));
    input.config = serde_json::json!({
        "agent": "codex",
        "fastMode": true,
        "model": "gpt-5",
    });
    let runtime = build_runtime(&input).expect("codex runtime");

    assert_eq!(runtime.acpx_agent, "codex");
    assert!(runtime.fast_mode);
    assert_eq!(runtime.requested_model, "gpt-5");
    assert!(runtime
        .env
        .get("CODEX_CONFIG")
        .map(|s| !s.is_empty())
        .unwrap_or(false));
}

#[test]
fn happy_path_gemini_agent_skips_codex_specific_env() {
    let mut input = minimal_input("gemini", "co_g", Path::new("/repo/g"));
    input.config = serde_json::json!({ "agent": "gemini", "model": "gemini-2.5-pro" });
    let runtime = build_runtime(&input).expect("gemini runtime");

    assert_eq!(runtime.acpx_agent, "gemini");
    assert_eq!(runtime.requested_model, "gemini-2.5-pro");
    assert!(runtime.env.get("CODEX_CONFIG").is_none());
    assert!(runtime.env.get("ANTHROPIC_MODEL").is_none());
}

#[test]
fn normalize_branches_apply_independently() {
    let mut input = minimal_input("claude", "co_n", Path::new("/repo/n"));
    input.config = serde_json::json!({
        "agent": "claude",
        "mode": "oneshot",
        "permissionMode": "approve-reads",
        "nonInteractivePermissions": "fail",
        "thinkingEffort": "  high  ",
        "model": "claude-opus-4-7",
    });
    let runtime = build_runtime(&input).expect("runtime");

    assert_eq!(runtime.mode, PreparedRuntimeMode::OneShot);
    assert_eq!(
        runtime.permission_mode,
        PreparedRuntimePermissionMode::ApproveReads
    );
    assert_eq!(
        runtime.non_interactive_permissions,
        PreparedRuntimeNonInteractivePermissions::Fail
    );
    assert_eq!(runtime.requested_thinking_effort, "high");
    assert_eq!(runtime.requested_model, "claude-opus-4-7");
    assert_eq!(
        runtime.env.get("ANTHROPIC_MODEL"),
        Some(&"claude-opus-4-7".to_string())
    );
}

#[test]
fn unknown_normalize_values_fall_back_to_defaults() {
    let mut input = minimal_input("claude", "co_n", Path::new("/repo/n"));
    input.config = serde_json::json!({
        "agent": "claude",
        "mode": "sandboxed", // unknown
        "permissionMode": "weird", // unknown
        "nonInteractivePermissions": "unknown", // unknown
        "thinkingEffort": "", // empty
    });
    let runtime = build_runtime(&input).expect("runtime");

    // Defaults: persistent / approve-all / deny / no model
    assert_eq!(runtime.mode, PreparedRuntimeMode::Persistent);
    assert_eq!(
        runtime.permission_mode,
        PreparedRuntimePermissionMode::ApproveAll
    );
    assert_eq!(
        runtime.non_interactive_permissions,
        PreparedRuntimeNonInteractivePermissions::Deny
    );
    assert_eq!(runtime.requested_thinking_effort, "");
}

#[test]
fn workspace_identity_is_injected_into_env() {
    let mut input = minimal_input("claude", "co_w", Path::new("/worktree"));
    input.workspace_id = "ws_42".into();
    input.workspace_repo_url = "git@github.com:foo/bar.git".into();
    input.workspace_repo_ref = "refs/heads/main".into();
    input.workspace_branch = "main".into();
    input.workspace_source = "realized".into();
    input.workspace_strategy = "worktree".into();
    input.workspace_worktree_path = "/worktree".into();
    input.agent_home = "/home/agent".into();

    let runtime = build_runtime(&input).expect("runtime");

    for (key, expected) in [
        ("PAPERCLIP_WORKSPACE_CWD", "/worktree"),
        ("PAPERCLIP_WORKSPACE_ID", "ws_42"),
        ("PAPERCLIP_WORKSPACE_REPO_URL", "git@github.com:foo/bar.git"),
        ("PAPERCLIP_WORKSPACE_REPO_REF", "refs/heads/main"),
        ("PAPERCLIP_WORKSPACE_BRANCH", "main"),
        ("PAPERCLIP_WORKSPACE_SOURCE", "realized"),
        ("PAPERCLIP_WORKSPACE_STRATEGY", "worktree"),
        ("PAPERCLIP_WORKSPACE_WORKTREE_PATH", "/worktree"),
        ("AGENT_HOME", "/home/agent"),
    ] {
        assert_eq!(
            runtime.env.get(key).cloned(),
            Some(expected.to_string()),
            "missing env var {key}"
        );
    }

    // session_key uses workspace_id when no taskKey provided.
    let segments: Vec<&str> = runtime.session_key.split(':').collect();
    assert_eq!(segments[3], "ws_42");
}

#[test]
fn wake_context_block_projects_onto_env() {
    let mut input = minimal_input("claude", "co_w", Path::new("/repo"));
    input.context = serde_json::json!({
        "taskId": "task_42",
        "wakeReason": "approval_pending",
        "wakeCommentId": "cmt_99",
        "approvalId": "apr_1",
        "approvalStatus": "approved",
        "issueIds": ["i1", "  ", "i3"],
    });
    let runtime = build_runtime(&input).expect("runtime");

    assert_eq!(
        runtime.env.get("PAPERCLIP_TASK_ID"),
        Some(&"task_42".to_string())
    );
    assert_eq!(
        runtime.env.get("PAPERCLIP_WAKE_REASON"),
        Some(&"approval_pending".to_string())
    );
    assert_eq!(
        runtime.env.get("PAPERCLIP_WAKE_COMMENT_ID"),
        Some(&"cmt_99".to_string())
    );
    assert_eq!(
        runtime.env.get("PAPERCLIP_APPROVAL_ID"),
        Some(&"apr_1".to_string())
    );
    assert_eq!(
        runtime.env.get("PAPERCLIP_APPROVAL_STATUS"),
        Some(&"approved".to_string())
    );
    // Empty linked-issue entries are dropped.
    assert_eq!(
        runtime.env.get("PAPERCLIP_LINKED_ISSUE_IDS"),
        Some(&"i1,i3".to_string())
    );
}

#[test]
fn auth_token_overrides_and_empty_is_dropped() {
    let mut input = minimal_input("claude", "co_a", Path::new("/repo"));
    input.auth_token = Some("token_xyz".into());
    let runtime = build_runtime(&input).expect("runtime");
    assert_eq!(
        runtime.env.get("PAPERCLIP_API_KEY"),
        Some(&"token_xyz".to_string())
    );

    let mut input = minimal_input("claude", "co_a", Path::new("/repo"));
    input.auth_token = Some(String::new());
    let runtime = build_runtime(&input).expect("runtime");
    assert!(runtime.env.get("PAPERCLIP_API_KEY").is_none());

    let input = minimal_input("claude", "co_a", Path::new("/repo"));
    let runtime = build_runtime(&input).expect("runtime");
    assert!(runtime.env.get("PAPERCLIP_API_KEY").is_none());
}

#[test]
fn fingerprint_changes_when_key_inputs_change() {
    let mut input_a = minimal_input("claude", "co_f", Path::new("/repo"));
    let mut input_b = minimal_input("claude", "co_f", Path::new("/repo"));
    input_a.config = serde_json::json!({ "agent": "claude", "model": "claude-opus" });
    input_b.config = serde_json::json!({ "agent": "claude", "model": "claude-sonnet" });
    let runtime_a = build_runtime(&input_a).expect("a");
    let runtime_b = build_runtime(&input_b).expect("b");
    assert_ne!(runtime_a.fingerprint, runtime_b.fingerprint);
    assert_ne!(runtime_a.session_key, runtime_b.session_key);
}

#[test]
fn fingerprint_changes_when_mode_or_permission_change() {
    let base = minimal_input("claude", "co_f", Path::new("/repo"));
    let mut one_shot = base.clone();
    one_shot.config = serde_json::json!({ "agent": "claude", "mode": "oneshot" });
    let mut deny = base.clone();
    deny.config = serde_json::json!({ "agent": "claude", "permissionMode": "deny-all" });
    let runtime_base = build_runtime(&base).expect("base");
    let runtime_one_shot = build_runtime(&one_shot).expect("oneshot");
    let runtime_deny = build_runtime(&deny).expect("deny");

    assert_ne!(runtime_base.fingerprint, runtime_one_shot.fingerprint);
    assert_ne!(runtime_base.fingerprint, runtime_deny.fingerprint);
}

#[test]
fn fingerprint_is_stable_for_identical_inputs() {
    let input_a = minimal_input("claude", "co_f", Path::new("/repo"));
    let input_b = minimal_input("claude", "co_f", Path::new("/repo"));
    let a = build_runtime(&input_a).expect("a");
    let b = build_runtime(&input_b).expect("b");
    assert_eq!(a.fingerprint, b.fingerprint);
    assert_eq!(a.session_key, b.session_key);
}

#[test]
fn remote_lane_fields_propagate_when_set() {
    let mut input = minimal_input("claude", "co_r", Path::new("/host/repo"));
    let staged = PreparedStagedRuntime::remote("/host/repo", "/sandbox/workspace");
    let mut delta = std::collections::BTreeMap::new();
    delta.insert("CODEX_HOME".to_string(), "/sandbox/home".to_string());
    let teardown = pc_acpx::AsyncCallback::new(|| async {});
    let dispose = pc_acpx::AsyncCallback::new(|| async {});

    input.staged_runtime = Some(staged.clone());
    input.execution_target_is_remote = true;

    let runtime = build_runtime(&input).expect("runtime");
    // staged_runtime wired through
    assert_eq!(runtime.staged_runtime, Some(staged));

    // We can also attach the other remote fields through the builder path,
    // not via build_runtime (which is the local / pure path). Validate the
    // builder wires them as expected.
    let built_with_callbacks = PreparedRuntime::builder("claude")
        .cwd("/repo")
        .remote_staging_env_delta(delta.clone())
        .remote_managed_home_teardown(teardown)
        .remote_staging_dispose(dispose)
        .build();
    assert_eq!(built_with_callbacks.remote_staging_env_delta, Some(delta));
    assert!(built_with_callbacks.remote_managed_home_teardown.is_some());
    assert!(built_with_callbacks.remote_staging_dispose.is_some());
}

#[test]
fn remote_lane_records_sandbox_note_in_timeout_resolution() {
    let mut input = minimal_input("claude", "co_r", Path::new("/repo"));
    input.execution_target_is_remote = true;
    let runtime = build_runtime(&input).expect("runtime");
    let note = runtime.timeout_resolution.note.as_deref().unwrap_or("");
    assert!(note.contains("sandbox"));
    assert_eq!(runtime.timeout_resolution.source, "adapterConfig");
}

#[test]
fn local_lane_records_default_timeout_source() {
    let input = minimal_input("claude", "co_l", Path::new("/repo"));
    let runtime = build_runtime(&input).expect("runtime");
    // Default timeout = DEFAULT_ACP_ENGINE_TIMEOUT_SEC when not set in config.
    assert_eq!(runtime.timeout_resolution.source, "default");
    assert_eq!(runtime.timeout_sec, pc_acpx::DEFAULT_ACP_ENGINE_TIMEOUT_SEC);
}

#[test]
fn explicit_timeout_overrides_default_source() {
    let mut input = minimal_input("claude", "co_t", Path::new("/repo"));
    input.config = serde_json::json!({
        "agent": "claude",
        "timeoutSec": 120,
    });
    let runtime = build_runtime(&input).expect("runtime");
    assert_eq!(runtime.timeout_sec, 120);
    assert_eq!(runtime.timeout_resolution.source, "adapterConfig");
    assert!(runtime.timeout_resolution.note.is_none());
}

#[test]
fn paperclip_env_helpers_compose_with_build_runtime() {
    // Direct verification of the helpers — these are the building blocks
    // build_runtime composes onto PreparedRuntime.
    let mut process_env = HashMap::new();
    process_env.insert("PAPERCLIP_LISTEN_PORT".into(), "4100".into());
    let agent = AgentIdentity::new("claude", "company_p");
    let env = build_paperclip_env(&agent, &process_env);
    assert_eq!(
        env.get("PAPERCLIP_API_URL"),
        Some(&"http://localhost:4100".to_string())
    );

    let mut env = std::collections::BTreeMap::new();
    apply_paperclip_workspace_env(
        &mut env, "/cwd", "realized", "worktree", "ws", "repo_url", "main", "main", "/wt", "/home",
    );
    assert_eq!(
        env.get("PAPERCLIP_WORKSPACE_CWD"),
        Some(&"/cwd".to_string())
    );
    assert_eq!(env.get("AGENT_HOME"), Some(&"/home".to_string()));
}

#[test]
fn codex_with_existing_codex_config_merges_through_helper() {
    let mut input = minimal_input("codex", "co_x", Path::new("/repo"));
    input.config = serde_json::json!({
        "agent": "codex",
        "model": "gpt-5",
        "thinkingEffort": "high",
        "CODEX_CONFIG": r#"{"existing":"value"}"#,
    });
    let runtime = build_runtime(&input).expect("runtime");
    let merged = runtime.env.get("CODEX_CONFIG").expect("codex config");
    // Existing "existing":"value" must survive the merge.
    assert!(merged.contains("existing"));
    assert!(merged.contains("value"));
    // And the runtime-requested model/effort must be appended.
    assert!(
        merged.contains("model") || merged.contains("gpt-5") || merged.contains("thinkingEffort")
    );
}

#[test]
fn state_dir_overrides_default_resolution() {
    let mut input = minimal_input("claude", "co_s", Path::new("/repo"));
    input.state_dir = Some(PathBuf::from("/custom/state"));
    let runtime = build_runtime(&input).expect("runtime");
    assert_eq!(runtime.state_dir, PathBuf::from("/custom/state"));
}

#[test]
fn fingerprint_input_includes_execution_target() {
    let mut input_local = minimal_input("claude", "co_e", Path::new("/repo"));
    let mut input_remote = minimal_input("claude", "co_e", Path::new("/repo"));
    input_remote.execution_target_is_remote = true;
    let local = build_runtime(&input_local).expect("local");
    let remote = build_runtime(&input_remote).expect("remote");
    assert_ne!(local.fingerprint, remote.fingerprint);
}

#[test]
fn logged_env_mirrors_runtime_env_in_pure_assembly() {
    // The pure assembly cannot redact secrets (no caller-provided
    // redaction layer in R374). The logged_env is byte-identical to env
    // until R375 wires the secret-redaction helper.
    let mut input = minimal_input("claude", "co_l", Path::new("/repo"));
    input.auth_token = Some("token_xyz".into());
    let runtime = build_runtime(&input).expect("runtime");
    assert_eq!(
        runtime.env.get("PAPERCLIP_API_KEY"),
        runtime.logged_env.get("PAPERCLIP_API_KEY")
    );
}

#[test]
fn unique_fingerprints_for_distinct_agents() {
    let claude = minimal_input("claude", "co_x", Path::new("/repo"));
    let codex = minimal_input("codex", "co_x", Path::new("/repo"));
    let gemini = minimal_input("gemini", "co_x", Path::new("/repo"));
    let a = build_runtime(&claude).unwrap();
    let b = build_runtime(&codex).unwrap();
    let c = build_runtime(&gemini).unwrap();
    // Without config.agent each defaults to "claude", so a and b may collide.
    // Setting config.agent explicitly gives a 3-way split.
    let mut codex_with_agent = codex.clone();
    codex_with_agent.config = serde_json::json!({ "agent": "codex" });
    let mut gemini_with_agent = gemini.clone();
    gemini_with_agent.config = serde_json::json!({ "agent": "gemini" });
    let b2 = build_runtime(&codex_with_agent).unwrap();
    let c2 = build_runtime(&gemini_with_agent).unwrap();
    assert_ne!(a.fingerprint, b2.fingerprint);
    assert_ne!(a.fingerprint, c2.fingerprint);
    assert_ne!(b2.fingerprint, c2.fingerprint);
}

#[test]
fn session_key_segments_use_compound_fingerprint() {
    let mut input = minimal_input("claude", "co_s", Path::new("/repo"));
    input.context = serde_json::json!({ "taskKey": "task_priority" });
    let runtime = build_runtime(&input).expect("runtime");
    let segments: Vec<&str> = runtime.session_key.split(':').collect();
    // paperclip:<company>:<agent>:<taskKey>:<fingerprint>
    assert_eq!(segments.len(), 5);
    assert_eq!(segments[3], "task_priority");
    assert_eq!(segments[4], runtime.fingerprint);
}

#[test]
fn unknown_agent_in_config_is_passed_through() {
    // normalize_agent passes through any non-empty value verbatim. The
    // agent validation happens downstream (e.g. via acpx_agent_id_for_adapter_type).
    let mut input = minimal_input("claude", "co_x", Path::new("/repo"));
    input.config = serde_json::json!({ "agent": "totally-bogus-agent" });
    let runtime = build_runtime(&input).expect("runtime");
    assert_eq!(runtime.acpx_agent, "totally-bogus-agent");
}

#[test]
fn empty_agent_in_config_falls_back_to_default() {
    let mut input = minimal_input("claude", "co_x", Path::new("/repo"));
    input.config = serde_json::json!({ "agent": "" });
    let runtime = build_runtime(&input).expect("runtime");
    assert_eq!(runtime.acpx_agent, "claude");
}
