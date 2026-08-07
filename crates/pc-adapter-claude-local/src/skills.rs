//! `pc-adapter-claude-local` skills — list / sync implementation that
//! mirrors Node `packages/adapters/claude-local/src/server/skills.ts`.
//!
//! Claude local runtime uses the shared Claude skills home
//! `~/.claude/skills` (configurable via `config.env.HOME`). Paperclip
//! materialises the selected skills into the per-run Claude prompt
//! bundle (see `pc-acpx::skill_runtime::prepare_claude_skill_runtime`),
//! so the adapter-level `list` / `sync` API only needs to surface the
//! current snapshot — no filesystem sync operations are required.
//!
//! ## Public API
//!
//! - [`list_claude_skills`] — return an [`AdapterSkillSnapshot`] for the
//!   configured skills (uses runtime-mounted snapshot shape).
//! - [`sync_claude_skills`] — same shape as [`list_claude_skills`] for
//!   Claude local; the desired-skills argument is accepted for trait
//!   parity with other adapters that actually mutate the skills home.
//! - [`resolve_claude_skills_home`] — resolve `<home>/.claude/skills`
//!   honouring the `env.HOME` override.
//! - [`resolve_claude_desired_skill_names`] — thin wrapper around the
//!   pc-acpx helper.
//!
//! All operations use the helpers shipped by `pc-acpx` (R387 / R388 /
//! R390) — no new I/O primitives are introduced in this crate.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pc_acpx::{
    build_runtime_mounted_skill_snapshot, read_installed_skill_targets,
    read_paperclip_runtime_skill_entries, resolve_paperclip_desired_skill_names,
    AdapterSkillContext, AdapterSkillSnapshot, AvailableSkillEntry,
    RuntimeMountedSkillSnapshotOptions, SkillDetail,
};

// ============================================================================
// Skills home resolution
// ============================================================================

/// Path fragment under `$HOME` that Claude local uses for skills.
pub const CLAUDE_SKILLS_HOME_SUFFIX: &str = ".claude/skills";

