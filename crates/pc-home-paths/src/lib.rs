#![forbid(unsafe_code)]
#![allow(clippy::doc_markdown)]

//! Paperclip home-directory + per-instance path resolution.
//!
//! R545: Direct port of `paperclip/packages/shared/src/home-paths.ts` (92 LOC).
//! Resolves the on-disk layout that the rest of the server reads / writes:
//!
//! ```text
//! <home>/
//!   instances/
//!     <instanceId>/
//!       config.json     # PAPERCLIP_CONFIG_BASENAME
//!       .env            # PAPERCLIP_ENV_FILENAME
//!       db/             # resolveDefaultEmbeddedPostgresDir
//!       logs/           # resolveDefaultLogsDir
//!       secrets/master.key
//!       data/storage/   # resolveDefaultStorageDir
//!       data/backups/   # resolveDefaultBackupDir
//! ```
//!
//! 设计原则:
//! - **Pure functions over injectable traits** — the underlying `std::env` /
//!   `dirs::home_dir` lookups are abstracted behind the [`Env`] trait, so
//!   tests can pin both the home directory and every environment variable
//!   without touching global state.
//! - **Single root resolver** — [`resolve_paperclip_instance_paths`] returns
//!   a [`PaperclipInstancePaths`] struct holding every default sub-path,
//!   eliminating 11 separate top-level functions for the call sites that
//!   need the full layout. The original 1-arg helpers remain for callers
//!   that only need one slot (mirror the upstream API).
//! - **`~` expansion** mirrors Node `expandHomePrefix` semantics:
//!   `~` → home dir; `~/foo` → `<home>/foo`; anything else left alone.
//! - **Strict instance id validation** (`[a-zA-Z0-9_-]+`) preserves the
//!   upstream guard that prevents path traversal via the `PAPERCLIP_INSTANCE_ID`
//!   environment variable.
//!
//! 设计 vs Node 上游:
//! - Replaces `process.env` + `os.homedir()` (Node, global + side-effecting)
//!   with an `Env` trait (Rust, dependency-injected + 100% deterministic).
//! - Adds `resolve_paperclip_instance_paths` aggregator that returns the
//!   full layout as one struct — the Node API forces callers to call
//!   each resolver individually.

use std::path::{Path, PathBuf};

// ============================================================================
// Constants
// ============================================================================

/// Default instance id when neither override nor `PAPERCLIP_INSTANCE_ID` env is
/// supplied. Mirrors Node `DEFAULT_PAPERCLIP_INSTANCE_ID = "default"`.
pub const DEFAULT_PAPERCLIP_INSTANCE_ID: &str = "default";

/// File name of the per-instance config file. Mirrors Node
/// `PAPERCLIP_CONFIG_BASENAME = "config.json"`.
pub const PAPERCLIP_CONFIG_BASENAME: &str = "config.json";

/// File name of the per-instance `.env` file. Mirrors Node
/// `PAPERCLIP_ENV_FILENAME = ".env"`.
pub const PAPERCLIP_ENV_FILENAME: &str = ".env";

// ============================================================================
// Env trait (testable abstraction over `std::env` + `dirs::home_dir`)
// ============================================================================

/// Abstraction over the lookup of `home_dir()` plus arbitrary environment
/// variables. Implementations:
///
/// - [`StdEnv`] — production, reads from `std::env::var` + `dirs::home_dir`.
/// - Custom mock in tests for deterministic behaviour.
///
/// All methods are infallible: an unset variable returns `None`, an
/// unreadable home directory returns `None` (which the resolvers treat as
/// "fall through to default" exactly like the Node upstream).
pub trait Env {
    /// Home directory of the current user, equivalent to Node `os.homedir()`.
    fn home_dir(&self) -> Option<PathBuf>;

    /// Read the value of an environment variable.
    fn var(&self, name: &str) -> Option<String>;
}

/// Production `Env` backed by `std::env::var` + `dirs::home_dir`.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdEnv;

impl Env for StdEnv {
    fn home_dir(&self) -> Option<PathBuf> {
        dirs::home_dir()
    }

    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

// ============================================================================
// Errors
// ============================================================================

/// Errors that the resolvers can surface. `InvalidInstanceId` is the only
/// one that can fire from the resolvers themselves; I/O errors are deferred
/// to the actual reader / writer of the resolved paths.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HomePathError {
    #[error("Invalid PAPERCLIP_INSTANCE_ID '{0}'. Allowed characters: a-z, A-Z, 0-9, _, -")]
    InvalidInstanceId(String),
}

