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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::error::AcpxError;
use crate::fs_ops::lstat_or_none;

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
// Materialize constants (mirrors Node L129-131)
// ============================================================================

/// Sentinel file written into the materialized target directory. Mirrors
/// Node `MATERIALIZED_SKILL_SENTINEL` (L129).
pub const MATERIALIZED_SKILL_SENTINEL: &str = ".paperclip-materialized-skill.json";

/// Owner file written inside the materialize lock directory. Mirrors
/// Node `MATERIALIZED_SKILL_LOCK_OWNER` (L130).
pub const MATERIALIZED_SKILL_LOCK_OWNER: &str = "owner.json";

/// Stale-lock threshold in milliseconds. Mirrors Node
/// `MATERIALIZED_SKILL_LOCK_STALE_MS` (L131) — 30 seconds.
pub const MATERIALIZED_SKILL_LOCK_STALE_MS: u64 = 30_000;

/// Default poll interval when waiting on the materialize lock. Mirrors
/// Node `await new Promise((resolve) => setTimeout(resolve, 50))`
/// (L2996).
const MATERIALIZED_SKILL_LOCK_POLL_MS: u64 = 50;

// ============================================================================
// materializePaperclipSkillCopy (L3038-3120)
// ============================================================================
pub async fn materialize_paperclip_skill_copy(
    source: impl AsRef<Path>,
    target: impl AsRef<Path>,
) -> Result<MaterializedSkillCopyResult, AcpxError> {
    let source_root = path_clean(
        std::path::absolute(source.as_ref()).unwrap_or_else(|_| source.as_ref().to_path_buf()),
    );
    let target_root = path_clean(
        std::path::absolute(target.as_ref()).unwrap_or_else(|_| target.as_ref().to_path_buf()),
    );
    let relative_target = pathdiff(&source_root, &target_root);
    let relative_source = pathdiff(&target_root, &source_root);
    let same_path = source_root == target_root;
    if same_path || relative_target.is_some() || relative_source.is_some() {
        return Err(AcpxError::MaterializeSelfReference {
            source_path: source_root.to_string_lossy().into_owned(),
            target_path: target_root.to_string_lossy().into_owned(),
        });
    }

    let root_meta = tokio::fs::symlink_metadata(&source_root)
        .await
        .map_err(|error| AcpxError::Io {
            path: source_root.clone(),
            error,
        })?;
    if root_meta.file_type().is_symlink() {
        return Err(AcpxError::MaterializeSymlinkRoot {
            path: source_root.to_string_lossy().into_owned(),
        });
    }
    if !root_meta.file_type().is_dir() {
        return Err(AcpxError::MaterializeNotDirectory {
            path: source_root.to_string_lossy().into_owned(),
        });
    }

    let mut result = MaterializedSkillCopyResult::default();
    let lock_dir = format!("{}.lock", target_root.to_string_lossy());
    let release_lock = acquire_materialize_lock(&lock_dir).await?;
    let pid = std::process::id();
    let suffix = random_uuid_string();
    let temp_root = format!("{}.tmp-{}-{}", target_root.to_string_lossy(), pid, suffix);
    let temp_root_path = PathBuf::from(&temp_root);

    let copy_result: Result<(), AcpxError> = async {
        let source_fingerprint = hash_skill_directory(&source_root).await?;
        if materialized_skill_fingerprint_matches(&target_root, &source_fingerprint).await {
            return Ok(());
        }
        copy_skill_tree(&source_root, &temp_root_path, "", &mut result).await?;
        let sentinel_value = json!({
            "version": 1,
            "sourceFingerprint": source_fingerprint,
            "copiedFiles": result.copied_files,
            "skippedSymlinks": result.skipped_symlinks,
        });
        let sentinel_path = temp_root_path.join(MATERIALIZED_SKILL_SENTINEL);
        tokio::fs::write(
            &sentinel_path,
            format!(
                "{}\n",
                serde_json::to_string_pretty(&sentinel_value).map_err(|error| {
                    AcpxError::Json {
                        context: "materialize sentinel".to_string(),
                        error,
                    }
                })?
            ),
        )
        .await
        .map_err(|error| AcpxError::Io {
            path: sentinel_path,
            error,
        })?;

        if materialized_skill_fingerprint_matches(&target_root, &source_fingerprint).await {
            return Ok(());
        }
        remove_recursive(&target_root).await?;
        rename(&temp_root_path, &target_root).await?;
        Ok(())
    }
    .await;

    let _ = remove_recursive(&temp_root_path).await;
    let _ = release_lock().await;
    copy_result?;
    Ok(result)
}

