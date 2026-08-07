//! R395 — Integration tests for `pc-adapter-pi-local::skills`.
//!
//! Mirrors Node `packages/adapters/pi-local/src/server/skills.ts`:
//! - `listPiSkills` (L41-43)
//! - `syncPiSkills` (L45-71)
//! - `resolvePiSkillsHome` (L17-24)
//! - `resolvePiDesiredSkillNames` (L73-76)
//! - `buildPiSkillSnapshot` (L26-39)
//!
//! Unit tests inside `skills::tests` cover each function in isolation;
//! this integration suite verifies the end-to-end sync flow against
//! the Pi skills home (`~/.pi/agent/skills`).

use pc_acpx::AdapterSkillContext;
use pc_adapter_pi_local::skills::{
    build_pi_skill_snapshot, list_pi_skills, resolve_pi_desired_skill_names,
    resolve_pi_skills_home, resolve_pi_skills_home_with, sync_pi_skills, PI_SKILLS_HOME_SUFFIX,
};
use serde_json::json;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn unique_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "pc-adapter-pi-local-r395-{label}-{nanos}-{}",
        std::process::id()
    ))
}

fn make_module_layout(label: &str) -> (PathBuf, PathBuf) {
    let parent = unique_dir(label);
    let module_dir = parent.join("a").join("b");
    (parent, module_dir)
}

fn make_ctx(config: serde_json::Value) -> AdapterSkillContext {
    AdapterSkillContext::new("agent-r395", "company-r395", "pi_local", config)
}

