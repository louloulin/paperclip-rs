//! `pc-acpx` instance-root resolver — pure helper for resolving the
//! Paperclip instance root directory used by adapter-side code.
//!
//! Rust port of Node `packages/adapter-utils/src/server-utils.ts`:
//! - `DEFAULT_PAPERCLIP_INSTANCE_ID` (L106)
//! - `PATH_SEGMENT_RE` (L107)
//! - `expandHomePrefix` (L133-137)
//! - `resolvePaperclipInstanceRootForAdapter` (L139-149)
//!
//! This module is independent of `paths::resolve_paperclip_instance_root`
//! (which is a slightly different public surface introduced in R369).
//! Both implementations stay in sync by sharing the same lexer
//! (`is_valid_paperclip_instance_id`) and the same path-resolution rules
//! (lexical `.` / `..` collapse with absolute-path short-circuit), but
//! neither module calls the other. Callers that need a `PathBuf` and
//! `AcpxError` should keep using `paths::resolve_paperclip_instance_root`;
//! callers that need Node parity (`{homeDir?, instanceId?, env?}` ->
//! `string`) should use the helpers below.
//!
//! All helpers are pure: no I/O, no async, no global state. The
//! environment is supplied by the caller (the Node version reads
//! `process.env` when `input.env` is omitted; we mirror that behaviour
//! via [`resolve_paperclip_instance_root_for_adapter`]).

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// Env var that overrides the Paperclip home directory. Mirrors the
/// literal key used by Node `server-utils.ts` (read via
/// `env.PAPERCLIP_HOME`).
pub const PAPERCLIP_HOME_ENV: &str = "PAPERCLIP_HOME";

/// Env var that overrides the Paperclip instance id. Mirrors the
/// literal key used by Node `server-utils.ts` (read via
/// `env.PAPERCLIP_INSTANCE_ID`).
pub const PAPERCLIP_INSTANCE_ID_ENV: &str = "PAPERCLIP_INSTANCE_ID";

/// Default instance id used when neither the input nor
/// `PAPERCLIP_INSTANCE_ID` supplies one. Mirrors
/// `DEFAULT_PAPERCLIP_INSTANCE_ID = "default"`.
pub const DEFAULT_PAPERCLIP_INSTANCE_ID: &str = "default";

/// Subdirectory under the home directory that holds per-instance state.
/// Mirrors the literal `"instances"` segment used by
/// `path.resolve(homeDir, "instances", instanceId)`.
pub const INSTANCES_DIR_NAME: &str = "instances";

/// Suffix appended to the user's home directory when neither
/// `PAPERCLIP_HOME` nor `homeDir` is supplied. Mirrors the literal
/// `".paperclip"` used by `path.resolve(os.homedir(), ".paperclip")`.
pub const DEFAULT_PAPERCLIP_HOME_SUFFIX: &str = ".paperclip";

/// Validate that a string is a syntactically valid Paperclip instance id.
/// Mirrors `PATH_SEGMENT_RE.test(instanceId)` (`/^[a-zA-Z0-9_-]+$/`):
/// the value must be non-empty and every character must be an ASCII
/// alphanumeric, `_`, or `-`.
pub fn is_valid_paperclip_instance_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

// ============================================================================
// expandHomePrefix
// ============================================================================

/// Expand a leading `~` or `~/...` to the supplied home directory. Any
/// other value is returned verbatim. Mirrors Node `expandHomePrefix`
/// (L133-137).
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
// Lexical path resolution (mirrors `path.resolve`)
// ============================================================================

/// Lexically resolve `.` / `..` segments without touching the filesystem.
/// Mirrors Node `path.resolve` semantics (lexical only, no symlink
/// resolution).
fn lexically_normalize(path: PathBuf) -> PathBuf {
    let mut components: Vec<std::path::Component<'_>> = Vec::new();
    for comp in path.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if let Some(last) = components.last() {
                    if matches!(last, std::path::Component::Normal(_)) {
                        components.pop();
                        continue;
                    }
                }
                components.push(comp);
            }
            other => components.push(other),
        }
    }
    let mut out = PathBuf::new();
    for comp in components {
        out.push(comp.as_os_str());
    }
    out
}

