//! `pc-acpx` filesystem helpers — async I/O wrappers that mirror Node
//! `pathExists`, `ensureParentDir`, `writeFileAtomically` from
//! `acpx-engine/execute.ts`. All paths are engine-relative absolute paths.

use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

use crate::error::AcpxError;

// ============================================================================
// Path existence
// ============================================================================

/// Returns `true` if `candidate` exists on disk. Hidden errors are folded into
/// `false` — the function answers the existence question, not the error.
pub async fn path_exists(candidate: impl AsRef<Path>) -> bool {
    tokio::fs::metadata(candidate).await.is_ok()
}

/// Returns `true` if `candidate` exists **and** is a regular file. Symlinks
/// that point to a regular file count as regular files.
pub async fn path_is_file(candidate: impl AsRef<Path>) -> bool {
    match tokio::fs::metadata(candidate).await {
        Ok(metadata) => metadata.is_file(),
        Err(_) => false,
    }
}

// ============================================================================
// mkdir -p
// ============================================================================

/// Create the parent directory of `target`, including any missing
/// intermediate directories. The target itself is **not** created — use
/// `write_file_atomically` for that.
pub async fn ensure_parent_dir(target: impl AsRef<Path>) -> Result<(), AcpxError> {
    let parent = target
        .as_ref()
        .parent()
        .ok_or_else(|| AcpxError::NoParent(target.as_ref().to_path_buf()))?;
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|error| AcpxError::Io {
            path: parent.to_path_buf(),
            error,
        })?;
    Ok(())
}

// ============================================================================
// Atomic write
// ============================================================================

/// Atomic-write input. Mirrors the Node `writeFileAtomically` argument shape.
#[derive(Debug, Clone)]
pub struct WriteFileAtomicallyInput {
    pub target: PathBuf,
    pub contents: String,
    pub mode: u32,
}

impl WriteFileAtomicallyInput {
    pub fn new(target: impl Into<PathBuf>, contents: impl Into<String>, mode: u32) -> Self {
        Self {
            target: target.into(),
            contents: contents.into(),
            mode,
        }
    }
}

/// Atomically write `contents` to `target`. The function:
/// 1. Creates the parent directory (recursively).
/// 2. Writes to a temporary file alongside the target (`<target>.tmp-<uuid>`).
/// 3. Renames the temporary file onto the target.
/// 4. Sets the requested mode on the final file (best-effort: ignored on
///    platforms where `chmod` is unavailable).
///
/// On any failure between steps 2 and 3 the temporary file is cleaned up.
pub async fn write_file_atomically(input: WriteFileAtomicallyInput) -> Result<(), AcpxError> {
    ensure_parent_dir(&input.target).await?;
    let temp_path = compose_temp_path(&input.target);
    let write_result = write_temp_and_rename(&input, &temp_path).await;
    if let Err(error) = write_result {
        // Best-effort cleanup.
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(error);
    }
    // Best-effort chmod — some platforms do not support it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = std::fs::Permissions::from_mode(input.mode & 0o777);
        if let Err(error) = tokio::fs::set_permissions(&input.target, permissions).await {
            // Match Node: ignore chmod errors silently.
            let _ = error;
        }
    }
    Ok(())
}

async fn write_temp_and_rename(
    input: &WriteFileAtomicallyInput,
    temp_path: &Path,
) -> Result<(), AcpxError> {
    let mut handle = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp_path)
        .await
        .map_err(|error| AcpxError::Io {
            path: temp_path.to_path_buf(),
            error,
        })?;
    if let Err(error) = handle.write_all(input.contents.as_bytes()).await {
        let _ = handle.shutdown().await;
        return Err(AcpxError::Io {
            path: temp_path.to_path_buf(),
            error,
        });
    }
    if let Err(error) = handle.shutdown().await {
        return Err(AcpxError::Io {
            path: temp_path.to_path_buf(),
            error,
        });
    }
    tokio::fs::rename(temp_path, &input.target)
        .await
        .map_err(|error| AcpxError::Io {
            path: input.target.clone(),
            error,
        })?;
    Ok(())
}

fn compose_temp_path(target: &Path) -> PathBuf {
    let pid = std::process::id();
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let mut name = target
        .file_name()
        .map(|os| os.to_os_string())
        .unwrap_or_default();
    name.push(format!(".tmp-{pid}-{suffix}"));
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    parent.join(name)
}

// ============================================================================
// Symbolic link helpers (R367 skill staging seam)
// ============================================================================

/// Create a symbolic link at `link` pointing to `target`. The parent
/// directory of `link` is created if missing. On platforms where
/// symlinks are unavailable the call returns `AcpxError::SymlinkUnsupported`.
pub async fn ensure_symlink(
    link: impl AsRef<Path>,
    target: impl AsRef<Path>,
) -> Result<(), AcpxError> {
    let link = link.as_ref();
    let target = target.as_ref();
    ensure_parent_dir(link).await?;
    let link_str = link.to_path_buf();
    let target_str = target.to_path_buf();
    tokio::task::spawn_blocking(move || {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&target_str, &link_str).map_err(|error| AcpxError::Io {
                path: link_str,
                error,
            })?;
            Ok::<(), AcpxError>(())
        }
        #[cfg(not(unix))]
        {
            let _ = (target_str, link_str);
            Err(AcpxError::SymlinkUnsupported)
        }
    })
    .await
    .map_err(|error| AcpxError::Join {
        context: "ensure_symlink".into(),
        error,
    })?
}

