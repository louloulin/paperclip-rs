//! `pc-acpx` skill materialization — port of `materializePaperclipSkillCopy`,
//! `hashPathContents`, and `buildSkillSetKey` from Node
//! `acpx-engine/execute.ts`.
//!
//! Skill materialization is the host-side staging step that copies the
//! user's paperclip skill source directories into the per-session skill
//! home, dropping every symlink along the way (the runtime sandbox
//! cannot trust arbitrary symlink targets). The companion content hash
//! produces a deterministic cache key per (agent, skills) tuple — same
//! input, same hash, so two consecutive runs with the same skill set
//! hit the same materialized directory.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::error::AcpxError;
use crate::fs_ops::{lstat_or_none, remove_path_if_exists};

// ============================================================================
// Public types
// ============================================================================

/// One entry in the `PaperclipSkillEntry[]` array the engine consumes.
/// Mirrors the Node interface from `packages/adapter-utils/src/server-utils.ts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaperclipSkillEntry {
    /// Stable identity (independent of filesystem path or runtime name).
    pub key: String,
    /// Name used at runtime (also the directory name under `skills/`).
    #[serde(rename = "runtimeName")]
    pub runtime_name: String,
    /// Source path on the host filesystem.
    #[serde(with = "path_buf_serde")]
    pub source: PathBuf,
    /// Optional version ID pinned for this entry.
    #[serde(rename = "versionId", skip_serializing_if = "Option::is_none", default)]
    pub version_id: Option<String>,
    /// Optional current version ID observed at scan time.
    #[serde(
        rename = "currentVersionId",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub current_version_id: Option<String>,
    /// Whether the source is currently reachable.
    #[serde(
        rename = "sourceStatus",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub source_status: Option<SkillSourceStatus>,
    /// Optional detail string for a missing source.
    #[serde(
        rename = "missingDetail",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub missing_detail: Option<String>,
}

/// `available` / `missing` tag the engine attaches to a skill entry at
/// scan time. Mirrors the Node literal union.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillSourceStatus {
    Available,
    Missing,
}

/// Result of materializing a single skill into a target directory.
/// Mirrors `MaterializedPaperclipSkillCopyResult`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MaterializedSkillCopyResult {
    pub copied_files: usize,
    pub skipped_symlinks: Vec<String>,
}

// ============================================================================
// materializePaperclipSkillCopy
// ============================================================================

/// Materialize `source` into `target` as a fresh directory.
///
/// Semantics (mirrors the Node implementation):
/// - When `source` resolves to the same path as `target`, the helper is
///   a no-op and returns `copied_files = 0` with `skipped_symlinks` set
///   to `[source]` (so the caller can log the circular reference).
/// - Otherwise the helper recursively copies `source` into `target`,
///   tracking every symlink it encounters. Symlinks are dropped, never
///   followed: the sandbox is not allowed to dereference arbitrary user
///   paths.
pub async fn materialize_paperclip_skill_copy(
    source: impl AsRef<Path>,
    target: impl AsRef<Path>,
) -> Result<MaterializedSkillCopyResult, AcpxError> {
    let source = source.as_ref();
    let target = target.as_ref();
    let source_resolved = source
        .canonicalize()
        .unwrap_or_else(|_| source.to_path_buf());
    let target_resolved = target
        .canonicalize()
        .unwrap_or_else(|_| target.to_path_buf());
    if source_resolved == target_resolved {
        return Ok(MaterializedSkillCopyResult {
            copied_files: 0,
            skipped_symlinks: vec![source_resolved.to_string_lossy().into_owned()],
        });
    }
    remove_path_if_exists(target).await?;
    let mut result = MaterializedSkillCopyResult::default();
    copy_tree(source, target, &mut result).await?;
    Ok(result)
}