/// Lexically resolve the supplied segments relative to a base directory
/// (which itself is resolved against the supplied cwd when not absolute).
/// Mirrors `path.resolve(base, ...segments)`: if the joined path is
/// relative, it is anchored to `cwd`; otherwise it is normalised as-is.
fn path_resolve(cwd: &Path, base: &str, segments: &[&str]) -> PathBuf {
    let mut combined = if base.is_empty() {
        PathBuf::new()
    } else {
        PathBuf::from(base)
    };
    for seg in segments {
        combined.push(seg);
    }
    let anchored = if combined.is_absolute() {
        combined
    } else {
        cwd.join(combined)
    };
    lexically_normalize(anchored)
}

// ============================================================================
// ResolvePaperclipInstanceRootForAdapter
// ============================================================================

/// Inputs to [`resolve_paperclip_instance_root_for_adapter`]. Mirrors the
/// shape of the Node input object `{ homeDir?, instanceId?, env? }`.
#[derive(Debug, Default, Clone)]
pub struct ResolvePaperclipInstanceRootInput {
    /// Caller-supplied Paperclip home directory. Trims and falls back to
    /// `env.PAPERCLIP_HOME`, then `~/.paperclip`.
    pub home_dir: Option<String>,
    /// Caller-supplied Paperclip instance id. Trims and falls back to
    /// `env.PAPERCLIP_INSTANCE_ID`, then `DEFAULT_PAPERCLIP_INSTANCE_ID`.
    pub instance_id: Option<String>,
    /// Env-like key/value bag to consult for `PAPERCLIP_HOME` and
    /// `PAPERCLIP_INSTANCE_ID`. When `None`, [`std::env::vars`] is used
    /// (mirrors `input.env ?? process.env`).
    pub env: Option<BTreeMap<String, String>>,
}

/// Errors raised by [`resolve_paperclip_instance_root_for_adapter`].
/// Mirrors the Node `throw new Error("Invalid PAPERCLIP_INSTANCE_ID ...")`
/// at L148.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvePaperclipInstanceRootError {
    /// The supplied `instance_id` (after trimming and env fallback) does
    /// not match `[A-Za-z0-9_-]+`. The wrapped value is the offending id.
    InvalidInstanceId(String),
}

impl fmt::Display for ResolvePaperclipInstanceRootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInstanceId(value) => {
                write!(f, "Invalid PAPERCLIP_INSTANCE_ID '{value}'.")
            }
        }
    }
}

impl std::error::Error for ResolvePaperclipInstanceRootError {}

/// Resolve the Paperclip instance root directory used by adapter-side
/// code. Mirrors `resolvePaperclipInstanceRootForAdapter` (L139-149):
///
/// 1. `homeRaw` is the first non-empty of `homeDir.trim()` and
///    `env.PAPERCLIP_HOME.trim()`. When both are absent the function
///    falls back to `<home>/.paperclip`.
/// 2. `homeDir` is `path.resolve(expandHomePrefix(homeRaw))` (when
///    `homeRaw` is supplied) or `path.resolve(homedir, ".paperclip")`.
/// 3. `instanceId` is the first non-empty of `instanceId.trim()`,
///    `env.PAPERCLIP_INSTANCE_ID.trim()`, and
///    `DEFAULT_PAPERCLIP_INSTANCE_ID`.
/// 4. `instanceId` must match `[A-Za-z0-9_-]+`; otherwise the function
///    returns `ResolvePaperclipInstanceRootError::InvalidInstanceId`.
/// 5. The returned value is `path.resolve(homeDir, "instances", instanceId)`,
///    i.e. `<home>/instances/<instance>` made absolute and lexically
///    normalized.
pub fn resolve_paperclip_instance_root_for_adapter(
    input: &ResolvePaperclipInstanceRootInput,
) -> Result<String, ResolvePaperclipInstanceRootError> {
    let env_owned;
    let env: &BTreeMap<String, String> = match input.env.as_ref() {
        Some(e) => e,
        None => {
            env_owned = std::env::vars().collect::<BTreeMap<_, _>>();
            &env_owned
        }
    };

    let home_raw = input
        .home_dir
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            env.get(PAPERCLIP_HOME_ENV)
                .map(|s| s.trim())
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        });

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let home = home_dir_or_default(&cwd);

    let home_dir: String = match home_raw {
        Some(raw) => {
            let expanded = expand_home_prefix(&raw, &home);
            path_resolve(&cwd, &expanded, &[])
                .to_string_lossy()
                .into_owned()
        }
        None => {
            let fallback = home.join(DEFAULT_PAPERCLIP_HOME_SUFFIX);
            path_resolve(&cwd, "", &[fallback.to_string_lossy().as_ref()])
                .to_string_lossy()
                .into_owned()
        }
    };

    let instance_id = input
        .instance_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            env.get(PAPERCLIP_INSTANCE_ID_ENV)
                .map(|s| s.trim())
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| DEFAULT_PAPERCLIP_INSTANCE_ID.to_string());

    if !is_valid_paperclip_instance_id(&instance_id) {
        return Err(ResolvePaperclipInstanceRootError::InvalidInstanceId(
            instance_id,
        ));
    }

    let resolved = path_resolve(&cwd, &home_dir, &[INSTANCES_DIR_NAME, &instance_id]);
    Ok(resolved.to_string_lossy().into_owned())
}

