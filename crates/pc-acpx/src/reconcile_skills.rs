//! `pc-acpx` managed Codex skill reconciliation — port of
//! `reconcileManagedCodexSkills` from Node `acpx-engine/execute.ts`.
//!
//! Reconciliation removes stale entries from the materialized
//! `skills/` home before the engine injects the desired set:
//!
//! 1. **Phase 1 — managed no longer desired**: every name in the
//!    manifest that the caller did not re-request is removed.
//! 2. **Phase 2 — legacy symlinks**: any existing symlink whose target
//!    no longer points at a desired source is removed (this catches
//!    leftover artifacts from older paperclip versions that used
//!    symlinks instead of materialized copies).
//! 3. **Phase 3 — managed but unavailable**: any managed name whose
//!    source disappeared from the available set is removed (defensive:
//!    if the manifest still references a name we cannot resolve, we
//!    drop it so the agent never sees a stale pointer).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::error::AcpxError;
use crate::fs_ops::{lstat_or_none, readlink_or_none, remove_path_if_exists};
use crate::managed_home::{read_managed_codex_skills_manifest, LogStream, OnLogSink};
use crate::skill_materialize::PaperclipSkillEntry;

/// Input for [`reconcile_managed_codex_skills`]. Mirrors the Node
/// `reconcileManagedCodexSkills` argument shape.
#[derive(Clone)]
pub struct ReconcileManagedCodexSkillsInput {
    pub skills_home: PathBuf,
    pub all_skills: Vec<PaperclipSkillEntry>,
    pub selected_skills: Vec<PaperclipSkillEntry>,
    pub on_log: Option<OnLogSink>,
}

impl std::fmt::Debug for ReconcileManagedCodexSkillsInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReconcileManagedCodexSkillsInput")
            .field("skills_home", &self.skills_home)
            .field("all_skills", &self.all_skills.len())
            .field("selected_skills", &self.selected_skills.len())
            .field("on_log", &self.on_log.as_ref().map(|_| "<sink>"))
            .finish()
    }
}

impl ReconcileManagedCodexSkillsInput {
    pub fn new(
        skills_home: impl Into<PathBuf>,
        all_skills: Vec<PaperclipSkillEntry>,
        selected_skills: Vec<PaperclipSkillEntry>,
    ) -> Self {
        Self {
            skills_home: skills_home.into(),
            all_skills,
            selected_skills,
            on_log: None,
        }
    }

    pub fn with_on_log(mut self, on_log: OnLogSink) -> Self {
        self.on_log = Some(on_log);
        self
    }
}

/// Phase-1 outcome for a single name — exposed so callers / tests can
/// inspect exactly which entries were revoked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevocationRecord {
    pub name: String,
    pub phase: RevocationPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevocationPhase {
    ManagedNoLongerDesired,
    LegacySymlink,
    ManagedButUnavailable,
}

/// Reconcile the per-`skills/` home against the desired and available
/// skill sets. Returns the list of revocations (with phase label) so
/// tests can assert the exact set of removals.
pub async fn reconcile_managed_codex_skills(
    input: ReconcileManagedCodexSkillsInput,
) -> Result<Vec<RevocationRecord>, AcpxError> {
    let desired: BTreeSet<String> = input
        .selected_skills
        .iter()
        .map(|entry| entry.runtime_name.clone())
        .collect();
    let managed = read_managed_codex_skills_manifest(&input.skills_home)
        .await
        .name_set();
    let available_by_runtime_name: HashMap<String, PaperclipSkillEntry> = input
        .all_skills
        .iter()
        .map(|entry| (entry.runtime_name.clone(), entry.clone()))
        .collect();

    let mut revocations = Vec::new();

    // Phase 1 — managed no longer desired.
    for name in managed.iter() {
        if desired.contains(name) {
            continue;
        }
        let target = input.skills_home.join(name);
        if remove_path_if_exists(&target).await? {
            log_revoke(
                input.on_log.as_ref(),
                LogStream::Stdout,
                "Revoked",
                name,
                &input.skills_home,
            );
            revocations.push(RevocationRecord {
                name: name.clone(),
                phase: RevocationPhase::ManagedNoLongerDesired,
            });
        }
    }

    // Phase 2 — legacy symlinks (no longer desired and not in the manifest).
    for entry in &input.all_skills {
        if desired.contains(&entry.runtime_name) || managed.contains(&entry.runtime_name) {
            continue;
        }
        let target = input.skills_home.join(&entry.runtime_name);
        let Some(existing) = lstat_or_none(&target).await else {
            continue;
        };
        if !existing.file_type().is_symlink() {
            continue;
        }
        let Some(linked_path) = readlink_or_none(&target).await else {
            continue;
        };
        let resolved_linked_path = resolve_link(&target, &linked_path);
        if resolved_linked_path != resolve_absolute(&entry.source) {
            continue;
        }
        if remove_path_if_exists(&target).await? {
            log_revoke(
                input.on_log.as_ref(),
                LogStream::Stdout,
                "Revoked legacy",
                &entry.runtime_name,
                &input.skills_home,
            );
            revocations.push(RevocationRecord {
                name: entry.runtime_name.clone(),
                phase: RevocationPhase::LegacySymlink,
            });
        }
    }

    // Phase 3 — managed but unavailable in `all_skills`.
    for name in managed.iter() {
        if desired.contains(name) || available_by_runtime_name.contains_key(name) {
            continue;
        }
        let target = input.skills_home.join(name);
        if remove_path_if_exists(&target).await? {
            log_revoke(
                input.on_log.as_ref(),
                LogStream::Stdout,
                "Revoked unavailable",
                name,
                &input.skills_home,
            );
            revocations.push(RevocationRecord {
                name: name.clone(),
                phase: RevocationPhase::ManagedButUnavailable,
            });
        }
    }

    Ok(revocations)
}

