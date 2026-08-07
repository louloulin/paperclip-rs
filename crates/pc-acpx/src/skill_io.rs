//! `pc-acpx` skill I/O helpers — async filesystem adapters for skill
//! discovery, symlink management, and snapshot reading.
//!
//! Rust port of Node `packages/adapter-utils/src/server-utils.ts`:
//! - `PAPERCLIP_SKILL_ROOT_RELATIVE_CANDIDATES` (L125-128, const)
//! - `isMaintainerOnlySkillTarget` (L290-292)
//! - `resolvePaperclipSkillsDir` (L2440-2457)
//! - `listPaperclipSkillEntries` (L2467-2477)
//! - `readInstalledSkillTargets` (L2481-2490)
//! - `normalizeConfiguredPaperclipRuntimeSkills` (L2740-2767)
//! - `readPaperclipRuntimeSkillEntries` (L2769-2773)
//! - `readPaperclipSkillMarkdown` (L2775-2787)
//! - `ensurePaperclipSkillSymlink` (L2891-2920)
//! - `removeMaintainerOnlySkillSymlinks` (L3121-3160)
//!
//! All helpers are async I/O wrappers around `tokio::fs`. They are the
//! adapter-side counterpart to the pure `skill_snapshot` builders (R388)
//! — every concrete adapter (claude-local / codex-local / gemini-local
//! / grok-local / opencode-local / pi-local) wires them together to
//! implement `listXxxSkills` / `syncXxxSkills` for its runtime.
//!
//! `unsafe_code = "forbid"` is honoured: every helper uses
//! `tokio::fs::*` exclusively.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::skill_snapshot::{
    InstalledSkillTarget, InstalledSkillTargetKind, PaperclipSkillEntry, PaperclipSkillSourceStatus,
};

// ============================================================================
// Constants (Node L125-128)
// ============================================================================

/// Candidate relative paths the adapter probes for the Paperclip skills
/// registry (relative to the adapter `module_dir`). Mirrors Node
/// `PAPERCLIP_SKILL_ROOT_RELATIVE_CANDIDATES` (L125-128).
pub const PAPERCLIP_SKILL_ROOT_RELATIVE_CANDIDATES: &[&str] =
    &["../../skills", "../../../../../skills"];

/// Key prefix attached to every Paperclip-managed skill key. Mirrors
/// Node `paperclipai/paperclip/${entry.name}` (L2475).
pub const PAPERCLIP_SKILL_KEY_PREFIX: &str = "paperclipai/paperclip";

// ============================================================================
// isMaintainerOnlySkillTarget (Node L290-292)
// ============================================================================

/// Replace Windows-style backslashes with POSIX slashes (lexical, no
/// filesystem touch).
fn normalize_path_slashes(value: &str) -> String {
    value.replace('\\', "/")
}

/// Mirrors Node `isMaintainerOnlySkillTarget` (L290-292): a path counts
/// as a maintainer-only target when its normalised form contains
/// `/.agents/skills/`.
pub fn is_maintainer_only_skill_target(candidate: &str) -> bool {
    normalize_path_slashes(candidate).contains("/.agents/skills/")
}

// ============================================================================
// resolvePaperclipSkillsDir (Node L2440-2457)
// ============================================================================

/// Resolve the Paperclip skills directory by probing each
/// `PAPERCLIP_SKILL_ROOT_RELATIVE_CANDIDATES` relative path plus any
/// caller-supplied candidates. Mirrors Node `resolvePaperclipSkillsDir`
/// (L2440-2457).
///
/// The first candidate whose path resolves to an existing directory
/// wins. `seenRoots` deduplicates identical resolutions so callers do
/// not probe the same filesystem location twice.
pub async fn resolve_paperclip_skills_dir(
    module_dir: &Path,
    additional_candidates: &[PathBuf],
) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = PAPERCLIP_SKILL_ROOT_RELATIVE_CANDIDATES
        .iter()
        .map(|relative| resolve_lexical(module_dir, relative))
        .collect();
    candidates.extend(
        additional_candidates
            .iter()
            .map(|candidate| resolve_lexical(candidate, "")),
    );

    let mut seen_roots: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
    for root in candidates {
        if !seen_roots.insert(root.clone()) {
            continue;
        }
        let meta = tokio::fs::metadata(&root).await;
        eprintln!(
            "TRACE root={:?} meta={:?}",
            root,
            meta.as_ref()
                .map(|m| (m.is_dir(), m.len()))
                .map_err(|e| format!("{e:?}"))
        );
        let is_directory = meta.map(|meta| meta.is_dir()).unwrap_or(false);
        if is_directory {
            return Some(root);
        }
    }
    None
}

