//! `pc-adapter-codex-local` skills — list / sync implementation that
//! mirrors Node `packages/adapters/codex-local/src/server/skills.ts`.
//!
//! Codex local runtime materialises skills into `CODEX_HOME/skills/`
//! (see `pc-acpx::skill_runtime::prepare_codex_skill_runtime`), so the
//! adapter-level `list` / `sync` API only needs to surface the current
//! snapshot — no filesystem sync operations are required and no
//! external `skillsHome` is surfaced (Codex has no concept of an
//! external user-managed skills directory like Claude's
//! `~/.claude/skills`).
//!
//! ## Public API
//!
//! - [`list_codex_skills`] — return an [`AdapterSkillSnapshot`] for the
//!   configured skills.
//! - [`sync_codex_skills`] — same shape as [`list_codex_skills`];
//!   `desired_skills` is accepted for trait parity with adapters that
//!   actually mutate the skills home.
//! - [`resolve_codex_desired_skill_names`] — thin wrapper around the
//!   pc-acpx helper.
//!
//! All operations use the helpers shipped by `pc-acpx` (R387 / R388 /
//! R390) — no new I/O primitives are introduced in this crate.

use std::path::Path;

use pc_acpx::{
    build_runtime_mounted_skill_snapshot, read_paperclip_runtime_skill_entries,
    resolve_paperclip_desired_skill_names, AdapterSkillContext, AdapterSkillSnapshot,
    AvailableSkillEntry, RuntimeMountedSkillSnapshotOptions, SkillDetail,
};

// ============================================================================
// Snapshot builder
// ============================================================================

/// Build an [`AdapterSkillSnapshot`] for the configured Codex skills
/// runtime. Mirrors Node `buildCodexSkillSnapshot` (L13-21).
///
/// Reads available entries from the configured
/// `paperclipRuntimeSkills` config (falling back to filesystem
/// discovery via `read_paperclip_runtime_skill_entries`), then layers
/// the resolved desired-skill keys into a runtime-mounted snapshot.
///
/// Unlike Claude local, Codex does not surface an external
/// `skillsHome` — the entire surface is owned by Paperclip.
pub async fn build_codex_skill_snapshot(
    ctx: &AdapterSkillContext,
    module_dir: &Path,
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
    let options = RuntimeMountedSkillSnapshotOptions {
        adapter_type: ctx.adapter_type.clone(),
        available_entries,
        desired_skills,
        configured_detail: SkillDetail::Static(
            "Will be linked into the effective CODEX_HOME/skills/ directory on the next run."
                .to_string(),
        ),
        missing_detail: None,
        mode: None,
        supported: None,
        unsupported_detail: None,
        warnings: None,
        external_installed: None,
        external_location_label: None,
        external_detail: None,
        skills_home: None,
    };
    build_runtime_mounted_skill_snapshot(&options)
}

// ============================================================================
// Public API
// ============================================================================

/// Return the current [`AdapterSkillSnapshot`] for the configured
/// Codex skills runtime. Mirrors Node `listCodexSkills` (L23-25).
pub async fn list_codex_skills(
    ctx: &AdapterSkillContext,
    module_dir: &Path,
) -> AdapterSkillSnapshot {
    build_codex_skill_snapshot(ctx, module_dir).await
}

/// Sync the configured desired skills into the Codex skills home.
///
/// Codex local does not need adapter-level filesystem sync because
/// `pc-acpx::skill_runtime::prepare_codex_skill_runtime` materialises
/// the selected skills into the per-company managed Codex home. The
/// signature accepts `desired_skills` for parity with Node but the
/// input is ignored — the snapshot is rebuilt from the same sources
/// as [`list_codex_skills`]. Mirrors Node `syncCodexSkills` (L27-30).
pub async fn sync_codex_skills(
    ctx: &AdapterSkillContext,
    _desired_skills: &[String],
    module_dir: &Path,
) -> AdapterSkillSnapshot {
    build_codex_skill_snapshot(ctx, module_dir).await
}

