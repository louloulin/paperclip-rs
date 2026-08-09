//! `pc-acpx` environment helpers — port of `ensurePathInEnv`,
//! `defaultPathForPlatform`, and `resolveRuntimeEnv` from Node
//! `acpx-engine/execute.ts` and `server-utils.ts`.
//!
//! The acpx runtime assumes a populated `PATH` even on minimal sandboxes
//! (some agents resolve their own CLI through `PATH` lookup). When the
//! caller does not pass one, the engine falls back to a platform-default
//! value. The Rust port keeps the same idempotent semantics — a missing
//! `PATH` is the only branch that mutates the env.

use std::collections::BTreeMap;
use std::env;

/// Build the platform-default `PATH` value the engine uses when the
/// caller did not pass one. Mirrors `defaultPathForPlatform` from the
/// Node implementation.
///
/// - Windows: `C:\\Windows\\System32;C:\\Windows;C:\\Windows\\System32\\Wbem`
/// - Unix: `/usr/local/bin:/opt/homebrew/bin:/usr/local/sbin:/usr/bin:/bin:/usr/sbin:/sbin`
pub fn default_path_for_platform() -> String {
    if cfg!(target_os = "windows") {
        return "C:\\Windows\\System32;C:\\Windows;C:\\Windows\\System32\\Wbem".to_string();
    }
    "/usr/local/bin:/opt/homebrew/bin:/usr/local/sbin:/usr/bin:/bin:/usr/sbin:/sbin".to_string()
}

/// Ensure `env` has a populated `PATH`. If neither `PATH` nor `Path` is
/// set, fall back to [`default_path_for_platform`]. Mirrors the Node
/// `ensurePathInEnv` helper: an existing `PATH` is never overwritten.
pub fn ensure_path_in_env(env: &mut BTreeMap<String, String>) -> &mut BTreeMap<String, String> {
    if env
        .get("PATH")
        .map(|value| !value.is_empty())
        .unwrap_or(false)
    {
        return env;
    }
    if env
        .get("Path")
        .map(|value| !value.is_empty())
        .unwrap_or(false)
    {
        return env;
    }
    env.insert("PATH".into(), default_path_for_platform());
    env
}

/// Compute the runtime env the engine hands to the acpx subprocess.
///
/// 1. Start from the current process env (via [`std::env::vars_os`]).
/// 2. Overlay the caller's `env` on top (caller wins).
/// 3. Ensure `PATH` is populated.
/// 4. Filter out any non-string values (the Rust port only accepts
///    string pairs, so the filter is a no-op in practice — preserved
///    for parity with the Node `.filter(([, value]) => typeof value === "string")`).
///
/// Mirrors `resolveRuntimeEnv` from Node `acpx-engine/execute.ts`.
pub fn resolve_runtime_env(env: BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut combined = BTreeMap::new();
    // Step 1: process env (filtered to UTF-8 keys + values).
    for (key, value) in env::vars_os() {
        if let (Ok(key), Some(value)) = (key.into_string(), value.to_str()) {
            combined.insert(key, value.to_string());
        }
    }
    // Step 2: overlay caller env (last write wins).
    for (key, value) in env {
        combined.insert(key, value);
    }
    // Step 3: ensure PATH.
    ensure_path_in_env(&mut combined);
    combined
}

/// 判断 env 中某个键是否"非空字符串"。
///
/// Node 等价：`hasNonEmptyEnvValue`（claude-local / codex-local 通用）。
/// `key` 缺失或值为非字符串 / 空字符串 / 纯空白 → 返回 `false`。
pub fn has_non_empty_env_value(
    env: &std::collections::BTreeMap<String, String>,
    key: &str,
) -> bool {
    env.get(key)
        .map(|raw| !raw.trim().is_empty())
        .unwrap_or(false)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_path_does_not_overwrite_existing_path() {
        let mut env = BTreeMap::new();
        env.insert("PATH".into(), "/custom/bin".into());
        let result = ensure_path_in_env(&mut env);
        assert_eq!(result.get("PATH"), Some(&"/custom/bin".to_string()));
    }

    #[test]
    fn ensure_path_accepts_windows_cased_path() {
        let mut env = BTreeMap::new();
        env.insert("Path".into(), "C:\\Windows\\System32".into());
        let result = ensure_path_in_env(&mut env);
        // Windows-cased `Path` is preserved as-is — we do not rewrite to
        // `PATH`. The Node implementation also accepts either case.
        assert_eq!(
            result.get("Path"),
            Some(&"C:\\Windows\\System32".to_string())
        );
        assert!(result.get("PATH").is_none());
    }

    #[test]
    fn ensure_path_inserts_default_when_missing() {
        let mut env = BTreeMap::new();
        env.insert("FOO".into(), "bar".into());
        ensure_path_in_env(&mut env);
        assert!(env.get("PATH").map(|v| !v.is_empty()).unwrap_or(false));
        assert_eq!(env.get("FOO"), Some(&"bar".to_string()));
    }

    #[test]
    fn ensure_path_ignores_empty_string() {
        let mut env = BTreeMap::new();
        env.insert("PATH".into(), "".into());
        ensure_path_in_env(&mut env);
        // Empty PATH must be replaced with a real default.
        assert!(env.get("PATH").map(|v| !v.is_empty()).unwrap_or(false));
    }

    #[test]
    fn default_path_is_non_empty_for_every_platform() {
        assert!(!default_path_for_platform().is_empty());
        if cfg!(target_os = "windows") {
            assert!(default_path_for_platform().contains("Windows"));
        } else {
            assert!(default_path_for_platform().contains("/usr/bin"));
        }
    }

    #[test]
    fn resolve_runtime_env_overlays_caller_on_process() {
        let mut caller = BTreeMap::new();
        caller.insert("PAPERCLIP_TEST".into(), "value".into());
        let result = resolve_runtime_env(caller);
        assert_eq!(result.get("PAPERCLIP_TEST"), Some(&"value".to_string()));
        assert!(result.get("PATH").map(|v| !v.is_empty()).unwrap_or(false));
    }

    #[test]
    fn resolve_runtime_env_caller_overrides_process() {
        let mut caller = BTreeMap::new();
        caller.insert("PATH".into(), "/caller/bin".into());
        let result = resolve_runtime_env(caller);
        // Caller's PATH wins (no platform default inserted).
        assert_eq!(result.get("PATH"), Some(&"/caller/bin".to_string()));
    }

    #[test]
    fn has_non_empty_env_value_命中() {
        let mut env = BTreeMap::new();
        env.insert("FOO".to_owned(), "bar".to_owned());
        assert!(has_non_empty_env_value(&env, "FOO"));
        env.insert("BAZ".to_owned(), "  spaced  ".to_owned());
        assert!(has_non_empty_env_value(&env, "BAZ"));
    }

    #[test]
    fn has_non_empty_env_value_空值或缺失() {
        let mut env = BTreeMap::new();
        env.insert("EMPTY".to_owned(), "".to_owned());
        env.insert("SPACES".to_owned(), "   ".to_owned());
        assert!(!has_non_empty_env_value(&env, "EMPTY"));
        assert!(!has_non_empty_env_value(&env, "SPACES"));
        assert!(!has_non_empty_env_value(&env, "MISSING"));
    }
}
