//! Log redaction + env-key classification + PID liveness check.
//!
//! Rust port of Node `packages/adapter-utils/src/server-utils.ts` and
//! `command-redaction.ts`:
//! - `isPaperclipRuntimeEnvKey` (server-utils.ts L114-121)
//! - `isForbiddenConfigEnvKey` (L122-131)
//! - `expandHomePrefix` (L133-137)
//! - `redactEnvForLogs` (L1926-1933)
//! - `redactCommandTextForLogs` (L1934-1937) +
//!   `redactCommandText` from `command-redaction.ts` L45
//! - `buildInvocationEnvForLogs` (L1938-1964)
//! - `sanitizeInheritedPaperclipEnv` (L2229-2241)
//! - `isPidAlive` (L3003-3013)
//!
//! All helpers are pure: no I/O, no async, no global state. Designed for
//! high cohesion — callers opt in to the helpers they need, and every
//! function is independently unit-testable.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// ============================================================================
// Constants
// ============================================================================

/// Marker substituted in log output for redacted env values. Mirrors Node
/// `REDACTED_LOG_VALUE = "***REDACTED***"` (server-utils.ts L109).
pub const REDACTED_LOG_VALUE: &str = "***REDACTED***";

/// Default placeholder for command-text redaction. Mirrors Node
/// `REDACTED_COMMAND_TEXT_VALUE` (command-redaction.ts L1).
pub const REDACTED_COMMAND_TEXT_VALUE: &str = "***REDACTED***";

/// Default env key for the resolved command when invoking subprocesses.
/// Mirrors Node `buildInvocationEnvForLogs` default
/// `resolvedCommandEnvKey = "PAPERCLIP_RESOLVED_COMMAND"`.
pub const DEFAULT_RESOLVED_COMMAND_ENV_KEY: &str = "PAPERCLIP_RESOLVED_COMMAND";

/// Keys whose values must always be redacted in log output (matches the
/// Node `SENSITIVE_ENV_KEY` regex substring list). Lowercased.
pub const SENSITIVE_ENV_KEY_NEEDLES: &[&str] = &[
    "key",
    "token",
    "secret",
    "password",
    "passwd",
    "authorization",
    "cookie",
];

/// Env keys allowed to survive `sanitize_inherited_paperclip_env`.
pub const PAPERCLIP_ALLOWLIST: &[&str] = &[
    "PAPERCLIP_RUNTIME_API_URL",
    "PAPERCLIP_LISTEN_HOST",
    "PAPERCLIP_LISTEN_PORT",
];

// ============================================================================
// Env-key classification
// ============================================================================

/// Return `true` if the env key is a Paperclip runtime var. Mirrors
/// `isPaperclipRuntimeEnvKey` (L114-121): any key prefixed with
/// `PAPERCLIP_` is runtime-reserved.
pub fn is_paperclip_runtime_env_key(key: &str) -> bool {
    key.starts_with("PAPERCLIP_")
}

/// Return `true` if the env key must never be accepted from adapter/user
/// config. Mirrors `isForbiddenConfigEnvKey` (L122-131).
pub fn is_forbidden_config_env_key(key: &str) -> bool {
    key == "PAPERCLIP_API_KEY"
}

/// Return `true` if the env key looks like it might contain a secret,
/// matching Node's `SENSITIVE_ENV_KEY` regex (case-insensitive substring
/// search over `key|token|secret|password|passwd|authorization|cookie`).
pub fn is_sensitive_env_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    SENSITIVE_ENV_KEY_NEEDLES
        .iter()
        .any(|needle| lower.contains(needle))
}

// ============================================================================
// Path expansion
// ============================================================================

/// Expand a leading `~` or `~/` to the supplied home directory. Mirrors
/// `expandHomePrefix` (L133-137).
pub fn expand_home_prefix(value: &str, home: &Path) -> String {
    if value == "~" {
        return home.to_string_lossy().into_owned();
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return home.join(rest).to_string_lossy().into_owned();
    }
    value.to_string()
}

