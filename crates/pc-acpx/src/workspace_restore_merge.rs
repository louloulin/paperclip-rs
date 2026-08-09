//! `pc-acpx::workspace_restore_merge` - port of `workspace-restore-merge.ts`
//! from Node `paperclip/packages/adapter-utils/src/`.
//!
//! Directory snapshotting and baseline-merge logic used by the
//! runtime-target layer to restore a workspace from a remote/sandbox
//! source without losing local-only files. The snapshot walks the
//! filesystem, hashing each file, and records directory / file /
//! symlink entries. A merge applies a source snapshot onto a target
//! directory while deleting any entry that was present in the baseline
//! but absent from the source.
//!
//! All filesystem I/O is async via `tokio::fs`. Pure snapshot logic
//! (comparison, sort) is synchronous.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncReadExt;

/// A snapshot entry: a directory, a file (with mode + sha256), or a
/// symlink (with target).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SnapshotEntry {
    Dir,
    File { mode: u32, hash: String },
    Symlink { target: String },
}

/// A directory snapshot: a set of relative-path entries + the exclude
/// pattern list used during capture.
#[derive(Debug, Clone, Default)]
pub struct DirectorySnapshot {
    pub exclude: Vec<String>,
    pub entries: BTreeMap<String, SnapshotEntry>,
}

/// Options for [`capture_directory_snapshot`].
#[derive(Debug, Clone, Default)]
pub struct CaptureOptions {
    pub exclude: Vec<String>,
}

/// Compute a sha256 hex digest of file contents. Mirrors Node `hashFile`.
pub async fn hash_file(file_path: &Path) -> std::io::Result<String> {
    let mut file = fs::File::open(file_path).await?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let n = file.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Read a single snapshot entry from the filesystem. Returns `Ok(None)`
/// if the entry does not exist. Mirrors Node `readSnapshotEntry`.
pub async fn read_snapshot_entry(
    root: &Path,
    relative: &str,
) -> std::io::Result<Option<SnapshotEntry>> {
    let full_path = root.join(relative);
    let metadata = match fs::symlink_metadata(&full_path).await {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let file_type = metadata.file_type();
    if file_type.is_dir() {
        return Ok(Some(SnapshotEntry::Dir));
    }
    if file_type.is_symlink() {
        let target = fs::read_link(&full_path).await?;
        return Ok(Some(SnapshotEntry::Symlink {
            target: target.to_string_lossy().into_owned(),
        }));
    }
    if !file_type.is_file() {
        return Ok(None);
    }
    let mode = permissions_to_mode(metadata.permissions());
    let hash = hash_file(&full_path).await?;
    Ok(Some(SnapshotEntry::File { mode, hash }))
}

/// Check whether two snapshot entries match. Mirrors Node
/// `entriesMatch`.
#[must_use]
pub fn entries_match(left: Option<&SnapshotEntry>, right: Option<&SnapshotEntry>) -> bool {
    let (Some(left), Some(right)) = (left, right) else {
        return false;
    };
    match (left, right) {
        (SnapshotEntry::Dir, SnapshotEntry::Dir) => true,
        (SnapshotEntry::Symlink { target: a }, SnapshotEntry::Symlink { target: b }) => a == b,
        (
            SnapshotEntry::File { mode: am, hash: ah },
            SnapshotEntry::File { mode: bm, hash: bh },
        ) => am == bm && ah == bh,
        _ => false,
    }
}

/// Capture a directory snapshot. Walks the tree, hashes each file, and
/// records dir/file/symlink entries. Mirrors Node `captureDirectorySnapshot`.
pub async fn capture_directory_snapshot(
    root_dir: &Path,
    options: CaptureOptions,
) -> std::io::Result<DirectorySnapshot> {
    let mut exclude = options.exclude;
    exclude.sort();
    exclude.dedup();
    let entries = walk_directory(root_dir, &exclude, "", BTreeMap::new()).await?;
    Ok(DirectorySnapshot { exclude, entries })
}

fn walk_directory<'a>(
    root: &'a Path,
    exclude: &'a [String],
    relative: &'a str,
    mut out: BTreeMap<String, SnapshotEntry>,
) -> std::pin::Pin<
    Box<
        dyn std::future::Future<Output = std::io::Result<BTreeMap<String, SnapshotEntry>>>
            + Send
            + 'a,
    >,
> {
    Box::pin(async move {
        let current = if relative.is_empty() {
            root.to_path_buf()
        } else {
            root.join(relative)
        };
        let mut entries = match fs::read_dir(&current).await {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e),
        };
        // Collect and sort by name for deterministic ordering
        let mut names: Vec<String> = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        names.sort();

        for name in names {
            let next_relative = if relative.is_empty() {
                name.clone()
            } else {
                format!("{relative}/{name}")
            };
            if pc_acpx_exclude_matches(&next_relative, exclude) {
                continue;
            }
            let full_path = root.join(&next_relative);
            let metadata = match fs::symlink_metadata(&full_path).await {
                Ok(m) => m,
                Err(_) => continue,
            };
            let file_type = metadata.file_type();
            if file_type.is_dir() {
                out.insert(next_relative.clone(), SnapshotEntry::Dir);
                out = walk_directory(root, exclude, &next_relative, out).await?;
            } else if file_type.is_symlink() {
                let target = fs::read_link(&full_path).await?;
                out.insert(
                    next_relative.clone(),
                    SnapshotEntry::Symlink {
                        target: target.to_string_lossy().into_owned(),
                    },
                );
            } else if file_type.is_file() {
                let mode = permissions_to_mode(metadata.permissions());
                let hash = hash_file(&full_path).await?;
                out.insert(next_relative, SnapshotEntry::File { mode, hash });
            }
        }
        Ok(out)
    })
}