// ============================================================================
// Public API
// ============================================================================

/// Expand a leading `~` to the current user's home directory. Mirrors
/// Node `expandHomePrefix`:
/// - `"~"` → home dir
/// - `"~/foo"` → `<home>/foo`
/// - everything else → returned unchanged (resolved to an absolute path)
pub fn expand_home_prefix<E: Env>(env: &E, value: &str) -> PathBuf {
    if value == "~" {
        return env.home_dir().unwrap_or_else(|| PathBuf::from("."));
    }
    if let Some(rest) = value.strip_prefix("~/") {
        let home = env.home_dir().unwrap_or_else(|| PathBuf::from("."));
        return home.join(rest);
    }
    PathBuf::from(value)
}

/// Resolve the top-level Paperclip home directory.
///
/// Order of precedence (matches Node `resolvePaperclipHomeDir`):
/// 1. `home_override` (if non-empty after trim)
/// 2. `PAPERCLIP_HOME` env (if non-empty after trim)
/// 3. `<user_home>/.paperclip`
pub fn resolve_paperclip_home_dir<E: Env>(env: &E, home_override: Option<&str>) -> PathBuf {
    let raw = home_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            env.var("PAPERCLIP_HOME")
                .map(|v| v.trim().to_string())
                .filter(|s| !s.is_empty())
        });
    if let Some(value) = raw {
        return expand_home_prefix(env, &value);
    }
    let home = env.home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".paperclip")
}

/// Validate and resolve the instance id.
///
/// Order of precedence (matches Node `resolvePaperclipInstanceId`):
/// 1. `instance_id_override` (if non-empty after trim)
/// 2. `PAPERCLIP_INSTANCE_ID` env (if non-empty after trim)
/// 3. [`DEFAULT_PAPERCLIP_INSTANCE_ID`]
pub fn resolve_paperclip_instance_id<E: Env>(
    env: &E,
    instance_id_override: Option<&str>,
) -> Result<String, HomePathError> {
    let raw = instance_id_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            env.var("PAPERCLIP_INSTANCE_ID")
                .map(|v| v.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| DEFAULT_PAPERCLIP_INSTANCE_ID.to_string());
    if !is_valid_instance_id(&raw) {
        return Err(HomePathError::InvalidInstanceId(raw));
    }
    Ok(raw)
}

/// `<home>/instances/<instanceId>`.
pub fn resolve_paperclip_instance_root<E: Env>(
    env: &E,
    input: PaperclipInstanceInput<'_>,
) -> Result<PathBuf, HomePathError> {
    let home = resolve_paperclip_home_dir(env, input.home_dir);
    let id = resolve_paperclip_instance_id(env, input.instance_id)?;
    Ok(home.join("instances").join(id))
}

/// `<instance_root>/<PAPERCLIP_CONFIG_BASENAME>` (i.e. `config.json`).
pub fn resolve_paperclip_instance_config_path<E: Env>(
    env: &E,
    input: PaperclipInstanceInput<'_>,
) -> Result<PathBuf, HomePathError> {
    Ok(resolve_paperclip_instance_root(env, input)?.join(PAPERCLIP_CONFIG_BASENAME))
}

/// Alias for [`resolve_paperclip_instance_config_path`] (preserved for
/// callers that prefer the longer name).
pub fn resolve_paperclip_config_path_for_instance<E: Env>(
    env: &E,
    input: PaperclipInstanceInput<'_>,
) -> Result<PathBuf, HomePathError> {
    resolve_paperclip_instance_config_path(env, input)
}

/// `<instance_root_parent>/<PAPERCLIP_ENV_FILENAME>` (i.e. `.env` next to
/// `config.json`).
pub fn resolve_paperclip_env_path_for_config(config_path: &Path) -> PathBuf {
    let parent = config_path.parent().unwrap_or_else(|| Path::new("."));
    parent.join(PAPERCLIP_ENV_FILENAME)
}

/// `<instance_root>/db`.
pub fn resolve_default_embedded_postgres_dir<E: Env>(
    env: &E,
    input: PaperclipInstanceInput<'_>,
) -> Result<PathBuf, HomePathError> {
    Ok(resolve_paperclip_instance_root(env, input)?.join("db"))
}

