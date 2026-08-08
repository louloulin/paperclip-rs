//! `pc-acpx::sandbox_managed_runtime` - port of `sandbox-managed-runtime.ts`
//! from Node `paperclip/packages/adapter-utils/src/`.
//!
//! Pure helpers for the sandbox-managed runtime path. Async functions
//! (`createTarballFromDirectory`, `mirrorDirectory`,
//! `extractTarballToDirectory`, `walkDirectory`,
//! `copySelectedWorkspaceEntries`, `prepareSandboxManagedRuntime`,
//! `withTempDir`, `execTar`, `emitRuntimeStatus`,
//! `makeTransferProgress` runtime-half) are deferred - they require
//! real filesystem access, the ssh runner, and the bubblewrap /
//! sandbox runtime plumbing. This module ports:
//!
//! - Core types: `SandboxRemoteExecutionSpec`,
//!   `SandboxManagedRuntimeAssetProvisionContext`,
//!   `SandboxManagedRuntimeAssetProvision`,
//!   `SandboxManagedRuntimeAssetRestoreContext`,
//!   `SandboxManagedRuntimeAsset`, `SandboxAdditionalSource`,
//!   `SandboxTransferProgressOptions`, `SandboxSyncFileMapping`,
//!   `SandboxPostUploadCommand`, `SandboxSyncOperation`,
//!   `SandboxSyncResult`, `SandboxManagedRuntimeClient`,
//!   `PreparedSandboxManagedRuntime`,
//!   `AdditionalSourceStagingFailure`
//! - Constants: `SANDBOX_WORKSPACE_HEAVY_DIR_NAMES`,
//!   `sandbox_workspace_heavy_dir_excludes()`
//! - Spec parser: `parse_sandbox_remote_execution_spec`
//! - Session identity helpers:
//!   `build_sandbox_execution_session_identity`,
//!   `sandbox_execution_session_matches`
//! - Confinement guard: `assert_sync_operations_confined`
//! - Shell-command builders:
//!   `build_default_extract_runtime_asset_command`,
//!   `build_workspace_tar_extract_command`,
//!   `build_remove_deleted_paths_command`,
//!   `create_remote_tarball_from_directory_command`
//! - Small helpers: `shell_quote`, `as_object`, `as_string`,
//!   `as_number`, `merge_excludes`, `preserve_find_args`,
//!   `tar_exclude_flags`, `build_unique_staging_path`,
//!   `posix_is_absolute`, `posix_normalize`

use serde::{Deserialize, Serialize};

// =============================================================================
// Constants - mirrored from Node SANDBOX_WORKSPACE_HEAVY_DIR_NAMES.
// =============================================================================

/// Directory names that are heavy (cache / build artefacts) and should
/// be excluded from sandbox-rsync when uploading a workspace tree.
/// Mirrors Node `SANDBOX_WORKSPACE_HEAVY_DIR_NAMES`.
pub const SANDBOX_WORKSPACE_HEAVY_DIR_NAMES: &[&str] = &[
    "node_modules",
    "vendor",
    "dist",
    "build",
    "out",
    "coverage",
    ".next",
    ".turbo",
    ".cache",
];

/// Tar-exclude patterns derived from
/// [`SANDBOX_WORKSPACE_HEAVY_DIR_NAMES`]. Each entry expands to
/// `<name>`, `<name>/*`, `*/<name>`, `*/<name>/*` so any path that
/// crosses a heavy dir is excluded regardless of nesting. Mirrors
/// Node `SANDBOX_WORKSPACE_HEAVY_DIR_EXCLUDES`.
#[must_use]
pub fn sandbox_workspace_heavy_dir_excludes() -> Vec<String> {
    let mut out = Vec::with_capacity(SANDBOX_WORKSPACE_HEAVY_DIR_NAMES.len() * 4);
    for entry in SANDBOX_WORKSPACE_HEAVY_DIR_NAMES {
        out.push((*entry).to_string());
        out.push(format!("{entry}/*"));
        out.push(format!("*/{entry}"));
        out.push(format!("*/{entry}/*"));
    }
    out
}

// =============================================================================
// Core types - mirrored 1:1 from Node interfaces.
// =============================================================================

/// Remote execution target for a sandbox-backed run. Mirrors Node
/// `SandboxRemoteExecutionSpec`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxRemoteExecutionSpec {
    pub transport: String,
    pub provider: String,
    pub sandbox_id: String,
    pub remote_cwd: String,
    pub timeout_ms: u64,
    pub api_key: Option<String>,
}

/// Remote paths handed to an asset's `provision.post_upload_command`.
/// Mirrors Node `SandboxManagedRuntimeAssetProvisionContext`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxManagedRuntimeAssetProvisionContext {
    pub asset_tar_path: String,
    pub asset_dir: String,
    pub runtime_root_dir: String,
}

/// A single staged file shipped alongside an asset tar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxManagedRuntimeStageFile {
    pub name: String,
    /// UTF-8 string contents. Byte-buffer payloads are deferred with
    /// the async tar runtime half.
    pub contents: String,
}

