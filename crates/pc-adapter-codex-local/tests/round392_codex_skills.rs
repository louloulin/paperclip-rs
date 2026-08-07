//! R392 — Integration tests for `pc-adapter-codex-local::skills`.
//!
//! Mirrors Node `packages/adapters/codex-local/src/server/skills.ts`:
//! - `listCodexSkills` (L23-25)
//! - `syncCodexSkills` (L27-30)
//! - `resolveCodexDesiredSkillNames` (L32-35)
//! - `buildCodexSkillSnapshot` (L13-21)
//!
//! Unit tests inside `skills::tests` already cover each function in
//! isolation; this integration suite verifies the layered pc-acpx
//! helpers compose correctly for the Codex runtime path.

use pc_acpx::AdapterSkillContext;
use pc_adapter_codex_local::skills::{
    build_codex_skill_snapshot, list_codex_skills, resolve_codex_desired_skill_names,
    sync_codex_skills,
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
        "pc-adapter-codex-local-r392-{label}-{nanos}-{}",
        std::process::id()
    ))
}

fn make_module_layout(label: &str) -> (PathBuf, PathBuf) {
    let parent = unique_dir(label);
    let module_dir = parent.join("a").join("b");
    (parent, module_dir)
}

fn make_ctx(config: serde_json::Value) -> AdapterSkillContext {
    AdapterSkillContext::new("agent-r392", "company-r392", "codex_local", config)
}

async fn cleanup(path: &PathBuf) {
    let _ = tokio::fs::remove_dir_all(path).await;
}

// ---------------------------------------------------------------------------
// list_codex_skills — end-to-end
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_codex_skills_returns_supported_snapshot() {
    let (parent, module_dir) = make_module_layout("list-supported");
    tokio::fs::create_dir_all(&module_dir).await.unwrap();
    // ../../skills lex-normalises to parent/skills
    let skills_dir = parent.join("skills");
    tokio::fs::create_dir_all(skills_dir.join("review"))
        .await
        .unwrap();
    tokio::fs::create_dir_all(skills_dir.join("summarize"))
        .await
        .unwrap();

    let ctx = make_ctx(json!({}));
    let snapshot = list_codex_skills(&ctx, &module_dir).await;

    assert_eq!(snapshot.adapter_type, "codex_local");
    assert!(snapshot.supported, "codex_local must be supported");
    assert!(snapshot.warnings.is_empty());
    assert_eq!(snapshot.entries.len(), 2);

    cleanup(&parent).await;
}

#[tokio::test]
async fn list_codex_skills_handles_missing_skills_directory() {
    let (parent, module_dir) = make_module_layout("list-missing");
    tokio::fs::create_dir_all(&module_dir).await.unwrap();
    // Don't create parent/skills — list returns empty
    let ctx = make_ctx(json!({}));
    let snapshot = list_codex_skills(&ctx, &module_dir).await;
    assert_eq!(snapshot.entries.len(), 0);
    assert!(snapshot.warnings.is_empty());
    cleanup(&parent).await;
}

// ---------------------------------------------------------------------------
// sync_codex_skills — end-to-end
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sync_codex_skills_returns_same_shape_as_list() {
    let (parent, module_dir) = make_module_layout("sync-shape");
    tokio::fs::create_dir_all(&module_dir).await.unwrap();
    let skills_dir = parent.join("skills");
    tokio::fs::create_dir_all(skills_dir.join("only"))
        .await
        .unwrap();

    let ctx = make_ctx(json!({}));
    let list = list_codex_skills(&ctx, &module_dir).await;
    let sync = sync_codex_skills(&ctx, &[], &module_dir).await;

    // Codex local sync is a no-op (skills are baked into the runtime
    // by prepare_codex_skill_runtime) — the snapshot must match list.
    assert_eq!(list.adapter_type, sync.adapter_type);
    assert_eq!(list.entries.len(), sync.entries.len());
    assert_eq!(list.mode, sync.mode);
    assert_eq!(list.supported, sync.supported);

    cleanup(&parent).await;
}

#[tokio::test]
async fn sync_codex_skills_accepts_desired_skills_argument() {
    let (parent, module_dir) = make_module_layout("sync-args");
    tokio::fs::create_dir_all(&module_dir).await.unwrap();

    let ctx = make_ctx(json!({}));
    let desired = vec!["paperclipai/paperclip/foo".to_string()];
    let snapshot = sync_codex_skills(&ctx, &desired, &module_dir).await;
    assert_eq!(snapshot.adapter_type, "codex_local");

    cleanup(&parent).await;
}

// ---------------------------------------------------------------------------
// resolve_codex_desired_skill_names — end-to-end
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
    let desired = resolve_codex_desired_skill_names(&ctx, &entries);
    assert_eq!(desired.len(), 2);
    assert!(desired.contains(&"paperclipai/paperclip/foo".to_string()));
    assert!(desired.contains(&"paperclipai/paperclip/bar".to_string()));
}

// ---------------------------------------------------------------------------
// build_codex_skill_snapshot — direct call parity
// ---------------------------------------------------------------------------

#[tokio::test]
async fn build_snapshot_matches_list_call() {
    let (parent, module_dir) = make_module_layout("build-vs-list");
    tokio::fs::create_dir_all(&module_dir).await.unwrap();
    let skills_dir = parent.join("skills");
    tokio::fs::create_dir_all(skills_dir.join("alpha"))
        .await
        .unwrap();

    let ctx = make_ctx(json!({}));
    let via_list = list_codex_skills(&ctx, &module_dir).await;
    let via_build = build_codex_skill_snapshot(&ctx, &module_dir).await;

    assert_eq!(via_list.adapter_type, via_build.adapter_type);
    assert_eq!(via_list.entries.len(), via_build.entries.len());
    assert_eq!(via_list.mode, via_build.mode);

    cleanup(&parent).await;
}

// ---------------------------------------------------------------------------
// Codex-specific: no skillsHome surfaced (unlike Claude)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn codex_snapshot_does_not_surface_skills_home() {
    // Unlike claude_local (which surfaces an external skillsHome +
    // externalInstalled block), codex_local keeps the snapshot
    // compact: no skillsHome, no external targets, no extra detail.
    let (parent, module_dir) = make_module_layout("no-home");
    tokio::fs::create_dir_all(&module_dir).await.unwrap();
    let skills_dir = parent.join("skills");
    tokio::fs::create_dir_all(skills_dir.join("only"))
        .await
        .unwrap();

    let ctx = make_ctx(json!({
        "env": { "HOME": "/tmp/somewhere-else" } // env is ignored by codex
    }));
    let snapshot = list_codex_skills(&ctx, &module_dir).await;

    // Snapshot remains compact — no warnings about the env override
    // being meaningful for codex (claude uses it; codex doesn't).
    assert!(snapshot.warnings.is_empty());
    assert_eq!(snapshot.entries.len(), 1);

    cleanup(&parent).await;
}