fn log_revoke(
    sink: Option<&OnLogSink>,
    stream: LogStream,
    prefix: &str,
    name: &str,
    skills_home: &Path,
) {
    if let Some(sink) = sink {
        let line = format!(
            "[paperclip] {prefix} ACPX Codex skill \"{name}\" from {}\n",
            skills_home.display()
        );
        sink(stream, &line);
    }
}

fn resolve_link(target: &Path, linked: &Path) -> PathBuf {
    let combined = if linked.is_absolute() {
        linked.to_path_buf()
    } else {
        target
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(linked)
    };
    // macOS resolves `/var` to `/private/var`; canonicalize both sides so
    // the equality check does not depend on which side was created via
    // an absolute path vs a realpath-resolved path.
    combined.canonicalize().unwrap_or(combined)
}

fn resolve_absolute(candidate: &Path) -> PathBuf {
    candidate
        .canonicalize()
        .unwrap_or_else(|_| candidate.to_path_buf())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    fn unique_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "pc-acpx-reconcile-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ))
    }

    fn entry(key: &str, name: &str, source: PathBuf) -> PaperclipSkillEntry {
        PaperclipSkillEntry {
            key: key.into(),
            runtime_name: name.into(),
            source,
            version_id: None,
            current_version_id: None,
            source_status: Some(crate::skill_materialize::SkillSourceStatus::Available),
            missing_detail: None,
        }
    }

    fn capture_sink() -> (Arc<Mutex<Vec<(LogStream, String)>>>, OnLogSink) {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_for_closure = captured.clone();
        let sink: OnLogSink = Arc::new(move |stream, line| {
            captured_for_closure
                .lock()
                .unwrap()
                .push((stream, line.to_string()));
        });
        (captured, sink)
    }

    #[tokio::test]
    async fn phase_one_revokes_managed_no_longer_desired() {
        let home = unique_dir("phase1");
        let src_a = unique_dir("src-a");
        let src_b = unique_dir("src-b");
        // Materialize two skills into the home (the manifest will pick
        // them up via the reconcile call).
        tokio::fs::create_dir_all(home.join("skill-alpha"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(home.join("skill-beta"))
            .await
            .unwrap();
        tokio::fs::write(home.join("skill-alpha/SKILL.md"), "alpha")
            .await
            .unwrap();
        tokio::fs::write(home.join("skill-beta/SKILL.md"), "beta")
            .await
            .unwrap();
        // Seed the manifest manually so Phase 1 has work to do.
        let manifest =
            crate::managed_home::ManagedSkillsManifest::from_names(["skill-alpha", "skill-beta"]);
        crate::managed_home::write_managed_codex_skills_manifest(&home, &manifest)
            .await
            .unwrap();

        let selected = vec![entry("alpha", "skill-alpha", src_a.clone())];
        let all = vec![
            entry("alpha", "skill-alpha", src_a.clone()),
            entry("beta", "skill-beta", src_b.clone()),
        ];
        let input = ReconcileManagedCodexSkillsInput::new(home.clone(), all, selected);
        let revocations = reconcile_managed_codex_skills(input).await.unwrap();
        assert_eq!(revocations.len(), 1);
        assert_eq!(revocations[0].name, "skill-beta");
        assert_eq!(
            revocations[0].phase,
            RevocationPhase::ManagedNoLongerDesired
        );
        assert!(tokio::fs::try_exists(home.join("skill-alpha"))
            .await
            .unwrap_or(false));
        assert!(!tokio::fs::try_exists(home.join("skill-beta"))
            .await
            .unwrap_or(false));

        let _ = tokio::fs::remove_dir_all(&home).await;
        let _ = tokio::fs::remove_dir_all(&src_a).await;
        let _ = tokio::fs::remove_dir_all(&src_b).await;
    }

    #[tokio::test]
    async fn phase_three_is_safety_net_after_phase_one() {
        let home = unique_dir("phase3");
        tokio::fs::create_dir_all(home.join("orphan"))
            .await
            .unwrap();
        tokio::fs::write(home.join("orphan/SKILL.md"), "orphan")
            .await
            .unwrap();
        let manifest = crate::managed_home::ManagedSkillsManifest::from_names(["orphan"]);
        crate::managed_home::write_managed_codex_skills_manifest(&home, &manifest)
            .await
            .unwrap();

        // `orphan` is NOT in selected and NOT in all_skills, so Phase 1
        // removes it. Phase 3 then re-checks the manifest entry but the
        // file is already gone, so it must be a no-op.
        let input = ReconcileManagedCodexSkillsInput::new(home.clone(), vec![], vec![]);
        let revocations = reconcile_managed_codex_skills(input).await.unwrap();
        assert_eq!(revocations.len(), 1);
        assert_eq!(revocations[0].name, "orphan");
        assert_eq!(
            revocations[0].phase,
            RevocationPhase::ManagedNoLongerDesired
        );
        assert!(!tokio::fs::try_exists(home.join("orphan"))
            .await
            .unwrap_or(false));
        let _ = tokio::fs::remove_dir_all(&home).await;
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn phase_two_revokes_legacy_symlink() {
        let home = unique_dir("phase2");
        tokio::fs::create_dir_all(&home).await.unwrap();
        let legacy = unique_dir("legacy-src");
        tokio::fs::create_dir_all(&legacy).await.unwrap();
        tokio::fs::write(legacy.join("SKILL.md"), "old")
            .await
            .unwrap();
        // Create a symlink in `home` that points at the legacy source.
        std::os::unix::fs::symlink(&legacy, home.join("legacy-skill")).unwrap();
        // The manifest does NOT contain `legacy-skill` (so Phase 1 skips
        // it), and selected_skills is empty (so Phase 1's "desired"
        // check passes). Phase 2 sees it as a legacy symlink.
        let all = vec![entry("legacy", "legacy-skill", legacy.clone())];
        let desired = vec![entry("keep", "keep", unique_dir("keep-src"))];
        let input = ReconcileManagedCodexSkillsInput::new(home.clone(), all, desired);
        let revocations = reconcile_managed_codex_skills(input).await.unwrap();
        // Phase 2 revokes `legacy-skill`. (Phase 1 sees an empty manifest
        // so it has no work, and Phase 3 has nothing because `legacy-skill`
        // is in `all_skills`.)
        let phases: Vec<&RevocationRecord> = revocations
            .iter()
            .filter(|r| r.name == "legacy-skill")
            .collect();
        assert_eq!(phases.len(), 1);
        assert_eq!(phases[0].phase, RevocationPhase::LegacySymlink);
        assert!(!tokio::fs::try_exists(home.join("legacy-skill"))
            .await
            .unwrap_or(false));
        let _ = tokio::fs::remove_dir_all(&home).await;
        let _ = tokio::fs::remove_dir_all(&legacy).await;
    }

    #[tokio::test]
    async fn reconcile_emits_log_lines_via_sink() {
        let home = unique_dir("log");
        tokio::fs::create_dir_all(home.join("stale")).await.unwrap();
        let manifest = crate::managed_home::ManagedSkillsManifest::from_names(["stale"]);
        crate::managed_home::write_managed_codex_skills_manifest(&home, &manifest)
            .await
            .unwrap();
        let (captured, sink) = capture_sink();
        // `stale` is in the manifest but NOT in selected and NOT in
        // all_skills. Phase 1 removes it as `ManagedNoLongerDesired`
        // and emits a `Revoked` log line.
        let input =
            ReconcileManagedCodexSkillsInput::new(home.clone(), vec![], vec![]).with_on_log(sink);
        reconcile_managed_codex_skills(input).await.unwrap();
        let log = captured.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0, LogStream::Stdout);
        assert!(log[0].1.contains("Revoked"));
        assert!(!log[0].1.contains("Revoked unavailable"));
        assert!(log[0].1.contains("stale"));
        let _ = tokio::fs::remove_dir_all(&home).await;
    }

    #[tokio::test]
    async fn reconcile_is_noop_when_everything_already_aligned() {
        let home = unique_dir("noop");
        let src = unique_dir("src-noop");
        tokio::fs::create_dir_all(&src).await.unwrap();
        tokio::fs::create_dir_all(home.join("keep")).await.unwrap();
        let manifest = crate::managed_home::ManagedSkillsManifest::from_names(["keep"]);
        crate::managed_home::write_managed_codex_skills_manifest(&home, &manifest)
            .await
            .unwrap();
        let all = vec![entry("keep", "keep", src.clone())];
        let selected = vec![entry("keep", "keep", src.clone())];
        let input = ReconcileManagedCodexSkillsInput::new(home.clone(), all, selected);
        let revocations = reconcile_managed_codex_skills(input).await.unwrap();
        assert!(revocations.is_empty());
        assert!(tokio::fs::try_exists(home.join("keep"))
            .await
            .unwrap_or(false));
        let _ = tokio::fs::remove_dir_all(&home).await;
        let _ = tokio::fs::remove_dir_all(&src).await;
    }
}