/// Copy `source` to `target` if `source` exists. The target's parent
/// directory is created if missing. Returns `true` when a copy was made,
/// `false` when the source did not exist.
pub async fn ensure_copied_file(
    target: impl AsRef<Path>,
    source: impl AsRef<Path>,
) -> Result<bool, AcpxError> {
    let target = target.as_ref();
    let source = source.as_ref();
    if !path_exists(source).await {
        return Ok(false);
    }
    ensure_parent_dir(target).await?;
    let target_str = target.to_path_buf();
    let source_str = source.to_path_buf();
    tokio::task::spawn_blocking(move || {
        std::fs::copy(&source_str, &target_str).map_err(|error| AcpxError::Io {
            path: target_str,
            error,
        })?;
        Ok::<(), AcpxError>(())
    })
    .await
    .map_err(|error| AcpxError::Join {
        context: "ensure_copied_file".into(),
        error,
    })??;
    Ok(true)
}

/// Symlink a single regular file from `source` to `target`. When the
/// platform does not support symlinks, fall back to a file copy. When
/// `source` does not exist, returns `false` without touching `target`.
pub async fn symlink_or_copy_file(
    source: impl AsRef<Path>,
    target: impl AsRef<Path>,
) -> Result<bool, AcpxError> {
    let source = source.as_ref();
    let target = target.as_ref();
    if !path_exists(source).await {
        return Ok(false);
    }
    ensure_parent_dir(target).await?;
    let source_buf = source.to_path_buf();
    let target_buf = target.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<bool, AcpxError> {
        #[cfg(unix)]
        {
            if std::os::unix::fs::symlink(&source_buf, &target_buf).is_ok() {
                return Ok(true);
            }
        }
        std::fs::copy(&source_buf, &target_buf).map_err(|error| AcpxError::Io {
            path: target_buf,
            error,
        })?;
        Ok(true)
    })
    .await
    .map_err(|error| AcpxError::Join {
        context: "symlink_or_copy_file".into(),
        error,
    })?
}

/// Async `lstat` that returns `None` when the path is missing or the
/// metadata cannot be read.
pub async fn lstat_or_none(candidate: impl AsRef<Path>) -> Option<std::fs::Metadata> {
    match tokio::fs::symlink_metadata(candidate.as_ref()).await {
        Ok(metadata) => Some(metadata),
        Err(_) => None,
    }
}

/// Async `readlink` that returns `None` when the path is missing or not a
/// symlink.
pub async fn readlink_or_none(candidate: impl AsRef<Path>) -> Option<PathBuf> {
    match tokio::fs::read_link(candidate.as_ref()).await {
        Ok(path) => Some(path),
        Err(_) => None,
    }
}