fn resolve_lexical(base: &Path, relative: &str) -> PathBuf {
    if relative.is_empty() {
        return lex_normalize(base);
    }
    lex_normalize(&base.join(relative))
}

/// Lexically normalize a path: collapse `.` and `..` components without
/// touching the filesystem. Mirrors `path.resolve` in Node, which is
/// purely lexical (no symlink following).
fn lex_normalize(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if let Some(last) = components.last() {
                    match last {
                        std::path::Component::Normal(_) => {
                            components.pop();
                        }
                        std::path::Component::Prefix(_) | std::path::Component::RootDir => {}
                        _ => {
                            components.push(component);
                        }
                    }
                } else {
                    components.push(component);
                }
            }
            other => components.push(other),
        }
    }
    components.iter().collect()
}

// ============================================================================
// listPaperclipSkillEntries (Node L2467-2477)
// ============================================================================

/// Discover Paperclip skill entries by reading `module_dir`'s skills
/// directory. Mirrors Node `listPaperclipSkillEntries` (L2467-2477).
///
/// Returns an empty list when the directory is missing or unreadable.
/// Each entry's `key` is the Paperclip namespace prefix concatenated
/// with the directory name.
pub async fn list_paperclip_skill_entries(
    module_dir: &Path,
    additional_candidates: &[PathBuf],
) -> Vec<PaperclipSkillEntry> {
    let Some(root) = resolve_paperclip_skills_dir(module_dir, additional_candidates).await else {
        return Vec::new();
    };
    let mut out: Vec<PaperclipSkillEntry> = Vec::new();
    let mut entries = match tokio::fs::read_dir(&root).await {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let file_type = match entry.file_type().await {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        out.push(PaperclipSkillEntry {
            key: format!("{PAPERCLIP_SKILL_KEY_PREFIX}/{name}"),
            runtime_name: name.clone(),
            source: root.join(&name).to_string_lossy().into_owned(),
            version_id: None,
            current_version_id: None,
            source_status: PaperclipSkillSourceStatus::Available,
            missing_detail: None,
        });
    }
    out
}

// ============================================================================
// readInstalledSkillTargets (Node L2481-2490)
// ============================================================================

/// Read every entry in `skills_home` and classify it as a symlink,
/// directory, or file. Mirrors Node `readInstalledSkillTargets`
/// (L2481-2490). A missing directory yields an empty map.
pub async fn read_installed_skill_targets(
    skills_home: &Path,
) -> BTreeMap<String, InstalledSkillTarget> {
    let mut out: BTreeMap<String, InstalledSkillTarget> = BTreeMap::new();
    let mut entries = match tokio::fs::read_dir(skills_home).await {
        Ok(entries) => entries,
        Err(_) => return out,
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        let full_path = skills_home.join(&name);
        let metadata = match tokio::fs::symlink_metadata(&full_path).await {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        let (target_path, kind) = if metadata.file_type().is_symlink() {
            let linked_path = tokio::fs::read_link(&full_path)
                .await
                .ok()
                .map(|target| target.to_string_lossy().into_owned());
            let resolved = linked_path.as_deref().map(|raw| {
                if Path::new(raw).is_absolute() {
                    PathBuf::from(raw)
                } else {
                    skills_home.join(raw)
                }
            });
            (
                resolved.map(|p| p.to_string_lossy().into_owned()),
                InstalledSkillTargetKind::Symlink,
            )
        } else if metadata.is_dir() {
            (
                Some(full_path.to_string_lossy().into_owned()),
                InstalledSkillTargetKind::Directory,
            )
        } else {
            (
                Some(full_path.to_string_lossy().into_owned()),
                InstalledSkillTargetKind::File,
            )
        };
        out.insert(name, InstalledSkillTarget { target_path, kind });
    }
    out
}

// ============================================================================
// normalizeConfiguredPaperclipRuntimeSkills (Node L2740-2767)
// ============================================================================

/// Parse `config.paperclipRuntimeSkills` into a list of typed
/// `PaperclipSkillEntry` values. Mirrors Node
/// `normalizeConfiguredPaperclipRuntimeSkills` (L2740-2767) — invalid
/// or partial entries are silently dropped.
pub fn normalize_configured_paperclip_runtime_skills(
    value: Option<&Value>,
) -> Vec<PaperclipSkillEntry> {
    let Some(value) = value else {
        return Vec::new();
    };
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    let mut out: Vec<PaperclipSkillEntry> = Vec::new();
    for raw_entry in items {
        let Some(entry) = parse_object(raw_entry) else {
            continue;
        };
        let key = as_trimmed_string(entry.get("key"))
            .or_else(|| as_trimmed_string(entry.get("name")))
            .unwrap_or_default();
        let runtime_name = as_trimmed_string(entry.get("runtimeName"))
            .or_else(|| as_trimmed_string(entry.get("name")))
            .unwrap_or_default();
        let source = as_trimmed_string(entry.get("source")).unwrap_or_default();
        if key.is_empty() || runtime_name.is_empty() || source.is_empty() {
            continue;
        }
        let version_id = non_blank_string(entry.get("versionId"));
        let current_version_id = non_blank_string(entry.get("currentVersionId"));
        let source_status = match entry.get("sourceStatus").and_then(Value::as_str) {
            Some("missing") => PaperclipSkillSourceStatus::Missing,
            _ => PaperclipSkillSourceStatus::Available,
        };
        let missing_detail = non_blank_string(entry.get("missingDetail"));
        out.push(PaperclipSkillEntry {
            key,
            runtime_name,
            source,
            version_id,
            current_version_id,
            source_status,
            missing_detail,
        });
    }
    out
}

fn parse_object(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

fn as_trimmed_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn non_blank_string(value: Option<&Value>) -> Option<String> {
    as_trimmed_string(value)
}

// ============================================================================
// readPaperclipRuntimeSkillEntries (Node L2769-2773)
// ============================================================================

/// Read the configured `paperclipRuntimeSkills` from `config`. When
/// the block contains at least one entry, return it verbatim;
/// otherwise fall back to the on-disk skills registry discovered via
/// `listPaperclipSkillEntries`. Mirrors Node
/// `readPaperclipRuntimeSkillEntries` (L2769-2773).
pub async fn read_paperclip_runtime_skill_entries(
    config: &Map<String, Value>,
    module_dir: &Path,
    additional_candidates: &[PathBuf],
) -> Vec<PaperclipSkillEntry> {
    let configured =
        normalize_configured_paperclip_runtime_skills(config.get("paperclipRuntimeSkills"));
    if !configured.is_empty() {
        return configured;
    }
    list_paperclip_skill_entries(module_dir, additional_candidates).await
}

// ============================================================================
// readPaperclipSkillMarkdown (Node L2775-2787)
// ============================================================================

/// Read a single skill's `SKILL.md` markdown body, looking the skill
/// up by lower-cased key. Mirrors Node `readPaperclipSkillMarkdown`
/// (L2775-2787).
pub async fn read_paperclip_skill_markdown(module_dir: &Path, skill_key: &str) -> Option<String> {
    let normalized = skill_key.trim().to_lowercase();
    if normalized.is_empty() {
        return None;
    }
    let entries = list_paperclip_skill_entries(module_dir, &[]).await;
    let match_entry = entries
        .iter()
        .find(|entry| entry.key.to_lowercase() == normalized)?;
    let path = PathBuf::from(&match_entry.source).join("SKILL.md");
    tokio::fs::read_to_string(&path).await.ok()
}

// ============================================================================
// ensurePaperclipSkillSymlink (Node L2891-2920)
// ============================================================================

/// Result of `ensure_paperclip_skill_symlink`. Mirrors Node
/// `"created" | "repaired" | "skipped"` (L2891).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSymlinkOutcome {
    Created,
    Repaired,
    Skipped,
}

/// Create or repair the symlink at `target` so it points at `source`.
/// Mirrors Node `ensurePaperclipSkillSymlink` (L2891-2920).
///
/// - When `target` does not exist: create the symlink (`Created`).
/// - When `target` is not a symlink: do nothing (`Skipped`).
/// - When the existing symlink already points at `source`: do nothing
///   (`Skipped`).
/// - When the existing symlink's target resolves to a real path: do
///   nothing (`Skipped`) so we do not clobber an external install.
/// - Otherwise: unlink the stale target and create a fresh symlink
///   (`Repaired`).
///
/// `link_skill` defaults to `tokio::fs::symlink`; tests inject a
/// recorder so they can verify the create/repair call without
/// touching the filesystem.
pub async fn ensure_paperclip_skill_symlink(source: &Path, target: &Path) -> SkillSymlinkOutcome {
    ensure_paperclip_skill_symlink_with_linker(source, target, |src, dst| async move {
        #[cfg(unix)]
        {
            tokio::fs::symlink(src, dst).await
        }
        #[cfg(not(unix))]
        {
            let _ = (src, dst);
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "symlink not supported on this platform",
            ))
        }
    })
    .await
}