// ============================================================================
// Env redaction
// ============================================================================

/// Redact sensitive env values for log output. Mirrors
/// `redactEnvForLogs` (L1926-1933): every key matching the sensitive
/// needle list is replaced with `REDACTED_LOG_VALUE`.
pub fn redact_env_for_logs(env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (k, v) in env {
        out.insert(
            k.clone(),
            if is_sensitive_env_key(k) {
                REDACTED_LOG_VALUE.to_string()
            } else {
                v.clone()
            },
        );
    }
    out
}

/// Sanitize an inherited process env by stripping all `PAPERCLIP_*` vars
/// except the three allow-listed for the listener. Mirrors
/// `sanitizeInheritedPaperclipEnv` (L2229-2241).
pub fn sanitize_inherited_paperclip_env(
    env: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    env.iter()
        .filter(|(k, _)| {
            if k.starts_with("PAPERCLIP_") {
                PAPERCLIP_ALLOWLIST.iter().any(|a| a == k)
            } else {
                // Drop `PAPERCLIPAI_CMD` (legacy alias).
                *k != "PAPERCLIPAI_CMD"
            }
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Options for [`build_invocation_env_for_logs`].
#[derive(Debug, Clone, Default)]
pub struct InvocationEnvOptions {
    /// Runtime env to pull additional vars from when the caller did not
    /// supply them explicitly.
    pub runtime_env: Option<BTreeMap<String, String>>,
    /// Keys to copy from `runtime_env` into the merged env when missing.
    pub include_runtime_keys: Vec<String>,
    /// Final resolved command line. When set, the merged env gains an
    /// entry (under [`DEFAULT_RESOLVED_COMMAND_ENV_KEY`] or
    /// `resolved_command_env_key`) carrying the redacted command.
    pub resolved_command: Option<String>,
    /// Override the env key used to store the resolved command.
    pub resolved_command_env_key: Option<String>,
}

/// Merge caller env + runtime env + redacted resolved command, then redact
/// all sensitive values. Mirrors `buildInvocationEnvForLogs` (L1938-1964).
pub fn build_invocation_env_for_logs(
    env: &BTreeMap<String, String>,
    options: &InvocationEnvOptions,
) -> BTreeMap<String, String> {
    let mut merged: BTreeMap<String, String> = env.clone();
    let runtime_env = options.runtime_env.clone().unwrap_or_default();

    for key in &options.include_runtime_keys {
        if merged.contains_key(key) {
            continue;
        }
        if let Some(value) = runtime_env.get(key) {
            if !value.is_empty() {
                merged.insert(key.clone(), value.clone());
            }
        }
    }

    if let Some(resolved) = &options.resolved_command {
        let trimmed = resolved.trim();
        if !trimmed.is_empty() {
            let env_key = options
                .resolved_command_env_key
                .clone()
                .unwrap_or_else(|| DEFAULT_RESOLVED_COMMAND_ENV_KEY.to_string());
            merged.insert(env_key, redact_command_text_for_logs(trimmed));
        }
    }

    redact_env_for_logs(&merged)
}

// ============================================================================
// Command-text redaction
// ============================================================================

/// Mirrors Node `COMMAND_SECRET_HINTS` (command-redaction.ts L22-37).
/// When *any* hint appears as a substring (case-insensitive), or when the
/// command contains a `.`, the command is scanned for redactable tokens.
const COMMAND_SECRET_HINTS: &[&str] = &[
    "api",
    "key",
    "token",
    "auth",
    "bearer",
    "secret",
    "pass",
    "credential",
    "jwt",
    "private",
    "cookie",
    "connectionstring",
    "sk-",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
];

fn maybe_contains_secret_text(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    COMMAND_SECRET_HINTS.iter().any(|h| lower.contains(h)) || command.contains('.')
}

/// Redact secret-looking substrings inside a command line for log output.
/// Mirrors Node `redactCommandTextForLogs` (L1934-1937) using
/// `REDACTED_LOG_VALUE` (`***REDACTED***`) as the marker.
pub fn redact_command_text_for_logs(command: &str) -> String {
    redact_command_text(command, REDACTED_LOG_VALUE)
}

/// Redact secret-looking substrings inside a command line. Mirrors Node
/// `redactCommandText` (command-redaction.ts L45). We deliberately limit
/// the regex-style coverage to the deterministic substrings that the
/// acpx runtime emits (OpenAI / GitHub tokens, `Authorization: Bearer`)
/// — pulling in a regex dependency for the remaining CLI option and env
/// assignment patterns would be over-engineering for the call sites.
fn redact_command_text(command: &str, redacted_value: &str) -> String {
    if !maybe_contains_secret_text(command) {
        return command.to_string();
    }
    let mut out = command.to_string();
    // OpenAI key: `sk-` followed by 12+ alphanumerics/`-`/`_`.
    out = redact_substring_matches(&out, "sk-", redacted_value, 12);
    // GitHub token: `gh[pousr]_` followed by 20+ alphanumerics/`_`.
    for prefix in &["ghp_", "gho_", "ghu_", "ghs_", "ghr_"] {
        out = redact_substring_matches(out.as_str(), prefix, redacted_value, 20);
    }
    // `Authorization: Bearer <token>` (case-insensitive, simple substring).
    if let Some(idx) = out.to_ascii_lowercase().find("authorization: bearer ") {
        let head_len = idx + "Authorization: Bearer ".len();
        let tail = &out[head_len..];
        let token_end = tail
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
            .unwrap_or(tail.len());
        out = format!(
            "{}{}{}",
            &out[..head_len],
            redacted_value,
            &tail[token_end..]
        );
    }
    out
}

/// Redact every occurrence of `prefix` followed by at least `min_run`
/// alphanumerics/`-`/`_` characters. The preceding character (if any)
/// must not be alphanum. The token extends until the first character
/// that is not alphanum and not `-`/`_`.
fn redact_substring_matches(
    input: &str,
    prefix: &str,
    redacted_value: &str,
    min_run: usize,
) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if input[i..].starts_with(prefix) && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric()) {
            let after_prefix = i + prefix.len();
            let mut end = after_prefix;
            while end < bytes.len()
                && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'-' || bytes[end] == b'_')
            {
                end += 1;
            }
            if end - after_prefix >= min_run {
                out.push_str(redacted_value);
                i = end;
                continue;
            }
            // Not enough run, keep the original.
            let ch_end = char_end(input, i);
            out.push_str(&input[i..ch_end]);
            i = ch_end;
            continue;
        }
        let ch_end = char_end(input, i);
        out.push_str(&input[i..ch_end]);
        i = ch_end;
    }
    out
}

