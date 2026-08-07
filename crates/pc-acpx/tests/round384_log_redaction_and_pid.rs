//! R384 — Integration tests for the `log_redaction` module:
//! - `isPaperclipRuntimeEnvKey` / `isForbiddenConfigEnvKey`
//! - `expandHomePrefix` / `sanitizeInheritedPaperclipEnv`
//! - `redactEnvForLogs` / `redactCommandTextForLogs`
//! - `buildInvocationEnvForLogs` (options + merging + redaction)
//! - `isPidAlive` (real-process smoke check)
//!
//! Mirrors the Node parity surface in
//! `packages/adapter-utils/src/server-utils.ts` L114-2241 / L3003-3013
//! and `command-redaction.ts` L1-L60.

use pc_acpx::{
    build_invocation_env_for_logs, expand_home_prefix, is_forbidden_config_env_key,
    is_paperclip_runtime_env_key, is_pid_alive, is_sensitive_env_key, redact_command_text_for_logs,
    redact_env_for_logs, sanitize_inherited_paperclip_env, InvocationEnvOptions,
    DEFAULT_RESOLVED_COMMAND_ENV_KEY, REDACTED_LOG_VALUE,
};
use std::collections::BTreeMap;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Env-key classification
// ---------------------------------------------------------------------------

#[test]
fn paperclip_runtime_env_key_distinguishes_prefix() {
    assert!(is_paperclip_runtime_env_key("PAPERCLIP_AGENT_ID"));
    assert!(is_paperclip_runtime_env_key("PAPERCLIP_API_URL"));
    assert!(is_paperclip_runtime_env_key("PAPERCLIP_RESOLVED_COMMAND"));
    assert!(!is_paperclip_runtime_env_key("PATH"));
    assert!(!is_paperclip_runtime_env_key("USER"));
    assert!(!is_paperclip_runtime_env_key("paperclip_lower"));
}

#[test]
fn forbidden_config_env_key_is_exactly_api_key() {
    assert!(is_forbidden_config_env_key("PAPERCLIP_API_KEY"));
    assert!(!is_forbidden_config_env_key("PAPERCLIP_API_URL"));
    assert!(!is_forbidden_config_env_key("PATH"));
    assert!(!is_forbidden_config_env_key(""));
}

// ---------------------------------------------------------------------------
// Sensitive-key detection
// ---------------------------------------------------------------------------

#[test]
fn sensitive_env_key_matches_case_insensitively() {
    assert!(is_sensitive_env_key("OPENAI_API_KEY"));
    assert!(is_sensitive_env_key("github_token"));
    assert!(is_sensitive_env_key("DB_PASSWORD"));
    assert!(is_sensitive_env_key("authorization"));
    assert!(is_sensitive_env_key("Set-Cookie"));
    assert!(!is_sensitive_env_key("USER"));
    assert!(!is_sensitive_env_key("PATH"));
    assert!(!is_sensitive_env_key("LANG"));
}

// ---------------------------------------------------------------------------
// Path expansion
// ---------------------------------------------------------------------------

#[test]
fn expand_home_prefix_handles_tilde_variants() {
    let home = PathBuf::from("/Users/alice");
    assert_eq!(expand_home_prefix("~", &home), "/Users/alice");
    assert_eq!(expand_home_prefix("~/skills", &home), "/Users/alice/skills");
    assert_eq!(
        expand_home_prefix("~/a/b/c.txt", &home),
        "/Users/alice/a/b/c.txt"
    );
    assert_eq!(expand_home_prefix("/etc/passwd", &home), "/etc/passwd");
    assert_eq!(expand_home_prefix("relative/path", &home), "relative/path");
    // `~name` is NOT expanded (matches Node behavior).
    assert_eq!(expand_home_prefix("~bob/x", &home), "~bob/x");
}

// ---------------------------------------------------------------------------
// redactEnvForLogs
// ---------------------------------------------------------------------------

