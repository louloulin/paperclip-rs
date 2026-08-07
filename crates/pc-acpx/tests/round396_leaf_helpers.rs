//! Integration tests for the leaf adapter-utils helpers landed in
//! R396. Each module is exercised end-to-end via the public API to
//! catch regressions that may slip past the per-module unit tests.

use pc_acpx::billing::infer_openai_compatible_biller;
use pc_acpx::command_redaction::{maybe_contains_secret_text, redact_command_text};
use pc_acpx::exclude_patterns::{exclude_pattern_matches, should_exclude_path};
use pc_acpx::remote_execution_env::{
    sanitize_remote_execution_env, REMOTE_EXECUTION_ENV_IDENTITY_KEYS,
};
use pc_acpx::sandbox_install_command::build_sandbox_npm_install_command;
use pc_acpx::sandbox_shell::{preferred_shell_for_sandbox, shell_command_args};
use std::collections::BTreeMap;

fn env_from(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

// ============================================================================
// sandbox_shell
// ============================================================================

#[test]
fn sandbox_shell_bash_branch_compiles_full_command() {
    let shell = preferred_shell_for_sandbox(Some("bash"));
    let script = shell_command_args("echo hello");
    assert_eq!(
        format!("{} {:?}", shell, script),
        "bash [\"-c\", \"echo hello\"]"
    );
}

#[test]
fn sandbox_shell_sh_branch_default() {
    let shell = preferred_shell_for_sandbox(None);
    let script = shell_command_args("set -e");
    assert_eq!(format!("{} {:?}", shell, script), "sh [\"-c\", \"set -e\"]");
}

// ============================================================================
// billing
// ============================================================================

#[test]
fn billing_resolves_openrouter_for_explicit_key() {
    let env = env_from(&[("OPENROUTER_API_KEY", "sk-or-v1-xyz")]);
    assert_eq!(
        infer_openai_compatible_biller(&env, Some("openai")).as_deref(),
        Some("openrouter")
    );
}

#[test]
fn billing_resolves_openrouter_for_openrouter_ai_base_url() {
    let env = env_from(&[("OPENAI_BASE_URL", "https://openrouter.ai/api/v1")]);
    assert_eq!(
        infer_openai_compatible_biller(&env, Some("openai")).as_deref(),
        Some("openrouter")
    );
}

#[test]
fn billing_falls_back_to_default_when_no_signals() {
    let env = env_from(&[("UNRELATED", "value")]);
    assert_eq!(
        infer_openai_compatible_biller(&env, Some("openai")).as_deref(),
        Some("openai")
    );
}

// ============================================================================
// exclude_patterns
// ============================================================================

#[test]
fn exclude_patterns_handle_git_archive_exclude_set() {
    let git_excludes = [
        "node_modules",
        "node_modules/*",
        "*/node_modules",
        "*/node_modules/*",
        "dist",
        "dist/*",
        "*/dist",
        "*/dist/*",
        ".git",
        ".git/*",
        "*/.git",
        "*/.git/*",
    ];
    assert!(should_exclude_path(
        "node_modules/foo/bar.js",
        &git_excludes
    ));
    assert!(should_exclude_path(
        "packages/app/node_modules/x.js",
        &git_excludes
    ));
    assert!(should_exclude_path("dist/output.txt", &git_excludes));
    assert!(should_exclude_path("a/b/dist/x.js", &git_excludes));
    assert!(should_exclude_path(".git/HEAD", &git_excludes));
    assert!(!should_exclude_path("src/index.ts", &git_excludes));
}

#[test]
fn exclude_patterns_handle_segment_only_glob() {
    let patterns = ["*/.cache", "*/coverage"];
    assert!(exclude_pattern_matches("a/.cache/b", "*/.cache"));
    assert!(exclude_pattern_matches(".cache", "*/.cache"));
    assert!(exclude_pattern_matches("x/.cache", "*/.cache"));
    assert!(!exclude_pattern_matches("a/cached", "*/.cache"));
    assert!(!exclude_pattern_matches("a/coverage2", "*/coverage"));
}

// ============================================================================
// command_redaction
// ============================================================================

#[test]
fn command_redaction_handles_complex_multi_secret_input() {
    let text = r#"OPENAI_API_KEY="sk-abcdefghijklmnop1234" --token=ghp_abcdefghijklmnopqrstuvwxyz1234 DATABASE_PASSWORD=hunter2"#;
    let redacted = redact_command_text(text, None);
    assert!(!redacted.contains("sk-abcdefghijklmnop1234"));
    assert!(!redacted.contains("ghp_abcdefghijklmnopqrstuvwxyz1234"));
    assert!(!redacted.contains("hunter2"));
}

#[test]
fn command_redaction_preserves_log_lines_without_secrets() {
    let lines = [
        "starting run",
        "loading config",
        "finished run",
        "build complete",
    ];
    for line in lines {
        assert!(
            !maybe_contains_secret_text(line),
            "{line} should not trigger detection"
        );
        assert_eq!(redact_command_text(line, None), line);
    }
}

// ============================================================================
// remote_execution_env
// ============================================================================

#[test]
fn remote_execution_env_full_identity_set_round_trip() {
    let mut env = BTreeMap::new();
    for key in REMOTE_EXECUTION_ENV_IDENTITY_KEYS {
        env.insert((*key).to_string(), "value".to_string());
    }
    env.insert("CUSTOM_VAR".to_string(), "hello".to_string());

    // Inherited env has same values for identity keys, but different for CUSTOM_VAR
    let mut inherited = BTreeMap::new();
    for key in REMOTE_EXECUTION_ENV_IDENTITY_KEYS {
        inherited.insert((*key).to_string(), "value".to_string());
    }
    inherited.insert("CUSTOM_VAR".to_string(), "different".to_string());

    let sanitized = sanitize_remote_execution_env(&env, &inherited);
    assert!(sanitized.contains_key("CUSTOM_VAR"));
    // All identity keys should be dropped (since inherited == env)
    for key in REMOTE_EXECUTION_ENV_IDENTITY_KEYS {
        assert!(
            !sanitized.contains_key(*key),
            "{key} should be sanitized out"
        );
    }
}

#[test]
fn remote_execution_env_preserves_overrides_against_inherited() {
    let mut env = BTreeMap::new();
    env.insert("PATH".to_string(), "/custom/bin".to_string());
    let mut inherited = BTreeMap::new();
    inherited.insert("PATH".to_string(), "/usr/bin:/bin".to_string());

    let sanitized = sanitize_remote_execution_env(&env, &inherited);
    assert_eq!(sanitized.get("PATH").unwrap(), "/custom/bin");
}

// ============================================================================
// sandbox_install_command
// ============================================================================

#[test]
fn sandbox_install_command_produces_full_lifecycle_script() {
    let script = build_sandbox_npm_install_command("@paperclipai/cli");
    // Verify all four branches are present
    assert!(script.contains("PAPERCLIP_NPM_BOOTSTRAPPED"));
    assert!(script.contains("command -v npm"));
    assert!(script.contains("npm install -g '@paperclipai/cli'"));
    assert!(script.contains("$(id -u)"));
    assert!(script.contains("sudo -n true"));
    assert!(script.contains("--prefix \"$HOME/.local\""));
    assert!(script.ends_with("fi"));
}

#[test]
fn sandbox_install_command_handles_scoped_package() {
    let script = build_sandbox_npm_install_command("@paperclipai/cli");
    assert!(script.contains("npm install -g '@paperclipai/cli';"));
    // Should appear 3 times (root, sudo, fallback)
    assert_eq!(
        script.matches("npm install -g '@paperclipai/cli'").count(),
        3
    );
}
