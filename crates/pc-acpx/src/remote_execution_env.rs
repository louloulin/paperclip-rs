//! `pc-acpx::remote_execution_env` — port of `remote-execution-env.ts`
//! from Node `paperclip/packages/adapter-utils/src/`.
//!
//! The runtime-target layer passes an env map to sandbox and SSH
//! subprocesses. The Node helper
//! [`sanitizeRemoteExecutionEnv`] strips any "identity" env var that
//! the caller happens to have set to the **same** value the host
//! process is already inheriting — preventing accidental override
//! of the user's `PATH`, `HOME`, etc.
//!
//! The Rust port keeps the same precedence: the allowlist is
//! `REMOTE_EXECUTION_ENV_IDENTITY_KEYS` (case-sensitive in the
//! allowlist, but compared against the inherited env
//! case-insensitively). For each identity key:
//!
//! - If `inherited[key]` exists and equals `value`, drop it.
//! - Otherwise keep the caller's value (it represents a real override).
//!
//! Non-identity keys are always preserved.

use std::collections::BTreeMap;

/// Identity env keys that get sanitized against the inherited env.
/// Mirrors Node `REMOTE_EXECUTION_ENV_IDENTITY_KEYS`.
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

/// Case-insensitive env lookup. Mirrors Node
/// `readEnvValueCaseInsensitive` (which compares uppercased keys).
fn read_env_value_case_insensitive(env: &BTreeMap<String, String>, key: &str) -> Option<String> {
    if let Some(value) = env.get(key) {
        return Some(value.clone());
    }
    let upper = key.to_uppercase();
    for (candidate_key, candidate_value) in env {
        if candidate_key.to_uppercase() == upper {
            return Some(candidate_value.clone());
        }
    }
    None
}

/// Sanitize a remote execution env map against an inherited (host)
/// env map. Mirrors Node `sanitizeRemoteExecutionEnv`:
///
/// - For each entry in `env`, if the uppercased key is **not** in
///   [`REMOTE_EXECUTION_ENV_IDENTITY_KEYS`], keep it verbatim.
/// - For identity keys, drop the entry when the inherited env has
///   the same value (case-insensitive key lookup). Otherwise keep
///   the caller's value.
#[must_use]
pub fn sanitize_remote_execution_env(
    env: &BTreeMap<String, String>,
    inherited_env: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut sanitized: BTreeMap<String, String> = BTreeMap::new();
    for (key, value) in env {
        let normalized_key = key.to_uppercase();
        if !REMOTE_EXECUTION_ENV_IDENTITY_KEYS
            .iter()
            .any(|k| k.to_uppercase() == normalized_key)
        {
            sanitized.insert(key.clone(), value.clone());
            continue;
        }
        let inherited_value = read_env_value_case_insensitive(inherited_env, key);
        if let Some(inherited) = inherited_value {
            if inherited == *value {
                continue;
            }
        }
        sanitized.insert(key.clone(), value.clone());
    }
    sanitized
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_from(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn non_identity_keys_pass_through() {
        let env = env_from(&[("CUSTOM_VAR", "hello"), ("FOO_BAR", "baz")]);
        let inherited = BTreeMap::new();
        let sanitized = sanitize_remote_execution_env(&env, &inherited);
        assert_eq!(sanitized.get("CUSTOM_VAR").unwrap(), "hello");
        assert_eq!(sanitized.get("FOO_BAR").unwrap(), "baz");
    }

    #[test]
    fn identity_key_dropped_when_matches_inherited() {
        let env = env_from(&[("PATH", "/usr/bin:/bin")]);
        let inherited = env_from(&[("PATH", "/usr/bin:/bin")]);
        let sanitized = sanitize_remote_execution_env(&env, &inherited);
        assert!(!sanitized.contains_key("PATH"));
    }

    #[test]
    fn identity_key_preserved_when_differs_from_inherited() {
        let env = env_from(&[("PATH", "/custom/bin")]);
        let inherited = env_from(&[("PATH", "/usr/bin:/bin")]);
        let sanitized = sanitize_remote_execution_env(&env, &inherited);
        assert_eq!(sanitized.get("PATH").unwrap(), "/custom/bin");
    }

    #[test]
    fn identity_key_preserved_when_no_inherited_value() {
        let env = env_from(&[("HOME", "/tmp/home")]);
        let inherited = BTreeMap::new();
        let sanitized = sanitize_remote_execution_env(&env, &inherited);
        assert_eq!(sanitized.get("HOME").unwrap(), "/tmp/home");
    }

    #[test]
    fn identity_lookup_case_insensitive() {
        let env = env_from(&[("Path", "/custom/bin")]);
        let inherited = env_from(&[("PATH", "/usr/bin:/bin")]);
        let sanitized = sanitize_remote_execution_env(&env, &inherited);
        assert_eq!(sanitized.get("Path").unwrap(), "/custom/bin");
    }

    #[test]
    fn identity_dropped_when_inherited_differs_only_by_case() {
        let env = env_from(&[("PATH", "/usr/bin:/bin")]);
        let inherited = env_from(&[("path", "/usr/bin:/bin")]);
        let sanitized = sanitize_remote_execution_env(&env, &inherited);
        assert!(!sanitized.contains_key("PATH"));
    }

    #[test]
    fn all_documented_identity_keys_are_handled() {
        for key in REMOTE_EXECUTION_ENV_IDENTITY_KEYS {
            let env = env_from(&[(*key, "value")]);
            let inherited = env_from(&[(*key, "value")]);
            let sanitized = sanitize_remote_execution_env(&env, &inherited);
            assert!(!sanitized.contains_key(*key), "{key} should be sanitized");
        }
    }

    #[test]
    fn empty_env_returns_empty() {
        let env = BTreeMap::new();
        let inherited = BTreeMap::new();
        let sanitized = sanitize_remote_execution_env(&env, &inherited);
        assert!(sanitized.is_empty());
    }
}
