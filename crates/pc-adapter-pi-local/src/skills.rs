//! `pc-adapter-pi-local` skills — list / sync implementation that
//! mirrors Node `packages/adapters/pi-local/src/server/skills.ts`.
//!
//! Pi local uses the persistent snapshot shape with sync side effects
//! (creates + repairs + removes symlinks under `~/.pi/agent/skills`).
//! Structurally identical to gemini-local (R393) and opencode-local
//! (R394) — only the `skillsHome` path and detail text strings differ.
//!
//! ## Public API
//!
//! - [`list_pi_skills`] — return an [`AdapterSkillSnapshot`] for the
//!   configured skills.
//! - [`sync_pi_skills`] — sync desired skills into
//!   `<home>/.pi/agent/skills` via symlinks.
//! - [`resolve_pi_skills_home`] / [`resolve_pi_skills_home_with`] —
//!   resolve `<home>/.pi/agent/skills` honouring `env.HOME`.
//! - [`resolve_pi_desired_skill_names`] — thin wrapper.
//!
//! All operations use the helpers shipped by `pc-acpx` (R387 / R388 /
//! R390) — no new I/O primitives are introduced in this crate.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pc_acpx::{
    build_persistent_skill_snapshot, ensure_paperclip_skill_symlink, read_installed_skill_targets,
    read_paperclip_runtime_skill_entries, resolve_paperclip_desired_skill_names,
    skill_snapshot::PaperclipSkillEntry, AdapterSkillContext, AdapterSkillSnapshot,
    AvailableSkillEntry, InstalledSkillTarget, PersistentSkillSnapshotOptions,
};

// ============================================================================
// Skills home resolution
// ============================================================================

/// Path fragment under `$HOME` that Pi local uses for skills.
pub const PI_SKILLS_HOME_SUFFIX: &str = ".pi/agent/skills";