// ============================================================================
// hash_skill_directory + materialized_skill_fingerprint_matches
// ============================================================================

pub async fn hash_skill_directory(root: &Path) -> Result<String, AcpxError> {
    let mut hash = Sha256::new();
    visit_for_hash(root, "", &mut hash).await?;
    Ok(format!("{:x}", hash.finalize()))
}

fn visit_for_hash<'a>(
    candidate: &'a Path,
    relative_path: &'a str,
    hash: &'a mut Sha256,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), AcpxError>> + Send + 'a>> {
    Box::pin(async move {
        let stat = tokio::fs::symlink_metadata(candidate)
            .await
            .map_err(|error| AcpxError::Io {
                path: candidate.to_path_buf(),
                error,
            })?;
        let file_type = stat.file_type();
        let mode = stat.permissions().mode();
        if file_type.is_symlink() {
            hash.update(format!("symlink:{relative_path}\n").as_bytes());
            return Ok(());
        }
        if file_type.is_dir() {
            hash.update(format!("dir:{relative_path}\n").as_bytes());
            let mut entries =
                tokio::fs::read_dir(candidate)
                    .await
                    .map_err(|error| AcpxError::Io {
                        path: candidate.to_path_buf(),
                        error,
                    })?;
            let mut names: Vec<std::ffi::OsString> = Vec::new();
            while let Some(entry) = entries.next_entry().await.map_err(|error| AcpxError::Io {
                path: candidate.to_path_buf(),
                error,
            })? {
                names.push(entry.file_name());
            }
            names.sort_by(|left, right| left.to_string_lossy().cmp(&right.to_string_lossy()));
            for name in names {
                let name_str = name.to_string_lossy().into_owned();
                let child_relative = if relative_path.is_empty() {
                    name_str.clone()
                } else {
                    format!("{relative_path}/{name_str}")
                };
                let child_candidate = candidate.join(&name);
                visit_for_hash(&child_candidate, &child_relative, hash).await?;
            }
            return Ok(());
        }
        if file_type.is_file() {
            hash.update(format!("file:{relative_path}:{mode}\n").as_bytes());
            let bytes = tokio::fs::read(candidate)
                .await
                .map_err(|error| AcpxError::Io {
                    path: candidate.to_path_buf(),
                    error,
                })?;
            hash.update(&bytes);
            hash.update(b"\n");
            return Ok(());
        }
        hash.update(format!("other:{relative_path}:{mode}\n").as_bytes());
        Ok(())
    })
}

pub async fn materialized_skill_fingerprint_matches(
    target_root: &Path,
    source_fingerprint: &str,
) -> bool {
    let sentinel = target_root.join(MATERIALIZED_SKILL_SENTINEL);
    let raw = match tokio::fs::read_to_string(&sentinel).await {
        Ok(raw) => raw,
        Err(_) => return false,
    };
    let value: Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let object = match value.as_object() {
        Some(object) => object,
        None => return false,
    };
    if object.get("version").and_then(Value::as_i64) != Some(1) {
        return false;
    }
    object.get("sourceFingerprint").and_then(Value::as_str) == Some(source_fingerprint)
}

// ============================================================================
// acquire_materialize_lock + remove_stale_materialize_lock + is_pid_alive
// ============================================================================

pub async fn acquire_materialize_lock(
    lock_dir: &str,
) -> Result<
    Box<
        dyn FnOnce() -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<(), AcpxError>> + Send>,
            > + Send,
    >,
    AcpxError,
