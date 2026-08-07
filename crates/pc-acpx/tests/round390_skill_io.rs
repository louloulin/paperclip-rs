//! R390 — Integration tests for `skill_io` (Node parity surface).
//!
//! Mirrors Node parity surface in `adapter-utils/src/server-utils.ts`:
//! - `PAPERCLIP_SKILL_ROOT_RELATIVE_CANDIDATES` (L125-128)
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
//! Unit tests inside `skill_io::tests` cover each function in
//! isolation; this integration suite verifies cross-cutting flows:
//! end-to-end list → read → markdown → symlink → cleanup.

use pc_acpx::{
    ensure_paperclip_skill_symlink, is_maintainer_only_skill_target, list_paperclip_skill_entries,
    normalize_configured_paperclip_runtime_skills, read_installed_skill_targets,
    read_paperclip_runtime_skill_entries, read_paperclip_skill_markdown,
    remove_maintainer_only_skill_symlinks, resolve_paperclip_skills_dir,
    skill_snapshot::PaperclipSkillEntry, SkillSymlinkOutcome, PAPERCLIP_SKILL_KEY_PREFIX,
    PAPERCLIP_SKILL_ROOT_RELATIVE_CANDIDATES,
};
use serde_json::{json, Map};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
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
        "pc-acpx-r390-{label}-{nanos}-{}",
        std::process::id()
    ))
}

/// Build a parent/module layout where `../../skills` lex-normalises to
/// `parent/skills` (mirrors Node `moduleDir/../../skills`).
fn make_module_layout(label: &str) -> (PathBuf, PathBuf, PathBuf) {
    let parent = unique_dir(label);
    let module_dir = parent.join("a").join("b");
    let skills_dir = parent.join("skills");
    (parent, module_dir, skills_dir)
}