fn pc_acpx_exclude_matches(relative: &str, exclude: &[String]) -> bool {
    exclude
        .iter()
        .any(|p| crate::exclude_patterns::exclude_pattern_matches(relative, p))
}

fn permissions_to_mode(perm: std::fs::Permissions) -> u32 {
    // Unix-only mode extraction; mask out file type bits to get just
    // permission bits (0o7777). On Windows the mode bits are not meaningful.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perm.mode() & 0o7777
    }
    #[cfg(not(unix))]
    {
        let _ = perm;
        0
    }
}

/// Check whether a directory entry matches the baseline. Mirrors Node
/// `directoryEntryMatchesBaseline`.
pub async fn directory_entry_matches_baseline(
    root_dir: &Path,
    relative: &str,
    baseline_entry: &SnapshotEntry,
) -> std::io::Result<bool> {
    let current = read_snapshot_entry(root_dir, relative).await?;
    Ok(entries_match(current.as_ref(), Some(baseline_entry)))
}

/// Merge a source directory into a target directory using a baseline
/// snapshot. Mirrors Node `mergeDirectoryWithBaseline`.
pub async fn merge_directory_with_baseline(input: MergeInput<'_>) -> std::io::Result<()> {
    let source = capture_directory_snapshot(
        input.source_dir,
        CaptureOptions {
            exclude: input.baseline.exclude.clone(),
        },
    )
    .await?;
    let target_path = input.target_dir.to_path_buf();
    with_directory_merge_lock(&target_path, || async {
        if let Some(cb) = input.before_apply {
            cb().await?;
        }
        let current = capture_directory_snapshot(
            input.target_dir,
            CaptureOptions {
                exclude: input.baseline.exclude.clone(),
            },
        )
        .await?;

        // 1. Delete leaf entries in baseline that are absent in source
        let mut deleted_leaves: Vec<(String, SnapshotEntry)> = input
            .baseline
            .entries
            .iter()
            .filter(|(_, entry)| !matches!(entry, SnapshotEntry::Dir))
            .filter(|(relative, _)| !source.entries.contains_key(relative.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        // Sort by length descending (longest paths first)
        deleted_leaves.sort_by(|(a, _), (b, _)| b.len().cmp(&a.len()));

        for (relative, baseline_entry) in deleted_leaves {
            if !entries_match(current.entries.get(&relative), Some(&baseline_entry)) {
                continue;
            }
            let _ = fs::remove_file(input.target_dir.join(&relative)).await;
            let _ = fs::remove_dir_all(input.target_dir.join(&relative)).await;
        }

        // 2. Delete dirs in baseline that are absent in source
        let mut deleted_dirs: Vec<String> = input
            .baseline
            .entries
            .iter()
            .filter(|(_, entry)| matches!(entry, SnapshotEntry::Dir))
            .filter(|(relative, _)| !source.entries.contains_key(relative.as_str()))
            .map(|(k, _)| k.clone())
            .collect();
        deleted_dirs.sort_by(|a, b| b.len().cmp(&a.len()));

        for relative in deleted_dirs {
            let _ = fs::remove_dir(input.target_dir.join(&relative)).await;
        }

        // 3. Copy changed source entries (not in baseline or differ)
        let mut changed: Vec<(String, SnapshotEntry)> = source
            .entries
            .iter()
            .filter(|(relative, entry)| {
                !entries_match(input.baseline.entries.get(relative.as_str()), Some(entry))
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        changed.sort_by(|a, b| a.0.cmp(&b.0));

        for (relative, entry) in changed {
            copy_snapshot_entry(input.source_dir, input.target_dir, &relative, &entry).await?;
        }

        if let Some(cb) = input.after_apply {
            cb().await?;
        }
        Ok(())
    })
    .await
}

/// Input for [`merge_directory_with_baseline`].
pub struct MergeInput<'a> {
    pub baseline: &'a DirectorySnapshot,
    pub source_dir: &'a Path,
    pub target_dir: &'a Path,
    pub before_apply: Option<
        Box<
            dyn FnOnce() -> std::pin::Pin<
                    Box<dyn std::future::Future<Output = std::io::Result<()>> + Send + 'a>,
                > + Send
                + 'a,
        >,
    >,
    pub after_apply: Option<
        Box<
            dyn FnOnce() -> std::pin::Pin<
                    Box<dyn std::future::Future<Output = std::io::Result<()>> + Send + 'a>,
                > + Send
                + 'a,
        >,
    >,
}

/// Acquire a directory merge lock and run `f` while holding it.
/// Mirrors Node `withDirectoryMergeLock`.
pub async fn with_directory_merge_lock<F, Fut, T>(target_dir: &Path, f: F) -> std::io::Result<T>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<T>>,
{
    let lock_dir = PathBuf::from(format!("{}.paperclip-restore.lock", target_dir.display()));
    let _guard = acquire_merge_lock(&lock_dir).await?;
    f().await
}

async fn acquire_merge_lock(lock_dir: &Path) -> std::io::Result<MergeLockGuard> {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match fs::create_dir(lock_dir).await {
            Ok(()) => {
                let owner_json = format!("{{\"pid\":{}}}\n", std::process::id());
                fs::write(lock_dir.join("owner.json"), owner_json).await?;
                return Ok(MergeLockGuard {
                    lock_dir: lock_dir.to_path_buf(),
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Stale-lock detection
                if !is_holder_alive(lock_dir).await {
                    let _ = fs::remove_dir_all(lock_dir).await;
                    continue;
                }
                if Instant::now() >= deadline {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!(
                            "Timed out waiting for workspace restore lock at {}",
                            lock_dir.display()
                        ),
                    ));
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

async fn is_holder_alive(lock_dir: &Path) -> bool {
    let raw = match fs::read_to_string(lock_dir.join("owner.json")).await {
        Ok(s) => s,
        Err(_) => return false,
    };
    let parsed: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let Some(pid) = parsed.get("pid").and_then(|v| v.as_u64()) else {
        return false;
    };
    // Signal 0 check via libc / proc
    is_pid_alive(pid)
}

/// Check if a PID is alive. Delegates to `log_redaction::is_pid_alive`
/// which wraps the platform signal-0 probe.
fn is_pid_alive(pid: u64) -> bool {
    crate::log_redaction::is_pid_alive(pid as u32)
}

struct MergeLockGuard {
    lock_dir: PathBuf,
}

impl Drop for MergeLockGuard {
    fn drop(&mut self) {
        // Best-effort cleanup; ignore errors
        let _ = std::fs::remove_dir_all(&self.lock_dir);
    }
}

async fn copy_snapshot_entry(
    source_dir: &Path,
    target_dir: &Path,
    relative: &str,
    entry: &SnapshotEntry,
) -> std::io::Result<()> {
    let source_path = source_dir.join(relative);
    let target_path = target_dir.join(relative);
    match entry {
        SnapshotEntry::Dir => {
            if let Ok(meta) = fs::metadata(&target_path).await {
                if meta.is_dir() {
                    return Ok(());
                }
                let _ = fs::remove_dir_all(&target_path).await;
            }
            fs::create_dir_all(&target_path).await?;
        }
        SnapshotEntry::Symlink { target } => {
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent).await?;
            }
            let _ = fs::remove_file(&target_path).await;
            let _ = fs::remove_dir_all(&target_path).await;
            std::os::unix::fs::symlink(target, &target_path)?;
        }
        SnapshotEntry::File { mode, .. } => {
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent).await?;
            }
            let _ = fs::remove_file(&target_path).await;
            fs::copy(&source_path, &target_path).await?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perm = fs::metadata(&target_path).await?.permissions();
                perm.set_mode(*mode);
                fs::set_permissions(&target_path, perm).await?;
            }
            let _ = mode; // suppress unused on non-unix
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_match_handles_all_combinations() {
        let dir = SnapshotEntry::Dir;
        let file = SnapshotEntry::File {
            mode: 0o644,
            hash: "abc".to_string(),
        };
        let file2 = SnapshotEntry::File {
            mode: 0o644,
            hash: "abc".to_string(),
        };
        let file3 = SnapshotEntry::File {
            mode: 0o644,
            hash: "xyz".to_string(),
        };
        let sym = SnapshotEntry::Symlink {
            target: "/a".to_string(),
        };
        let sym2 = SnapshotEntry::Symlink {
            target: "/b".to_string(),
        };

        assert!(entries_match(Some(&dir), Some(&dir)));
        assert!(entries_match(Some(&file), Some(&file2)));
        assert!(!entries_match(Some(&file), Some(&file3)));
        assert!(entries_match(Some(&sym), Some(&sym)));
        assert!(!entries_match(Some(&sym), Some(&sym2)));
        assert!(!entries_match(Some(&dir), Some(&file)));
        assert!(!entries_match(None, Some(&file)));
        assert!(!entries_match(Some(&file), None));
    }

    #[tokio::test]
    async fn hash_file_computes_sha256() {
        let dir = tempdir();
        let file_path = dir.join("test.txt");
        std::fs::write(&file_path, b"hello world").unwrap();
        let hash = hash_file(&file_path).await.unwrap();
        // sha256("hello world")
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[tokio::test]
    async fn capture_snapshot_returns_entries_for_simple_tree() {
        let dir = tempdir();
        std::fs::create_dir_all(dir.join("subdir")).unwrap();
        std::fs::write(dir.join("a.txt"), b"content a").unwrap();
        std::fs::write(dir.join("subdir/b.txt"), b"content b").unwrap();

        let snap = capture_directory_snapshot(&dir, CaptureOptions { exclude: vec![] })
            .await
            .unwrap();

        assert!(matches!(
            snap.entries.get("a.txt"),
            Some(SnapshotEntry::File { .. })
        ));
        assert!(matches!(
            snap.entries.get("subdir"),
            Some(SnapshotEntry::Dir)
        ));
        assert!(matches!(
            snap.entries.get("subdir/b.txt"),
            Some(SnapshotEntry::File { .. })
        ));
    }

    #[tokio::test]
    async fn capture_snapshot_respects_exclude() {
        let dir = tempdir();
        std::fs::create_dir_all(dir.join("node_modules")).unwrap();
        std::fs::write(dir.join("node_modules/x.js"), b"x").unwrap();
        std::fs::write(dir.join("src.txt"), b"s").unwrap();

        let snap = capture_directory_snapshot(
            &dir,
            CaptureOptions {
                exclude: vec!["node_modules".to_string(), "*/node_modules/*".to_string()],
            },
        )
        .await
        .unwrap();

        assert!(snap.entries.contains_key("src.txt"));
        assert!(!snap.entries.contains_key("node_modules"));
        assert!(!snap.entries.contains_key("node_modules/x.js"));
    }

    #[tokio::test]
    async fn read_snapshot_entry_returns_none_for_missing() {
        let dir = tempdir();
        let result = read_snapshot_entry(&dir, "missing.txt").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn read_snapshot_entry_returns_file_entry() {
        let dir = tempdir();
        std::fs::write(dir.join("file.txt"), b"hello").unwrap();
        let entry = read_snapshot_entry(&dir, "file.txt")
            .await
            .unwrap()
            .unwrap();
        match entry {
            SnapshotEntry::File { hash, .. } => {
                assert_eq!(
                    hash,
                    "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
                );
            }
            _ => panic!("expected File entry"),
        }
    }

    #[tokio::test]
    async fn directory_entry_matches_baseline_true() {
        let dir = tempdir();
        std::fs::write(dir.join("file.txt"), b"hello").unwrap();
        let baseline = SnapshotEntry::File {
            mode: 0o644,
            hash: "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".to_string(),
        };
        let result = directory_entry_matches_baseline(&dir, "file.txt", &baseline)
            .await
            .unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn directory_entry_matches_baseline_false_when_changed() {
        let dir = tempdir();
        std::fs::write(dir.join("file.txt"), b"different").unwrap();
        let baseline = SnapshotEntry::File {
            mode: 0o644,
            hash: "oldhash".to_string(),
        };
        let result = directory_entry_matches_baseline(&dir, "file.txt", &baseline)
            .await
            .unwrap();
        assert!(!result);
    }

    #[tokio::test]
    async fn merge_directory_with_baseline_copies_new_files() {
        let source = tempdir();
        let target = tempdir();
        std::fs::write(source.join("new.txt"), b"new content").unwrap();

        let baseline = DirectorySnapshot::default();
        merge_directory_with_baseline(MergeInput {
            baseline: &baseline,
            source_dir: &source,
            target_dir: &target,
            before_apply: None,
            after_apply: None,
        })
        .await
        .unwrap();

        let copied = std::fs::read(target.join("new.txt")).unwrap();
        assert_eq!(copied, b"new content");
    }

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "paperclip-restore-merge-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
