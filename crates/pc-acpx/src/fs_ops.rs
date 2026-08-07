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
}