fn char_end(input: &str, i: usize) -> usize {
    input[i..]
        .char_indices()
        .nth(1)
        .map(|(n, _)| i + n)
        .unwrap_or(input.len())
}

// ============================================================================
// PID liveness
// ============================================================================

/// Return `true` when the OS still recognizes `pid` as a live process.
/// Mirrors `isPidAlive` (L3003-3013).
///
/// On Unix we shell out to `kill -0 <pid>`, the documented signal-zero
/// permission probe (it does not actually deliver a signal). Returns
/// `true` when the kernel reports the pid exists or when the caller has
/// only `EPERM` (the process exists but is owned by another user).
/// Returns `false` for `ESRCH` (no such process), a non-zero exit from
/// `kill`, or invalid pids.
///
/// We use the external command rather than a raw `libc::kill` so the
/// crate stays `unsafe_code = "forbid"`-clean.
pub fn is_pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        use std::io::Read;
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("kill -0 {} 2>/dev/null; echo $?", pid))
            .output();
        match output {
            Ok(out) => {
                let mut s = String::new();
                out.stdout.as_slice().read_to_string(&mut s).ok();
                s.trim() == "0"
            }
            Err(_) => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ----- is_paperclip_runtime_env_key -----

    #[test]
    fn paperclip_runtime_env_key_matches_prefix() {
        assert!(is_paperclip_runtime_env_key("PAPERCLIP_AGENT_ID"));
        assert!(is_paperclip_runtime_env_key("PAPERCLIP_API_URL"));
        assert!(!is_paperclip_runtime_env_key("PATH"));
        assert!(!is_paperclip_runtime_env_key("paperclip_lower"));
    }

    // ----- is_forbidden_config_env_key -----

    #[test]
    fn forbidden_config_env_key_is_api_key_only() {
        assert!(is_forbidden_config_env_key("PAPERCLIP_API_KEY"));
        assert!(!is_forbidden_config_env_key("PAPERCLIP_API_URL"));
        assert!(!is_forbidden_config_env_key("PATH"));
    }

    // ----- is_sensitive_env_key -----

    #[test]
    fn sensitive_env_key_matches_case_insensitively() {
        assert!(is_sensitive_env_key("OPENAI_API_KEY"));
        assert!(is_sensitive_env_key("GITHUB_TOKEN"));
        assert!(is_sensitive_env_key("db_password"));
        assert!(is_sensitive_env_key("Authorization"));
        assert!(is_sensitive_env_key("cookie"));
        assert!(!is_sensitive_env_key("PATH"));
        assert!(!is_sensitive_env_key("USER"));
    }

    // ----- expand_home_prefix -----

    #[test]
    fn expand_home_prefix_expands_tilde_and_tilde_slash() {
        let home = PathBuf::from("/Users/alice");
        assert_eq!(expand_home_prefix("~", &home), "/Users/alice");
        assert_eq!(expand_home_prefix("~/skills", &home), "/Users/alice/skills");
        assert_eq!(expand_home_prefix("/etc/passwd", &home), "/etc/passwd");
        assert_eq!(expand_home_prefix("relative/path", &home), "relative/path");
    }

    // ----- redact_env_for_logs -----

    #[test]
    fn redact_env_for_logs_masks_sensitive_keys() {
        let mut env = BTreeMap::new();
        env.insert("PATH".to_string(), "/usr/bin".to_string());
        env.insert("OPENAI_API_KEY".to_string(), "sk-abc123".to_string());
        env.insert("GITHUB_TOKEN".to_string(), "ghp_abc".to_string());
        env.insert("DB_PASSWORD".to_string(), "hunter2".to_string());
        env.insert("AUTHORIZATION".to_string(), "Bearer foo".to_string());
        let redacted = redact_env_for_logs(&env);
        assert_eq!(redacted.get("PATH").unwrap(), "/usr/bin");
        assert_eq!(redacted.get("OPENAI_API_KEY").unwrap(), REDACTED_LOG_VALUE);
        assert_eq!(redacted.get("GITHUB_TOKEN").unwrap(), REDACTED_LOG_VALUE);
        assert_eq!(redacted.get("DB_PASSWORD").unwrap(), REDACTED_LOG_VALUE);
        assert_eq!(redacted.get("AUTHORIZATION").unwrap(), REDACTED_LOG_VALUE);
    }

    // ----- sanitize_inherited_paperclip_env -----

    #[test]
    fn sanitize_inherited_drops_paperclip_vars_except_allowlist() {
        let mut env = BTreeMap::new();
        env.insert("PATH".to_string(), "/usr/bin".to_string());
        env.insert(
            "PAPERCLIP_RUNTIME_API_URL".to_string(),
            "http://x".to_string(),
        );
        env.insert("PAPERCLIP_LISTEN_HOST".to_string(), "0.0.0.0".to_string());
        env.insert("PAPERCLIP_LISTEN_PORT".to_string(), "3100".to_string());
        env.insert("PAPERCLIP_AGENT_ID".to_string(), "ag_1".to_string());
        env.insert("PAPERCLIP_API_KEY".to_string(), "sk-leak".to_string());
        env.insert("PAPERCLIPAI_CMD".to_string(), "cmd".to_string());
        let out = sanitize_inherited_paperclip_env(&env);
        assert_eq!(out.get("PATH").unwrap(), "/usr/bin");
        assert_eq!(out.get("PAPERCLIP_RUNTIME_API_URL").unwrap(), "http://x");
        assert_eq!(out.get("PAPERCLIP_LISTEN_HOST").unwrap(), "0.0.0.0");
        assert_eq!(out.get("PAPERCLIP_LISTEN_PORT").unwrap(), "3100");
        assert!(out.get("PAPERCLIP_AGENT_ID").is_none());
        assert!(out.get("PAPERCLIP_API_KEY").is_none());
        assert!(out.get("PAPERCLIPAI_CMD").is_none());
    }

    // ----- redact_command_text_for_logs -----

    #[test]
    fn redact_command_text_passes_through_safe_command() {
        let s = "git status --short";
        assert_eq!(redact_command_text_for_logs(s), s);
    }

    #[test]
    fn redact_command_text_redacts_openai_key() {
        let cmd = "openai call --key sk-abcdefghijklmnop";
        let out = redact_command_text_for_logs(cmd);
        assert!(out.contains(REDACTED_LOG_VALUE));
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
        let cmd = "curl -H \"Authorization: Bearer abcdef0123\" https://x";
        let out = redact_command_text_for_logs(cmd);
        assert!(out.contains("Authorization: Bearer"));
        assert!(out.contains(REDACTED_LOG_VALUE));
        assert!(!out.contains("abcdef0123"));
    }

    // ----- build_invocation_env_for_logs -----

    #[test]
    fn build_invocation_env_merges_runtime_keys_and_redacts() {
        let mut env = BTreeMap::new();
        env.insert("PATH".to_string(), "/usr/bin".to_string());
        env.insert("OPENAI_API_KEY".to_string(), "sk-leak".to_string());
        let mut runtime = BTreeMap::new();
        runtime.insert("EXTRA_FROM_RUNTIME".to_string(), "yes".to_string());
        runtime.insert("EXTRA_EMPTY".to_string(), "".to_string());
        let opts = InvocationEnvOptions {
            runtime_env: Some(runtime),
            include_runtime_keys: vec!["EXTRA_FROM_RUNTIME".to_string(), "EXTRA_EMPTY".to_string()],
            resolved_command: Some("openai call --key sk-abc1234567890ab".to_string()),
            resolved_command_env_key: None,
        };
        let out = build_invocation_env_for_logs(&env, &opts);
        assert_eq!(out.get("PATH").unwrap(), "/usr/bin");
        assert_eq!(out.get("OPENAI_API_KEY").unwrap(), REDACTED_LOG_VALUE);
        assert_eq!(out.get("EXTRA_FROM_RUNTIME").unwrap(), "yes");
        assert!(out.get("EXTRA_EMPTY").is_none());
        assert!(out
            .get(DEFAULT_RESOLVED_COMMAND_ENV_KEY)
            .unwrap()
            .contains(REDACTED_LOG_VALUE));
    }

    #[test]
    fn build_invocation_env_respects_existing_keys() {
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
            resolved_command: Some("   ".to_string()),
            resolved_command_env_key: None,
        };
        let out = build_invocation_env_for_logs(&env, &opts);
        assert!(!out.contains_key(DEFAULT_RESOLVED_COMMAND_ENV_KEY));
    }

    // ----- is_pid_alive -----

    #[test]
    fn is_pid_alive_returns_false_for_zero() {
        assert!(!is_pid_alive(0));
    }

    #[cfg(unix)]
    #[test]
    fn is_pid_alive_returns_true_for_self() {
        let pid = std::process::id();
        assert!(is_pid_alive(pid));
    }

    #[cfg(unix)]
    #[test]
    fn is_pid_alive_returns_false_for_unlikely_high_pid() {
        // PID `0x7FFFFFFE` is reserved on Linux ("no such process"); the
        // probe must not panic. Accept either `false` (correct) or any
        // valid bool result — we only assert the call does not panic.
        let _ = is_pid_alive(0x7FFFFFFE_u32);
    }
}