async fn cleanup(path: &Path) {
    let _ = tokio::fs::remove_dir_all(path).await;
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

#[test]
fn constants_match_node_literals() {
    assert_eq!(
        PAPERCLIP_SKILL_ROOT_RELATIVE_CANDIDATES,
        &["../../skills", "../../../../../skills"],
    );
    assert_eq!(PAPERCLIP_SKILL_KEY_PREFIX, "paperclipai/paperclip");
}

// ---------------------------------------------------------------------------
// isMaintainerOnlySkillTarget
// ---------------------------------------------------------------------------

#[test]
fn maintainer_target_recognises_dot_agents_segment() {
    assert!(is_maintainer_only_skill_target(
        "/home/alice/.agents/skills/foo"
    ));
    assert!(is_maintainer_only_skill_target(
        "/Users/alice/.agents/skills/bar"
    ));
    // absolute path needed for the "/.agents/skills/" segment to appear.
    assert!(is_maintainer_only_skill_target("/srv/.agents/skills/baz"));
}

#[test]
fn maintainer_target_rejects_other_paths() {
    assert!(!is_maintainer_only_skill_target("/home/alice/skills/foo"));
    assert!(!is_maintainer_only_skill_target(
        "/Users/alice/agents/skills/foo"
    ));
    assert!(!is_maintainer_only_skill_target("/tmp/foo"));
    assert!(!is_maintainer_only_skill_target(""));
}

#[test]
fn maintainer_target_handles_windows_backslashes() {
    assert!(is_maintainer_only_skill_target(
        "C:\\Users\\alice\\.agents\\skills\\foo"
    ));
    assert!(!is_maintainer_only_skill_target(
        "C:\\Users\\alice\\skills\\foo"
    ));
}

// ---------------------------------------------------------------------------
// resolvePaperclipSkillsDir — end-to-end
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resolve_uses_additional_candidate_when_relative_missing() {
    let (parent, module_dir, _skills_dir) = make_module_layout("r390-addl");
    tokio::fs::create_dir_all(&module_dir).await.unwrap();
    // Create an additional absolute candidate that exists.
    let extra = parent.join("extra-skills");
    tokio::fs::create_dir_all(&extra).await.unwrap();
    let resolved = resolve_paperclip_skills_dir(&module_dir, std::slice::from_ref(&extra)).await;
    assert_eq!(resolved, Some(extra));
    cleanup(&parent).await;
}

#[tokio::test]
async fn resolve_first_existing_wins_over_additional() {
    let (parent, module_dir, skills_dir) = make_module_layout("r390-prefer");
    tokio::fs::create_dir_all(&module_dir).await.unwrap();
    tokio::fs::create_dir_all(&skills_dir).await.unwrap();
    let extra = parent.join("extra-skills");
    tokio::fs::create_dir_all(&extra).await.unwrap();
    let resolved = resolve_paperclip_skills_dir(&module_dir, std::slice::from_ref(&extra)).await;
    // skills_dir (../../skills) is tried before the additional candidate.
    assert_eq!(resolved, Some(skills_dir));
    cleanup(&parent).await;
}

// ---------------------------------------------------------------------------
// listPaperclipSkillEntries — end-to-end with read_paperclip_skill_markdown
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_then_markdown_round_trip() {
    let (parent, module_dir, skills_dir) = make_module_layout("r390-rt");
    tokio::fs::create_dir_all(&module_dir).await.unwrap();
    tokio::fs::create_dir_all(skills_dir.join("alpha"))
        .await
        .unwrap();
    tokio::fs::create_dir_all(skills_dir.join("beta"))
        .await
        .unwrap();
    let alpha_md = skills_dir.join("alpha").join("SKILL.md");
    let beta_md = skills_dir.join("beta").join("SKILL.md");
    tokio::fs::write(&alpha_md, "# Alpha\n\nbody")
        .await
        .unwrap();
    tokio::fs::write(&beta_md, "# Beta\n\nbody").await.unwrap();

    let entries = list_paperclip_skill_entries(&module_dir, &[]).await;
    assert_eq!(entries.len(), 2);
    for entry in &entries {
        assert!(entry.key.starts_with(PAPERCLIP_SKILL_KEY_PREFIX));
        let key = entry.key.clone();
        let markdown = read_paperclip_skill_markdown(&module_dir, &key).await;
        assert!(markdown.is_some(), "expected markdown for {key}, got None");
    }
    cleanup(&parent).await;
}

#[tokio::test]
async fn markdown_returns_none_for_unknown_key() {
    let (parent, module_dir, _skills_dir) = make_module_layout("r390-md-none");
    tokio::fs::create_dir_all(&module_dir).await.unwrap();
    let markdown =
        read_paperclip_skill_markdown(&module_dir, "paperclipai/paperclip/missing").await;
    assert!(markdown.is_none());
    cleanup(&parent).await;
}

// ---------------------------------------------------------------------------
// readInstalledSkillTargets — end-to-end (unix only)
// ---------------------------------------------------------------------------

#[tokio::test]
#[cfg(unix)]
async fn read_installed_classifies_dir_file_symlink() {
    let skills_home = unique_dir("r390-inst");
    // Plain directory (no SKILL.md)
    tokio::fs::create_dir_all(skills_home.join("plain-dir"))
        .await
        .unwrap();
    // Regular file
    tokio::fs::write(skills_home.join("readme.txt"), "hi")
        .await
        .unwrap();
    // Symlink
    let target = unique_dir("r390-inst-target");
    tokio::fs::create_dir_all(&target).await.unwrap();
    std::os::unix::fs::symlink(&target, skills_home.join("linked")).unwrap();

    let map: BTreeMap<String, pc_acpx::InstalledSkillTarget> =
        read_installed_skill_targets(&skills_home).await;
    assert_eq!(map.len(), 3);
    use pc_acpx::InstalledSkillTargetKind;
    match map.get("plain-dir").unwrap().kind {
        InstalledSkillTargetKind::Directory => {}
        _ => panic!("plain-dir should classify as Directory"),
    }
    match map.get("readme.txt").unwrap().kind {
        InstalledSkillTargetKind::File => {}
        _ => panic!("readme.txt should classify as File"),
    }
    match map.get("linked").unwrap().kind {
        InstalledSkillTargetKind::Symlink { .. } => {}
        _ => panic!("linked should classify as Symlink"),
    }

    cleanup(&skills_home).await;
    cleanup(&target).await;
}

// ---------------------------------------------------------------------------
// normalizeConfiguredPaperclipRuntimeSkills
// ---------------------------------------------------------------------------

#[test]
fn normalize_drops_invalid_shapes() {
    let invalid = json!([
        // valid (full fields) — kept
        {
            "key": "paperclipai/paperclip/foo",
            "runtimeName": "foo",
            "source": "/skills/foo",
        },
        // trimmed key — kept
        {
            "key": "  paperclipai/paperclip/bar  ",
            "runtimeName": "bar",
            "source": "/skills/bar",
        },
        // missing runtimeName — dropped
        { "key": "paperclipai/paperclip/x", "source": "/x" },
        // missing source — dropped
        { "key": "paperclipai/paperclip/y", "runtimeName": "y" },
        // non-string key — dropped
        { "key": 42, "runtimeName": "z", "source": "/z" },
        // not an object — dropped
        "not-an-object",
        // null — dropped
        null,
    ]);
    let entries = normalize_configured_paperclip_runtime_skills(Some(&invalid));
    assert_eq!(entries.len(), 2);
    let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
    assert!(keys.contains(&"paperclipai/paperclip/foo"));
    // trimmed
    assert!(keys.contains(&"paperclipai/paperclip/bar"));
}

#[test]
fn normalize_returns_empty_for_non_array_value() {
    let scalar = json!({ "key": "paperclipai/paperclip/foo" });
    let entries = normalize_configured_paperclip_runtime_skills(Some(&scalar));
    assert!(entries.is_empty());
}

#[test]
fn normalize_returns_empty_for_none() {
    let entries = normalize_configured_paperclip_runtime_skills(None);
    assert!(entries.is_empty());
}

// ---------------------------------------------------------------------------
// readPaperclipRuntimeSkillEntries — configured preferred, fallback filesystem
// ---------------------------------------------------------------------------

#[tokio::test]
async fn runtime_entries_prefers_configured_when_present() {
    let (parent, module_dir, skills_dir) = make_module_layout("r390-pref");
    tokio::fs::create_dir_all(&module_dir).await.unwrap();
    // Filesystem has alpha + beta
    tokio::fs::create_dir_all(skills_dir.join("alpha"))
        .await
        .unwrap();
    tokio::fs::create_dir_all(skills_dir.join("beta"))
        .await
        .unwrap();
    // Configured has foo (not on filesystem)
    let mut config = Map::new();
    config.insert(
        "paperclipRuntimeSkills".to_string(),
        json!([
            {
                "key": "paperclipai/paperclip/foo",
                "runtimeName": "foo",
                "source": "/skills/foo",
            }
        ]),
    );

    let entries = read_paperclip_runtime_skill_entries(&config, &module_dir, &[]).await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].key, "paperclipai/paperclip/foo");
    cleanup(&parent).await;
}

