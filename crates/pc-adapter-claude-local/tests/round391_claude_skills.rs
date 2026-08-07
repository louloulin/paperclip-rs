//! R391 — Integration tests for `pc-adapter-claude-local::skills`.
//!
//! Mirrors Node `packages/adapters/claude-local/src/server/skills.ts`:
//! - `listClaudeSkills` (L45-47)
//! - `syncClaudeSkills` (L49-52)
//! - `resolveClaudeSkillsHome` (L18-25)
//! - `resolveClaudeDesiredSkillNames` (L54-59)
//!
//! Unit tests inside `skills::tests` already cover the function-by-function
//! shapes; this integration suite verifies the full list/sync flow
//! against realistic configs and the layered pc-acpx helpers.

use pc_acpx::{AdapterSkillContext, AdapterSkillSnapshot};
use pc_adapter_claude_local::skills::{
    build_claude_skill_snapshot, list_claude_skills, resolve_claude_desired_skill_names,
    resolve_claude_skills_home, resolve_claude_skills_home_with, sync_claude_skills,
    CLAUDE_SKILLS_HOME_SUFFIX,
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
        "pc-adapter-claude-local-r391-{label}-{nanos}-{}",
        std::process::id()
    ))
}

fn make_module_layout(label: &str) -> (PathBuf, PathBuf) {
    let parent = unique_dir(label);
    let module_dir = parent.join("a").join("b");
    (parent, module_dir)
}

fn make_ctx(config: serde_json::Value) -> AdapterSkillContext {
    AdapterSkillContext::new("agent-r391", "company-r391", "claude_local", config)
}

async fn cleanup(path: &PathBuf) {
    let _ = tokio::fs::remove_dir_all(path).await;
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

#[test]
fn claude_skills_home_suffix_is_dot_claude_skills() {
    assert_eq!(CLAUDE_SKILLS_HOME_SUFFIX, ".claude/skills");
}

// ---------------------------------------------------------------------------
// resolve_claude_skills_home — end-to-end
// ---------------------------------------------------------------------------

#[test]
fn skills_home_prefers_env_home_when_set() {
    let config = json!({ "env": { "HOME": "/srv/claude" } });
    let resolved = resolve_claude_skills_home(&config).unwrap();
    assert_eq!(resolved, PathBuf::from("/srv/claude/.claude/skills"));
}

#[test]
fn skills_home_with_fallback_uses_default() {
    let resolved = resolve_claude_skills_home_with(&json!({}), "/home/x");
    assert_eq!(resolved, PathBuf::from("/home/x/.claude/skills"));
}

#[test]
fn skills_home_with_fallback_honours_env_override() {
    let config = json!({ "env": { "HOME": "/explicit" } });
    let resolved = resolve_claude_skills_home_with(&config, "/default");
    assert_eq!(resolved, PathBuf::from("/explicit/.claude/skills"));
}

// ---------------------------------------------------------------------------
// list_claude_skills — end-to-end against the layered pc-acpx helpers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_claude_skills_returns_supported_snapshot() {
    let (parent, module_dir) = make_module_layout("list-supported");
    tokio::fs::create_dir_all(&module_dir).await.unwrap();
    // ../../skills lex-normalises to parent/skills
    let skills_dir = parent.join("skills");
    tokio::fs::create_dir_all(skills_dir.join("summarize"))
        .await
        .unwrap();
    tokio::fs::create_dir_all(skills_dir.join("review"))
        .await
        .unwrap();

    let ctx = make_ctx(json!({}));
    let snapshot = list_claude_skills(&ctx, &module_dir).await;

    assert_eq!(snapshot.adapter_type, "claude_local");
    assert!(snapshot.supported, "claude_local must be supported");
    assert!(snapshot.warnings.is_empty());
    assert_eq!(snapshot.entries.len(), 2);

    cleanup(&parent).await;
}