/// Recursively remove a path (file, symlink, or directory). Returns
/// `true` when something was removed, `false` when the path did not
/// exist.
pub async fn remove_path_if_exists(candidate: impl AsRef<Path>) -> Result<bool, AcpxError> {
    let candidate = candidate.as_ref().to_path_buf();
    if tokio::fs::symlink_metadata(&candidate).await.is_err() {
        return Ok(false);
    }
    if tokio::fs::remove_dir_all(&candidate).await.is_ok() {
        return Ok(true);
    }
    tokio::fs::remove_file(&candidate)
        .await
        .map_err(|error| AcpxError::Io {
            path: candidate,
            error,
        })?;
    Ok(true)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_tempdir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "pc-acpx-fs-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        path
    }

    #[tokio::test]
    async fn path_exists_returns_true_for_existing_dirs() {
        let dir = unique_tempdir("dir");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        assert!(path_exists(&dir).await);
        assert!(!path_exists(dir.join("missing")).await);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn ensure_parent_dir_creates_intermediate_dirs() {
        let dir = unique_tempdir("parents");
        let target = dir.join("a/b/c/file.txt");
        ensure_parent_dir(&target).await.unwrap();
        assert!(path_exists(dir.join("a/b/c")).await);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn ensure_parent_dir_is_noop_for_bare_filename() {
        let target = PathBuf::from("file.txt");
        ensure_parent_dir(&target).await.unwrap();
    }

    #[tokio::test]
    async fn write_file_atomically_creates_target_with_mode() {
        let dir = unique_tempdir("write");
        let target = dir.join("nested/file.txt");
        write_file_atomically(WriteFileAtomicallyInput::new(&target, "hello", 0o600))
            .await
            .unwrap();
        let contents = tokio::fs::read_to_string(&target).await.unwrap();
        assert_eq!(contents, "hello");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = tokio::fs::metadata(&target).await.unwrap();
            let mode = metadata.permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "expected mode 0o600, got {mode:o}");
        }
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn write_file_atomically_overwrites_existing_file() {
        let dir = unique_tempdir("overwrite");
        let target = dir.join("file.txt");
        write_file_atomically(WriteFileAtomicallyInput::new(&target, "first", 0o644))
            .await
            .unwrap();
        write_file_atomically(WriteFileAtomicallyInput::new(&target, "second", 0o644))
            .await
            .unwrap();
        let contents = tokio::fs::read_to_string(&target).await.unwrap();
        assert_eq!(contents, "second");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn write_file_atomically_cleans_up_partial_writes() {
        let dir = unique_tempdir("cleanup");
        let target = dir.join("file.txt");
        // Pre-create the target file so write_file_atomically's rename fails
        // (a real `rename` overwrites on POSIX, but we use a directory as the
        // target to force a deterministic failure).
        tokio::fs::create_dir_all(&target).await.unwrap();
        let result =
            write_file_atomically(WriteFileAtomicallyInput::new(&target, "fail", 0o644)).await;
        assert!(result.is_err());
        // Temp files should be cleaned up.
        let mut dirents = tokio::fs::read_dir(&dir).await.unwrap();
        let mut leftovers = 0;
        while let Some(entry) = dirents.next_entry().await.unwrap() {
            if entry.file_name().to_string_lossy().contains(".tmp-") {
                leftovers += 1;
            }
        }
        assert_eq!(leftovers, 0, "all temp files should be cleaned up");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn ensure_symlink_creates_link_with_original_target() {
        let dir = unique_tempdir("symlink");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let target = dir.join("source.txt");
        tokio::fs::write(&target, "hello").await.unwrap();
        let link = dir.join("link.txt");
        ensure_symlink(&link, &target).await.unwrap();
        let meta = tokio::fs::symlink_metadata(&link).await.unwrap();
        assert!(meta.file_type().is_symlink());
        let contents = tokio::fs::read_to_string(&link).await.unwrap();
        assert_eq!(contents, "hello");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn ensure_copied_file_returns_false_when_source_missing() {
        let dir = unique_tempdir("copy-missing");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let source = dir.join("does-not-exist.txt");
        let target = dir.join("target.txt");
        let result = ensure_copied_file(&target, &source).await.unwrap();
        assert!(!result);
        assert!(!tokio::fs::try_exists(&target).await.unwrap_or(false));
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn ensure_copied_file_copies_existing_file() {
        let dir = unique_tempdir("copy");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let source = dir.join("source.txt");
        tokio::fs::write(&source, "abc").await.unwrap();
        let target = dir.join("nested/target.txt");
        ensure_copied_file(&target, &source).await.unwrap();
        let contents = tokio::fs::read_to_string(&target).await.unwrap();
        assert_eq!(contents, "abc");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn symlink_or_copy_file_returns_false_when_source_missing() {
        let dir = unique_tempdir("sl-or-copy-missing");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let source = dir.join("missing.txt");
        let target = dir.join("target.txt");
        let result = symlink_or_copy_file(&source, &target).await.unwrap();
        assert!(!result);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn symlink_or_copy_file_prefers_symlink_on_unix() {
        let dir = unique_tempdir("sl-or-copy");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let source = dir.join("source.txt");
        tokio::fs::write(&source, "x").await.unwrap();
        let target = dir.join("target.txt");
        let result = symlink_or_copy_file(&source, &target).await.unwrap();
        assert!(result);
        let meta = tokio::fs::symlink_metadata(&target).await.unwrap();
        assert!(meta.file_type().is_symlink());
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn lstat_or_none_returns_none_for_missing_path() {
        let result = lstat_or_none("/nonexistent/path").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn lstat_or_none_returns_metadata_for_existing_path() {
        let dir = unique_tempdir("lstat");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let result = lstat_or_none(&dir).await;
        assert!(result.is_some());
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn readlink_or_none_returns_none_for_regular_file() {
        let dir = unique_tempdir("readlink");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let file = dir.join("regular.txt");
        tokio::fs::write(&file, "x").await.unwrap();
        let result = readlink_or_none(&file).await;
        assert!(result.is_none());
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn readlink_or_none_returns_target_for_symlink() {
        let dir = unique_tempdir("readlink-sl");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let target = dir.join("real.txt");
        tokio::fs::write(&target, "x").await.unwrap();
        let link = dir.join("link.txt");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let result = readlink_or_none(&link).await;
        assert!(result.is_some());
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn remove_path_if_exists_returns_false_when_missing() {
        let dir = unique_tempdir("remove-missing");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let result = remove_path_if_exists(dir.join("missing")).await.unwrap();
        assert!(!result);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn remove_path_if_exists_removes_directory() {
        let dir = unique_tempdir("remove-dir");
        let target = dir.join("subdir");
        tokio::fs::create_dir_all(&target).await.unwrap();
        tokio::fs::write(target.join("file.txt"), "x")
            .await
            .unwrap();
        let result = remove_path_if_exists(&target).await.unwrap();
        assert!(result);
        assert!(!tokio::fs::try_exists(&target).await.unwrap_or(false));
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