async fn cleanup(path: &PathBuf) {
    let _ = tokio::fs::remove_dir_all(path).await;
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

#[test]
fn pi_skills_home_suffix_is_dot_pi_agent_skills() {
    assert_eq!(PI_SKILLS_HOME_SUFFIX, ".pi/agent/skills");
}

// ---------------------------------------------------------------------------
// resolve_pi_skills_home — end-to-end
// ---------------------------------------------------------------------------

#[test]
fn skills_home_prefers_env_home_when_set() {
    let config = json!({ "env": { "HOME": "/srv/pi" } });
    let resolved = resolve_pi_skills_home(&config).unwrap();
    assert_eq!(resolved, PathBuf::from("/srv/pi/.pi/agent/skills"));
}

#[test]
fn skills_home_with_fallback_uses_default() {
    let resolved = resolve_pi_skills_home_with(&json!({}), "/home/x");
    assert_eq!(resolved, PathBuf::from("/home/x/.pi/agent/skills"));
}

#[test]
fn skills_home_with_fallback_honours_env_override() {
    let config = json!({ "env": { "HOME": "/explicit" } });
    let resolved = resolve_pi_skills_home_with(&config, "/default");
    assert_eq!(resolved, PathBuf::from("/explicit/.pi/agent/skills"));
}

// ---------------------------------------------------------------------------
// list_pi_skills — end-to-end
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_pi_skills_returns_supported_snapshot() {
    let (parent, module_dir) = make_module_layout("list-supported");
    tokio::fs::create_dir_all(&module_dir).await.unwrap();
    let skills_dir = parent.join("skills");
    tokio::fs::create_dir_all(skills_dir.join("review"))
        .await
        .unwrap();
    tokio::fs::create_dir_all(skills_dir.join("summarize"))
        .await
        .unwrap();

    let ctx = make_ctx(json!({ "env": { "HOME": parent.to_string_lossy() } }));
    let snapshot = list_pi_skills(&ctx, &module_dir, Some(&skills_dir)).await;

    assert_eq!(snapshot.adapter_type, "pi_local");
    assert!(snapshot.supported);
    // Pi snapshot has no default warnings.
    assert!(snapshot.warnings.is_empty());
    assert_eq!(snapshot.entries.len(), 2);

    cleanup(&parent).await;
}

#[tokio::test]
async fn list_pi_skills_surfaces_warning_when_no_skills_home() {
    let (parent, module_dir) = make_module_layout("list-no-home");
    tokio::fs::create_dir_all(&module_dir).await.unwrap();
    let ctx = make_ctx(json!({}));
    let snapshot = list_pi_skills(&ctx, &module_dir, None).await;
    assert!(!snapshot.warnings.is_empty());

    cleanup(&parent).await;
}

// ---------------------------------------------------------------------------
// sync_pi_skills — end-to-end
// ---------------------------------------------------------------------------

#[tokio::test]
#[cfg(unix)]
async fn sync_pi_skills_end_to_end_full_lifecycle() {
    let (parent, module_dir) = make_module_layout("sync-e2e");
    let source_a = parent.join("src-a");
    let source_b = parent.join("src-b");
    tokio::fs::create_dir_all(&source_a).await.unwrap();
    tokio::fs::create_dir_all(&source_b).await.unwrap();
    let skills_home = parent.join("home");
    tokio::fs::create_dir_all(&skills_home).await.unwrap();
    let external_root = parent.join("external");
    tokio::fs::create_dir_all(&external_root).await.unwrap();
    std::os::unix::fs::symlink(&external_root, skills_home.join("external-symlink")).unwrap();

    let ctx = make_ctx(json!({
        "paperclipRuntimeSkills": [
            { "key": "paperclipai/paperclip/a", "runtimeName": "a", "source": source_a.to_string_lossy() },
            { "key": "paperclipai/paperclip/b", "runtimeName": "b", "source": source_b.to_string_lossy() },
        ]
    }));

    // 1. Sync with both desired → a & b symlinks created.
    let desired = vec![
        "paperclipai/paperclip/a".to_string(),
        "paperclipai/paperclip/b".to_string(),
    ];
    let snap1 = sync_pi_skills(&ctx, &desired, &module_dir, &skills_home).await;
    assert_eq!(snap1.adapter_type, "pi_local");
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

    // 2. Sync with only "a" desired → "b" symlink removed.
    let desired_a_only = vec!["paperclipai/paperclip/a".to_string()];
    let _ = sync_pi_skills(&ctx, &desired_a_only, &module_dir, &skills_home).await;
    assert!(tokio::fs::symlink_metadata(skills_home.join("a"))
        .await
        .is_ok());
    assert!(tokio::fs::symlink_metadata(skills_home.join("b"))
        .await
        .is_err());

    // 3. Sync with no desired → "a" also removed; external untouched.
    let _ = sync_pi_skills(&ctx, &[], &module_dir, &skills_home).await;
    assert!(tokio::fs::symlink_metadata(skills_home.join("a"))
        .await
        .is_err());
    let meta = tokio::fs::symlink_metadata(skills_home.join("external-symlink"))
        .await
        .unwrap();
    assert!(meta.file_type().is_symlink());

    cleanup(&parent).await;
}

#[tokio::test]
async fn sync_pi_skills_accepts_empty_desired_skills() {
    let (parent, module_dir) = make_module_layout("sync-empty");
    let source = parent.join("src");
    tokio::fs::create_dir_all(&source).await.unwrap();
    let skills_home = parent.join("home");
    tokio::fs::create_dir_all(&skills_home).await.unwrap();
    let ctx = make_ctx(json!({
        "paperclipRuntimeSkills": [
            { "key": "paperclipai/paperclip/only", "runtimeName": "only", "source": source.to_string_lossy() },
        ]
    }));
    let snap = sync_pi_skills(&ctx, &[], &module_dir, &skills_home).await;
    assert_eq!(snap.adapter_type, "pi_local");
    assert_eq!(snap.entries.len(), 1);

    cleanup(&parent).await;
}

// ---------------------------------------------------------------------------
// resolve_pi_desired_skill_names — end-to-end
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resolve_desired_names_with_configured_sync_preference() {
    let ctx = make_ctx(json!({
        "paperclipSkillSync": {
            "desiredSkills": [
                "paperclipai/paperclip/foo",
                "paperclipai/paperclip/bar",
            ]
        }
    }));
    let entries = vec![
        pc_acpx::skill_snapshot::AdapterSkillEntry {
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
        },
        pc_acpx::skill_snapshot::AdapterSkillEntry {
            key: "paperclipai/paperclip/bar".to_string(),
            runtime_name: Some("bar".to_string()),
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
        },
    ];
    let desired = resolve_pi_desired_skill_names(&ctx, &entries);
    assert_eq!(desired.len(), 2);
    assert!(desired.contains(&"paperclipai/paperclip/foo".to_string()));
    assert!(desired.contains(&"paperclipai/paperclip/bar".to_string()));
}

// ---------------------------------------------------------------------------
// build_pi_skill_snapshot — direct call parity
// ---------------------------------------------------------------------------

#[tokio::test]
async fn build_snapshot_matches_list_call() {
    let (parent, module_dir) = make_module_layout("build-vs-list");
    tokio::fs::create_dir_all(&module_dir).await.unwrap();
    let skills_dir = parent.join("skills");
    tokio::fs::create_dir_all(skills_dir.join("alpha"))
        .await
        .unwrap();

    let ctx = make_ctx(json!({ "env": { "HOME": parent.to_string_lossy() } }));
    let via_list = list_pi_skills(&ctx, &module_dir, Some(&skills_dir)).await;
    let via_build = build_pi_skill_snapshot(&ctx, &module_dir, &skills_dir).await;

    assert_eq!(via_list.adapter_type, via_build.adapter_type);
    assert_eq!(via_list.entries.len(), via_build.entries.len());
    assert_eq!(via_list.mode, via_build.mode);

    cleanup(&parent).await;
}