/// Per-asset provision. Mirrors Node
/// `SandboxManagedRuntimeAssetProvision`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SandboxManagedRuntimeAssetProvision {
    pub stage_files: Vec<SandboxManagedRuntimeStageFile>,
    /// Precomputed post-upload command (the async half builds this
    /// from the asset provision context once paths are confined).
    pub post_upload_command: Option<SandboxManagedRuntimeAssetProvisionPostUploadCommand>,
}

/// Marker type for the optional builder function `(ctx) => string`
/// in Node. The Rust surface captures the precomputed command the
/// async runtime executes verbatim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxManagedRuntimeAssetProvisionPostUploadCommand {
    pub command: String,
}

/// Restore contribution context. Mirrors Node
/// `SandboxManagedRuntimeAssetRestoreContext`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxManagedRuntimeAssetRestoreContext {
    pub asset_dir: String,
    /// Async hook flag - true when the runtime registered a readFile
    /// callback during prepare.
    pub has_read_file: bool,
}

/// Per-asset descriptor. Mirrors Node
/// `SandboxManagedRuntimeAsset` (minus the async `restore` builder,
/// which is deferred).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxManagedRuntimeAsset {
    pub key: String,
    pub local_dir: String,
    pub follow_symlinks: Option<bool>,
    pub exclude: Option<Vec<String>>,
    pub provision: Option<SandboxManagedRuntimeAssetProvision>,
    /// Optional restore builder captured as a flag - the async
    /// runtime half reads the asset back per-key.
    pub has_restore: bool,
}

/// A referenced (additional) project staged into the run sandbox as
/// a plain, read-only tree. Mirrors Node `SandboxAdditionalSource`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxAdditionalSource {
    pub local_path: String,
    pub project_id: String,
}

/// Per-call byte-level progress hook. Mirrors Node
/// `SandboxTransferProgressOptions`. Async transport is deferred;
/// the struct is included so the Rust API surface remains aligned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxTransferProgressOptions {
    pub has_on_progress: bool,
}

/// A single source -> target file or directory transfer. Mirrors
/// Node `SandboxSyncFileMapping`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxSyncFileMapping {
    pub source_path: String,
    pub target_path: String,
    pub kind: String,
    pub mode: Option<u32>,
    pub exclude: Option<Vec<String>>,
    pub follow_symlinks: Option<bool>,
    pub access: Option<String>,
    pub writable_path: Option<String>,
}

impl SandboxSyncFileMapping {
    /// Default `access` is `Some("ro")` (read-only is the safe
    /// default for an advisory signal). Mirrors Node semantics.
    #[must_use]
    pub fn access_or_default(&self) -> &str {
        self.access.as_deref().unwrap_or("ro")
    }
}

/// A control command run against the sandbox after a sync
/// operation's files have landed. Mirrors Node
/// `SandboxPostUploadCommand`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxPostUploadCommand {
    pub command: String,
    pub cwd: Option<String>,
    pub timeout_ms: Option<u64>,
}

/// An ordered, opaque unit of work handed to the native sync
/// transport. Mirrors Node `SandboxSyncOperation`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxSyncOperation {
    pub operation_id: String,
    pub files: Vec<SandboxSyncFileMapping>,
    pub post_upload_commands: Option<Vec<SandboxPostUploadCommand>>,
}

impl SandboxSyncOperation {
    /// `None` is byte-identical to an empty `Vec`.
    #[must_use]
    pub fn post_upload_commands_or_empty(&self) -> &[SandboxPostUploadCommand] {
        self.post_upload_commands.as_deref().unwrap_or(&[])
    }
}

/// Aggregate result of a sync run. Mirrors Node `SandboxSyncResult`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxSyncResult {
    pub operations: Vec<SandboxSyncResultOperation>,
}

/// Per-operation result row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxSyncResultOperation {
    pub operation_id: String,
    pub files_transferred: u64,
    pub bytes_transferred: u64,
}

/// The runtime client surface returned by
/// `prepare_sandbox_managed_runtime`. Async methods are
/// mirrored as capability flags so callers know what the deferred
/// runtime will eventually expose. Mirrors Node
/// `SandboxManagedRuntimeClient`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxManagedRuntimeClient {
    pub has_native_sync_in: bool,
    pub has_native_sync_out: bool,
}

/// The precomputed, ready-to-run view of a sandbox-managed
/// runtime. Mirrors Node `PreparedSandboxManagedRuntime` (minus the
/// async `restore_workspace` builder, which is deferred).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreparedSandboxManagedRuntime {
    pub spec: SandboxRemoteExecutionSpec,
    pub workspace_local_dir: String,
    pub workspace_remote_dir: String,
    pub runtime_root_dir: String,
    pub asset_dirs: std::collections::BTreeMap<String, String>,
    pub additional_source_dirs: std::collections::BTreeMap<String, String>,
    pub additional_source_failures: Vec<AdditionalSourceStagingFailure>,
    /// Per-project timing flag for parity with Node source.
    pub has_restore_workspace: bool,
}

