//! `pc-acpx::git_workspace_sync` - port of `git-workspace-sync.ts`
//! from Node `paperclip/packages/adapter-utils/src/`.
//!
//! Pure helpers for git workspace synchronization between a local
//! workspace and a remote/sandbox runtime. The async functions that
//! actually invoke the `git` CLI are deferred to a follow-up round;
//! this module ports the constant, the pure ref-name generators, the
//! script builder, and the prerequisite-error classifier.

/// Default excludes for git archive operations. Mirrors Node
/// `GIT_ARCHIVE_EXCLUDES`.
pub const GIT_ARCHIVE_EXCLUDES: &[&str] = &[".git", ".git/*"];

/// Substrings that `git` emits when a bundle import failed because the
/// importer lacks a prerequisite commit. Mirrors Node
/// `GIT_MISSING_PREREQUISITE_MARKERS`.
pub const GIT_MISSING_PREREQUISITE_MARKERS: &[&str] = &[
    "did not send all necessary objects",
    "lacks these prerequisite commits",
    "revision walk setup failed",
];

/// Create a git ref name under which an imported (delta) bundle is
/// stored. Mirrors Node `createImportedGitRef`.
#[must_use]
pub fn create_imported_git_ref(scope: &str) -> String {
    format!("refs/paperclip/git-sync/imported/{scope}/{}", uuid::Uuid::new_v4())
}

/// Create a git ref name used by the remote to export its current
/// `HEAD`. Mirrors Node `createRemoteGitExportRef`.
#[must_use]
pub fn create_remote_git_export_ref(scope: &str) -> String {
    format!("refs/paperclip/git-sync/export/{scope}/{}", uuid::Uuid::new_v4())
}