#[tokio::test]
async fn runtime_entries_falls_back_to_filesystem_when_unconfigured() {
    let (parent, module_dir, skills_dir) = make_module_layout("r390-fs");
    tokio::fs::create_dir_all(&module_dir).await.unwrap();
    tokio::fs::create_dir_all(skills_dir.join("alpha"))
        .await
        .unwrap();
    tokio::fs::create_dir_all(skills_dir.join("beta"))
        .await
        .unwrap();
    let config = Map::new();

    let entries = read_paperclip_runtime_skill_entries(&config, &module_dir, &[]).await;
    assert_eq!(entries.len(), 2);
    cleanup(&parent).await;
}

// ---------------------------------------------------------------------------
// ensurePaperclipSkillSymlink — end-to-end (unix only)
// ---------------------------------------------------------------------------

#[tokio::test]
#[cfg(unix)]
async fn ensure_creates_skips_repairs_real_path() {
    let source = unique_dir("r390-sym-src");
    tokio::fs::create_dir_all(&source).await.unwrap();
    let target_dir = unique_dir("r390-sym-target");
    tokio::fs::create_dir_all(&target_dir).await.unwrap();

    // 1. Missing target -> Created
    let target = target_dir.join("skill-link");
    let outcome = ensure_paperclip_skill_symlink(&source, &target).await;
    assert_eq!(outcome, SkillSymlinkOutcome::Created);
    let meta = tokio::fs::symlink_metadata(&target).await.unwrap();
    assert!(meta.file_type().is_symlink());

    // 2. Symlink already correct -> Skipped
    let outcome2 = ensure_paperclip_skill_symlink(&source, &target).await;
    assert_eq!(outcome2, SkillSymlinkOutcome::Skipped);

    // 3. Regular file in target's place -> Skipped
    let target2 = target_dir.join("file-link");
    tokio::fs::write(&target2, "I am a file").await.unwrap();
    let outcome3 = ensure_paperclip_skill_symlink(&source, &target2).await;
    assert_eq!(outcome3, SkillSymlinkOutcome::Skipped);
    assert!(tokio::fs::symlink_metadata(&target2).await.is_ok());

    // 4. Broken symlink -> Repaired
    let broken = target_dir.join("broken-link");
    std::os::unix::fs::symlink("/nonexistent/expected/path", &broken).unwrap();
    let outcome4 = ensure_paperclip_skill_symlink(&source, &broken).await;
    assert_eq!(outcome4, SkillSymlinkOutcome::Repaired);
    let meta2 = tokio::fs::symlink_metadata(&broken).await.unwrap();
    assert!(meta2.file_type().is_symlink());

    cleanup(&source).await;
    cleanup(&target_dir).await;
}