/// Per-project failure row. Mirrors Node
/// `AdditionalSourceStagingFailure`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdditionalSourceStagingFailure {
    pub project_id: String,
    pub error: String,
}

// =============================================================================
// Internal small helpers.
// =============================================================================

/// `value` coerced to a plain object map (returns an empty map when
/// not an object). Mirrors the Node `asObject` helper.
#[must_use]
pub fn as_object(value: &serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match value {
        serde_json::Value::Object(map) => map.clone(),
        _ => serde_json::Map::new(),
    }
}

/// Coerce a JSON value to a string, defaulting to `""`. Mirrors
/// Node `asString`.
#[must_use]
pub fn as_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

/// Coerce a JSON value to a number; returns `NaN` when not numeric.
/// Mirrors Node `asNumber` (without loose coercion; only JSON
/// numbers count).
#[must_use]
pub fn as_number(value: &serde_json::Value) -> f64 {
    match value {
        serde_json::Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        _ => f64::NAN,
    }
}

/// POSIX single-quote a string. Mirrors Node `shellQuote`.
#[must_use]
pub fn shell_quote(value: &str) -> String {
    let escaped = value.replace('\'', r#"'"'"'"#);
    format!("'{escaped}'")
}

/// Check whether a POSIX path is absolute (starts with `/`).
/// Mirrors `path.posix.isAbsolute`.
#[must_use]
pub fn posix_is_absolute(path: &str) -> bool {
    path.starts_with('/')
}

/// Normalize a POSIX path by collapsing `.` and `..` segments
/// without filesystem access. Mirrors `path.posix.normalize`.
#[must_use]
pub fn posix_normalize(path: &str) -> String {
    let is_absolute = path.starts_with('/');
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/').filter(|s| !s.is_empty()) {
        match segment {
            "." => {}
".." => {
                segments.pop();
            }
            _ => segments.push(segment),
        }
    }
    let joined = segments.join("/");
    if is_absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

/// Build a UUID-suffixed staging path. Mirrors Node
/// `buildUniqueStagingPath`.
#[must_use]
pub fn build_unique_staging_path(target_path: &str, suffix: &str) -> String {
    format!("{target_path}{suffix}.{}", uuid::Uuid::new_v4())
}

/// Merge exclude groups into a deduplicated list, preserving
/// first-occurrence order. Mirrors Node `mergeExcludes`.
#[must_use]
pub fn merge_excludes(groups: &[Option<&[String]>]) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for group in groups.iter().flatten() {
        for entry in *group {
            if seen.insert(entry.clone()) {
                out.push(entry.clone());
            }
        }
    }
    out
}

