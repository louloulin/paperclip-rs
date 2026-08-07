//! `pc-acpx` gemini ACP shell helper — wraps the Node
//! `normalizeGeminiAcpCommandShell` so a `--acp` flag is downgraded to
//! `--experimental-acp` for gemini versions below
//! `GEMINI_NATIVE_ACP_FLAG_MIN_VERSION`.
//!
//! The Node implementation shells out to `gemini --version` to learn the
//! installed version. We keep the rewrite pure by sourcing the version from
//! a caller-provided env (or `std::env`) so tests are deterministic and the
//! engine avoids spawning a sidecar just to learn whether the flag is valid.

use std::collections::HashMap;

use crate::gemini_version::{
    gemini_acp_command_tokens, gemini_version_supports_native_acp_flag, parse_gemini_version_parts,
    rewrite_gemini_acp_flag_for_version,
};

const GEMINI_VERSION_OVERRIDE_ENV: &str = "PAPERCLIP_GEMINI_VERSION_OVERRIDE";

/// Rewrite a gemini command shell if needed. The version is sourced from
/// `env`, keyed on `PAPERCLIP_GEMINI_VERSION_OVERRIDE`. When the override is
/// missing, unparseable, or the command does not look like a `gemini --acp`
/// invocation, the input is returned verbatim.
pub fn normalize_gemini_acp_command_shell_with_env(
    command_shell: &str,
    env: &HashMap<String, String>,
) -> String {
    let tokens = match gemini_acp_command_tokens(command_shell) {
        Some(tokens) => tokens,
        None => return command_shell.to_string(),
    };
    if !tokens.iter().any(|token| *token == "--acp") {
        return command_shell.to_string();
    }
    let version_text = env
        .get(GEMINI_VERSION_OVERRIDE_ENV)
        .map(String::as_str)
        .unwrap_or("");
    let parts = parse_gemini_version_parts(Some(version_text));
    if gemini_version_supports_native_acp_flag(parts) {
        command_shell.to_string()
    } else {
        rewrite_gemini_acp_flag_for_version(command_shell, parts)
    }
}

/// Production variant — reads the override from `std::env`.
pub fn normalize_gemini_acp_command_shell(command_shell: &str) -> String {
    let env: HashMap<String, String> = std::env::vars().collect();
    normalize_gemini_acp_command_shell_with_env(command_shell, &env)
}