> {
    let lock_path = PathBuf::from(lock_dir);
    if let Some(parent) = lock_path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| AcpxError::Io {
                    path: parent.to_path_buf(),
                    error,
                })?;
        }
    }
    let started = now_ms();
    let deadline = started + MATERIALIZED_SKILL_LOCK_STALE_MS;
    loop {
        match tokio::fs::create_dir(&lock_path).await {
            Ok(()) => {
                let owner = json!({
                    "pid": std::process::id(),
                    "createdAt": now_iso8601(),
                });
                let owner_path = lock_path.join(MATERIALIZED_SKILL_LOCK_OWNER);
                tokio::fs::write(
                    &owner_path,
                    format!(
                        "{}\n",
                        serde_json::to_string(&owner).map_err(|error| AcpxError::Json {
                            context: "materialize lock owner".to_string(),
                            error,
                        })?
                    ),
                )
                .await
                .map_err(|error| AcpxError::Io {
                    path: owner_path,
                    error,
                })?;
                let lock_path_for_release = lock_path.clone();
                let release: Box<
                    dyn FnOnce() -> std::pin::Pin<
                            Box<dyn std::future::Future<Output = Result<(), AcpxError>> + Send>,
                        > + Send,
                > = Box::new(move || {
                    let path = lock_path_for_release.clone();
                    Box::pin(async move { remove_recursive(&path).await })
                });
                return Ok(release);
            }
            Err(error) => {
                if !is_dir_exists_error(&error) {
                    return Err(AcpxError::Io {
                        path: lock_path.clone(),
                        error,
                    });
                }
                if remove_stale_materialize_lock(&lock_path, MATERIALIZED_SKILL_LOCK_STALE_MS).await
                {
                    continue;
                }
                if now_ms() >= deadline {
                    return Err(AcpxError::MaterializeLockTimeout {
                        lock_dir: lock_path.to_string_lossy().into_owned(),
                    });
                }
                tokio::time::sleep(Duration::from_millis(MATERIALIZED_SKILL_LOCK_POLL_MS)).await;
            }
        }
    }
}

pub async fn remove_stale_materialize_lock(lock_dir: &Path, stale_ms: u64) -> bool {
    let owner_path = lock_dir.join(MATERIALIZED_SKILL_LOCK_OWNER);
    let mut should_remove = false;
    match tokio::fs::read_to_string(&owner_path).await {
        Ok(raw) => {
            let parsed: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
            let pid = parsed.get("pid").and_then(Value::as_u64).unwrap_or(0);
            let created_at_ms = parsed
                .get("createdAt")
                .and_then(Value::as_str)
                .and_then(chrono_like_parse_ms)
                .unwrap_or(0);
            let age_ms = if created_at_ms > 0 {
                now_ms().saturating_sub(created_at_ms)
            } else {
                stale_ms + 1
            };
            should_remove = pid == 0 || !is_pid_alive(pid as u32) || age_ms > stale_ms;
        }
        Err(_) => {
            let stat = tokio::fs::metadata(lock_dir).await.ok();
            let mtime_ms: u64 = stat
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            should_remove = mtime_ms == 0 || now_ms().saturating_sub(mtime_ms) > stale_ms;
        }
    }
    if !should_remove {
        return false;
    }
    let _ = remove_recursive(lock_dir).await;
    true
}

pub fn is_pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let probe = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("kill -0 {pid} >/dev/null 2>&1; echo $?"))
        .output();
    match probe {
        Ok(output) => {
            let trimmed = String::from_utf8_lossy(&output.stdout);
            trimmed.trim() == "0"
        }
        Err(_) => false,
    }
}

// ============================================================================
// Internal helpers
// ============================================================================

fn pathdiff(from: &Path, to: &Path) -> Option<String> {
    let rel = pathdiff_full(from, to);
    if rel.is_empty() {
        return None;
    }
    if rel.starts_with("..") || rel.starts_with('/') {
        return None;
    }
    Some(rel)
}

fn pathdiff_full(from: &Path, to: &Path) -> String {
    let from_components: Vec<String> = from
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let to_components: Vec<String> = to
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let mut common = 0usize;
    while common < from_components.len()
        && common < to_components.len()
        && from_components[common] == to_components[common]
    {
        common += 1;
    }
    let mut out: Vec<String> = Vec::new();
    for _ in common..from_components.len() {
        out.push("..".to_string());
    }
    for part in &to_components[common..] {
        out.push(part.clone());
    }
    out.join("/")
}