/// Custom-linker variant. The closure receives the resolved source and
/// target paths and returns a future that performs the actual
/// `symlink` call.
pub async fn ensure_paperclip_skill_symlink_with_linker<F, Fut>(
    source: &Path,
    target: &Path,
    link_skill: F,
) -> SkillSymlinkOutcome
where
    F: FnOnce(PathBuf, PathBuf) -> Fut,
    Fut: std::future::Future<Output = std::io::Result<()>>,
{
    let source_buf = source.to_path_buf();
    let target_buf = target.to_path_buf();
    let existing = tokio::fs::symlink_metadata(&target_buf).await.ok();
    if existing.is_none() {
        match link_skill(source_buf, target_buf.clone()).await {
            Ok(()) => return SkillSymlinkOutcome::Created,
            Err(_) => return SkillSymlinkOutcome::Skipped,
        }
    }
    let existing = match existing {
        Some(meta) => meta,
        None => return SkillSymlinkOutcome::Skipped,
    };
    if !existing.file_type().is_symlink() {
        return SkillSymlinkOutcome::Skipped;
    }
    let linked_path = match tokio::fs::read_link(&target_buf).await {
        Ok(p) => p,
        Err(_) => return SkillSymlinkOutcome::Skipped,
    };
    let resolved_linked_path: PathBuf = if linked_path.is_absolute() {
        linked_path
    } else {
        match target_buf.parent() {
            Some(parent) => parent.join(&linked_path),
            None => PathBuf::from(linked_path),
        }
    };
    if resolved_linked_path == source_buf {
        return SkillSymlinkOutcome::Skipped;
    }
    let linked_path_exists = tokio::fs::metadata(&resolved_linked_path).await.is_ok();
    if linked_path_exists {
        return SkillSymlinkOutcome::Skipped;
    }
    let _ = tokio::fs::remove_file(&target_buf).await;
    match link_skill(source_buf, target_buf).await {
        Ok(()) => SkillSymlinkOutcome::Repaired,
        Err(_) => SkillSymlinkOutcome::Skipped,
    }
}

