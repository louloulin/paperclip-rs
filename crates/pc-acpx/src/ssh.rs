//! `pc-acpx::ssh` - port of `ssh.ts` from Node
//! `paperclip/packages/adapter-utils/src/`.
//!
//! Pure helpers for SSH-backed remote execution. Async functions
//! (`createSshCommandManagedRuntimeRunner`,
//! `runSshCommand`, `buildSshSpawnTarget`,
//! `syncDirectoryToSsh`, `syncDirectoryFromSsh`,
//! `prepareWorkspaceForSshExecution`,
//! `restoreWorkspaceFromSshExecution`,
//! `ensureSshWorkspaceReady`, `startSshEnvLabFixture`,
//! `buildSshEnvLabFixtureConfig`,
//! `getSshEnvLabSupport`, `isSshEnvLabFixtureProcess`,
//! `readSshEnvLabFixtureState`, `stopSshEnvLabFixture`,
//! `readSshEnvLabFixtureStatus`, `fileExists`,
//! `estimateLocalDirSize`, `probeRemoteDirSize`,
//! `withTempFile`, `execFileText`, `spawnText`,
//! `runLocalGit`, `commandExists`, `resolveCommandPath`,
//! `tarExcludeArgs_estimate`, `createSshAuthArgs`,
//! etc.) are deferred - they require real `ssh` process
//! invocation, port allocation, file streaming, and an in-process
//! sshd fixture spawn. This module ports:
//!
//! - Canonical types: `SshConnectionConfig`,
//!   `SshCommandResult`, `SshRemoteExecutionSpec`
//! - Pure helpers: `shell_quote`,
//!   `is_valid_shell_env_key`,
//!   `parse_ssh_remote_execution_spec`,
//!   `tar_exclude_args`,
//!   `tar_spawn_env`,
//!   `tar_pattern_to_regexp`,
//!   `build_known_hosts_entry`
//! - Re-exports the SSH session identity helpers + remote spec
//!   identity so callers can transition into the dedicated module
//!   without changing call sites in `execution_target` /
//!   `remote_managed_runtime`.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// =============================================================================
// Canonical SSH types
// =============================================================================

/// SSH connection configuration used by every SSH-backed remote
/// execution. Mirrors Node `SshConnectionConfig`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshConnectionConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub remote_workspace_path: String,
    pub private_key: Option<String>,
    pub known_hosts: Option<String>,
    pub strict_host_key_checking: bool,
}

/// Standard `{stdout, stderr}` payload returned by every SSH script
/// invocation. Mirrors Node `SshCommandResult`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SshCommandResult {
    pub stdout: String,
    pub stderr: String,
}

/// Full SSH remote execution spec. `SshRemoteExecutionSpec` extends
/// `SshConnectionConfig` with the per-run working directory.
/// Mirrors Node `SshRemoteExecutionSpec`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshRemoteExecutionSpec {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub remote_workspace_path: String,
    pub private_key: Option<String>,
    pub known_hosts: Option<String>,
    pub strict_host_key_checking: bool,
    pub remote_cwd: String,
}

impl SshRemoteExecutionSpec {
    /// Construct a spec from its connection + cwd inputs.
    #[must_use]
    pub fn from_parts(config: SshConnectionConfig, remote_cwd: String) -> Self {
        Self {
            host: config.host,
            port: config.port,
            username: config.username,
            remote_workspace_path: config.remote_workspace_path,
            private_key: config.private_key,
            known_hosts: config.known_hosts,
            strict_host_key_checking: config.strict_host_key_checking,
            remote_cwd,
        }
    }

    /// Borrow as a `SshConnectionConfig` (drops `remote_cwd`).
    #[must_use]
    pub fn as_connection_config(&self) -> SshConnectionConfig {
        SshConnectionConfig {
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            remote_workspace_path: self.remote_workspace_path.clone(),
            private_key: self.private_key.clone(),
            known_hosts: self.known_hosts.clone(),
            strict_host_key_checking: self.strict_host_key_checking,
        }
    }

    /// Effective remote workspace path: explicit field when set,
    /// else `remote_cwd`.
    #[must_use]
    pub fn effective_remote_workspace_path(&self) -> &str {
        if self.remote_workspace_path.is_empty() {
            &self.remote_cwd
        } else {
            &self.remote_workspace_path
        }
    }
}

// =============================================================================
// Pure helpers.
// =============================================================================