/// Build a `find` argument fragment that excludes each supplied
/// entry by name. Mirrors Node `preserveFindArgs`.
#[must_use]
pub fn preserve_find_args(entries: &[String]) -> String {
    entries
        .iter()
        .map(|entry| format!("! -name {}", shell_quote(entry)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Build a tar `-exclude` flag fragment. The Node helper always
/// prepends `"._*"` (Mac resource fork metadata) before any
/// caller-supplied excludes. Mirrors Node `tarExcludeFlags`.
#[must_use]
pub fn tar_exclude_flags(exclude: Option<&[String]>) -> String {
    let mut all: Vec<String> = vec!["._*".to_string()];
    if let Some(e) = exclude {
        all.extend(e.iter().cloned());
    }
    all.iter()
        .map(|entry| format!("--exclude {}", shell_quote(entry)))
        .collect::<Vec<_>>()
        .join(" ")
}

// =============================================================================
// Spec parser.
// =============================================================================

/// Parse a JSON-ish value into a `SandboxRemoteExecutionSpec`.
/// Returns `None` when transport is not "sandbox" or any required
/// field is missing/invalid. Mirrors Node
/// `parseSandboxRemoteExecutionSpec`.
#[must_use]
pub fn parse_sandbox_remote_execution_spec(value: &serde_json::Value) -> Option<SandboxRemoteExecutionSpec> {
    let parsed = as_object(value);

    fn get_str_field(parsed: &serde_json::Map<String, serde_json::Value>, key: &str) -> String {
        parsed
            .get(key)
            .map(as_string)
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    let transport = get_str_field(&parsed, "transport");
    let provider = get_str_field(&parsed, "provider");
    let sandbox_id = get_str_field(&parsed, "sandboxId");
    let remote_cwd = get_str_field(&parsed, "remoteCwd");
    let api_key_raw = get_str_field(&parsed, "apiKey");
    let timeout_ms_val = parsed
        .get("timeoutMs")
        .map(as_number)
        .unwrap_or(f64::NAN);

    if transport != "sandbox"
        || provider.is_empty()
        || sandbox_id.is_empty()
        || remote_cwd.is_empty()
        || !timeout_ms_val.is_finite()
        || timeout_ms_val <= 0.0
    {
        return None;
    }

    let api_key = if api_key_raw.is_empty() {
        None
    } else {
        Some(api_key_raw)
    };

    Some(SandboxRemoteExecutionSpec {
        transport: "sandbox".to_string(),
        provider,
        sandbox_id,
        remote_cwd,
        timeout_ms: timeout_ms_val as u64,
        api_key,
    })
}

// =============================================================================
// Session identity.
// =============================================================================

/// Reduce a `SandboxRemoteExecutionSpec` to its session-identity
/// 4-tuple (transport / provider / sandboxId / remoteCwd). Returns
/// `None` for a `None` spec. Mirrors Node
/// `buildSandboxExecutionSessionIdentity`.
#[must_use]
pub fn build_sandbox_execution_session_identity(
    spec: Option<&SandboxRemoteExecutionSpec>,
) -> Option<SandboxExecutionSessionIdentity> {
    spec.map(|s| SandboxExecutionSessionIdentity {
        transport: s.transport.clone(),
        provider: s.provider.clone(),
        sandbox_id: s.sandbox_id.clone(),
        remote_cwd: s.remote_cwd.clone(),
    })
}

/// The 4-tuple that decides whether two specs describe the same
/// sandbox session. Mirrors the inner object returned by the Node
/// helper.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxExecutionSessionIdentity {
    pub transport: String,
    pub provider: String,
    pub sandbox_id: String,
    pub remote_cwd: String,
}

/// Compare a previously-saved session row against a current spec.
/// Returns `true` only when the saved transport/provider/sandbox_id/
/// remote_cwd all equal the current spec's session-identity fields.
/// Mirrors Node `sandboxExecutionSessionMatches`.
#[must_use]
pub fn sandbox_execution_session_matches(
    saved: &serde_json::Value,
    current: Option<&SandboxRemoteExecutionSpec>,
) -> bool {
    let Some(current_identity) = build_sandbox_execution_session_identity(current) else {
        return false;
    };
    let parsed = as_object(saved);
    let transport = parsed.get("transport").map(as_string).unwrap_or_default();
    let provider = parsed.get("provider").map(as_string).unwrap_or_default();
    let sandbox_id = parsed.get("sandboxId").map(as_string).unwrap_or_default();
    let remote_cwd = parsed.get("remoteCwd").map(as_string).unwrap_or_default();
    transport == current_identity.transport
        && provider == current_identity.provider
        && sandbox_id == current_identity.sandbox_id
        && remote_cwd == current_identity.remote_cwd
}

// =============================================================================
// Confinement guard.
// =============================================================================

/// Confinement roots used by the sync guard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncConfinementRoots {
    pub source_roots: Vec<String>,
    pub target_roots: Vec<String>,
}

/// Host-side complete-mediation guard for native sync operations.
/// Each mapping's `source_path` must lie under one of
/// `roots.source_roots`; each `target_path` must lie under one of
/// `roots.target_roots`. Absolute escapes and `..` traversal are
/// rejected fail-closed. Mirrors Node
/// `assertSyncOperationsConfined`.
///
/// # Errors
///
/// Returns `Err` with a descriptive label if any path is not a
/// confined absolute POSIX path or escapes its root.
pub fn assert_sync_operations_confined(
    operations: &[SandboxSyncOperation],
    roots: &SyncConfinementRoots,
) -> Result<(), String> {
    fn confine(
        candidate: &str,
        allowed: &[String],
        label: &str,
    ) -> Result<(), String> {
        let normalized = posix_normalize(candidate);
        if !posix_is_absolute(&normalized)
            || normalized == ".."
            || normalized.contains("/../")
            || normalized.ends_with("/..")
        {
            return Err(format!(
                "sync operation {label} path is not a confined absolute path: {candidate}"
            ));
        }
        let within = allowed.iter().any(|root| {
            let normalized_root = posix_normalize(root);
            let with_prefix = normalized_root.ends_with('/');
            let prefix = if with_prefix {
                normalized_root.clone()
            } else {
                format!("{normalized_root}/")
            };
            normalized == normalized_root || normalized.starts_with(&prefix)
        });
        if !within {
            return Err(format!(
                "sync operation {label} path escapes its confinement root: {candidate}"
            ));
        }
        Ok(())
    }
    for operation in operations {
        for mapping in &operation.files {
            confine(&mapping.source_path, &roots.source_roots, "source")?;
            confine(&mapping.target_path, &roots.target_roots, "target")?;
        }
    }
    Ok(())
}

// =============================================================================
// Shell-command builders.
// =============================================================================

/// Build the default extract-runtime-asset command. Mirrors Node
/// `buildDefaultExtractRuntimeAssetCommand`.
#[must_use]
pub fn build_default_extract_runtime_asset_command(
    remote_asset_dir: &str,
    remote_asset_tar: &str,
) -> String {
    let dir = shell_quote(remote_asset_dir);
    let tar = shell_quote(remote_asset_tar);
    format!(
        "rm -rf {dir} && mkdir -p {dir} && tar -xf {tar} -C {dir} && rm -f {tar}"
    )
}

/// Build a workspace-tar extract command. Mirrors Node
/// `buildWorkspaceTarExtractCommand`. When `wipe_except_names` is
/// `Some`, the target's direct children except the preserved names
/// are destroyed before extraction (destroy-then-replace). When
/// `None`, the tarball is overlaid on top of the existing tree.
#[must_use]
pub fn build_workspace_tar_extract_command(
    workspace_remote_dir: &str,
    remote_tar: &str,
    wipe_except_names: Option<&[String]>,
) -> String {
    let dir = shell_quote(workspace_remote_dir);
    let tar = shell_quote(remote_tar);
    let wipe = wipe_except_names
        .map(|entries| {
            format!(
                " && find {dir} -mindepth 1 -maxdepth 1 {args} -exec rm -rf -- {{}} +",
                args = preserve_find_args(entries)
            )
        })
        .unwrap_or_default();
    format!(
        "mkdir -p {dir}{wipe} && tar -xf {tar} -C {dir} && rm -f {tar}"
    )
}

/// Build a remove-deleted-paths command. Mirrors Node
/// `buildRemoveDeletedPathsCommand`.
#[must_use]
pub fn build_remove_deleted_paths_command(remote_dir: &str, deleted_paths: &[String]) -> String {
    let quoted_paths = deleted_paths
        .iter()
        .map(|entry| shell_quote(entry))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "cd {dir} && rm -rf -- {paths}",
        dir = shell_quote(remote_dir),
        paths = quoted_paths
    )
}

/// Build the remote tarball-from-directory command. Mirrors Node
/// `createRemoteTarballFromDirectoryCommand`.
#[must_use]
pub fn create_remote_tarball_from_directory_command(
    remote_dir: &str,
    archive_path: &str,
    exclude: Option<&[String]>,
) -> String {
    let archive_dir = match archive_path.rfind('/') {
        Some(i) => &archive_path[..i],
        None => ".",
    };
    let archive_path_q = shell_quote(archive_path);
    let exclude_flags = tar_exclude_flags(exclude);
    let archive_dir_q = shell_quote(archive_dir);
    let tar_payload = if exclude_flags.is_empty() {
        format!("tar -cf {archive_path_q} -- \"$@\"; fi")
    } else {
        format!("tar -cf {archive_path_q} {exclude_flags} -- \"$@\"; fi")
    };
    let tar_branch = format!(
        "if [ \"$#\" -eq 0 ]; then dd if=/dev/zero of={archive_path_q} bs=1024 count=1; else {tar_payload}"
    );
    [
        format!("mkdir -p {archive_dir_q}"),
        format!("cd {}", shell_quote(remote_dir)),
        "set -- *".to_string(),
        r#"if [ "$#" -eq 1 ] && [ "$1" = "*" ] && [ ! -e "$1" ] && [ ! -L "$1" ]; then set --; fi"#.to_string(),
        r#"for entry in .[!.]* ..?*; do [ -e "$entry" ] || [ -L "$entry" ] || continue; set -- "$@" "$entry"; done"#.to_string(),
        tar_branch,
    ]
    .join(" && ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_mapping(source: &str, target: &str) -> SandboxSyncFileMapping {
        SandboxSyncFileMapping {
            source_path: source.to_string(),
            target_path: target.to_string(),
            kind: "file".to_string(),
            mode: Some(0o644),
            exclude: None,
            follow_symlinks: None,
            access: None,
            writable_path: None,
        }
    }

    fn test_op(id: &str, files: Vec<SandboxSyncFileMapping>) -> SandboxSyncOperation {
        SandboxSyncOperation {
            operation_id: id.to_string(),
            files,
            post_upload_commands: None,
        }
    }

    // ---- constants / dedup ----

    #[test]
    fn heavy_dir_excludes_expand_correctly() {
        let xs = sandbox_workspace_heavy_dir_excludes();
        assert!(xs.contains(&"node_modules".to_string()));
        assert!(xs.contains(&"node_modules/*".to_string()));
        assert!(xs.contains(&"*/node_modules".to_string()));
        assert!(xs.contains(&"*/node_modules/*".to_string()));
        assert_eq!(xs.len(), SANDBOX_WORKSPACE_HEAVY_DIR_NAMES.len() * 4);
    }

    // ---- helpers ----

    #[test]
    fn as_object_returns_map_for_object_value() {
        let v = json!({"a": 1, "b": "two"});
        let m = as_object(&v);
        assert_eq!(m.len(), 2);
        assert_eq!(m.get("a").and_then(|x| x.as_i64()), Some(1));
    }

    #[test]
    fn as_object_returns_empty_for_non_object() {
        assert!(as_object(&json!(null)).is_empty());
        assert!(as_object(&json!("string")).is_empty());
        assert!(as_object(&json!(42)).is_empty());
        assert!(as_object(&json!([1, 2])).is_empty());
    }

    #[test]
    fn as_string_returns_string_when_present() {
        assert_eq!(as_string(&json!("hello")), "hello");
        assert_eq!(as_string(&json!(null)), "");
        assert_eq!(as_string(&json!(42)), "");
    }

    #[test]
    fn as_number_returns_number_when_present() {
        assert_eq!(as_number(&json!(3.5)), 3.5);
        assert!(as_number(&json!("x")).is_nan());
    }

    #[test]
    fn shell_quote_handles_plain_and_special() {
        assert_eq!(shell_quote("plain"), "'plain'");
        let q = shell_quote("with'quote");
        assert!(q.starts_with("'"));
        assert!(q.ends_with("'"));
        assert!(q.contains("'\"'\"'"));
        let cmd = shell_quote("/tmp/with space/dir");
        assert_eq!(cmd, "'/tmp/with space/dir'");
    }

    #[test]
    fn posix_normalize_handles_basic_segments() {
        assert_eq!(posix_normalize("/a/b/c"), "/a/b/c");
        assert_eq!(posix_normalize("/a/./b"), "/a/b");
        assert_eq!(posix_normalize("/a/b/../c"), "/a/c");
        assert_eq!(posix_normalize("a/b"), "a/b");
        assert_eq!(posix_normalize(""), ".");
    }

    #[test]
    fn posix_is_absolute_distinguishes_root_vs_relative() {
        assert!(posix_is_absolute("/"));
        assert!(posix_is_absolute("/a"));
        assert!(!posix_is_absolute("a"));
        assert!(!posix_is_absolute("./a"));
    }

    #[test]
    fn build_unique_staging_path_appends_uuid_suffix() {
        let p = build_unique_staging_path("/workspace/staging", "tar");
        assert!(p.starts_with("/workspace/stagingtar."));
        let q = build_unique_staging_path("/workspace/staging", "tar");
        assert_ne!(p, q);
    }

    #[test]
    fn merge_excludes_dedupes_in_order() {
        let a = vec!["node_modules".to_string()];
        let b = vec!["target".to_string(), "node_modules".to_string()];
        let merged = merge_excludes(&[Some(&a), Some(&b)]);
        assert_eq!(merged, vec!["node_modules".to_string(), "target".to_string()]);
    }

    #[test]
    fn merge_excludes_handles_none_groups() {
        let a = vec!["x".to_string()];
        let merged = merge_excludes(&[None, Some(&a), None]);
        assert_eq!(merged, vec!["x".to_string()]);
    }

    #[test]
    fn preserve_find_args_quotes_each_entry() {
        let args = preserve_find_args(&["foo".into(), "bar baz".into()]);
        assert!(args.contains("! -name 'foo'"));
        assert!(args.contains("! -name 'bar baz'"));
    }

    #[test]
    fn tar_exclude_flags_prepends_resource_fork_pattern() {
        let flags = tar_exclude_flags(Some(&["node_modules".into()]));
        assert!(flags.starts_with("--exclude '._*'"));
        assert!(flags.contains("--exclude 'node_modules'"));
    }

    #[test]
    fn tar_exclude_flags_without_exclude_only_has_resource_fork() {
        let flags = tar_exclude_flags(None);
        assert_eq!(flags, "--exclude '._*'");
    }

    // ---- spec parser ----

    #[test]
    fn parse_spec_accepts_valid_payload() {
        let v = json!({
            "transport": "sandbox",
            "provider": "e2b",
            "sandboxId": "sbx-1",
            "remoteCwd": "/workspace",
            "timeoutMs": 30000,
            "apiKey": "secret",
        });
        let s = parse_sandbox_remote_execution_spec(&v).expect("must parse");
        assert_eq!(s.provider, "e2b");
        assert_eq!(s.sandbox_id, "sbx-1");
        assert_eq!(s.remote_cwd, "/workspace");
        assert_eq!(s.timeout_ms, 30000);
        assert_eq!(s.api_key, Some("secret".to_string()));
    }

    #[test]
    fn parse_spec_rejects_wrong_transport() {
        let v = json!({
            "transport": "ssh",
            "provider": "e2b",
            "sandboxId": "sbx-1",
            "remoteCwd": "/workspace",
            "timeoutMs": 30000,
        });
        assert!(parse_sandbox_remote_execution_spec(&v).is_none());
    }

    #[test]
    fn parse_spec_rejects_missing_required_fields() {
        let v = json!({"transport": "sandbox"});
        assert!(parse_sandbox_remote_execution_spec(&v).is_none());
    }

    #[test]
    fn parse_spec_rejects_zero_timeout() {
        let v = json!({
            "transport": "sandbox",
            "provider": "e2b",
            "sandboxId": "sbx-1",
            "remoteCwd": "/workspace",
            "timeoutMs": 0,
        });
        assert!(parse_sandbox_remote_execution_spec(&v).is_none());
    }

    #[test]
    fn parse_spec_omits_api_key_when_empty() {
        let v = json!({
            "transport": "sandbox",
            "provider": "e2b",
            "sandboxId": "sbx-1",
            "remoteCwd": "/workspace",
            "timeoutMs": 1000,
            "apiKey": "",
        });
        let s = parse_sandbox_remote_execution_spec(&v).expect("must parse");
        assert!(s.api_key.is_none());
    }

    // ---- session identity ----

    fn spec_fixture() -> SandboxRemoteExecutionSpec {
        SandboxRemoteExecutionSpec {
            transport: "sandbox".to_string(),
            provider: "e2b".to_string(),
            sandbox_id: "sbx-1".to_string(),
            remote_cwd: "/workspace".to_string(),
            timeout_ms: 30000,
            api_key: Some("secret".to_string()),
        }
    }

    #[test]
    fn session_identity_returns_none_for_none_spec() {
        assert!(build_sandbox_execution_session_identity(None).is_none());
    }

    #[test]
    fn session_identity_drops_timeout_and_api_key() {
        let s = spec_fixture();
        let id = build_sandbox_execution_session_identity(Some(&s)).unwrap();
        assert_eq!(id.transport, "sandbox");
        assert_eq!(id.provider, "e2b");
        assert_eq!(id.sandbox_id, "sbx-1");
        assert_eq!(id.remote_cwd, "/workspace");
    }

    #[test]
    fn session_matches_when_saved_fields_match_current() {
        let s = spec_fixture();
        let saved = json!({
            "transport": "sandbox",
            "provider": "e2b",
            "sandboxId": "sbx-1",
            "remoteCwd": "/workspace",
            "extraIgnored": "value",
        });
        assert!(sandbox_execution_session_matches(&saved, Some(&s)));
    }

    #[test]
    fn session_mismatch_on_any_field() {
        let s = spec_fixture();
        let saved = json!({
            "transport": "sandbox",
            "provider": "e2b",
            "sandboxId": "sbx-2",
            "remoteCwd": "/workspace",
        });
        assert!(!sandbox_execution_session_matches(&saved, Some(&s)));
    }

    #[test]
    fn session_match_returns_false_for_none_current_spec() {
        let saved = json!({
            "transport": "sandbox",
            "provider": "e2b",
            "sandboxId": "sbx-1",
            "remoteCwd": "/workspace",
        });
        assert!(!sandbox_execution_session_matches(&saved, None));
    }

    // ---- confinement ----

    #[test]
    fn confinement_passes_when_all_paths_inside_roots() {
        let ops = vec![test_op(
            "op-1",
            vec![test_mapping("/host/a.txt", "/sandbox/a.txt")],
        )];
        let roots = SyncConfinementRoots {
            source_roots: vec!["/host".to_string()],
            target_roots: vec!["/sandbox".to_string()],
        };
        assert!(assert_sync_operations_confined(&ops, &roots).is_ok());
    }

    #[test]
    fn confinement_rejects_dotdot_target_normalizes_then_escapes() {
        // `/sandbox/../escape` normalizes to `/escape` which no
        // longer contains `/../` segments. The "not a confined"
        // branch only fires when *normalized* still has `/../`,
        // ends with `/..`, or equals `..`. Otherwise the post-
        // normalize `escapes` branch fires (mirrors Node).
        let ops = vec![test_op(
            "op-1",
            vec![test_mapping("/host/a.txt", "/sandbox/../escape")],
        )];
        let roots = SyncConfinementRoots {
            source_roots: vec!["/host".to_string()],
            target_roots: vec!["/sandbox".to_string()],
        };
        let err = assert_sync_operations_confined(&ops, &roots).unwrap_err();
        assert!(err.contains("escapes its confinement root"), "got: {err}");
    }

    #[test]
    fn confinement_rejects_relative_target_with_dotdot() {
        // `../escape` is not absolute after normalize, so the
        // early "not a confined absolute path" branch fires
        // regardless of whether the segments escape the root.
        let ops = vec![test_op(
            "op-1",
            vec![test_mapping("/host/a.txt", "../escape")],
        )];
        let roots = SyncConfinementRoots {
            source_roots: vec!["/host".to_string()],
            target_roots: vec!["/sandbox".to_string()],
        };
        let err = assert_sync_operations_confined(&ops, &roots).unwrap_err();
        assert!(err.contains("not a confined absolute path"), "got: {err}");
    }

    #[test]
    fn confinement_rejects_target_escaping_target_root() {
        let ops = vec![test_op(
            "op-1",
            vec![test_mapping("/host/a.txt", "/other/a.txt")],
        )];
        let roots = SyncConfinementRoots {
            source_roots: vec!["/host".to_string()],
            target_roots: vec!["/sandbox".to_string()],
        };
        let err = assert_sync_operations_confined(&ops, &roots).unwrap_err();
        assert!(err.contains("escapes its confinement root"), "got: {err}");
    }

    #[test]
    fn confinement_rejects_source_escaping_source_root() {
        let ops = vec![test_op(
            "op-1",
            vec![test_mapping("/elsewhere/a.txt", "/sandbox/a.txt")],
        )];
        let roots = SyncConfinementRoots {
            source_roots: vec!["/host".to_string()],
            target_roots: vec!["/sandbox".to_string()],
        };
        let err = assert_sync_operations_confined(&ops, &roots).unwrap_err();
        assert!(err.contains("escapes its confinement root"), "got: {err}");
    }

    #[test]
    fn confinement_rejects_relative_target() {
        let ops = vec![test_op(
            "op-1",
            vec![test_mapping("/host/a.txt", "relative/a.txt")],
        )];
        let roots = SyncConfinementRoots {
            source_roots: vec!["/host".to_string()],
            target_roots: vec!["/sandbox".to_string()],
        };
        let err = assert_sync_operations_confined(&ops, &roots).unwrap_err();
        assert!(err.contains("not a confined absolute path"), "got: {err}");
    }

    #[test]
    fn confinement_passes_when_root_is_exact_prefix() {
        let ops = vec![test_op(
            "op-1",
            vec![test_mapping("/host/sub/a.txt", "/sandbox/sub/a.txt")],
        )];
        let roots = SyncConfinementRoots {
            source_roots: vec!["/host".to_string()],
            target_roots: vec!["/sandbox".to_string()],
        };
        assert!(assert_sync_operations_confined(&ops, &roots).is_ok());
    }

    // ---- builders ----

    #[test]
    fn build_default_extract_runtime_asset_command_emits_full_sequence() {
        let cmd = build_default_extract_runtime_asset_command("/sandbox/asset", "/sandbox/asset.tar");
        assert!(cmd.contains("rm -rf '/sandbox/asset'"));
        assert!(cmd.contains("mkdir -p '/sandbox/asset'"));
        assert!(cmd.contains("tar -xf '/sandbox/asset.tar'"));
        assert!(cmd.contains("rm -f '/sandbox/asset.tar'"));
    }

    #[test]
    fn build_workspace_tar_extract_command_overlay_form() {
        let cmd = build_workspace_tar_extract_command("/sandbox/ws", "/sandbox/ws.tar", None);
        assert!(cmd.contains("mkdir -p '/sandbox/ws'"));
        assert!(cmd.contains("tar -xf '/sandbox/ws.tar' -C '/sandbox/ws'"));
        assert!(!cmd.contains("find"));
    }

    #[test]
    fn build_workspace_tar_extract_command_destroy_then_replace_form() {
        let cmd = build_workspace_tar_extract_command(
            "/sandbox/ws",
            "/sandbox/ws.tar",
            Some(&["repo".to_string()]),
        );
        assert!(cmd.contains("find '/sandbox/ws' -mindepth 1 -maxdepth 1"));
        assert!(cmd.contains("! -name 'repo'"));
        assert!(cmd.contains("rm -rf -- {} +"));
    }

    #[test]
    fn build_remove_deleted_paths_command_quotes_each_path() {
        let cmd = build_remove_deleted_paths_command(
            "/sandbox/ws",
            &["a.txt".to_string(), "with space/b.txt".to_string()],
        );
        assert!(cmd.contains("cd '/sandbox/ws'"));
        assert!(cmd.contains("'a.txt'"));
        assert!(cmd.contains("'with space/b.txt'"));
        assert!(cmd.contains("rm -rf --"));
    }

    #[test]
    fn create_remote_tarball_command_includes_archive_dir_and_tar() {
        let cmd = create_remote_tarball_from_directory_command(
            "/sandbox/ws",
            "/sandbox/archives/sync.tar",
            None,
        );
        assert!(cmd.contains("mkdir -p '/sandbox/archives'"));
        assert!(cmd.contains("cd '/sandbox/ws'"));
        assert!(cmd.contains("tar -cf '/sandbox/archives/sync.tar'"));
    }

    #[test]
    fn create_remote_tarball_command_with_excludes_includes_flags() {
        let cmd = create_remote_tarball_from_directory_command(
            "/sandbox/ws",
            "/sandbox/archives/sync.tar",
            Some(&["node_modules".into()]),
        );
        assert!(cmd.contains("--exclude '._*'"));
        assert!(cmd.contains("--exclude 'node_modules'"));
    }

    // ---- type defaults ----

    #[test]
    fn access_or_default_is_ro_when_absent() {
        let m = SandboxSyncFileMapping {
            source_path: "a".into(),
            target_path: "b".into(),
            kind: "file".into(),
            mode: None,
            exclude: None,
            follow_symlinks: None,
            access: None,
            writable_path: None,
        };
        assert_eq!(m.access_or_default(), "ro");
    }

    #[test]
    fn post_upload_commands_or_empty_returns_slice_for_none() {
        let op = SandboxSyncOperation {
            operation_id: "op".into(),
            files: vec![],
            post_upload_commands: None,
        };
        assert!(op.post_upload_commands_or_empty().is_empty());
    }
}