/// Thin wrapper around `pc_acpx::resolve_paperclip_desired_skill_names`
/// for adapter-level reuse. Mirrors Node `resolveCodexDesiredSkillNames`
/// (L32-35).
pub fn resolve_codex_desired_skill_names(
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
            "pc-adapter-codex-local-{label}-{nanos}-{}",
            std::process::id()
        ))
    }

    fn make_ctx(config: serde_json::Value) -> AdapterSkillContext {
        AdapterSkillContext::new("agent-1", "company-1", "codex_local", config)
    }

    // ---- build / list / sync snapshot ----

    #[tokio::test]
    async fn list_uses_filesystem_when_config_empty() {
        let parent = unique_dir("list-fs");
        let module_dir = parent.join("a").join("b");
        tokio::fs::create_dir_all(&module_dir).await.unwrap();
        let skills_dir = parent.join("skills");
        tokio::fs::create_dir_all(skills_dir.join("alpha"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(skills_dir.join("beta"))
            .await
            .unwrap();

        let ctx = make_ctx(json!({}));
        let snapshot = list_codex_skills(&ctx, &module_dir).await;

        assert_eq!(snapshot.adapter_type, "codex_local");
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
    async fn list_reflects_configured_runtime_skills() {
        let parent = unique_dir("list-cfg");
        let module_dir = parent.join("a").join("b");
        tokio::fs::create_dir_all(&module_dir).await.unwrap();

        let ctx = make_ctx(json!({
            "paperclipRuntimeSkills": [
                {
                    "key": "paperclipai/paperclip/configured-only",
                    "runtimeName": "configured-only",
                    "source": "/skills/configured-only",
                }
            ]
        }));
        let snapshot = list_codex_skills(&ctx, &module_dir).await;

        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(
            snapshot.entries[0].runtime_name.as_deref(),
            Some("configured-only"),
        );

        let _ = tokio::fs::remove_dir_all(&parent).await;
    }

    #[tokio::test]
    async fn sync_returns_same_shape_as_list() {
        let parent = unique_dir("sync-same");
        let module_dir = parent.join("a").join("b");
        tokio::fs::create_dir_all(&module_dir).await.unwrap();
        let skills_dir = parent.join("skills");
        tokio::fs::create_dir_all(skills_dir.join("only"))
            .await
            .unwrap();

        let ctx = make_ctx(json!({}));
        let list_snap = list_codex_skills(&ctx, &module_dir).await;
        let sync_snap = sync_codex_skills(&ctx, &[], &module_dir).await;

        // Same adapter, same entries, same mode.
        assert_eq!(list_snap.adapter_type, sync_snap.adapter_type);
        assert_eq!(list_snap.entries.len(), sync_snap.entries.len());
        assert_eq!(list_snap.mode, sync_snap.mode);
        assert_eq!(list_snap.supported, sync_snap.supported);

        let _ = tokio::fs::remove_dir_all(&parent).await;
    }

    #[tokio::test]
    async fn snapshot_has_no_external_installed_block() {
        // Codex local does not surface externalInstalled / skillsHome
        // — the snapshot remains compact even when the filesystem
        // contains entries.
        let parent = unique_dir("no-ext");
        let module_dir = parent.join("a").join("b");
        tokio::fs::create_dir_all(&module_dir).await.unwrap();
        let skills_dir = parent.join("skills");
        tokio::fs::create_dir_all(skills_dir.join("only"))
            .await
            .unwrap();

        let ctx = make_ctx(json!({}));
        let snapshot = list_codex_skills(&ctx, &module_dir).await;

        // No warnings about missing skills home / external targets.
        assert!(snapshot.warnings.is_empty());

        let _ = tokio::fs::remove_dir_all(&parent).await;
    }

    // ---- resolve_codex_desired_skill_names ----

    #[test]
    fn desired_names_delegate_to_pc_acpx() {
        let ctx = make_ctx(json!({}));
        let entries: Vec<pc_acpx::skill_snapshot::AdapterSkillEntry> = Vec::new();
        let desired = resolve_codex_desired_skill_names(&ctx, &entries);
        assert!(desired.is_empty());
    }

    #[test]
    fn desired_names_resolve_configured_keys() {
        let ctx = make_ctx(json!({
            "paperclipSkillSync": {
                "desiredSkills": ["paperclipai/paperclip/foo"]
            }
        }));
        let entries = vec![pc_acpx::skill_snapshot::AdapterSkillEntry {
            key: "paperclipai/paperclip/foo".to_string(),
            runtime_name: Some("foo".to_string()),
            version_id: None,
            current_version_id: None,
            desired: false,
            managed: false,
            state: pc_acpx::skill_snapshot::AdapterSkillState::Available,
            origin: None,
            origin_label: None,
            location_label: None,
            read_only: false,
            source_path: None,
            target_path: None,
            detail: None,
        }];
        let desired = resolve_codex_desired_skill_names(&ctx, &entries);
        assert_eq!(desired, vec!["paperclipai/paperclip/foo".to_string()]);
    }
}