// ============================================================================
// removeMaintainerOnlySkillSymlinks (Node L3121-3160)
// ============================================================================

/// Remove any symlink in `skills_home` whose target (raw or
/// resolved) lives under `/.agents/skills/`, except for entries whose
/// names are in `allowed_skill_names`. Mirrors Node
/// `removeMaintainerOnlySkillSymlinks` (L3121-3160).
///
/// Returns the list of removed entry names. Missing `skills_home`
/// directories yield an empty list.
pub async fn remove_maintainer_only_skill_symlinks(
    skills_home: &Path,
    allowed_skill_names: &[String],
) -> Vec<String> {
    let allowed: std::collections::BTreeSet<String> = allowed_skill_names.iter().cloned().collect();
    let mut entries = match tokio::fs::read_dir(skills_home).await {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    let mut removed: Vec<String> = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        if allowed.contains(&name) {
            continue;
        }
        let target = skills_home.join(&name);
        let existing = match tokio::fs::symlink_metadata(&target).await {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        if !existing.file_type().is_symlink() {
            continue;
        }
        let linked_path = match tokio::fs::read_link(&target).await {
            Ok(p) => p.to_string_lossy().into_owned(),
            Err(_) => continue,
        };
        let resolved_linked_path = if Path::new(&linked_path).is_absolute() {
            linked_path.clone()
        } else {
            target
                .parent()
                .map(|parent| parent.join(&linked_path).to_string_lossy().into_owned())
                .unwrap_or(linked_path.clone())
        };
        if !is_maintainer_only_skill_target(&linked_path)
            && !is_maintainer_only_skill_target(&resolved_linked_path)
        {
            continue;
        }
        if tokio::fs::remove_file(&target).await.is_ok() {
            removed.push(name);
        }
    }
    removed
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn unique_dir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "pc-acpx-skill-io-{label}-{nanos}-{}",
            std::process::id()
        ))
    }

    // ----- Constants -----

    #[test]
    fn candidates_match_node_literal() {
        assert_eq!(
            PAPERCLIP_SKILL_ROOT_RELATIVE_CANDIDATES,
            &["../../skills", "../../../../../skills"]
        );
        assert_eq!(PAPERCLIP_SKILL_KEY_PREFIX, "paperclipai/paperclip");
    }

    // ----- isMaintainerOnlySkillTarget -----

    #[test]
    fn maintainer_target_detection_matches_node() {
        assert!(is_maintainer_only_skill_target(
            "/home/alice/.agents/skills/foo"
        ));
        assert!(is_maintainer_only_skill_target("~/.agents/skills/bar"));
        assert!(!is_maintainer_only_skill_target("/home/alice/skills/foo"));
        // Backslashes are normalised to forward slashes before matching.
        assert!(is_maintainer_only_skill_target(
            "C:\\Users\\alice\\.agents\\skills\\foo"
        ));
    }

    // ----- resolvePaperclipSkillsDir -----

    #[tokio::test]
    async fn resolve_returns_none_when_no_candidate_exists() {
        // Use a wholly-isolated parent so we cannot collide with any
        // pre-existing `skills` directory on the host. The relative
        // candidates are still rooted at module_dir, so without us
        // creating them they must not exist.
        let parent =
            std::env::temp_dir().join(format!("pc-acpx-skill-io-iso-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&parent).await;
        tokio::fs::create_dir_all(&parent).await.unwrap();
        let module_dir = parent.join("a").join("b");
        tokio::fs::create_dir_all(&module_dir).await.unwrap();
        let resolved = resolve_paperclip_skills_dir(&module_dir, &[]).await;
        assert!(resolved.is_none());
        let _ = tokio::fs::remove_dir_all(&parent).await;
    }

    #[tokio::test]
    async fn resolve_picks_first_existing_relative_candidate() {
        let parent =
            std::env::temp_dir().join(format!("pc-acpx-skill-io-first-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&parent).await;
        tokio::fs::create_dir_all(&parent).await.unwrap();
        let module_dir = parent.join("a").join("b");
        tokio::fs::create_dir_all(&module_dir).await.unwrap();
        let skills_dir = parent.join("skills");
        tokio::fs::create_dir_all(&skills_dir).await.unwrap();
        let resolved = resolve_paperclip_skills_dir(&module_dir, &[]).await;
        assert_eq!(resolved, Some(lex_normalize(&skills_dir)));
        let _ = tokio::fs::remove_dir_all(&parent).await;
    }

    // ----- listPaperclipSkillEntries -----

    #[tokio::test]
    async fn list_returns_empty_when_root_missing() {
        let parent =
            std::env::temp_dir().join(format!("pc-acpx-skill-io-listempty-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&parent).await;
        tokio::fs::create_dir_all(&parent).await.unwrap();
        let module_dir = parent.join("a").join("b");
        tokio::fs::create_dir_all(&module_dir).await.unwrap();
        let entries = list_paperclip_skill_entries(&module_dir, &[]).await;
        assert!(entries.is_empty());
        let _ = tokio::fs::remove_dir_all(&parent).await;
    }

    #[tokio::test]
    async fn list_emits_namespaced_keys() {
        let parent =
            std::env::temp_dir().join(format!("pc-acpx-skill-io-list-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&parent).await;
        tokio::fs::create_dir_all(&parent).await.unwrap();
        let module_dir = parent.join("a").join("b");
        tokio::fs::create_dir_all(&module_dir).await.unwrap();
        let skills_dir = parent.join("skills");
        tokio::fs::create_dir_all(skills_dir.join("alpha"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(skills_dir.join("beta"))
            .await
            .unwrap();
        tokio::fs::write(skills_dir.join("not-a-dir.txt"), "skip me")
            .await
            .unwrap();
        let entries = list_paperclip_skill_entries(&module_dir, &[]).await;
        assert_eq!(entries.len(), 2);
        let names: Vec<&str> = entries.iter().map(|e| e.runtime_name.as_str()).collect();
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"beta"));
        for entry in &entries {
            assert!(entry.key.starts_with("paperclipai/paperclip/"));
            assert_eq!(
                entry.source,
                lex_normalize(&skills_dir.join(&entry.runtime_name)).to_string_lossy()
            );
        }
        let _ = tokio::fs::remove_dir_all(&parent).await;
    }

    // ----- readInstalledSkillTargets -----

    #[tokio::test]
    async fn read_installed_returns_empty_when_missing() {
        let dir = unique_dir("inst-empty");
        let map = read_installed_skill_targets(&dir).await;
        assert!(map.is_empty());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn read_installed_classifies_entries() {
        let dir = unique_dir("inst-mixed");
        tokio::fs::create_dir_all(dir.join("a-dir")).await.unwrap();
        tokio::fs::write(dir.join("a-file.txt"), "data")
            .await
            .unwrap();
        let target = unique_dir("inst-link-target");
        tokio::fs::create_dir_all(&target).await.unwrap();
        std::os::unix::fs::symlink(&target, dir.join("a-link")).unwrap();
        let map = read_installed_skill_targets(&dir).await;
        assert_eq!(
            map.get("a-dir").unwrap().kind,
            InstalledSkillTargetKind::Directory
        );
        assert_eq!(
            map.get("a-file.txt").unwrap().kind,
            InstalledSkillTargetKind::File
        );
        assert_eq!(
            map.get("a-link").unwrap().kind,
            InstalledSkillTargetKind::Symlink
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
        let _ = tokio::fs::remove_dir_all(&target).await;
    }

    // ----- normalizeConfiguredPaperclipRuntimeSkills -----

    #[test]
    fn normalize_drops_invalid_entries() {
        let value = serde_json::json!([
            { "key": "alpha", "runtimeName": "alpha", "source": "/skills/alpha" },
            { "key": "beta", "source": "/skills/beta" },
            { "runtimeName": "gamma", "source": "/skills/gamma" },
            { "key": "", "runtimeName": "empty", "source": "/skills/empty" },
            "not-an-object",
        ]);
        let entries = normalize_configured_paperclip_runtime_skills(Some(&value));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "alpha");
        assert_eq!(entries[0].runtime_name, "alpha");
    }

    #[test]
    fn normalize_trims_strings_and_handles_missing_fields() {
        let value = serde_json::json!([
            { "key": "  spaced  ", "runtimeName": "spaced", "source": "/skills/spaced", "versionId": "  v1  " },
            { "key": "missing-version", "runtimeName": "missing-version", "source": "/skills/missing-version" },
            { "key": "missing-source-status", "runtimeName": "missing-source-status", "source": "/x", "sourceStatus": "missing", "missingDetail": "  reason  " },
        ]);
        let entries = normalize_configured_paperclip_runtime_skills(Some(&value));
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].key, "spaced");
        assert_eq!(entries[0].version_id, Some("v1".to_string()));
        assert!(entries[1].version_id.is_none());
        assert_eq!(
            entries[2].source_status,
            PaperclipSkillSourceStatus::Missing
        );
        assert_eq!(entries[2].missing_detail, Some("reason".to_string()));
    }

    #[test]
    fn normalize_accepts_non_array_or_null() {
        let null = serde_json::json!(null);
        let array = serde_json::json!([1, 2, 3]);
        assert!(normalize_configured_paperclip_runtime_skills(Some(&null)).is_empty());
        assert!(normalize_configured_paperclip_runtime_skills(Some(&array)).is_empty());
        assert!(normalize_configured_paperclip_runtime_skills(None).is_empty());
    }

    // ----- readPaperclipRuntimeSkillEntries -----

    #[tokio::test]
    async fn read_runtime_prefers_configured_over_filesystem() {
        let module_dir = unique_dir("runtime-prefer");
        let skills_dir = module_dir.join("../../skills");
        tokio::fs::create_dir_all(skills_dir.join("from-fs"))
            .await
            .unwrap();
        let mut config = serde_json::Map::new();
        config.insert(
            "paperclipRuntimeSkills".to_string(),
            serde_json::json!([
                { "key": "from-config", "runtimeName": "from-config", "source": "/skills/from-config" }
            ]),
        );
        let entries = read_paperclip_runtime_skill_entries(&config, &module_dir, &[]).await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "from-config");
        let _ = tokio::fs::remove_dir_all(&module_dir).await;
    }

    // ----- readPaperclipSkillMarkdown -----

    #[tokio::test]
    async fn read_markdown_returns_body_for_matching_key() {
        let module_dir = unique_dir("md-match");
        let skills_dir = module_dir.join("../../skills");
        tokio::fs::create_dir_all(skills_dir.join("alpha"))
            .await
            .unwrap();
        tokio::fs::write(skills_dir.join("alpha").join("SKILL.md"), "# alpha body")
            .await
            .unwrap();
        let body = read_paperclip_skill_markdown(&module_dir, "paperclipai/paperclip/alpha")
            .await
            .unwrap();
        assert_eq!(body, "# alpha body");
        let _ = tokio::fs::remove_dir_all(&module_dir).await;
    }

    #[tokio::test]
    async fn read_markdown_returns_none_for_unknown_key() {
        let module_dir = unique_dir("md-none");
        let skills_dir = module_dir.join("../../skills");
        tokio::fs::create_dir_all(skills_dir.join("alpha"))
            .await
            .unwrap();
        let body = read_paperclip_skill_markdown(&module_dir, "ghost").await;
        assert!(body.is_none());
        let _ = tokio::fs::remove_dir_all(&module_dir).await;
    }

    // ----- ensurePaperclipSkillSymlink -----

    #[tokio::test]
    #[cfg(unix)]
    async fn ensure_creates_when_target_missing() {
        let source = unique_dir("ensure-src");
        let target = unique_dir("ensure-tgt");
        tokio::fs::create_dir_all(&source).await.unwrap();
        let outcome = ensure_paperclip_skill_symlink(&source, &target).await;
        assert_eq!(outcome, SkillSymlinkOutcome::Created);
        let meta = tokio::fs::symlink_metadata(&target).await.unwrap();
        assert!(meta.file_type().is_symlink());
        let _ = tokio::fs::remove_dir_all(&source).await;
        let _ = tokio::fs::remove_dir_all(&target).await;
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn ensure_skips_when_target_is_correct_link() {
        let source = unique_dir("ensure-correct");
        let target = unique_dir("ensure-correct-tgt");
        tokio::fs::create_dir_all(&source).await.unwrap();
        std::os::unix::fs::symlink(&source, &target).unwrap();
        let outcome = ensure_paperclip_skill_symlink(&source, &target).await;
        assert_eq!(outcome, SkillSymlinkOutcome::Skipped);
        let _ = tokio::fs::remove_dir_all(&source).await;
        let _ = tokio::fs::remove_dir_all(&target).await;
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn ensure_skips_when_target_is_regular_file() {
        let source = unique_dir("ensure-file");
        let target = unique_dir("ensure-file-tgt");
        tokio::fs::create_dir_all(&source).await.unwrap();
        tokio::fs::write(&target, "not a symlink").await.unwrap();
        let outcome = ensure_paperclip_skill_symlink(&source, &target).await;
        assert_eq!(outcome, SkillSymlinkOutcome::Skipped);
        let _ = tokio::fs::remove_dir_all(&source).await;
        let _ = tokio::fs::remove_file(&target).await;
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn ensure_skips_when_target_resolves_to_existing_path() {
        let real = unique_dir("ensure-real");
        let target = unique_dir("ensure-real-tgt");
        let bogus_source = unique_dir("ensure-bogus");
        tokio::fs::create_dir_all(&real).await.unwrap();
        tokio::fs::create_dir_all(&bogus_source).await.unwrap();
        std::os::unix::fs::symlink(&real, &target).unwrap();
        let outcome = ensure_paperclip_skill_symlink(&bogus_source, &target).await;
        // Linked path resolves to a real location → do not clobber.
        assert_eq!(outcome, SkillSymlinkOutcome::Skipped);
        let _ = tokio::fs::remove_dir_all(&real).await;
        let _ = tokio::fs::remove_dir_all(&target).await;
        let _ = tokio::fs::remove_dir_all(&bogus_source).await;
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn ensure_repairs_when_target_link_is_broken() {
        let source = unique_dir("ensure-rep-src");
        let target = unique_dir("ensure-rep-tgt");
        let stale = unique_dir("ensure-rep-stale");
        tokio::fs::create_dir_all(&source).await.unwrap();
        tokio::fs::create_dir_all(&stale).await.unwrap();
        // Symlink target points to a path that no longer exists.
        std::os::unix::fs::symlink(&stale, &target).unwrap();
        let _ = tokio::fs::remove_dir_all(&stale).await;
        let outcome = ensure_paperclip_skill_symlink(&source, &target).await;
        assert_eq!(outcome, SkillSymlinkOutcome::Repaired);
        let meta = tokio::fs::symlink_metadata(&target).await.unwrap();
        assert!(meta.file_type().is_symlink());
        let _ = tokio::fs::remove_dir_all(&source).await;
        let _ = tokio::fs::remove_dir_all(&target).await;
    }

    #[tokio::test]
    async fn ensure_uses_injected_linker() {
        let calls = Arc::new(Mutex::new(Vec::<(PathBuf, PathBuf)>::new()));
        let calls_for_linker = calls.clone();
        let outcome = ensure_paperclip_skill_symlink_with_linker(
            &PathBuf::from("/source"),
            &PathBuf::from("/target"),
            move |src, dst| {
                let calls = calls_for_linker.clone();
                async move {
                    calls.lock().await.push((src, dst));
                    Ok(())
                }
            },
        )
        .await;
        assert_eq!(outcome, SkillSymlinkOutcome::Created);
        let recorded = calls.lock().await.clone();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, PathBuf::from("/source"));
        assert_eq!(recorded[0].1, PathBuf::from("/target"));
    }

    // ----- removeMaintainerOnlySkillSymlinks -----

    #[tokio::test]
    #[cfg(unix)]
    async fn remove_maintainer_only_drops_only_under_dot_agents() {
        // skills_home — the directory scanned for stale symlinks.
        let skills_home = unique_dir("rm-maint");
        // Maintainer-only target root: the symlinks we want removed
        // point INTO this directory.
        let maintainer_root = unique_dir("rm-maint-agents");
        let dot_agents_skills = maintainer_root.join(".agents").join("skills");
        tokio::fs::create_dir_all(&dot_agents_skills).await.unwrap();
        tokio::fs::create_dir_all(skills_home.join("extra"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(&skills_home).await.unwrap();
        // foo-link -> .../.agents/skills/foo — should be removed.
        std::os::unix::fs::symlink(dot_agents_skills.join("foo"), skills_home.join("foo-link"))
            .unwrap();
        // bar-link -> .../extra/bar — not under .agents, should remain.
        std::os::unix::fs::symlink(
            skills_home.join("extra").join("bar"),
            skills_home.join("bar-link"),
        )
        .unwrap();
        // baz-link -> .../.agents/skills/baz — but on allowed list.
        std::os::unix::fs::symlink(dot_agents_skills.join("baz"), skills_home.join("baz-link"))
            .unwrap();

        let removed =
            remove_maintainer_only_skill_symlinks(&skills_home, &[String::from("baz-link")]).await;
        assert_eq!(removed, vec!["foo-link".to_string()]);
        assert!(tokio::fs::symlink_metadata(skills_home.join("foo-link"))
            .await
            .is_err());
        assert!(tokio::fs::symlink_metadata(skills_home.join("baz-link"))
            .await
            .is_ok());
        assert!(tokio::fs::symlink_metadata(skills_home.join("bar-link"))
            .await
            .is_ok());

        let _ = tokio::fs::remove_dir_all(&skills_home).await;
        let _ = tokio::fs::remove_dir_all(&maintainer_root).await;
    }

    #[tokio::test]
    async fn remove_maintainer_only_returns_empty_when_missing() {
        let dir = unique_dir("rm-maint-missing");
        let removed = remove_maintainer_only_skill_symlinks(&dir, &[]).await;
        assert!(removed.is_empty());
    }
}