/// `<instance_root>/logs`.
pub fn resolve_default_logs_dir<E: Env>(
    env: &E,
    input: PaperclipInstanceInput<'_>,
) -> Result<PathBuf, HomePathError> {
    Ok(resolve_paperclip_instance_root(env, input)?.join("logs"))
}

/// `<instance_root>/secrets/master.key`.
pub fn resolve_default_secrets_key_file_path<E: Env>(
    env: &E,
    input: PaperclipInstanceInput<'_>,
) -> Result<PathBuf, HomePathError> {
    Ok(resolve_paperclip_instance_root(env, input)?
        .join("secrets")
        .join("master.key"))
}

/// `<instance_root>/data/storage`.
pub fn resolve_default_storage_dir<E: Env>(
    env: &E,
    input: PaperclipInstanceInput<'_>,
) -> Result<PathBuf, HomePathError> {
    Ok(resolve_paperclip_instance_root(env, input)?
        .join("data")
        .join("storage"))
}

/// `<instance_root>/data/backups`.
pub fn resolve_default_backup_dir<E: Env>(
    env: &E,
    input: PaperclipInstanceInput<'_>,
) -> Result<PathBuf, HomePathError> {
    Ok(resolve_paperclip_instance_root(env, input)?
        .join("data")
        .join("backups"))
}

/// Expand `~` (or `~/...`) in an arbitrary user-supplied path and return an
/// absolute path. Mirrors Node `resolveHomeAwarePath`.
pub fn resolve_home_aware_path<E: Env>(env: &E, value: &str) -> PathBuf {
    expand_home_prefix(env, value)
}

/// Aggregate resolver — returns every default path for one instance in a
/// single struct. Prefer this in business code that needs the full layout
/// to avoid the 11 separate resolver calls.
pub fn resolve_paperclip_instance_paths<E: Env>(
    env: &E,
    input: PaperclipInstanceInput<'_>,
) -> Result<PaperclipInstancePaths, HomePathError> {
    let root = resolve_paperclip_instance_root(env, input)?;
    let id = resolve_paperclip_instance_id(env, input.instance_id)?;
    let config_path = root.join(PAPERCLIP_CONFIG_BASENAME);
    let env_path = resolve_paperclip_env_path_for_config(&config_path);
    Ok(PaperclipInstancePaths {
        home: resolve_paperclip_home_dir(env, input.home_dir),
        instance_id: id,
        root,
        config_path,
        env_path,
        embedded_postgres_dir: resolve_default_embedded_postgres_dir(env, input)?,
        logs_dir: resolve_default_logs_dir(env, input)?,
        secrets_key_file: resolve_default_secrets_key_file_path(env, input)?,
        storage_dir: resolve_default_storage_dir(env, input)?,
        backup_dir: resolve_default_backup_dir(env, input)?,
    })
}

/// Input bundle for every resolver that takes both a home + instance id.
/// Mirrors the `{ homeDir?, instanceId? }` record used in Node.
#[derive(Debug, Clone, Copy, Default)]
pub struct PaperclipInstanceInput<'a> {
    pub home_dir: Option<&'a str>,
    pub instance_id: Option<&'a str>,
}

/// Result of [`resolve_paperclip_instance_paths`] — every default sub-path
/// pre-computed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaperclipInstancePaths {
    pub home: PathBuf,
    pub instance_id: String,
    pub root: PathBuf,
    pub config_path: PathBuf,
    pub env_path: PathBuf,
    pub embedded_postgres_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub secrets_key_file: PathBuf,
    pub storage_dir: PathBuf,
    pub backup_dir: PathBuf,
}

// ============================================================================
// Helpers
// ============================================================================

fn is_valid_instance_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

// ============================================================================
// Internal unit tests
// ============================================================================

#[cfg(test)]
mod internal_tests {
    use super::*;

    #[test]
    fn is_valid_instance_id_rules() {
        assert!(is_valid_instance_id("default"));
        assert!(is_valid_instance_id("instance-1"));
        assert!(is_valid_instance_id("foo_bar"));
        assert!(is_valid_instance_id("abc123"));
        assert!(!is_valid_instance_id(""));
        assert!(!is_valid_instance_id("a/b"));
        assert!(!is_valid_instance_id("a b"));
        assert!(!is_valid_instance_id("../etc"));
        assert!(!is_valid_instance_id(".dotfile"));
    }
}