/// POSIX single-quote a string. Same algorithm as
/// `command_managed_runtime::shell_quote`; this is the SSH-own
/// copy preserved for parity with `ssh.ts`'s `shellQuote` export.
/// Mirrors Node `shellQuote`.
#[must_use]
pub fn shell_quote(value: &str) -> String {
    let escaped = value.replace('\'', r#"'"'"'"#);
    format!("'{escaped}'")
}

/// `true` when a value is a valid `bash`/`sh` env variable name
/// (POSIX: starts with letter or underscore, followed by any number
/// of letters / digits / underscores). Mirrors Node
/// `isValidShellEnvKey`.
#[must_use]
pub fn is_valid_shell_env_key(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Parse a JSON-ish value into an `SshRemoteExecutionSpec`.
/// Returns `None` when any required field is missing/invalid.
/// Mirrors Node `parseSshRemoteExecutionSpec`.
#[must_use]
pub fn parse_ssh_remote_execution_spec(value: &serde_json::Value) -> Option<SshRemoteExecutionSpec> {
    let parsed = match value {
        serde_json::Value::Object(m) => m,
        _ => return None,
    };
    let host = parsed
        .get("host")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    let username = parsed
        .get("username")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    let remote_cwd = parsed
        .get("remoteCwd")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    let port_value = match parsed.get("port") {
        Some(serde_json::Value::Number(n)) => n.as_u64(),
        Some(serde_json::Value::String(s)) => s.parse::<u64>().ok(),
        _ => None,
    };
    if host.is_empty()
        || username.is_empty()
        || remote_cwd.is_empty()
        || !matches!(port_value, Some(1..=65535))
    {
        return None;
    }

    let remote_workspace_path = parsed
        .get("remoteWorkspacePath")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| remote_cwd.clone());

    let private_key = parsed
        .get("privateKey")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let known_hosts = parsed
        .get("knownHosts")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let strict_host_key_checking = parsed
        .get("strictHostKeyChecking")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);

    Some(SshRemoteExecutionSpec {
        host,
        port: port_value.unwrap_or(22) as u16,
        username,
        remote_workspace_path,
        private_key,
        known_hosts,
        strict_host_key_checking,
        remote_cwd,
    })
}

/// Build the `tar --exclude <pattern>` argv fragment. Always
/// prepends `._*` (Mac resource fork metadata) before any
/// caller-supplied excludes. Mirrors Node `tarExcludeArgs`.
#[must_use]
pub fn tar_exclude_args(exclude: Option<&[String]>) -> Vec<String> {
    let mut combined: Vec<String> = vec!["._*".to_string()];
    if let Some(e) = exclude {
        combined.extend(e.iter().cloned());
    }
    combined
        .into_iter()
        .flat_map(|entry| [String::from("--exclude"), entry])
        .collect()
}

/// Build the env map the SSH tar spawn uses. Node's
/// `tarSpawnEnv` returns a `process.env`-derived object with
/// `COPYFILE_DISABLE=1` layered on top. The Rust helper is a pure
/// default (host env merging is async-side). Mirrors Node
/// `tarSpawnEnv`.
#[must_use]
pub fn tar_spawn_env_defaults() -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    // Prevent macOS bsdtar from emitting AppleDouble metadata
    // files like ._README.md.
    m.insert("COPYFILE_DISABLE".to_string(), "1".to_string());
    m
}

/// Convert a tar `--exclude` pattern into a regexp for the local
/// size estimate (the estimate feeds a clamped percent, so we only
/// need approximate fidelity). Supports literal names plus `*` /
/// `?` glob characters. Mirrors Node `tarPatternToRegExp`.
#[must_use]
pub fn tar_pattern_to_regexp(pattern: &str) -> Result<Regex, String> {
    let mut escaped = String::with_capacity(pattern.len());
    for c in pattern.chars() {
        match c {
            '.' | '+' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\' => {
                escaped.push('\\');
                escaped.push(c);
            }
            '*' => escaped.push_str("[^/]*"),
            '?' => escaped.push_str("[^/]"),
            _ => escaped.push(c),
        }
    }
    Regex::new(&format!("^{escaped}$")).map_err(|e| e.to_string())
}

/// Direct helper that converts a tar `--exclude` pattern into a
/// regexp, building on `tar_pattern_to_regexp`. Returns an
/// `Option` that the SSH side uses to skip walks for already
/// excluded entries.
#[must_use]
pub fn try_tar_pattern_to_regexp(pattern: &str) -> Option<Regex> {
    tar_pattern_to_regexp(pattern).ok()
}

/// Build one line of a `~/.ssh/known_hosts` file from a host /
/// port / public-key tuple. The bracketed `[host]:port` form
/// disambiguates non-default ports. Mirrors Node
/// `buildKnownHostsEntry`.
#[must_use]
pub fn build_known_hosts_entry(input: KnownHostsEntryInput) -> String {
    format!(
        "[{}]:{} {}",
        input.host.trim(),
        input.port,
        input.public_key.trim()
    )
}