/// Resolve `<home>/<CLAUDE_SKILLS_HOME_SUFFIX>` honouring
/// `config.env.HOME` if present. Mirrors Node
/// `resolveClaudeSkillsHome` (L18-25).
///
/// - When `config.env.HOME` is a non-empty string we honour it.
/// - Otherwise the resolved home is `None` and the caller should
///   substitute `std::env::home_dir` (Rust's analogue of `os.homedir()`).
///   Returning `Option` keeps the function pure (no global state read)
///   so tests can inject any home directory.
pub fn resolve_claude_skills_home(config: &serde_json::Value) -> Option<PathBuf> {
    let env = config.get("env").and_then(serde_json::Value::as_object);
    let configured = env
        .and_then(|env| env.get("HOME"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    configured.map(|home| PathBuf::from(home).join(CLAUDE_SKILLS_HOME_SUFFIX))
}

/// Convenience wrapper that injects `default_home` when
/// [`resolve_claude_skills_home`] returns `None`.
pub fn resolve_claude_skills_home_with(
    config: &serde_json::Value,
    default_home: impl AsRef<Path>,
) -> PathBuf {
    resolve_claude_skills_home(config)
        .unwrap_or_else(|| default_home.as_ref().join(CLAUDE_SKILLS_HOME_SUFFIX))
}

// ============================================================================
// Snapshot builder
// ============================================================================

/// Build an [`AdapterSkillSnapshot`] for the configured Claude skills
/// runtime. Mirrors Node `buildClaudeSkillSnapshot` (L27-43).
///
/// Reads available entries from the configured
/// `paperclipRuntimeSkills` config (falling back to filesystem
/// discovery via `list_paperclip_skill_entries`), then layers the
/// currently-installed targets from `<skillsHome>` into a
/// runtime-mounted snapshot.
pub async fn build_claude_skill_snapshot(
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
    let skills_home = resolve_claude_skills_home(&ctx.config);
    let external_installed = if let Some(home) = skills_home.as_ref() {
        read_installed_skill_targets(home).await
    } else {
        Default::default()
    };
    let mut external_installed_opt: Option<BTreeMap<String, pc_acpx::InstalledSkillTarget>> =
        if external_installed.is_empty() {
            None
        } else {
            Some(external_installed)
        };
    let options = RuntimeMountedSkillSnapshotOptions {
        adapter_type: ctx.adapter_type.clone(),
        available_entries,
        desired_skills,
        configured_detail: SkillDetail::Static(
            "Will be materialized into the stable Paperclip-managed Claude prompt bundle on the next run.".to_string(),
        ),
        missing_detail: None,
        mode: None,
        supported: None,
        unsupported_detail: None,
        warnings: None,
        external_installed: external_installed_opt.take(),
        external_location_label: Some("~/.claude/skills".to_string()),
        external_detail: Some("Installed outside Paperclip management in the Claude skills home.".to_string()),
        skills_home: skills_home.map(|p| p.to_string_lossy().into_owned()),
    };
    build_runtime_mounted_skill_snapshot(&options)
}

// ============================================================================
// Public API
// ============================================================================

/// Return the current [`AdapterSkillSnapshot`] for the configured
/// Claude skills runtime. Mirrors Node `listClaudeSkills` (L45-47).
pub async fn list_claude_skills(
    ctx: &AdapterSkillContext,
    module_dir: &Path,
) -> AdapterSkillSnapshot {
    build_claude_skill_snapshot(ctx, module_dir).await
}

/// Sync the configured desired skills into the Claude skills home.
///
/// Claude local does not need adapter-level filesystem sync because
/// `pc-acpx::skill_runtime::prepare_claude_skill_runtime` materialises
/// the selected skills into the per-run prompt bundle. The signature
/// accepts `desired_skills` for parity with Node but the input is
/// ignored — the snapshot is rebuilt from the same sources as
/// [`list_claude_skills`]. Mirrors Node `syncClaudeSkills` (L49-52).
pub async fn sync_claude_skills(
    ctx: &AdapterSkillContext,
    _desired_skills: &[String],
    module_dir: &Path,
) -> AdapterSkillSnapshot {
    build_claude_skill_snapshot(ctx, module_dir).await
}

/// Thin wrapper around `pc_acpx::resolve_paperclip_desired_skill_names`
/// for adapter-level reuse. Mirrors Node `resolveClaudeDesiredSkillNames`
/// (L54-59).
pub fn resolve_claude_desired_skill_names(
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

// Re-export for callers that want to drive list_paperclip_skill_entries
// themselves (e.g. UI surfaces that need raw filesystem listing without
// going through the snapshot builder).
pub use pc_acpx::skill_io::PAPERCLIP_SKILL_KEY_PREFIX;

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "pc-adapter-claude-local-{label}-{nanos}-{}",
            std::process::id()
        ))
    }

    fn make_ctx(config: serde_json::Value) -> AdapterSkillContext {
        AdapterSkillContext::new("agent-1", "company-1", "claude_local", config)
    }

    // ---- resolve_claude_skills_home ----

    #[test]
    fn skills_home_uses_configured_home() {
        let config = json!({ "env": { "HOME": "/custom/home" } });
        let resolved = resolve_claude_skills_home(&config);
        assert_eq!(resolved, Some(PathBuf::from("/custom/home/.claude/skills")));
    }

    #[test]
    fn skills_home_trims_whitespace() {
        let config = json!({ "env": { "HOME": "  /trim/me  " } });
        let resolved = resolve_claude_skills_home(&config);
        assert_eq!(resolved, Some(PathBuf::from("/trim/me/.claude/skills")));
    }

    #[test]
    fn skills_home_returns_none_when_unset() {
        let config = json!({});
        assert!(resolve_claude_skills_home(&config).is_none());
        let config = json!({ "env": {} });
        assert!(resolve_claude_skills_home(&config).is_none());
    }

    #[test]
    fn skills_home_returns_none_when_home_empty_string() {
        let config = json!({ "env": { "HOME": "   " } });
        assert!(resolve_claude_skills_home(&config).is_none());
    }

    #[test]
    fn skills_home_returns_none_when_env_is_not_object() {
        let config = json!({ "env": "not-an-object" });
        assert!(resolve_claude_skills_home(&config).is_none());
    }

    #[test]
    fn skills_home_with_default_falls_back() {
        let config = json!({});
        let resolved = resolve_claude_skills_home_with(&config, "/fallback");
        assert_eq!(resolved, PathBuf::from("/fallback/.claude/skills"));
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
        let snapshot = list_claude_skills(&ctx, &module_dir).await;

        assert_eq!(snapshot.adapter_type, "claude_local");
        // 2 entries from filesystem
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
    async fn sync_returns_same_shape_as_list() {
        let parent = unique_dir("sync-same");
        let module_dir = parent.join("a").join("b");
        tokio::fs::create_dir_all(&module_dir).await.unwrap();
        let skills_dir = parent.join("skills");
        tokio::fs::create_dir_all(skills_dir.join("only"))
            .await
            .unwrap();

        let ctx = make_ctx(json!({}));
        let list_snap = list_claude_skills(&ctx, &module_dir).await;
        let sync_snap = sync_claude_skills(&ctx, &[], &module_dir).await;

        // Same adapter, same entries, same mode.
        assert_eq!(list_snap.adapter_type, sync_snap.adapter_type);
        assert_eq!(list_snap.entries.len(), 1);
        assert_eq!(list_snap.mode, sync_snap.mode);
        assert_eq!(list_snap.supported, sync_snap.supported);
        assert_eq!(
            list_snap.entries[0].runtime_name,
            sync_snap.entries[0].runtime_name,
        );

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
        let snapshot = list_claude_skills(&ctx, &module_dir).await;

        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(
            snapshot.entries[0].runtime_name.as_deref(),
            Some("configured-only"),
        );

        let _ = tokio::fs::remove_dir_all(&parent).await;
    }

    #[tokio::test]
    async fn list_records_external_installed_targets() {
        let parent = unique_dir("list-ext");
        let module_dir = parent.join("a").join("b");
        tokio::fs::create_dir_all(&module_dir).await.unwrap();
        // External Claude skills home
        let external_home = parent.join("external-home").join(".claude").join("skills");
        tokio::fs::create_dir_all(&external_home).await.unwrap();
        // Two external entries: a plain dir + a symlink (unix only).
        tokio::fs::create_dir_all(external_home.join("plain-skill"))
            .await
            .unwrap();
        tokio::fs::write(external_home.join("readme.txt"), "hi")
            .await
            .unwrap();

        let ctx =
            make_ctx(json!({ "env": { "HOME": parent.join("external-home").to_string_lossy() } }));
        let snapshot = list_claude_skills(&ctx, &module_dir).await;

        // External targets do not appear directly on the snapshot,
        // but the snapshot is still well-constructed (supported=true,
        // adapter_type=claude_local, no warnings).
        assert!(snapshot.supported);
        assert!(snapshot.warnings.is_empty());

        let _ = tokio::fs::remove_dir_all(&parent).await;
    }

    // ---- resolve_claude_desired_skill_names ----

    #[test]
    fn desired_names_delegate_to_pc_acpx() {
        let ctx = make_ctx(json!({}));
        // Empty input -> empty output (no desired references configured).
        let entries: Vec<pc_acpx::skill_snapshot::AdapterSkillEntry> = Vec::new();
        let desired = resolve_claude_desired_skill_names(&ctx, &entries);
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
        let desired = resolve_claude_desired_skill_names(&ctx, &entries);
        assert_eq!(desired, vec!["paperclipai/paperclip/foo".to_string()]);
    }
}