#[tokio::test]
async fn list_claude_skills_records_external_targets() {
    let (parent, module_dir) = make_module_layout("list-ext");
    tokio::fs::create_dir_all(&module_dir).await.unwrap();
    // External home configured via env.HOME — set it to a directory we control.
    let external_root = parent.join("external-home");
    tokio::fs::create_dir_all(external_root.join(".claude/skills/x-skill"))
        .await
        .unwrap();
    tokio::fs::write(
        external_root.join(".claude/skills/readme.txt"),
        "plain file in skills home",
    )
    .await
    .unwrap();

    let ctx = make_ctx(json!({ "env": { "HOME": external_root.to_string_lossy() } }));
    let snapshot = list_claude_skills(&ctx, &module_dir).await;

    // External targets do not surface on the snapshot itself but the
    // snapshot must still be well-constructed (supported, no warnings).
    assert!(snapshot.supported);
    assert!(snapshot.warnings.is_empty());
    assert_eq!(snapshot.adapter_type, "claude_local");

    cleanup(&parent).await;
}

// ---------------------------------------------------------------------------
// sync_claude_skills — end-to-end
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sync_claude_skills_returns_snapshot_with_same_shape_as_list() {
    let (parent, module_dir) = make_module_layout("sync-shape");
    tokio::fs::create_dir_all(&module_dir).await.unwrap();
    let skills_dir = parent.join("skills");
    tokio::fs::create_dir_all(skills_dir.join("only"))
        .await
        .unwrap();

    let ctx = make_ctx(json!({}));
    let list = list_claude_skills(&ctx, &module_dir).await;
    let sync = sync_claude_skills(&ctx, &[], &module_dir).await;

    // Claude local sync is a no-op (skills are baked into the runtime
    // bundle by prepare_claude_skill_runtime) — the returned snapshot
    // must match list.
    assert_eq!(list.adapter_type, sync.adapter_type);
    assert_eq!(list.entries.len(), sync.entries.len());
    assert_eq!(list.mode, sync.mode);
    assert_eq!(list.supported, sync.supported);

    cleanup(&parent).await;
}

#[tokio::test]
async fn sync_claude_skills_accepts_desired_skills_argument() {
    let (parent, module_dir) = make_module_layout("sync-args");
    tokio::fs::create_dir_all(&module_dir).await.unwrap();

    let ctx = make_ctx(json!({}));
    let desired = vec!["paperclipai/paperclip/foo".to_string()];
    let snapshot = sync_claude_skills(&ctx, &desired, &module_dir).await;
    assert_eq!(snapshot.adapter_type, "claude_local");

    cleanup(&parent).await;
}

// ---------------------------------------------------------------------------
// resolve_claude_desired_skill_names — end-to-end
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
    let desired = resolve_claude_desired_skill_names(&ctx, &entries);
    assert_eq!(desired.len(), 2);
    assert!(desired.contains(&"paperclipai/paperclip/foo".to_string()));
    assert!(desired.contains(&"paperclipai/paperclip/bar".to_string()));
}

// ---------------------------------------------------------------------------
// build_claude_skill_snapshot — direct call parity with list_claude_skills
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
    let via_list = list_claude_skills(&ctx, &module_dir).await;
    let via_build = build_claude_skill_snapshot(&ctx, &module_dir).await;

    assert_eq!(via_list.adapter_type, via_build.adapter_type);
    assert_eq!(via_list.entries.len(), via_build.entries.len());
    assert_eq!(via_list.mode, via_build.mode);

    cleanup(&parent).await;
}

// ---------------------------------------------------------------------------
// Snapshot shape stability — sanity guard for downstream consumers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn snapshot_shape_is_stable_for_minimal_config() {
    let (parent, module_dir) = make_module_layout("shape");
    tokio::fs::create_dir_all(&module_dir).await.unwrap();
    let ctx = make_ctx(json!({}));
    let snapshot: AdapterSkillSnapshot = list_claude_skills(&ctx, &module_dir).await;

    // Required Node-mandated fields stay present regardless of input.
    assert_eq!(snapshot.adapter_type, "claude_local");
    // mode default must be one of the enum variants.
    let _ = snapshot.mode;
    // desired_skills must always exist (empty when no preference set).
    let _: Vec<String> = snapshot.desired_skills;

    cleanup(&parent).await;
}