/// Input shape for [`build_known_hosts_entry`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownHostsEntryInput {
    pub host: String,
    pub port: u16,
    pub public_key: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- types ----

    #[test]
    fn ssh_remote_execution_spec_from_parts_round_trip() {
        let cfg = SshConnectionConfig {
            host: "h".to_string(),
            port: 22,
            username: "u".to_string(),
            remote_workspace_path: "/w".to_string(),
            private_key: Some("pk".to_string()),
            known_hosts: None,
            strict_host_key_checking: true,
        };
        let spec = SshRemoteExecutionSpec::from_parts(cfg.clone(), "/w/cwd".to_string());
        assert_eq!(spec.remote_cwd, "/w/cwd");
        assert_eq!(spec.host, "h");
        assert_eq!(spec.port, 22);
        assert_eq!(spec.as_connection_config(), cfg);
    }

    #[test]
    fn effective_remote_workspace_path_falls_back_to_remote_cwd() {
        let mut spec = SshRemoteExecutionSpec {
            host: "h".into(),
            port: 22,
            username: "u".into(),
            remote_workspace_path: String::new(),
            private_key: None,
            known_hosts: None,
            strict_host_key_checking: true,
            remote_cwd: "/w".into(),
        };
        assert_eq!(spec.effective_remote_workspace_path(), "/w");
        spec.remote_workspace_path = "/w/explicit".into();
        assert_eq!(spec.effective_remote_workspace_path(), "/w/explicit");
    }

    // ---- shell_quote ----

    #[test]
    fn shell_quote_handles_plain() {
        assert_eq!(shell_quote("plain"), "'plain'");
    }

    #[test]
    fn shell_quote_escapes_single_quote() {
        // A single ' in input becomes '"'"' (5 chars)
        // inside the outer pair of 's. Combined with 2 outer quotes,
        // input `with'quote` produces a 16-char output with 5 's.
        let q = shell_quote("with'quote");
        assert_eq!(q.len(), 16);
        let q_quote_count = q.chars().filter(|c| *c == '\'').count();
        assert_eq!(q_quote_count, 5);
        assert!(q.starts_with("'with'"));
        assert!(q.ends_with("'quote'"));
    }

    #[test]
    fn shell_quote_handles_spaces() {
        assert_eq!(
            shell_quote("/tmp/with space/dir"),
            "'/tmp/with space/dir'"
        );
    }

    // ---- is_valid_shell_env_key ----

    #[test]
    fn valid_shell_env_keys() {
        assert!(is_valid_shell_env_key("PATH"));
        assert!(is_valid_shell_env_key("_PRIVATE"));
        assert!(is_valid_shell_env_key("a1_b2"));
    }

    #[test]
    fn invalid_shell_env_keys() {
        assert!(!is_valid_shell_env_key("1ST"));
        assert!(!is_valid_shell_env_key("a-b"));
        assert!(!is_valid_shell_env_key(""));
        assert!(!is_valid_shell_env_key("a.b"));
    }

    // ---- parse_ssh_remote_execution_spec ----

    #[test]
    fn ssh_parser_accepts_valid_payload() {
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
    }

    #[test]
    fn ssh_parser_round_trips_via_camelcase() {
        let original = SshRemoteExecutionSpec {
            host: "h.example".into(),
            port: 22,
            username: "u".into(),
            remote_workspace_path: "/w".into(),
            private_key: Some("pk-mock".into()),
            known_hosts: Some("kh-mock".into()),
            strict_host_key_checking: true,
            remote_cwd: "/w".into(),
        };
        let json = serde_json::to_value(&original).expect("to_value");
        assert_eq!(json["host"], "h.example");
        assert_eq!(json["port"], 22);
        assert!(json["privateKey"].is_string());
        assert!(json["knownHosts"].is_string());
        assert!(json["strictHostKeyChecking"].as_bool().unwrap_or(false));
        let back = parse_ssh_remote_execution_spec(&json).expect("must parse back");
        assert_eq!(back, original);
    }

    #[test]
    fn ssh_parser_defaults_remote_workspace_path_to_remote_cwd() {
        let v = json!({"host": "h", "username": "u", "remoteCwd": "/w", "port": 22});
        let s = parse_ssh_remote_execution_spec(&v).expect("must parse");
        assert_eq!(s.remote_workspace_path, "/w");
    }

    #[test]
    fn ssh_parser_rejects_invalid_port() {
        let zero = json!({"host": "h", "username": "u", "remoteCwd": "/w", "port": 0});
        assert!(parse_ssh_remote_execution_spec(&zero).is_none());
        let overflow = json!({"host": "h", "username": "u", "remoteCwd": "/w", "port": 70_000});
        assert!(parse_ssh_remote_execution_spec(&overflow).is_none());
    }

    #[test]
    fn ssh_parser_rejects_missing_required_fields() {
        let v = json!({"host": "h", "port": 22});
        assert!(parse_ssh_remote_execution_spec(&v).is_none());
    }

    #[test]
    fn ssh_parser_rejects_non_object_value() {
        assert!(parse_ssh_remote_execution_spec(&json!(null)).is_none());
        assert!(parse_ssh_remote_execution_spec(&json!("str")).is_none());
        assert!(parse_ssh_remote_execution_spec(&json!(42)).is_none());
        assert!(parse_ssh_remote_execution_spec(&json!([1, 2, 3])).is_none());
    }

    #[test]
    fn ssh_parser_accepts_string_port() {
        let v = json!({"host": "h", "username": "u", "remoteCwd": "/w", "port": "2222"});
        let s = parse_ssh_remote_execution_spec(&v).expect("must parse");
        assert_eq!(s.port, 2222);
    }

    #[test]
    fn ssh_parser_omits_empty_optional_fields() {
        let v = json!({
            "host": "h", "username": "u", "remoteCwd": "/w", "port": 22,
            "privateKey": "", "knownHosts": "",
        });
        let s = parse_ssh_remote_execution_spec(&v).expect("must parse");
        assert!(s.private_key.is_none());
        assert!(s.known_hosts.is_none());
    }

    // ---- tar_exclude_args ----

    #[test]
    fn tar_exclude_args_prepends_resource_fork_pattern() {
        let args = tar_exclude_args(Some(&["node_modules".into(), "target".into()]));
        assert_eq!(
            args,
            vec![
                "--exclude", "._*",
                "--exclude", "node_modules",
                "--exclude", "target",
            ]
        );
    }

    #[test]
    fn tar_exclude_args_without_excludes_has_only_resource_fork() {
        let args = tar_exclude_args(None);
        assert_eq!(args, vec!["--exclude", "._*"]);
    }

    // ---- tar_spawn_env_defaults ----

    #[test]
    fn tar_spawn_env_sets_copyfile_disable() {
        let env = tar_spawn_env_defaults();
        assert_eq!(env.get("COPYFILE_DISABLE").map(String::as_str), Some("1"));
        // BTreeMap iteration is sorted - useful for deterministic shell test
        let keys: Vec<&str> = env.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["COPYFILE_DISABLE"]);
    }

    // ---- tar_pattern_to_regexp ----

    #[test]
    fn tar_pattern_to_regexp_matches_literal() {
        let re = tar_pattern_to_regexp("node_modules").expect("valid regex");
        assert!(re.is_match("node_modules"));
        assert!(!re.is_match("sub/node_modules"));
    }

    #[test]
    fn tar_pattern_to_regexp_handles_star_glob() {
        // `*` becomes `[^/]*` so it does NOT span `/`s.
        let re = tar_pattern_to_regexp("*/target").expect("valid regex");
        assert!(re.is_match("a/target"));
        assert!(!re.is_match("a/b/target"));
        assert!(!re.is_match("target"));
    }

    #[test]
    fn tar_pattern_to_regexp_handles_question_glob() {
        let re = tar_pattern_to_regexp("?").expect("valid regex");
        assert!(re.is_match("a"));
        assert!(re.is_match("b"));
        assert!(!re.is_match("ab"));
        assert!(!re.is_match(""));
    }

    #[test]
    fn tar_pattern_to_regexp_escapes_special_chars() {
        // `.` is a regex special but should match a literal `.`
        let re = tar_pattern_to_regexp("file.txt").expect("valid");
        assert!(re.is_match("file.txt"));
        assert!(!re.is_match("fileXtxt"));
    }

    // ---- build_known_hosts_entry ----

    #[test]
    fn build_known_hosts_entry_formats_bracketed_host_port() {
        let entry = build_known_hosts_entry(KnownHostsEntryInput {
            host: "h.example".to_string(),
            port: 2222,
            public_key: "ssh-ed25519 AAAA...rest".to_string(),
        });
        assert_eq!(
            entry,
            "[h.example]:2222 ssh-ed25519 AAAA...rest"
        );
    }

    #[test]
    fn build_known_hosts_entry_strips_whitespace() {
        let entry = build_known_hosts_entry(KnownHostsEntryInput {
            host: "  h.example  ".to_string(),
            port: 22,
            public_key: "  ssh-ed25519 AAAA  ".to_string(),
        });
        assert_eq!(entry, "[h.example]:22 ssh-ed25519 AAAA");
    }
}
