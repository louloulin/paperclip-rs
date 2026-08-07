//! `pc-acpx` ancestor binary lookup — async I/O port of `findAncestorBin` from
//! Node `acpx-engine/execute.ts`.
//!
//! The function walks up from `start_dir` looking for `node_modules/.bin/<bin>`.
//! This matches npm/pnpm binary hoisting in packaged installs while
//! preserving monorepo dev layouts. On Windows, callers should also probe the
//! `<bin>.cmd` shim.

use std::path::{Path, PathBuf};

use crate::fs_ops::path_is_file;

// ============================================================================
// Public types
// ============================================================================

/// Operating-system hint for which binary/proxy paths to probe. Mirrors the
/// Node `process.platform === "win32"` branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Platform {
    #[default]
    Posix,
    Windows,
}

impl Platform {
    /// Best-effort detection of the current host platform. The detection is
    /// only used to choose which file extensions to probe; it can be overridden
    /// in tests.
    pub fn detect() -> Self {
        if cfg!(target_os = "windows") {
            Platform::Windows
        } else {
            Platform::Posix
        }
    }

    /// Build the candidate binary paths for `bin_name` rooted at `bin_dir`.
    pub fn candidate_paths(&self, bin_dir: &Path, bin_name: &str) -> Vec<PathBuf> {
        match self {
            Platform::Windows => vec![
                bin_dir.join(format!("{bin_name}.cmd")),
                bin_dir.join(bin_name),
            ],
            Platform::Posix => vec![bin_dir.join(bin_name)],
        }
    }
}

// ============================================================================
// Main entry
// ============================================================================

/// Walk up from `start_dir` looking for `node_modules/.bin/<bin_name>`. Returns
/// the absolute path of the first match, or `None` when the search reaches the
/// filesystem root without finding one.
pub async fn find_ancestor_bin(
    start_dir: impl AsRef<Path>,
    bin_name: &str,
    platform: Platform,
) -> Option<PathBuf> {
    let mut current: PathBuf = start_dir.as_ref().components().collect();
    // The Node implementation uses `path.resolve` — but we want a deterministic
    // absolute path even when the start directory doesn't exist. Anchoring on
    // the current working directory is the closest Rust equivalent.
    if !current.is_absolute() {
        current = match std::env::current_dir() {
            Ok(cwd) => cwd.join(&current),
            Err(_) => current,
        };
    }
    loop {
        let bin_dir = current.join("node_modules").join(".bin");
        for candidate in platform.candidate_paths(&bin_dir, bin_name) {
            if path_is_file(&candidate).await {
                return Some(candidate);
            }
        }
        let parent = current.parent()?.to_path_buf();
        if parent == current {
            return None;
        }
        current = parent;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn unique_root(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "pc-acpx-bin-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        path
    }

    #[tokio::test]
    async fn returns_none_when_no_ancestor_has_the_bin() {
        let root = unique_root("missing");
        tokio::fs::create_dir_all(&root).await.unwrap();
        let found = find_ancestor_bin(&root, "definitely-not-installed", Platform::Posix).await;
        assert!(found.is_none());
        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn finds_bin_in_start_dir_node_modules_dot_bin() {
        let root = unique_root("start");
        let bin_dir = root.join("node_modules").join(".bin");
        tokio::fs::create_dir_all(&bin_dir).await.unwrap();
        let bin_path = bin_dir.join("fake-cli");
        tokio::fs::write(&bin_path, "#!/bin/sh\necho ok\n")
            .await
            .unwrap();
        let found = find_ancestor_bin(&root, "fake-cli", Platform::Posix)
            .await
            .expect("found");
        assert_eq!(found, bin_path);
        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn finds_bin_in_ancestor_node_modules_dot_bin() {
        let root = unique_root("ancestor");
        let bin_dir = root.join("node_modules").join(".bin");
        tokio::fs::create_dir_all(&bin_dir).await.unwrap();
        let bin_path = bin_dir.join("fake-cli");
        tokio::fs::write(&bin_path, "#!/bin/sh\n").await.unwrap();
        // Probe from a descendant directory.
        let nested = root.join("a/b/c");
        tokio::fs::create_dir_all(&nested).await.unwrap();
        let found = find_ancestor_bin(&nested, "fake-cli", Platform::Posix)
            .await
            .expect("found");
        assert_eq!(found, bin_path);
        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn windows_platform_prefers_cmd_shim() {
        let root = unique_root("windows");
        let bin_dir = root.join("node_modules").join(".bin");
        tokio::fs::create_dir_all(&bin_dir).await.unwrap();
        let cmd_path = bin_dir.join("fake-cli.cmd");
        let plain_path = bin_dir.join("fake-cli");
        tokio::fs::write(&cmd_path, "@echo off\n").await.unwrap();
        tokio::fs::write(&plain_path, "fake\n").await.unwrap();
        let found = find_ancestor_bin(&root, "fake-cli", Platform::Windows)
            .await
            .expect("found");
        assert_eq!(found, cmd_path);
        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[test]
    fn candidate_paths_match_platform() {
        let posix = Platform::Posix.candidate_paths(Path::new("/d/bin"), "claude");
        assert_eq!(posix, vec![PathBuf::from("/d/bin/claude")]);
        let windows = Platform::Windows.candidate_paths(Path::new("/d/bin"), "claude");
        assert_eq!(
            windows,
            vec![
                PathBuf::from("/d/bin/claude.cmd"),
                PathBuf::from("/d/bin/claude")
            ]
        );
    }
}