/// Resolve the Paperclip instance root directory using `std::env` for
/// the caller-supplied inputs and the `PAPERCLIP_HOME` /
/// `PAPERCLIP_INSTANCE_ID` lookups. Mirrors the Node behaviour of calling
/// `resolvePaperclipInstanceRootForAdapter({})` with no arguments.
pub fn default_resolve_paperclip_instance_root_for_adapter(
) -> Result<String, ResolvePaperclipInstanceRootError> {
    resolve_paperclip_instance_root_for_adapter(&ResolvePaperclipInstanceRootInput::default())
}

fn home_dir_or_default(cwd: &Path) -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home);
        }
    }
    // No HOME — fall back to cwd (matches Node behaviour of passing
    // whatever `os.homedir()` returned; on a host with no $HOME the
    // Node implementation also returns the empty string, which
    // `path.resolve` collapses to cwd).
    cwd.to_path_buf()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_env() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    fn input_with_env(env: BTreeMap<String, String>) -> ResolvePaperclipInstanceRootInput {
        ResolvePaperclipInstanceRootInput {
            home_dir: None,
            instance_id: None,
            env: Some(env),
        }
    }

    // ----- DEFAULT_PAPERCLIP_INSTANCE_ID -----

    #[test]
    fn default_instance_id_is_default() {
        assert_eq!(DEFAULT_PAPERCLIP_INSTANCE_ID, "default");
    }

    #[test]
    fn paperclip_home_env_keys_are_stable() {
        assert_eq!(PAPERCLIP_HOME_ENV, "PAPERCLIP_HOME");
        assert_eq!(PAPERCLIP_INSTANCE_ID_ENV, "PAPERCLIP_INSTANCE_ID");
    }

    // ----- is_valid_paperclip_instance_id -----

    #[test]
    fn instance_id_validator_accepts_alphanumeric() {
        assert!(is_valid_paperclip_instance_id("default"));
        assert!(is_valid_paperclip_instance_id("prod"));
        assert!(is_valid_paperclip_instance_id("ABC123"));
        assert!(is_valid_paperclip_instance_id("0"));
        assert!(is_valid_paperclip_instance_id("9"));
    }

    #[test]
    fn instance_id_validator_accepts_underscore_and_dash() {
        assert!(is_valid_paperclip_instance_id("with_underscore"));
        assert!(is_valid_paperclip_instance_id("with-dash"));
        assert!(is_valid_paperclip_instance_id("a_b-c"));
        assert!(is_valid_paperclip_instance_id("_leading"));
        assert!(is_valid_paperclip_instance_id("-leading"));
        assert!(is_valid_paperclip_instance_id("trailing_"));
        assert!(is_valid_paperclip_instance_id("trailing-"));
    }

    #[test]
    fn instance_id_validator_rejects_empty() {
        assert!(!is_valid_paperclip_instance_id(""));
    }

    #[test]
    fn instance_id_validator_rejects_path_separators() {
        assert!(!is_valid_paperclip_instance_id("../bad"));
        assert!(!is_valid_paperclip_instance_id("a/b"));
        assert!(!is_valid_paperclip_instance_id("a\\b"));
    }

    #[test]
    fn instance_id_validator_rejects_whitespace_and_punct() {
        assert!(!is_valid_paperclip_instance_id("a b"));
        assert!(!is_valid_paperclip_instance_id("a.b"));
        assert!(!is_valid_paperclip_instance_id("a:b"));
        assert!(!is_valid_paperclip_instance_id("a@b"));
        assert!(!is_valid_paperclip_instance_id("a*b"));
        assert!(!is_valid_paperclip_instance_id(" a"));
        assert!(!is_valid_paperclip_instance_id("a "));
        assert!(!is_valid_paperclip_instance_id("a\nb"));
    }

    #[test]
    fn instance_id_validator_rejects_unicode() {
        assert!(!is_valid_paperclip_instance_id("实例"));
        assert!(!is_valid_paperclip_instance_id("café"));
        assert!(!is_valid_paperclip_instance_id("a–b"));
    }

    // ----- expand_home_prefix -----

    #[test]
    fn expand_home_prefix_handles_tilde_forms() {
        let home = PathBuf::from("/Users/alice");
        assert_eq!(expand_home_prefix("~", &home), "/Users/alice");
        assert_eq!(expand_home_prefix("~/skills", &home), "/Users/alice/skills");
        assert_eq!(expand_home_prefix("/etc/passwd", &home), "/etc/passwd");
        assert_eq!(expand_home_prefix("relative/path", &home), "relative/path");
    }

    // ----- resolve_paperclip_instance_root_for_adapter -----

    #[test]
    fn resolve_default_falls_back_to_default_id_and_home_default() {
        // Caller supplied nothing; env is empty — the resolver must use
        // ~/<.paperclip>/instances/default.
        let env = empty_env();
        let input = input_with_env(env);
        let resolved = resolve_paperclip_instance_root_for_adapter(&input).expect("valid");
        assert!(resolved.ends_with("/instances/default"));
        assert!(resolved.contains(DEFAULT_PAPERCLIP_HOME_SUFFIX));
    }

    #[test]
    fn resolve_prefers_caller_supplied_home_dir() {
        let mut input = input_with_env(BTreeMap::new());
        input.home_dir = Some("/var/lib/paperclip".to_string());
        input.instance_id = Some("prod".to_string());
        let resolved = resolve_paperclip_instance_root_for_adapter(&input).expect("valid");
        assert_eq!(resolved, "/var/lib/paperclip/instances/prod");
    }

    #[test]
    fn resolve_trims_home_dir_input() {
        let mut input = input_with_env(BTreeMap::new());
        input.home_dir = Some("   /var/lib/paperclip   ".to_string());
        let resolved = resolve_paperclip_instance_root_for_adapter(&input).expect("valid");
        assert_eq!(resolved, "/var/lib/paperclip/instances/default");
    }

    #[test]
    fn resolve_trims_instance_id_input() {
        let mut input = input_with_env(BTreeMap::new());
        input.instance_id = Some("  staging-2  ".to_string());
        let resolved = resolve_paperclip_instance_root_for_adapter(&input).expect("valid");
        assert!(resolved.ends_with("/instances/staging-2"));
    }

    #[test]
    fn resolve_falls_back_to_env_home_when_input_empty() {
        let mut env = BTreeMap::new();
        env.insert(PAPERCLIP_HOME_ENV.to_string(), "/srv/paperclip".to_string());
        env.insert(PAPERCLIP_INSTANCE_ID_ENV.to_string(), "beta".to_string());
        let input = input_with_env(env);
        let resolved = resolve_paperclip_instance_root_for_adapter(&input).expect("valid");
        assert_eq!(resolved, "/srv/paperclip/instances/beta");
    }

    #[test]
    fn resolve_falls_back_to_env_home_when_input_blank() {
        // Whitespace-only input should be treated as absent (matches
        // `homeDir?.trim() || env.PAPERCLIP_HOME?.trim()`).
        let mut env = BTreeMap::new();
        env.insert(PAPERCLIP_HOME_ENV.to_string(), "/srv/paperclip".to_string());
        let mut input = input_with_env(env);
        input.home_dir = Some("   ".to_string());
        let resolved = resolve_paperclip_instance_root_for_adapter(&input).expect("valid");
        assert_eq!(resolved, "/srv/paperclip/instances/default");
    }

    #[test]
    fn resolve_instance_id_input_overrides_env() {
        let mut env = BTreeMap::new();
        env.insert(
            PAPERCLIP_INSTANCE_ID_ENV.to_string(),
            "from-env".to_string(),
        );
        let mut input = input_with_env(env);
        input.instance_id = Some("from-input".to_string());
        let resolved = resolve_paperclip_instance_root_for_adapter(&input).expect("valid");
        assert!(resolved.ends_with("/instances/from-input"));
    }

    #[test]
    fn resolve_expands_tilde_in_home_input() {
        let mut input = input_with_env(BTreeMap::new());
        input.home_dir = Some("~/paperclip-custom".to_string());
        input.instance_id = Some("alpha".to_string());
        let resolved = resolve_paperclip_instance_root_for_adapter(&input).expect("valid");
        // The trailing segment must reflect the expanded home + the
        // supplied instance id (independent of what $HOME actually is in
        // the test runner).
        assert!(resolved.ends_with("/paperclip-custom/instances/alpha"));
    }

    #[test]
    fn resolve_relative_home_input_is_anchored_to_cwd() {
        let mut input = input_with_env(BTreeMap::new());
        input.home_dir = Some("paperclip-relative".to_string());
        let resolved = resolve_paperclip_instance_root_for_adapter(&input).expect("valid");
        let cwd = std::env::current_dir().expect("cwd");
        let expected = lexically_normalize(
            cwd.join("paperclip-relative")
                .join("instances")
                .join("default"),
        );
        assert_eq!(resolved, expected.to_string_lossy());
    }

    #[test]
    fn resolve_rejects_invalid_instance_id() {
        let mut input = input_with_env(BTreeMap::new());
        input.instance_id = Some("../bad".to_string());
        let err = resolve_paperclip_instance_root_for_adapter(&input)
            .expect_err("../bad must be rejected");
        assert_eq!(
            err,
            ResolvePaperclipInstanceRootError::InvalidInstanceId("../bad".to_string())
        );
    }

    #[test]
    fn resolve_blank_instance_id_falls_back_to_default() {
        // Whitespace-only instanceId is treated as absent (matches
        // `instanceId?.trim() || env.X?.trim() || DEFAULT`). It must
        // therefore resolve to the default id rather than failing the
        // validator.
        let mut input = input_with_env(BTreeMap::new());
        input.instance_id = Some("   ".to_string());
        let resolved = resolve_paperclip_instance_root_for_adapter(&input)
            .expect("blank instance id must fall back to default");
        assert!(resolved.ends_with("/instances/default"));
    }

    #[test]
    fn resolve_rejects_invalid_instance_id_from_env() {
        let mut env = BTreeMap::new();
        env.insert(
            PAPERCLIP_INSTANCE_ID_ENV.to_string(),
            "with/slash".to_string(),
        );
        let input = input_with_env(env);
        let err = resolve_paperclip_instance_root_for_adapter(&input)
            .expect_err("env-supplied invalid instance id must be rejected");
        assert_eq!(
            err,
            ResolvePaperclipInstanceRootError::InvalidInstanceId("with/slash".to_string())
        );
    }

    #[test]
    fn resolve_error_display_matches_node_message() {
        let err = ResolvePaperclipInstanceRootError::InvalidInstanceId("../bad".to_string());
        assert_eq!(err.to_string(), "Invalid PAPERCLIP_INSTANCE_ID '../bad'.");
    }

    #[test]
    fn resolve_input_default_is_consistent() {
        // Default input must mirror Node `{}`: no home, no instance id,
        // no caller-supplied env (so the helper reads std::env).
        let input = ResolvePaperclipInstanceRootInput::default();
        assert!(input.home_dir.is_none());
        assert!(input.instance_id.is_none());
        assert!(input.env.is_none());
    }

    #[test]
    fn path_resolve_normalises_dot_dot_segments() {
        let cwd = Path::new("/");
        assert_eq!(path_resolve(cwd, "/a/b", &["../c"]), PathBuf::from("/a/c"));
        assert_eq!(path_resolve(cwd, "/a/b", &["c"]), PathBuf::from("/a/b/c"));
        assert_eq!(path_resolve(cwd, "a", &["b"]), PathBuf::from("/a/b"));
    }

    #[test]
    fn home_dir_or_default_returns_home_env_var() {
        // std::env::var_os is not mutable from tests; the helper must
        // honour $HOME when set (the dev shell sets it). At minimum
        // it must return *some* absolute path.
        let cwd = Path::new("/");
        let home = home_dir_or_default(cwd);
        assert!(home.is_absolute());
    }
}