#[tokio::test]
#[cfg(unix)]
async fn ensure_skips_when_target_resolves_to_real_existing_path() {
    // Source exists, target is a symlink that resolves to a real path
    // (not the source) — must NOT clobber, even though the target is
    // a symlink. Mirrors Node parity.
    let source = unique_dir("r390-sym-src-real");
    tokio::fs::create_dir_all(&source).await.unwrap();
    let other = unique_dir("r390-sym-other-real");
    tokio::fs::create_dir_all(&other).await.unwrap();
    let target_dir = unique_dir("r390-sym-target-real");
    tokio::fs::create_dir_all(&target_dir).await.unwrap();
    let target = target_dir.join("external-link");
    std::os::unix::fs::symlink(&other, &target).unwrap();
    let outcome = ensure_paperclip_skill_symlink(&source, &target).await;
    assert_eq!(outcome, SkillSymlinkOutcome::Skipped);
    let linked = tokio::fs::read_link(&target).await.unwrap();
    assert_eq!(linked, other);

    cleanup(&source).await;
    cleanup(&other).await;
    cleanup(&target_dir).await;
}

// ---------------------------------------------------------------------------
// removeMaintainerOnlySkillSymlinks — end-to-end (unix only)
// ---------------------------------------------------------------------------

#[tokio::test]
#[cfg(unix)]
async fn remove_maintainer_only_filters_only_dot_agents_targets() {
    let skills_home = unique_dir("r390-rm");
    tokio::fs::create_dir_all(&skills_home).await.unwrap();
    let maintainer = unique_dir("r390-rm-maint");
    let dot_agents_skills = maintainer.join(".agents").join("skills");
    tokio::fs::create_dir_all(&dot_agents_skills).await.unwrap();
    let plain = unique_dir("r390-rm-plain");
    tokio::fs::create_dir_all(&plain).await.unwrap();

    // foo-link -> .../.agents/skills/foo (maintainer, should be removed)
    std::os::unix::fs::symlink(dot_agents_skills.join("foo"), skills_home.join("foo-link"))
        .unwrap();
    // bar-link -> .../plain/bar (not maintainer, should remain)
    std::os::unix::fs::symlink(plain.join("bar"), skills_home.join("bar-link")).unwrap();
    // baz-link -> .../.agents/skills/baz (maintainer, but allowed)
    std::os::unix::fs::symlink(dot_agents_skills.join("baz"), skills_home.join("baz-link"))
        .unwrap();
    // qlink.txt (regular file, not a symlink) — should remain untouched
    tokio::fs::write(skills_home.join("readme.txt"), "not a symlink")
        .await
        .unwrap();

    let removed =
        remove_maintainer_only_skill_symlinks(&skills_home, &[String::from("baz-link")]).await;
    assert_eq!(removed, vec!["foo-link".to_string()]);
    // foo-link gone
    assert!(tokio::fs::symlink_metadata(skills_home.join("foo-link"))
        .await
        .is_err());
    // baz-link (allowed) remains
    assert!(tokio::fs::symlink_metadata(skills_home.join("baz-link"))
        .await
        .is_ok());
    // bar-link (not maintainer) remains
    assert!(tokio::fs::symlink_metadata(skills_home.join("bar-link"))
        .await
        .is_ok());
    // readme.txt untouched
    assert!(tokio::fs::symlink_metadata(skills_home.join("readme.txt"))
        .await
        .is_ok());

    cleanup(&skills_home).await;
    cleanup(&maintainer).await;
    cleanup(&plain).await;
}

// ---------------------------------------------------------------------------
// Cross-cutting sanity: PaperclipSkillEntry shape from list
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_emits_well_formed_entries() {
    let (parent, module_dir, skills_dir) = make_module_layout("r390-shape");
    tokio::fs::create_dir_all(&module_dir).await.unwrap();
    tokio::fs::create_dir_all(skills_dir.join("summarize"))
        .await
        .unwrap();
    let entries: Vec<PaperclipSkillEntry> = list_paperclip_skill_entries(&module_dir, &[]).await;
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.runtime_name, "summarize");
    assert_eq!(entry.key, "paperclipai/paperclip/summarize");
    assert!(entry.source.ends_with("skills/summarize"));
    cleanup(&parent).await;
}