fn copy_tree<'a>(
    source: &'a Path,
    target: &'a Path,
    result: &'a mut MaterializedSkillCopyResult,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), AcpxError>> + Send + 'a>> {
    Box::pin(async move {
        let meta = lstat_or_none(source).await;
        let Some(meta) = meta else {
            return Ok(());
        };
        let file_type = meta.file_type();
        if file_type.is_symlink() {
            result
                .skipped_symlinks
                .push(source.to_string_lossy().into_owned());
            return Ok(());
        }
        if file_type.is_dir() {
            tokio::fs::create_dir_all(target)
                .await
                .map_err(|error| AcpxError::Io {
                    path: target.to_path_buf(),
                    error,
                })?;
            let mut entries = tokio::fs::read_dir(source)
                .await
                .map_err(|error| AcpxError::Io {
                    path: source.to_path_buf(),
                    error,
                })?;
            let mut names = Vec::new();
            while let Some(entry) = entries.next_entry().await.map_err(|error| AcpxError::Io {
                path: source.to_path_buf(),
                error,
            })? {
                names.push(entry.file_name());
            }
            names.sort();
            for name in names {
                let child_source = source.join(&name);
                let child_target = target.join(&name);
                copy_tree(&child_source, &child_target, result).await?;
            }
            return Ok(());
        }
        if file_type.is_file() {
            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|error| AcpxError::Io {
                        path: parent.to_path_buf(),
                        error,
                    })?;
            }
            let bytes = tokio::fs::read(source)
                .await
                .map_err(|error| AcpxError::Io {
                    path: source.to_path_buf(),
                    error,
                })?;
            let mut handle = tokio::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(target)
                .await
                .map_err(|error| AcpxError::Io {
                    path: target.to_path_buf(),
                    error,
                })?;
            handle
                .write_all(&bytes)
                .await
                .map_err(|error| AcpxError::Io {
                    path: target.to_path_buf(),
                    error,
                })?;
            let _ = handle.shutdown().await;
            result.copied_files += 1;
            return Ok(());
        }
        Ok(())
    })
}

// ============================================================================
// hashPathContents + buildSkillSetKey
// ============================================================================

/// Recursively hash `candidate` into the supplied `Sha256` digest.
/// Symlinks are noted but not followed; directory loops are broken via
/// `seen_directories`. Mirrors the Node `hashPathContents` helper.
///
/// Implemented as a boxed-recursive future to avoid the
/// `recursion in an async fn requires boxing` error.
pub fn hash_path_contents<'a>(
    candidate: &'a Path,
    hash: &'a mut Sha256,
    relative_path: &'a str,
    seen_directories: &'a mut HashSet<PathBuf>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        let meta = match tokio::fs::symlink_metadata(candidate).await {
            Ok(meta) => meta,
            Err(_) => return,
        };
        let file_type = meta.file_type();
        if file_type.is_symlink() {
            hash.update(format!("symlink-skipped:{relative_path}\n"));
            return;
        }
        if file_type.is_dir() {
            let real_dir = tokio::fs::canonicalize(candidate)
                .await
                .unwrap_or_else(|_| candidate.to_path_buf());
            hash.update(format!("dir:{relative_path}\n"));
            if !seen_directories.insert(real_dir.clone()) {
                hash.update(b"loop\n");
                return;
            }
            let mut entries = match tokio::fs::read_dir(candidate).await {
                Ok(entries) => entries,
                Err(_) => return,
            };
            let mut names = Vec::new();
            while let Ok(Some(entry)) = entries.next_entry().await {
                names.push(entry.file_name());
            }
            names.sort();
            for name in names {
                let child_relative = if relative_path.is_empty() {
                    name.to_string_lossy().into_owned()
                } else {
                    format!("{relative_path}/{}", name.to_string_lossy())
                };
                hash_path_contents(
                    &candidate.join(&name),
                    hash,
                    &child_relative,
                    seen_directories,
                )
                .await;
            }
            return;
        }
        if file_type.is_file() {
            hash.update(format!("file:{relative_path}\n"));
            if let Ok(bytes) = tokio::fs::read(candidate).await {
                hash.update(&bytes);
            }
            hash.update(b"\n");
            return;
        }
        let mode = meta.permissions().mode();
        hash.update(format!("other:{relative_path}:{mode}\n"));
    })
}

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Compute a deterministic hex SHA-256 cache key for a `(agent, skills)`
/// tuple. The key is stable across runs and across hosts (modulo
/// filesystem contents). Mirrors `buildSkillSetKey` from the Node
/// implementation.
pub async fn build_skill_set_key(skills: &[PaperclipSkillEntry], label: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(format!("paperclip-acpx-{label}-skills:v1\n"));
    let mut sorted: Vec<&PaperclipSkillEntry> = skills.iter().collect();
    sorted.sort_by(|left, right| left.runtime_name.cmp(&right.runtime_name));
    for entry in sorted {
        hash.update(format!("skill:{}:{}\n", entry.key, entry.runtime_name));
        let mut seen = HashSet::new();
        hash_path_contents(&entry.source, &mut hash, &entry.runtime_name, &mut seen).await;
    }
    let digest = hash.finalize();
    format!("{digest:x}")
}