fn path_clean(input: PathBuf) -> PathBuf {
    let mut components: Vec<std::path::Component<'_>> = Vec::new();
    for comp in input.components() {
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

fn copy_skill_tree<'a>(
    source_path: &'a Path,
    target_path: &'a Path,
    relative_path: &'a str,
    result: &'a mut MaterializedSkillCopyResult,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), AcpxError>> + Send + 'a>> {
    Box::pin(async move {
        let stat = tokio::fs::symlink_metadata(source_path)
            .await
            .map_err(|error| AcpxError::Io {
                path: source_path.to_path_buf(),
                error,
            })?;
        let file_type = stat.file_type();
        if file_type.is_symlink() {
            let entry = if relative_path.is_empty() {
                ".".to_string()
            } else {
                relative_path.to_string()
            };
            result.skipped_symlinks.push(entry);
            return Ok(());
        }
        if file_type.is_dir() {
            tokio::fs::create_dir_all(target_path)
                .await
                .map_err(|error| AcpxError::Io {
                    path: target_path.to_path_buf(),
                    error,
                })?;
            let mut entries =
                tokio::fs::read_dir(source_path)
                    .await
                    .map_err(|error| AcpxError::Io {
                        path: source_path.to_path_buf(),
                        error,
                    })?;
            let mut names: Vec<std::ffi::OsString> = Vec::new();
            while let Some(entry) = entries.next_entry().await.map_err(|error| AcpxError::Io {
                path: source_path.to_path_buf(),
                error,
            })? {
                names.push(entry.file_name());
            }
            names.sort_by(|left, right| left.to_string_lossy().cmp(&right.to_string_lossy()));
            for name in names {
                let name_str = name.to_string_lossy().into_owned();
                let child_relative = if relative_path.is_empty() {
                    name_str.clone()
                } else {
                    format!("{relative_path}/{name_str}")
                };
                let child_source = source_path.join(&name);
                let child_target = target_path.join(&name);
                copy_skill_tree(&child_source, &child_target, &child_relative, result).await?;
            }
            return Ok(());
        }
        if file_type.is_file() {
            if let Some(parent) = target_path.parent() {
                if !parent.as_os_str().is_empty() {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .map_err(|error| AcpxError::Io {
                            path: parent.to_path_buf(),
                            error,
                        })?;
                }
            }
            tokio::fs::copy(source_path, target_path)
                .await
                .map_err(|error| AcpxError::Io {
                    path: target_path.to_path_buf(),
                    error,
                })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = tokio::fs::set_permissions(
                    target_path,
                    std::fs::Permissions::from_mode(stat.permissions().mode()),
                )
                .await;
            }
            result.copied_files += 1;
            return Ok(());
        }
        Ok(())
    })
}

async fn remove_recursive(path: &Path) -> Result<(), AcpxError> {
    if !tokio::fs::try_exists(path).await.unwrap_or(false) {
        return Ok(());
    }
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|error| AcpxError::Io {
            path: path.to_path_buf(),
            error,
        })?;
    let result = if meta.file_type().is_dir() {
        tokio::fs::remove_dir_all(path).await
    } else {
        tokio::fs::remove_file(path).await
    };
    match result {
        Ok(()) => Ok(()),
        Err(error) => Err(AcpxError::Io {
            path: path.to_path_buf(),
            error,
        }),
    }
}

async fn rename(from: &Path, to: &Path) -> Result<(), AcpxError> {
    tokio::fs::rename(from, to)
        .await
        .map_err(|error| AcpxError::Io {
            path: to.to_path_buf(),
            error,
        })
}

fn is_dir_exists_error(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::AlreadyExists
        || matches!(error.raw_os_error(), Some(code) if code == libc_eexist())
}

#[cfg(unix)]
fn libc_eexist() -> i32 {
    17
}