#[test]
fn redact_env_for_logs_masks_all_sensitive_keys() {
    let mut env = BTreeMap::new();
    env.insert("PATH".to_string(), "/usr/bin".to_string());
    env.insert("LANG".to_string(), "en_US.UTF-8".to_string());
    env.insert("USER".to_string(), "alice".to_string());
    env.insert("OPENAI_API_KEY".to_string(), "sk-abc123".to_string());
    env.insert("GITHUB_TOKEN".to_string(), "ghp_xyz".to_string());
    env.insert("DB_PASSWORD".to_string(), "hunter2".to_string());
    env.insert("SESSION_COOKIE".to_string(), "abc".to_string());
    env.insert("AUTHORIZATION".to_string(), "Bearer foo".to_string());
    let r = redact_env_for_logs(&env);
    assert_eq!(r.get("PATH").unwrap(), "/usr/bin");
    assert_eq!(r.get("LANG").unwrap(), "en_US.UTF-8");
    assert_eq!(r.get("USER").unwrap(), "alice");
    assert_eq!(r.get("OPENAI_API_KEY").unwrap(), REDACTED_LOG_VALUE);
    assert_eq!(r.get("GITHUB_TOKEN").unwrap(), REDACTED_LOG_VALUE);
    assert_eq!(r.get("DB_PASSWORD").unwrap(), REDACTED_LOG_VALUE);
    assert_eq!(r.get("SESSION_COOKIE").unwrap(), REDACTED_LOG_VALUE);
    assert_eq!(r.get("AUTHORIZATION").unwrap(), REDACTED_LOG_VALUE);
    assert_eq!(r.len(), env.len());
}

// ---------------------------------------------------------------------------
// sanitizeInheritedPaperclipEnv
// ---------------------------------------------------------------------------

#[test]
fn sanitize_inherited_keeps_allowlist_drops_other_paperclip() {
    let mut env = BTreeMap::new();
    env.insert("PATH".to_string(), "/usr/bin".to_string());
    env.insert("USER".to_string(), "alice".to_string());
    env.insert(
        "PAPERCLIP_RUNTIME_API_URL".to_string(),
        "http://api".to_string(),
    );
    env.insert("PAPERCLIP_LISTEN_HOST".to_string(), "0.0.0.0".to_string());
    env.insert("PAPERCLIP_LISTEN_PORT".to_string(), "3100".to_string());
    env.insert("PAPERCLIP_AGENT_ID".to_string(), "ag_1".to_string());
    env.insert("PAPERCLIP_API_KEY".to_string(), "sk-leak".to_string());
    env.insert(
        "PAPERCLIP_RESOLVED_COMMAND".to_string(),
        "echo secret".to_string(),
    );
    env.insert("PAPERCLIPAI_CMD".to_string(), "legacy".to_string());

    let s = sanitize_inherited_paperclip_env(&env);
    // Non-Paperclip keys pass through.
    assert_eq!(s.get("PATH").unwrap(), "/usr/bin");
    assert_eq!(s.get("USER").unwrap(), "alice");
    // Allowlist keys pass through.
    assert_eq!(s.get("PAPERCLIP_RUNTIME_API_URL").unwrap(), "http://api");
    assert_eq!(s.get("PAPERCLIP_LISTEN_HOST").unwrap(), "0.0.0.0");
    assert_eq!(s.get("PAPERCLIP_LISTEN_PORT").unwrap(), "3100");
    // Other Paperclip keys drop.
    assert!(s.get("PAPERCLIP_AGENT_ID").is_none());
    assert!(s.get("PAPERCLIP_API_KEY").is_none());
    assert!(s.get("PAPERCLIP_RESOLVED_COMMAND").is_none());
    assert!(s.get("PAPERCLIPAI_CMD").is_none());
}

// ---------------------------------------------------------------------------
// redactCommandTextForLogs
// ---------------------------------------------------------------------------

#[test]
fn redact_command_text_passes_safe_commands_through() {
    assert_eq!(
        redact_command_text_for_logs("git status --short"),
        "git status --short"
    );
    assert_eq!(redact_command_text_for_logs("ls -la /tmp"), "ls -la /tmp");
}

#[test]
fn redact_command_text_redacts_openai_key() {
    let cmd = "openai call --key sk-abcdefghijklmnop";
    let out = redact_command_text_for_logs(cmd);
    assert!(out.contains(REDACTED_LOG_VALUE), "out={}", out);
    assert!(!out.contains("sk-abcdefghijklmnop"));
}

#[test]
fn redact_command_text_redacts_github_token() {
    let cmd = "gh auth login --token ghp_abcdefghijklmnopqrstuvwxyz";
    let out = redact_command_text_for_logs(cmd);
    assert!(out.contains(REDACTED_LOG_VALUE));
    assert!(!out.contains("ghp_abcdefghijklmnopqrstuvwxyz"));
}

#[test]
fn redact_command_text_redacts_authorization_bearer() {
    let cmd = "curl -H \"Authorization: Bearer abcdef0123\" https://api.example.com";
    let out = redact_command_text_for_logs(cmd);
    assert!(out.contains("Authorization: Bearer"));
    assert!(out.contains(REDACTED_LOG_VALUE));
    assert!(!out.contains("abcdef0123"));
}