// ============================================================================
// PathBuf serde helper
// ============================================================================

mod path_buf_serde {
    use std::path::PathBuf;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(path: &PathBuf, serializer: S) -> Result<S::Ok, S::Error> {
        path.to_string_lossy().into_owned().serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PathBuf, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(PathBuf::from(raw))
    }
}
// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn unique_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "pc-acpx-skillmat-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ))
    }

    fn write_file(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[tokio::test]
    async fn materialize_returns_self_when_source_equals_target() {
        let dir = unique_dir("self");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let result = materialize_paperclip_skill_copy(&dir, &dir).await.unwrap();
        assert_eq!(result.copied_files, 0);
        assert_eq!(result.skipped_symlinks.len(), 1);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn materialize_copies_files_and_directories_recursively() {
        let source = unique_dir("copy-src");
        let target = unique_dir("copy-tgt");
        write_file(&source.join("SKILL.md"), "skill doc");
        write_file(&source.join("scripts/run.sh"), "#!/bin/sh\n");
        write_file(&source.join("a/b/c/deep.txt"), "deep");
        let result = materialize_paperclip_skill_copy(&source, &target)
            .await
            .unwrap();
        assert!(result.copied_files >= 3);
        assert!(result.skipped_symlinks.is_empty());
        assert_eq!(
            tokio::fs::read_to_string(target.join("SKILL.md"))
                .await
                .unwrap(),
            "skill doc"
        );
        assert_eq!(
            tokio::fs::read_to_string(target.join("scripts/run.sh"))
                .await
                .unwrap(),
            "#!/bin/sh\n"
        );
        assert_eq!(
            tokio::fs::read_to_string(target.join("a/b/c/deep.txt"))
                .await
                .unwrap(),
            "deep"
        );
        let _ = tokio::fs::remove_dir_all(&source).await;
        let _ = tokio::fs::remove_dir_all(&target).await;
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn materialize_drops_symlinks() {
        let source = unique_dir("sl-src");
        let target = unique_dir("sl-tgt");
        write_file(&source.join("file.txt"), "real");
        // Create a symlink inside source that points outside.
        std::os::unix::fs::symlink(source.join("file.txt"), source.join("link.txt")).unwrap();
        let result = materialize_paperclip_skill_copy(&source, &target)
            .await
            .unwrap();
        assert!(!result.skipped_symlinks.is_empty());
        // Target must NOT contain the symlink.
        assert!(!tokio::fs::symlink_metadata(target.join("link.txt"))
            .await
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false));
        // Target must contain the regular file.
        assert_eq!(
            tokio::fs::read_to_string(target.join("file.txt"))
                .await
                .unwrap(),
            "real"
        );
        let _ = tokio::fs::remove_dir_all(&source).await;
        let _ = tokio::fs::remove_dir_all(&target).await;
    }

    #[tokio::test]
    async fn materialize_overwrites_existing_target() {
        let source = unique_dir("ow-src");
        let target = unique_dir("ow-tgt");
        write_file(&source.join("new.txt"), "fresh");
        write_file(&target.join("stale.txt"), "stale");
        materialize_paperclip_skill_copy(&source, &target)
            .await
            .unwrap();
        assert!(tokio::fs::try_exists(target.join("new.txt"))
            .await
            .unwrap_or(false));
        assert!(!tokio::fs::try_exists(target.join("stale.txt"))
            .await
            .unwrap_or(false));
        let _ = tokio::fs::remove_dir_all(&source).await;
        let _ = tokio::fs::remove_dir_all(&target).await;
    }

    #[tokio::test]
    async fn build_skill_set_key_is_deterministic() {
        let dir = unique_dir("hash");
        write_file(&dir.join("a.txt"), "alpha");
        write_file(&dir.join("b/b.txt"), "beta");
        let skill = PaperclipSkillEntry {
            key: "k1".into(),
            runtime_name: "skill-one".into(),
            source: dir.clone(),
            version_id: None,
            current_version_id: None,
            source_status: Some(SkillSourceStatus::Available),
            missing_detail: None,
        };
        let hash_a = build_skill_set_key(&[skill.clone()], "claude").await;
        let hash_b = build_skill_set_key(&[skill], "claude").await;
        assert_eq!(hash_a, hash_b, "same input → same key");
        assert_eq!(hash_a.len(), 64, "sha256 hex digest is 64 chars");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn build_skill_set_key_changes_when_skill_contents_change() {
        let dir_a = unique_dir("hash-a");
        let dir_b = unique_dir("hash-b");
        write_file(&dir_a.join("SKILL.md"), "v1");
        write_file(&dir_b.join("SKILL.md"), "v2");
        let entry_a = PaperclipSkillEntry {
            key: "k".into(),
            runtime_name: "skill".into(),
            source: dir_a.clone(),
            version_id: None,
            current_version_id: None,
            source_status: Some(SkillSourceStatus::Available),
            missing_detail: None,
        };
        let entry_b = PaperclipSkillEntry {
            source: dir_b.clone(),
            ..entry_a.clone()
        };
        let hash_a = build_skill_set_key(&[entry_a], "claude").await;
        let hash_b = build_skill_set_key(&[entry_b], "claude").await;
        assert_ne!(hash_a, hash_b);
        let _ = tokio::fs::remove_dir_all(&dir_a).await;
        let _ = tokio::fs::remove_dir_all(&dir_b).await;
    }

    #[tokio::test]
    async fn build_skill_set_key_changes_with_label() {
        let dir = unique_dir("label");
        write_file(&dir.join("SKILL.md"), "v1");
        let entry = PaperclipSkillEntry {
            key: "k".into(),
            runtime_name: "skill".into(),
            source: dir.clone(),
            version_id: None,
            current_version_id: None,
            source_status: Some(SkillSourceStatus::Available),
            missing_detail: None,
        };
        let hash_claude = build_skill_set_key(&[entry.clone()], "claude").await;
        let hash_codex = build_skill_set_key(&[entry], "codex").await;
        assert_ne!(hash_claude, hash_codex);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[test]
    fn skill_entry_round_trips_through_json() {
        let entry = PaperclipSkillEntry {
            key: "k1".into(),
            runtime_name: "skill-one".into(),
            source: PathBuf::from("/tmp/skill"),
            version_id: Some("v1".into()),
            current_version_id: None,
            source_status: Some(SkillSourceStatus::Available),
            missing_detail: None,
        };
        let value = serde_json::to_value(&entry).unwrap();
        assert_eq!(value["key"], "k1");
        assert_eq!(value["runtimeName"], "skill-one");
        assert_eq!(value["source"], "/tmp/skill");
        let round_trip: PaperclipSkillEntry = serde_json::from_value(value).unwrap();
        assert_eq!(round_trip, entry);
    }
}