#[cfg(not(unix))]
fn libc_eexist() -> i32 {
    183
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn now_iso8601() -> String {
    let secs = now_ms() / 1000;
    let ms = now_ms() % 1000;
    let (year, month, day, hour, minute, second) = epoch_seconds_to_civil(secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{ms:03}Z")
}

fn epoch_seconds_to_civil(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let secs_of_day = (secs % 86_400) as u32;
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d: i64 = (doy as i64) - ((153 * mp + 2) / 5) as i64 + 1;
    let m: i64 = if mp < 10 {
        (mp + 3) as i64
    } else {
        (mp - 9) as i64
    };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u32, m as u32, d as u32, hour, minute, second)
}

fn chrono_like_parse_ms(s: &str) -> Option<u64> {
    let bytes = s.as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month: u64 = s.get(5..7)?.parse().ok()?;
    let day: u64 = s.get(8..10)?.parse().ok()?;
    if bytes.get(10) != Some(&b'T') && bytes.get(10) != Some(&b' ') {
        return None;
    }
    let hour: u64 = s.get(11..13)?.parse().ok()?;
    let minute: u64 = s.get(14..16)?.parse().ok()?;
    let second: u64 = s.get(17..19)?.parse().ok()?;
    let (rest, ms) = if bytes.len() > 19 && bytes[19] == b'.' {
        let fraction = &s[20..];
        let ms_str: String = fraction
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        let digits: String = ms_str.chars().take(3).collect();
        let ms = if digits.is_empty() {
            0
        } else {
            digits.parse::<u64>().ok()?
        };
        (&fraction[ms_str.len()..], ms)
    } else {
        (&s[19..], 0u64)
    };
    if !rest.starts_with('Z') && !rest.starts_with('+') && !rest.starts_with('-') {
        return None;
    }
    let days_from_civil = |y: i64, m: i64, d: i64| -> i64 {
        let y = if m <= 2 { y - 1 } else { y };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe: i64 = y - era * 400;
        let mp: i64 = if m > 2 { m - 3 } else { m + 9 };
        let doy: i64 = (153 * mp + 2) / 5 + d - 1;
        let doe: i64 = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    };
    let days = days_from_civil(year, month as i64, day as i64);
    let total_ms = (days as i64).max(0) as u64 * 86_400_000
        + hour * 3_600_000
        + minute * 60_000
        + second * 1000
        + ms;
    Some(total_ms)
}

fn random_uuid_string() -> String {
    let mut hash = Sha256::new();
    hash.update(now_ms().to_be_bytes());
    hash.update(std::process::id().to_be_bytes());
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let mut buf = [0u8; 16];
        if f.read_exact(&mut buf).is_ok() {
            hash.update(buf);
        }
    }
    let digest = hash.finalize();
    let mut out = String::with_capacity(32);
    for byte in &digest[..16] {
        out.push_str(&format!("{byte:02x}"));
    }
    out
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
    async fn materialize_rejects_self_reference() {
        // Mirrors Node `Refusing to materialize a skill into itself,
        // an ancestor, or one of its descendants.` (L3053). Source
        // resolving to the same canonical path as target is a
        // programming error and must surface as
        // `AcpxError::MaterializeSelfReference`.
        let dir = unique_dir("self");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let err = materialize_paperclip_skill_copy(&dir, &dir)
            .await
            .expect_err("self-reference must be rejected");
        assert!(
            matches!(err, AcpxError::MaterializeSelfReference { .. }),
            "expected MaterializeSelfReference, got {err:?}"
        );
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

    // ----- R389: Node-faithful materialize tests -----

    #[tokio::test]
    async fn materialize_rejects_ancestor_target() {
        // Mirrors Node: target inside source is rejected as a
        // descendant reference (L3053).
        let parent = unique_dir("ancestor-parent");
        let child = parent.join("child");
        tokio::fs::create_dir_all(&child).await.unwrap();
        let err = materialize_paperclip_skill_copy(&parent, &child)
            .await
            .expect_err("descendant target must be rejected");
        assert!(matches!(err, AcpxError::MaterializeSelfReference { .. }));
        let _ = tokio::fs::remove_dir_all(&parent).await;
    }

    #[tokio::test]
    async fn materialize_rejects_descendant_source() {
        // Source inside target is also rejected.
        let outer = unique_dir("ancestor-outer");
        let inner = outer.join("inner");
        tokio::fs::create_dir_all(&inner).await.unwrap();
        let err = materialize_paperclip_skill_copy(&inner, &outer)
            .await
            .expect_err("source-inside-target must be rejected");
        assert!(matches!(err, AcpxError::MaterializeSelfReference { .. }));
        let _ = tokio::fs::remove_dir_all(&outer).await;
    }

    #[tokio::test]
    async fn materialize_rejects_symlink_root() {
        // Mirrors Node `Refusing to materialize a skill root that is
        // itself a symlink.` (L3056).
        let real_dir = unique_dir("sl-root-real");
        let link_dir = unique_dir("sl-root-link");
        tokio::fs::create_dir_all(&real_dir).await.unwrap();
        std::os::unix::fs::symlink(&real_dir, &link_dir).unwrap();
        let target = unique_dir("sl-root-tgt");
        let err = materialize_paperclip_skill_copy(&link_dir, &target)
            .await
            .expect_err("symlink root must be rejected");
        assert!(matches!(err, AcpxError::MaterializeSymlinkRoot { .. }));
        let _ = tokio::fs::remove_dir_all(&real_dir).await;
        let _ = tokio::fs::remove_dir_all(&link_dir).await;
        let _ = tokio::fs::remove_dir_all(&target).await;
    }

    #[tokio::test]
    async fn materialize_rejects_non_directory_root() {
        let file = unique_dir("non-dir");
        tokio::fs::write(&file, "not a directory").await.unwrap();
        let target = unique_dir("non-dir-tgt");
        let err = materialize_paperclip_skill_copy(&file, &target)
            .await
            .expect_err("non-directory root must be rejected");
        assert!(matches!(err, AcpxError::MaterializeNotDirectory { .. }));
        let _ = tokio::fs::remove_dir_all(&file).await;
        let _ = tokio::fs::remove_dir_all(&target).await;
    }

    #[tokio::test]
    async fn materialize_writes_sentinel_with_fingerprint() {
        let source = unique_dir("sent-src");
        let target = unique_dir("sent-tgt");
        write_file(&source.join("SKILL.md"), "skill body");
        let result = materialize_paperclip_skill_copy(&source, &target)
            .await
            .unwrap();
        assert!(result.copied_files >= 1);
        let sentinel_path = target.join(MATERIALIZED_SKILL_SENTINEL);
        assert!(tokio::fs::try_exists(&sentinel_path).await.unwrap_or(false));
        let sentinel_raw = tokio::fs::read_to_string(&sentinel_path).await.unwrap();
        let value: serde_json::Value = serde_json::from_str(&sentinel_raw).unwrap();
        assert_eq!(value["version"], 1);
        assert!(value["sourceFingerprint"].is_string());
        assert!(value["copiedFiles"].as_u64().unwrap() >= 1);
        let _ = tokio::fs::remove_dir_all(&source).await;
        let _ = tokio::fs::remove_dir_all(&target).await;
    }

    #[tokio::test]
    async fn materialize_repeated_call_is_cache_hit() {
        let source = unique_dir("cache-src");
        let target = unique_dir("cache-tgt");
        write_file(&source.join("SKILL.md"), "v1");
        let first = materialize_paperclip_skill_copy(&source, &target)
            .await
            .unwrap();
        assert!(first.copied_files >= 1);
        let second = materialize_paperclip_skill_copy(&source, &target)
            .await
            .unwrap();
        // Cache hit: no work done on the second call.
        assert_eq!(second.copied_files, 0);
        assert!(second.skipped_symlinks.is_empty());
        let _ = tokio::fs::remove_dir_all(&source).await;
        let _ = tokio::fs::remove_dir_all(&target).await;
    }

    #[tokio::test]
    async fn materialize_invalidates_cache_when_source_changes() {
        let source = unique_dir("cache-inv-src");
        let target = unique_dir("cache-inv-tgt");
        write_file(&source.join("a.txt"), "v1");
        materialize_paperclip_skill_copy(&source, &target)
            .await
            .unwrap();
        // Mutate the source tree.
        write_file(&source.join("a.txt"), "v2-different");
        let second = materialize_paperclip_skill_copy(&source, &target)
            .await
            .unwrap();
        assert!(second.copied_files >= 1);
        let _ = tokio::fs::remove_dir_all(&source).await;
        let _ = tokio::fs::remove_dir_all(&target).await;
    }

    #[tokio::test]
    async fn hash_skill_directory_is_deterministic() {
        let dir = unique_dir("hash-det");
        write_file(&dir.join("a.txt"), "alpha");
        write_file(&dir.join("b/c.txt"), "beta");
        let a = hash_skill_directory(&dir).await.unwrap();
        let b = hash_skill_directory(&dir).await.unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn hash_skill_directory_changes_with_content() {
        let a = unique_dir("hash-chg-a");
        let b = unique_dir("hash-chg-b");
        write_file(&a.join("f.txt"), "v1");
        write_file(&b.join("f.txt"), "v2");
        let ha = hash_skill_directory(&a).await.unwrap();
        let hb = hash_skill_directory(&b).await.unwrap();
        assert_ne!(ha, hb);
        let _ = tokio::fs::remove_dir_all(&a).await;
        let _ = tokio::fs::remove_dir_all(&b).await;
    }

    #[tokio::test]
    async fn materialized_skill_fingerprint_matches_returns_false_without_sentinel() {
        let dir = unique_dir("no-sentinel");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let result = materialized_skill_fingerprint_matches(&dir, "any-fingerprint").await;
        assert!(!result);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn fingerprint_match_requires_version_one() {
        let dir = unique_dir("ver");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let sentinel = dir.join(MATERIALIZED_SKILL_SENTINEL);
        tokio::fs::write(
            &sentinel,
            "{\"version\": 2, \"sourceFingerprint\": \"x\"}\n",
        )
        .await
        .unwrap();
        let result = materialized_skill_fingerprint_matches(&dir, "x").await;
        assert!(!result, "version mismatch must short-circuit to false");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn is_pid_alive_detects_self_and_zero() {
        assert!(!is_pid_alive(0));
        let self_pid = std::process::id();
        assert!(is_pid_alive(self_pid));
        assert!(!is_pid_alive(u32::MAX / 2));
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