/// Resolve `<home>/<PI_SKILLS_HOME_SUFFIX>` honouring
/// `config.env.HOME` if present. Mirrors Node
/// `resolvePiSkillsHome` (L17-24).
pub fn resolve_pi_skills_home(config: &serde_json::Value) -> Option<PathBuf> {
    let env = config.get("env").and_then(serde_json::Value::as_object);
    let configured = env
        .and_then(|env| env.get("HOME"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    configured.map(|home| PathBuf::from(home).join(PI_SKILLS_HOME_SUFFIX))
}

/// Convenience wrapper that injects `default_home` when
/// [`resolve_pi_skills_home`] returns `None`.
pub fn resolve_pi_skills_home_with(
    config: &serde_json::Value,
    default_home: impl AsRef<Path>,
) -> PathBuf {
    resolve_pi_skills_home(config)
        .unwrap_or_else(|| default_home.as_ref().join(PI_SKILLS_HOME_SUFFIX))
}

// ============================================================================
// Snapshot builder
// ============================================================================

/// Build an [`AdapterSkillSnapshot`] for the configured Pi skills
/// runtime. Mirrors Node `buildPiSkillSnapshot` (L26-39).
///
/// Reads available entries from the configured
/// `paperclipRuntimeSkills` config (falling back to filesystem
/// discovery), then layers the currently-installed targets from the
/// `<home>/.pi/agent/skills` directory into a **persistent** snapshot.
pub async fn build_pi_skill_snapshot(
    ctx: &AdapterSkillContext,
    module_dir: &Path,
    skills_home: &Path,
) -> AdapterSkillSnapshot {
    let config_map = ctx.config.as_object().cloned().unwrap_or_default();
    let available_entries =
        read_paperclip_runtime_skill_entries(&config_map, module_dir, &[]).await;
    let available_for_resolve: Vec<AvailableSkillEntry> = available_entries
        .iter()
        .map(|entry| AvailableSkillEntry {
            key: entry.key.clone(),
            runtime_name: Some(entry.runtime_name.clone()),
        })
        .collect();
    let desired_skills = resolve_paperclip_desired_skill_names(&config_map, &available_for_resolve);
    let installed = read_installed_skill_targets(skills_home).await;
    let options = PersistentSkillSnapshotOptions {
        adapter_type: ctx.adapter_type.clone(),
        available_entries,
        desired_skills,
        installed,
        skills_home: skills_home.to_string_lossy().into_owned(),
        location_label: Some("~/.pi/agent/skills".to_string()),
        installed_detail: None,
        missing_detail: "Configured but not currently linked into the Pi skills home.".to_string(),
        external_conflict_detail: "Skill name is occupied by an external installation.".to_string(),
        external_detail: "Installed outside Paperclip management.".to_string(),
        warnings: None,
    };
    build_persistent_skill_snapshot(&options)
}

// ============================================================================
// Public API
// ============================================================================

/// Return the current [`AdapterSkillSnapshot`] for the configured Pi
/// skills runtime. Mirrors Node `listPiSkills` (L41-43).
///
/// `skills_home` is the resolved `<home>/.pi/agent/skills` directory.
/// If `None`, the snapshot is still built with an empty `installed`
/// map and a warning noting the missing home.
pub async fn list_pi_skills(
    ctx: &AdapterSkillContext,
    module_dir: &Path,
    skills_home: Option<&Path>,
) -> AdapterSkillSnapshot {
    match skills_home {
        Some(home) => build_pi_skill_snapshot(ctx, module_dir, home).await,
        None => {
            let config_map = ctx.config.as_object().cloned().unwrap_or_default();
            let available_entries =
                read_paperclip_runtime_skill_entries(&config_map, module_dir, &[]).await;
            let available_for_resolve: Vec<AvailableSkillEntry> = available_entries
                .iter()
                .map(|entry| AvailableSkillEntry {
                    key: entry.key.clone(),
                    runtime_name: Some(entry.runtime_name.clone()),
                })
                .collect();
            let desired_skills =
                resolve_paperclip_desired_skill_names(&config_map, &available_for_resolve);
            let empty: BTreeMap<String, InstalledSkillTarget> = BTreeMap::new();
            let options = PersistentSkillSnapshotOptions {
                adapter_type: ctx.adapter_type.clone(),
                available_entries,
                desired_skills,
                installed: empty,
                skills_home: String::new(),
                location_label: Some("~/.pi/agent/skills".to_string()),
                installed_detail: None,
                missing_detail: "Configured but not currently linked into the Pi skills home."
                    .to_string(),
                external_conflict_detail: "Skill name is occupied by an external installation."
                    .to_string(),
                external_detail: "Installed outside Paperclip management.".to_string(),
                warnings: Some(vec!["Skills home could not be resolved.".to_string()]),
            };
            build_persistent_skill_snapshot(&options)
        }
    }
}

/// Sync the configured desired skills into the Pi skills home.
///
/// For each `desired` skill, `ensure_paperclip_skill_symlink` creates
/// or repairs a symlink at `<skillsHome>/<runtime_name>` pointing at
/// the source. Then any installed entry whose `target_path` matches a
/// now-*undesired* available source is unlinked.
///
/// Mirrors Node `syncPiSkills` (L45-71).
pub async fn sync_pi_skills(
    ctx: &AdapterSkillContext,
    desired_skills: &[String],
    module_dir: &Path,
    skills_home: &Path,
) -> AdapterSkillSnapshot {
    let config_map = ctx.config.as_object().cloned().unwrap_or_default();
    let available_entries =
        read_paperclip_runtime_skill_entries(&config_map, module_dir, &[]).await;
    let desired_set: std::collections::BTreeSet<&str> =
        desired_skills.iter().map(String::as_str).collect();
    // Ensure the home exists so symlink calls do not race creation.
    let _ = tokio::fs::create_dir_all(skills_home).await;
    let installed = read_installed_skill_targets(skills_home).await;
    let available_by_runtime_name: std::collections::HashMap<&str, &PaperclipSkillEntry> =
        available_entries
            .iter()
            .map(|entry| (entry.runtime_name.as_str(), entry))
            .collect();

    // 1. Create / repair symlinks for each desired skill.
    for available in &available_entries {
        if !desired_set.contains(available.key.as_str()) {
            continue;
        }
        let target = skills_home.join(&available.runtime_name);
        ensure_paperclip_skill_symlink(&PathBuf::from(&available.source), &target).await;
    }

    // 2. Unlink stale Paperclip-managed symlinks for skills that are
    //    no longer desired.
    for (name, installed_entry) in &installed {
        let Some(available) = available_by_runtime_name.get(name.as_str()) else {
            continue;
        };
        if desired_set.contains(available.key.as_str()) {
            continue;
        }
        let Some(target_path) = &installed_entry.target_path else {
            continue;
        };
        if target_path != &available.source {
            continue;
        }
        let target = skills_home.join(name);
        let _ = tokio::fs::remove_file(&target).await;
    }

    build_pi_skill_snapshot(ctx, module_dir, skills_home).await
}

/// Thin wrapper around `pc_acpx::resolve_paperclip_desired_skill_names`.
/// Mirrors Node `resolvePiDesiredSkillNames` (L73-76).
pub fn resolve_pi_desired_skill_names(
    ctx: &AdapterSkillContext,
    available_entries: &[pc_acpx::skill_snapshot::AdapterSkillEntry],
) -> Vec<String> {
    let config_map = ctx.config.as_object().cloned().unwrap_or_default();
    let available: Vec<AvailableSkillEntry> = available_entries
        .iter()
        .map(|entry| AvailableSkillEntry {
            key: entry.key.clone(),
            runtime_name: entry.runtime_name.clone(),
        })
        .collect();
    resolve_paperclip_desired_skill_names(&config_map, &available)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_dir(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "pc-adapter-pi-local-{label}-{nanos}-{}",
            std::process::id()
        ))
    }

    fn make_ctx(config: serde_json::Value) -> AdapterSkillContext {
        AdapterSkillContext::new("agent-1", "company-1", "pi_local", config)
    }

    fn make_module_layout(label: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let parent = unique_dir(label);
        let module_dir = parent.join("a").join("b");
        (parent, module_dir)
    }

    // ---- resolve_pi_skills_home ----

    #[test]
    fn skills_home_uses_configured_home() {
        let config = json!({ "env": { "HOME": "/custom/home" } });
        let resolved = resolve_pi_skills_home(&config);
        assert_eq!(
            resolved,
            Some(PathBuf::from("/custom/home/.pi/agent/skills"))
        );
    }

    #[test]
    fn skills_home_trims_whitespace() {
        let config = json!({ "env": { "HOME": "  /trim/me  " } });
        let resolved = resolve_pi_skills_home(&config);
        assert_eq!(resolved, Some(PathBuf::from("/trim/me/.pi/agent/skills")));
    }

    #[test]
    fn skills_home_returns_none_when_unset() {
        assert!(resolve_pi_skills_home(&json!({})).is_none());
        assert!(resolve_pi_skills_home(&json!({ "env": {} })).is_none());
    }

    #[test]
    fn skills_home_with_default_falls_back() {
        let resolved = resolve_pi_skills_home_with(&json!({}), "/fallback");
        assert_eq!(resolved, PathBuf::from("/fallback/.pi/agent/skills"));
    }

    // ---- build / list ----

    #[tokio::test]
    async fn list_uses_filesystem_when_config_empty() {
        let (parent, module_dir) = make_module_layout("list-fs");
        tokio::fs::create_dir_all(&module_dir).await.unwrap();
        let skills_dir = parent.join("skills");
        tokio::fs::create_dir_all(skills_dir.join("alpha"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(skills_dir.join("beta"))
            .await
            .unwrap();

        let ctx = make_ctx(json!({ "env": { "HOME": parent.to_string_lossy() } }));
        let snapshot = list_pi_skills(&ctx, &module_dir, Some(&skills_dir)).await;

        assert_eq!(snapshot.adapter_type, "pi_local");
        assert!(snapshot.supported);
        assert!(snapshot.warnings.is_empty());
        let mut names: Vec<String> = snapshot
            .entries
            .iter()
            .filter_map(|e| e.runtime_name.clone())
            .collect();
        names.sort();
        assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);

        let _ = tokio::fs::remove_dir_all(&parent).await;
    }

    #[tokio::test]
    async fn list_with_no_skills_home_still_builds_snapshot() {
        let (parent, module_dir) = make_module_layout("list-none");
        tokio::fs::create_dir_all(&module_dir).await.unwrap();
        let ctx = make_ctx(json!({}));
        let snapshot = list_pi_skills(&ctx, &module_dir, None).await;
        assert_eq!(snapshot.adapter_type, "pi_local");
        // Missing-home warning surfaces the gap to callers.
        assert!(!snapshot.warnings.is_empty());

        let _ = tokio::fs::remove_dir_all(&parent).await;
    }

    // ---- sync (R393 / R394 模板复用) ----

    #[tokio::test]
    #[cfg(unix)]
    async fn sync_creates_symlinks_for_desired_skills() {
        let (parent, module_dir) = make_module_layout("sync-create");
        let source_a = parent.join("src-a");
        let source_b = parent.join("src-b");
        tokio::fs::create_dir_all(&source_a).await.unwrap();
        tokio::fs::create_dir_all(&source_b).await.unwrap();
        let skills_home = parent.join("home");
        tokio::fs::create_dir_all(&skills_home).await.unwrap();

        let ctx = make_ctx(json!({
            "paperclipRuntimeSkills": [
                { "key": "paperclipai/paperclip/a", "runtimeName": "a", "source": source_a.to_string_lossy() },
                { "key": "paperclipai/paperclip/b", "runtimeName": "b", "source": source_b.to_string_lossy() },
            ]
        }));
        let desired = vec![
            "paperclipai/paperclip/a".to_string(),
            "paperclipai/paperclip/b".to_string(),
        ];
        let _ = sync_pi_skills(&ctx, &desired, &module_dir, &skills_home).await;
        assert!(tokio::fs::symlink_metadata(skills_home.join("a"))
            .await
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(tokio::fs::symlink_metadata(skills_home.join("b"))
            .await
            .unwrap()
            .file_type()
            .is_symlink());

        let _ = tokio::fs::remove_dir_all(&parent).await;
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn sync_removes_stale_symlinks_for_undesired_skills() {
        let (parent, module_dir) = make_module_layout("sync-remove");
        let source = parent.join("src");
        tokio::fs::create_dir_all(&source).await.unwrap();
        let skills_home = parent.join("home");
        tokio::fs::create_dir_all(&skills_home).await.unwrap();

        let ctx = make_ctx(json!({
            "paperclipRuntimeSkills": [
                { "key": "paperclipai/paperclip/only", "runtimeName": "only", "source": source.to_string_lossy() },
            ]
        }));

        let desired = vec!["paperclipai/paperclip/only".to_string()];
        let _ = sync_pi_skills(&ctx, &desired, &module_dir, &skills_home).await;
        assert!(tokio::fs::symlink_metadata(skills_home.join("only"))
            .await
            .is_ok());

        let _ = sync_pi_skills(&ctx, &[], &module_dir, &skills_home).await;
        assert!(tokio::fs::symlink_metadata(skills_home.join("only"))
            .await
            .is_err());

        let _ = tokio::fs::remove_dir_all(&parent).await;
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn sync_does_not_remove_external_symlinks() {
        let (parent, module_dir) = make_module_layout("sync-external");
        let external_source = parent.join("external-src");
        tokio::fs::create_dir_all(&external_source).await.unwrap();
        let skills_home = parent.join("home");
        tokio::fs::create_dir_all(&skills_home).await.unwrap();
        std::os::unix::fs::symlink(&external_source, skills_home.join("external-link")).unwrap();

        let ctx = make_ctx(json!({
            "paperclipRuntimeSkills": [
                { "key": "paperclipai/paperclip/external-link", "runtimeName": "external-link", "source": "/some/other/path" },
            ]
        }));
        let _ = sync_pi_skills(&ctx, &[], &module_dir, &skills_home).await;
        let meta = tokio::fs::symlink_metadata(skills_home.join("external-link"))
            .await
            .unwrap();
        assert!(meta.file_type().is_symlink());

        let _ = tokio::fs::remove_dir_all(&parent).await;
    }

    #[tokio::test]
    async fn sync_creates_skills_home_when_missing() {
        let (parent, module_dir) = make_module_layout("sync-mkdir");
        let source = parent.join("src");
        tokio::fs::create_dir_all(&source).await.unwrap();
        let skills_home = parent.join("does-not-exist-yet").join("home");

        let ctx = make_ctx(json!({
            "paperclipRuntimeSkills": [
                { "key": "paperclipai/paperclip/only", "runtimeName": "only", "source": source.to_string_lossy() },
            ]
        }));
        let desired = vec!["paperclipai/paperclip/only".to_string()];
        let _ = sync_pi_skills(&ctx, &desired, &module_dir, &skills_home).await;
        assert!(tokio::fs::metadata(&skills_home).await.is_ok());

        let _ = tokio::fs::remove_dir_all(&parent).await;
    }

    // ---- resolve_pi_desired_skill_names ----

    #[test]
    fn desired_names_delegate_to_pc_acpx() {
        let ctx = make_ctx(json!({}));
        let entries: Vec<pc_acpx::skill_snapshot::AdapterSkillEntry> = Vec::new();
        let desired = resolve_pi_desired_skill_names(&ctx, &entries);
        assert!(desired.is_empty());
    }
}
