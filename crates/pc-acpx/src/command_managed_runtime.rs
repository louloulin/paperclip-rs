//! `pc-acpx::command_managed_runtime` - port of `command-managed-runtime.ts`
//! from Node `paperclip/packages/adapter-utils/src/`.
//!
//! Pure helpers for the command-managed runtime path. The async
//! `createCommandManagedRuntimeClient` and `prepareCommandManagedRuntime`
//! are deferred (they require a `runner` runtime + sandbox plumbing).
//! This module ports the type definitions, the sync-command builders,
//! the confinement guard, and the staging-path generator.

use serde::{Deserialize, Serialize};

/// A command to execute after a file has been uploaded into the sandbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostUploadCommand {
    /// Optional working directory. Must be absolute POSIX path without
    /// `..` segments and confined to the operation's target root.
    pub cwd: Option<String>,
    /// The shell command to execute.
    pub command: String,
}

/// A file mapping for a sandbox sync operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxFileMapping {
    /// Host-side source path (relative to the staging root).
    pub source_path: String,
    /// Sandbox-side target path (absolute POSIX).
    pub target_path: String,
    /// File mode (POSIX permissions, e.g. 0o644).
    pub mode: u32,
}

/// A single sandbox sync operation: a set of file mappings and the
/// post-upload commands to run after the files land.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxSyncOperation {
    pub files: Vec<SandboxFileMapping>,
    #[serde(default)]
    pub post_upload_commands: Vec<PostUploadCommand>,
}