/// True when a bundle import failed because the importer lacks a
/// prerequisite commit. Mirrors Node `isMissingGitPrerequisiteError`.
/// Such a failure is recoverable by re-exporting a full, self-contained
/// bundle from the still-live sandbox rather than discarding the run.
#[must_use]
pub fn is_missing_git_prerequisite_error(error: &(dyn std::error::Error + 'static)) -> bool {
    let message = error.to_string();
    GIT_MISSING_PREREQUISITE_MARKERS
        .iter()
        .any(|marker| message.contains(marker))
}

/// True for any error-like value that exposes a `.message` (Error) or
/// coerces to a string. Convenience wrapper that accepts
/// `Box<dyn Error>` and `String` uniformly.
#[must_use]
pub fn is_missing_git_prerequisite_error_anyhow(error_message: &str) -> bool {
    GIT_MISSING_PREREQUISITE_MARKERS
        .iter()
        .any(|marker| error_message.contains(marker))
}

/// Options for [`build_remote_git_delta_bundle_script`]. Mirrors the
/// input shape of Node `buildRemoteGitDeltaBundleScript`.
#[derive(Debug, Clone)]
pub struct RemoteGitDeltaBundleOptions {
    pub remote_dir: String,
    pub base_sha: String,
    pub export_ref: String,
    pub bundle_path: String,
    pub status_path: Option<String>,
    pub cat_bundle: bool,
    pub cleanup_bundle: bool,
    /// Skip the delta boundary entirely and always emit a full,
    /// self-contained bundle (no prerequisites).
    pub force_full_bundle: bool,
}

/// Build a shell script that creates a git bundle on the remote,
/// optionally relative to `base_sha`. Mirrors Node
/// `buildRemoteGitDeltaBundleScript`.
#[must_use]
pub fn build_remote_git_delta_bundle_script(input: &RemoteGitDeltaBundleOptions) -> String {
    let remote_dir = shell_quote(&input.remote_dir);
    let bundle_path = shell_quote(&input.bundle_path);
    let export_ref = shell_quote(&input.export_ref);
    let base_sha = shell_quote(&input.base_sha);
    let status_path = input.status_path.as_deref().map(shell_quote);

    let cleanup_parts = [
        format!("rm -f {bundle_path}"),
        status_path
            .as_ref()
            .map(|s| format!("rm -f {s}"))
            .unwrap_or_default(),
        format!(
            "git -C {remote_dir} update-ref -d {export_ref} >/dev/null 2>&1 || true"
        ),
    ];

    let mut lines: Vec<String> = Vec::new();
    lines.push("set -e".to_string());
    if input.cleanup_bundle {
        lines.push(format!(
            "cleanup() {{ {}; }}",
            cleanup_parts.join("; ")
        ));
        lines.push("trap cleanup EXIT".to_string());
    }
    let parent_dir = std::path::Path::new(&input.bundle_path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    lines.push(format!("mkdir -p {}", shell_quote(&parent_dir)));
    lines.push(format!("rm -f {bundle_path}"));

    if input.force_full_bundle {
        lines.push("bundle_base=\"\"".to_string());
    } else {
        lines.push(format!(
            "if git -C {remote_dir} cat-file -e {base_sha}^{{commit}} 2>/dev/null; then"
        ));
        lines.push(format!(
            "  bundle_base=$(git -C {remote_dir} merge-base {base_sha} HEAD 2>/dev/null || true)"
        ));
        lines.push("else".to_string());
        lines.push("  bundle_base=\"\"".to_string());
        lines.push("fi".to_string());
    }

    lines.push("if [ -n \"$bundle_base\" ]; then".to_string());
    lines.push(format!(
        "  commit_count=$(git -C {remote_dir} rev-list --count HEAD --not \"$bundle_base\")"
    ));
    lines.push("else".to_string());
    lines.push(format!(
        "  commit_count=$(git -C {remote_dir} rev-list --count HEAD)"
    ));
    lines.push("fi".to_string());
    lines.push("if [ \"$commit_count\" -gt 0 ]; then".to_string());
    lines.push(format!(
        "  git -C {remote_dir} update-ref {export_ref} HEAD"
    ));
    lines.push("  if [ -n \"$bundle_base\" ]; then".to_string());
    lines.push(format!(
        "    git -C {remote_dir} bundle create {bundle_path} {export_ref} --not \"$bundle_base\" >/dev/null"
    ));
    lines.push("  else".to_string());
    lines.push(format!(
        "    git -C {remote_dir} bundle create {bundle_path} {export_ref} >/dev/null"
    ));
    lines.push("  fi".to_string());
    lines.push("else".to_string());
    lines.push(format!("  : > {bundle_path}"));
    lines.push("fi".to_string());

    if let Some(status) = &status_path {
        lines.push(format!(
            "if [ -z \"$(git -C {remote_dir} status --porcelain=v1 --untracked-files=normal)\" ]; then"
        ));
        lines.push(format!("  printf clean > {status}"));
        lines.push("else".to_string());
        lines.push(format!("  printf dirty > {status}"));
        lines.push("fi".to_string());
    }

    if input.cat_bundle {
        lines.push(format!("cat {bundle_path}"));
    }

    lines.join("\n")
}

/// POSIX shell single-quote a string. Mirrors the Node helper.
fn shell_quote(value: &str) -> String {
    let escaped = value.replace('\'', r#"'"'"'"#);
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_archive_excludes_constant() {
        assert_eq!(GIT_ARCHIVE_EXCLUDES, &[".git", ".git/*"]);
    }

    #[test]
    fn create_imported_git_ref_uses_scope_and_uuid() {
        let ref_name = create_imported_git_ref("remote");
        assert!(ref_name.starts_with("refs/paperclip/git-sync/imported/remote/"));
        // UUID v4 is 36 chars
        assert_eq!(ref_name.len(), "refs/paperclip/git-sync/imported/remote/".len() + 36);
    }

    #[test]
    fn create_remote_git_export_ref_uses_scope_and_uuid() {
        let ref_name = create_remote_git_export_ref("sandbox");
        assert!(ref_name.starts_with("refs/paperclip/git-sync/export/sandbox/"));
    }

    #[test]
    fn refs_are_unique_across_calls() {
        let a = create_imported_git_ref("remote");
        let b = create_imported_git_ref("remote");
        assert_ne!(a, b);
    }

    #[test]
    fn is_missing_prerequisite_detects_known_markers() {
        // Simulate errors
        struct FakeErr(String);
        impl std::fmt::Display for FakeErr {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl std::fmt::Debug for FakeErr {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl std::error::Error for FakeErr {}

        let err1 = FakeErr("fatal: remote did not send all necessary objects".to_string());
        let err2 = FakeErr("error: lacks these prerequisite commits".to_string());
        let err3 = FakeErr("fatal: revision walk setup failed".to_string());
        let err4 = FakeErr("some other error".to_string());

        assert!(is_missing_git_prerequisite_error(&err1));
        assert!(is_missing_git_prerequisite_error(&err2));
        assert!(is_missing_git_prerequisite_error(&err3));
        assert!(!is_missing_git_prerequisite_error(&err4));
    }

    #[test]
    fn is_missing_prerequisite_anyhow_wrapper() {
        assert!(is_missing_git_prerequisite_error_anyhow(
            "fatal: remote did not send all necessary objects"
        ));
        assert!(!is_missing_git_prerequisite_error_anyhow("other error"));
    }

    #[test]
    fn build_delta_bundle_script_basic() {
        let opts = RemoteGitDeltaBundleOptions {
            remote_dir: "/workspace".to_string(),
            base_sha: "abc123".to_string(),
            export_ref: "refs/paperclip/export".to_string(),
            bundle_path: "/tmp/bundle.git".to_string(),
            status_path: None,
            cat_bundle: false,
            cleanup_bundle: false,
            force_full_bundle: false,
        };
        let script = build_remote_git_delta_bundle_script(&opts);
        assert!(script.starts_with("set -e"));
        assert!(script.contains("git -C '/workspace'"));
        assert!(script.contains("'abc123'"));
        assert!(script.contains("merge-base 'abc123' HEAD"));
        assert!(script.contains("bundle create"));
    }

    #[test]
    fn build_delta_bundle_script_force_full() {
        let opts = RemoteGitDeltaBundleOptions {
            remote_dir: "/workspace".to_string(),
            base_sha: "abc".to_string(),
            export_ref: "refs/export".to_string(),
            bundle_path: "/tmp/b.git".to_string(),
            status_path: None,
            cat_bundle: false,
            cleanup_bundle: false,
            force_full_bundle: true,
        };
        let script = build_remote_git_delta_bundle_script(&opts);
        assert!(script.contains("bundle_base=\"\""));
        // No merge-base call when force_full
        assert!(!script.contains("merge-base"));
    }

    #[test]
    fn build_delta_bundle_script_with_status_path() {
        let opts = RemoteGitDeltaBundleOptions {
            remote_dir: "/ws".to_string(),
            base_sha: "abc".to_string(),
            export_ref: "refs/export".to_string(),
            bundle_path: "/tmp/b.git".to_string(),
            status_path: Some("/tmp/status.txt".to_string()),
            cat_bundle: true,
            cleanup_bundle: true,
            force_full_bundle: false,
        };
        let script = build_remote_git_delta_bundle_script(&opts);
        assert!(script.contains("trap cleanup EXIT"));
        assert!(script.contains("printf clean >"));
        assert!(script.contains("printf dirty >"));
        assert!(script.contains("cat '/tmp/b.git'"));
        assert!(script.contains("rm -f '/tmp/status.txt'"));
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("hello"), "'hello'");
        assert_eq!(shell_quote("it's"), "'it'\"'\"'s'");
    }
}