#[test]
fn redact_command_text_keeps_short_sk_prefixes() {
    // `sk-abc` (3 chars) is below the 12-char minimum — must not be redacted.
    let cmd = "echo sk-abc";
    let out = redact_command_text_for_logs(cmd);
    assert!(out.contains("sk-abc"));
}

// ---------------------------------------------------------------------------
// buildInvocationEnvForLogs
// ---------------------------------------------------------------------------

#[test]
fn build_invocation_env_merges_includes_runtime_and_redacts() {
    let mut env = BTreeMap::new();
    env.insert("PATH".to_string(), "/usr/bin".to_string());
    env.insert("OPENAI_API_KEY".to_string(), "sk-leak".to_string());

    let mut runtime = BTreeMap::new();
    runtime.insert("EXTRA_FROM_RUNTIME".to_string(), "yes".to_string());
    runtime.insert("EXTRA_EMPTY".to_string(), "".to_string());
    runtime.insert("EXTRA_OVERRIDDEN".to_string(), "from-runtime".to_string());

    let opts = InvocationEnvOptions {
        runtime_env: Some(runtime),
        include_runtime_keys: vec![
            "EXTRA_FROM_RUNTIME".to_string(),
            "EXTRA_EMPTY".to_string(),
            "EXTRA_OVERRIDDEN".to_string(),
        ],
        resolved_command: Some("openai call --key sk-abc1234567890ab".to_string()),
        resolved_command_env_key: None,
    };
    let out = build_invocation_env_for_logs(&env, &opts);

    // Caller values preserved and redacted.
    assert_eq!(out.get("PATH").unwrap(), "/usr/bin");
    assert_eq!(out.get("OPENAI_API_KEY").unwrap(), REDACTED_LOG_VALUE);
    // Runtime keys fill missing slots.
    assert_eq!(out.get("EXTRA_FROM_RUNTIME").unwrap(), "yes");
    assert!(out.get("EXTRA_EMPTY").is_none());
    assert_eq!(out.get("EXTRA_OVERRIDDEN").unwrap(), "from-runtime");
    // Resolved command is redacted.
    assert!(out
        .get(DEFAULT_RESOLVED_COMMAND_ENV_KEY)
        .unwrap()
        .contains(REDACTED_LOG_VALUE));
}

#[test]
fn build_invocation_env_caller_value_wins_over_runtime() {
    let mut env = BTreeMap::new();
    env.insert("EXTRA".to_string(), "from-caller".to_string());
    let mut runtime = BTreeMap::new();
    runtime.insert("EXTRA".to_string(), "from-runtime".to_string());
    let opts = InvocationEnvOptions {
        runtime_env: Some(runtime),
        include_runtime_keys: vec!["EXTRA".to_string()],
        resolved_command: None,
        resolved_command_env_key: None,
    };
    let out = build_invocation_env_for_logs(&env, &opts);
    assert_eq!(out.get("EXTRA").unwrap(), "from-caller");
}

#[test]
fn build_invocation_env_custom_command_env_key() {
    let env = BTreeMap::new();
    let opts = InvocationEnvOptions {
        runtime_env: None,
        include_runtime_keys: vec![],
        resolved_command: Some("echo hello".to_string()),
        resolved_command_env_key: Some("PAPERCLIP_CMD".to_string()),
    };
    let out = build_invocation_env_for_logs(&env, &opts);
    assert!(out.contains_key("PAPERCLIP_CMD"));
    assert!(!out.contains_key(DEFAULT_RESOLVED_COMMAND_ENV_KEY));
}

#[test]
fn build_invocation_env_blank_resolved_command_omits_key() {
    let env = BTreeMap::new();
    let opts = InvocationEnvOptions {
        runtime_env: None,
        include_runtime_keys: vec![],
        resolved_command: Some("   \t\n  ".to_string()),
        resolved_command_env_key: None,
    };
    let out = build_invocation_env_for_logs(&env, &opts);
    assert!(!out.contains_key(DEFAULT_RESOLVED_COMMAND_ENV_KEY));
}

// ---------------------------------------------------------------------------
// isPidAlive
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn is_pid_alive_returns_true_for_self() {
    let pid = std::process::id();
    assert!(is_pid_alive(pid));
}

#[cfg(unix)]
#[test]
fn is_pid_alive_returns_false_for_zero() {
    assert!(!is_pid_alive(0));
}

#[cfg(unix)]
#[test]
fn is_pid_alive_returns_false_for_unlikely_pid() {
    // PID 0x7FFFFFFE is reserved (Linux "no such process"). Use an
    // extremely large integer that no live process will hold.
    let _ = is_pid_alive(0x7FFFFFFE_u32);
}