/// POSIX shell single-quote a string. Mirrors the Node helper.
fn shell_quote(value: &str) -> String {
    let escaped = value.replace('\'', r#"'"'"'"#);
    format!("'{escaped}'")
}

/// Build the shell command that extracts a tarball into a target
/// directory. Mirrors Node `buildSyncInExtractDirectoryCommand`.
#[must_use]
pub fn build_sync_in_extract_directory_command(
    remote_tar_path: &str,
    target_dir: &str,
) -> String {
    format!(
        "rm -rf {target} && mkdir -p {target} && tar -xf {tar} -C {target} && rm -f {tar}",
        tar = shell_quote(remote_tar_path),
        target = shell_quote(target_dir),
    )
}

/// Build the `chmod` shell command that applies a POSIX mode to a
/// placed file. Mirrors Node `buildSyncInChmodCommand`.
#[must_use]
pub fn build_sync_in_chmod_command(mode: u32, target_path: &str) -> String {
    format!(
        "chmod {mode:o} {path}",
        mode = (mode & 0o7777),
        path = shell_quote(target_path),
    )
}

/// Build the `mv -f` shell command that renames a source to a target.
/// Mirrors Node `buildSyncInRenameCommand`.
#[must_use]
pub fn build_sync_in_rename_command(source_path: &str, target_path: &str) -> String {
    format!(
        "mv -f {src} {dst}",
        src = shell_quote(source_path),
        dst = shell_quote(target_path),
    )
}

/// Build a unique staging path by appending a suffix and a UUID.
/// Mirrors Node `buildUniqueStagingPath`.
#[must_use]
pub fn build_unique_staging_path(target_path: &str, suffix: &str) -> String {
    format!("{target_path}{suffix}.{}", uuid::Uuid::new_v4())
}

/// Host-side confinement guard for post-upload command `cwd` values.
/// Mirrors Node `assertPostUploadCommandsConfined`. Runs BEFORE any
/// handoff — native delegation OR the generic fallback — so an
/// out-of-root `cwd` is rejected fail-closed before a provider ever
/// sees it.
///
/// `cwd` (when present) MUST be:
/// 1. An absolute POSIX path (no `..` segments).
/// 2. Confined to (equal to or under) one of the operation's own
///    file-mapping target paths.
///
/// Commands with no `cwd` are unconstrained here and default to the
/// runtime's stable command cwd at exec time.
///
/// # Errors
///
/// Returns `Err` with a descriptive message if any `cwd` violates
/// confinement.
pub fn assert_post_upload_commands_confined(
    operations: &[SandboxSyncOperation],
) -> Result<(), String> {
    for operation in operations {
        if operation.post_upload_commands.is_empty() {
            continue;
        }
        let target_roots: Vec<String> = operation
            .files
            .iter()
            .map(|m| posix_normalize(&m.target_path))
            .collect();
        for command in &operation.post_upload_commands {
            let Some(raw) = &command.cwd else {
                continue;
            };
            if !posix_is_absolute(raw) || raw.split('/').any(|s| s == "..") {
                return Err(format!(
                    "post-upload command cwd is not a confined absolute POSIX path: {raw}"
                ));
            }
            let normalized = posix_normalize(raw);
            let within = target_roots
                .iter()
                .any(|root| normalized == *root || normalized.starts_with(&format!("{root}/")));
            if !within {
                return Err(format!(
                    "post-upload command cwd escapes the operation's target root: {raw}"
                ));
            }
        }
    }
    Ok(())
}

/// Check whether a POSIX path is absolute (starts with `/`).
fn posix_is_absolute(path: &str) -> bool {
    path.starts_with('/')
}

/// Normalize a POSIX path by collapsing `.` and `..` segments without
/// filesystem access. Mirrors Node `path.posix.normalize`.
fn posix_normalize(path: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping(target: &str) -> SandboxFileMapping {
        SandboxFileMapping {
            source_path: "host/path".to_string(),
            target_path: target.to_string(),
            mode: 0o644,
        }
    }

    fn op_with_cwd(cwd: Option<&str>) -> SandboxSyncOperation {
        SandboxSyncOperation {
            files: vec![mapping("/workspace/target")],
            post_upload_commands: vec![PostUploadCommand {
                cwd: cwd.map(String::from),
                command: "echo hi".to_string(),
            }],
        }
    }

    #[test]
    fn posix_normalize_handles_basic_cases() {
        assert_eq!(posix_normalize("/a/b/c"), "/a/b/c");
        assert_eq!(posix_normalize("/a/./b"), "/a/b");
        assert_eq!(posix_normalize("/a/b/../c"), "/a/c");
        assert_eq!(posix_normalize("/a/../../b"), "/b");
        assert_eq!(posix_normalize("a/b"), "a/b");
        assert_eq!(posix_normalize("./a"), "a");
    }

    #[test]
    fn posix_is_absolute_recognizes_root_paths() {
        assert!(posix_is_absolute("/"));
        assert!(posix_is_absolute("/workspace"));
        assert!(!posix_is_absolute("workspace"));
        assert!(!posix_is_absolute("./workspace"));
    }

    #[test]
    fn assert_confined_passes_for_absent_cwd() {
        let ops = vec![op_with_cwd(None)];
        assert!(assert_post_upload_commands_confined(&ops).is_ok());
    }

    #[test]
    fn assert_confined_passes_for_cwd_within_target() {
        let ops = vec![op_with_cwd(Some("/workspace/target/sub"))];
        assert!(assert_post_upload_commands_confined(&ops).is_ok());
    }

    #[test]
    fn assert_confined_passes_for_cwd_equal_to_target() {
        let ops = vec![op_with_cwd(Some("/workspace/target"))];
        assert!(assert_post_upload_commands_confined(&ops).is_ok());
    }

    #[test]
    fn assert_confined_rejects_relative_cwd() {
        let ops = vec![op_with_cwd(Some("relative/path"))];
        assert!(assert_post_upload_commands_confined(&ops).is_err());
    }

    #[test]
    fn assert_confined_rejects_dotdot_cwd() {
        let ops = vec![op_with_cwd(Some("/workspace/../escape"))];
        assert!(assert_post_upload_commands_confined(&ops).is_err());
    }

    #[test]
    fn assert_confined_rejects_outside_root_cwd() {
        let ops = vec![op_with_cwd(Some("/elsewhere"))];
        let err = assert_post_upload_commands_confined(&ops).unwrap_err();
        assert!(err.contains("escapes the operation's target root"));
    }

    #[test]
    fn assert_confined_skips_operations_with_no_commands() {
        let ops = vec![SandboxSyncOperation {
            files: vec![mapping("/workspace/target")],
            post_upload_commands: vec![],
        }];
        assert!(assert_post_upload_commands_confined(&ops).is_ok());
    }

    #[test]
    fn assert_confined_rejects_dotdot_segment_in_middle() {
        // A `..` segment anywhere in the cwd is rejected early with the
        // "not a confined absolute POSIX path" message (matching the Node
        // guard, which short-circuits on `..` before normalize-then-check).
        let ops = vec![op_with_cwd(Some("/workspace/target/../escape"))];
        let err = assert_post_upload_commands_confined(&ops).unwrap_err();
        assert!(
            err.contains("not a confined absolute POSIX path"),
            "expected dotdot rejection, got: {err}"
        );
    }

    #[test]
    fn assert_confined_catches_normalized_escape_without_dotdot() {
        // A path with NO `..` segments that still normalizes outside the
        // target root (because the root has a trailing segment the cwd
        // is not contained in) is caught by the post-normalize "escapes"
        // branch. Here target root is /workspace/target and cwd is
        // /workspace/other — no `..` in cwd, but it does not lie under
        // /workspace/target.
        let op = SandboxSyncOperation {
            files: vec![mapping("/workspace/target")],
            post_upload_commands: vec![PostUploadCommand {
                cwd: Some("/workspace/other".to_string()),
                command: "ls".to_string(),
            }],
        };
        let err = assert_post_upload_commands_confined(&[op]).unwrap_err();
        assert!(
            err.contains("escapes the operation"),
            "expected escape rejection, got: {err}"
        );
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("with'quote"), "'with'\"'\"'quote'");
    }

    #[test]
    fn build_sync_in_extract_directory_command_uses_quoted_paths() {
        let cmd = build_sync_in_extract_directory_command("/tmp/x.tar", "/workspace/y");
        assert!(cmd.contains("rm -rf"));
        assert!(cmd.contains("mkdir -p"));
        assert!(cmd.contains("tar -xf"));
        assert!(cmd.contains("'/tmp/x.tar'"));
        assert!(cmd.contains("'/workspace/y'"));
    }

    #[test]
    fn build_sync_in_chmod_command_uses_octal_mode() {
        let cmd = build_sync_in_chmod_command(0o755, "/workspace/file");
        assert!(cmd.starts_with("chmod 755 "));
        assert!(cmd.contains("'/workspace/file'"));
    }

    #[test]
    fn build_sync_in_chmod_command_masks_mode() {
        // 0o100755 (with file type bit) should be masked to 0o755
        let cmd = build_sync_in_chmod_command(0o100755, "/workspace/file");
        assert!(cmd.contains("chmod 755 "));
    }

    #[test]
    fn build_sync_in_rename_command_uses_quoted_paths() {
        let cmd = build_sync_in_rename_command("/tmp/src", "/tmp/dst");
        assert!(cmd.starts_with("mv -f "));
        assert!(cmd.contains("'/tmp/src'"));
        assert!(cmd.contains("'/tmp/dst'"));
    }

    #[test]
    fn build_unique_staging_path_appends_uuid() {
        let p = build_unique_staging_path("/workspace/file", ".tmp");
        assert!(p.starts_with("/workspace/file.tmp."));
        // UUID v4 is 36 chars
        assert_eq!(p.len(), "/workspace/file.tmp.".len() + 36);
    }

    #[test]
    fn build_unique_staging_paths_are_unique() {
        let a = build_unique_staging_path("/p", ".s");
        let b = build_unique_staging_path("/p", ".s");
        assert_ne!(a, b);
    }
}
