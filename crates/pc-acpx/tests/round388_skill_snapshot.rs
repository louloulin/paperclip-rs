//! R388 — Integration tests for `skill_snapshot` (Node parity surface).
//!
//! Mirrors Node parity surface in `adapter-utils/src/server-utils.ts`:
//! - `skillLocationLabel` (L294-298, internal)
//! - `buildManagedSkillOrigin` (L300-309, internal)
//! - `isPaperclipSkillSourceMissing` (L311-313, internal)
//! - `resolvePaperclipSkillMissingDetail` (L315-320, internal)
//! - `resolveSkillDetail` (L322-330, internal)
//! - `buildRuntimeMountedSkillSnapshot` (L2491-2608)
//! - `buildPersistentSkillSnapshot` (L2609-2734)
//!
//! The unit tests inside `skill_snapshot::tests` already exercise every
//! branch in isolation; the integration tests below focus on
//! cross-cutting behaviour:
//!
//! - end-to-end coverage of the two snapshot builders against the
//!   realistic Node data shapes
//! - interaction between `SkillDetail` static / closure / default
//!   variants
//! - sort + warning + desired-skill-entry invariants

use pc_acpx::{
    build_managed_skill_origin, build_persistent_skill_snapshot,
    build_runtime_mounted_skill_snapshot, is_paperclip_skill_source_missing,
    resolve_paperclip_skill_missing_detail, resolve_skill_detail, skill_location_label,
    skill_snapshot::PaperclipSkillEntry, AdapterSkillEntry, AdapterSkillOrigin, AdapterSkillState,
    AdapterSkillSyncMode, InstalledSkillTarget, InstalledSkillTargetKind,
    PaperclipSkillSourceStatus, PersistentSkillSnapshotOptions, RuntimeMountedSkillSnapshotOptions,
    SkillDetail,
};
use std::collections::BTreeMap;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn entry(key: &str, runtime_name: &str, source: &str) -> PaperclipSkillEntry {
    PaperclipSkillEntry {
        key: key.to_string(),
        runtime_name: runtime_name.to_string(),
        source: source.to_string(),
        version_id: None,
        current_version_id: None,
        source_status: PaperclipSkillSourceStatus::Available,
        missing_detail: None,
    }
}

fn missing_entry(
    key: &str,
    runtime_name: &str,
    source: &str,
    detail: Option<&str>,
) -> PaperclipSkillEntry {
    PaperclipSkillEntry {
        key: key.to_string(),
        runtime_name: runtime_name.to_string(),
        source: source.to_string(),
        version_id: None,
        current_version_id: None,
        source_status: PaperclipSkillSourceStatus::Missing,
        missing_detail: detail.map(|s| s.to_string()),
    }
}

fn installed(target_path: Option<&str>) -> InstalledSkillTarget {
    InstalledSkillTarget {
        target_path: target_path.map(|s| s.to_string()),
        kind: InstalledSkillTargetKind::Symlink,
    }
}

fn detail_lookup<'a>(entries: &'a [AdapterSkillEntry], key: &str) -> Option<&'a str> {
    entries
        .iter()
        .find(|entry| entry.key == key)
        .and_then(|entry| entry.detail.as_deref())
}

// ---------------------------------------------------------------------------
// Cross-module parity
// ---------------------------------------------------------------------------

#[test]
fn node_origin_labels_match_const_table() {
    // `AdapterSkillOrigin::label()` is the canonical Node text. We
    // re-derive it via `build_managed_skill_origin` to verify both
    // helpers stay in sync.
    let (origin, label, read_only) = build_managed_skill_origin();
    assert_eq!(origin, AdapterSkillOrigin::CompanyManaged);
    assert_eq!(label, "Managed by Paperclip");
    assert!(!read_only);
    assert_eq!(AdapterSkillOrigin::UserInstalled.label(), "User-installed");
    assert_eq!(
        AdapterSkillOrigin::ExternalUnknown.label(),
        "External or unavailable"
    );
}

#[test]
fn location_label_normalises_node_semantics() {
    assert_eq!(skill_location_label(None), None);
    assert_eq!(skill_location_label(Some("")), None);
    assert_eq!(skill_location_label(Some("   ")), None);
    assert_eq!(
        skill_location_label(Some("  /home/alice/.agents  ")),
        Some("/home/alice/.agents".to_string())
    );
}

#[test]
fn source_missing_detection_returns_node_truthiness() {
    let available = entry("a", "a", "/a");
    assert!(!is_paperclip_skill_source_missing(&available));
    let missing = missing_entry("a", "a", "/a", None);
    assert!(is_paperclip_skill_source_missing(&missing));
}

#[test]
fn missing_detail_falls_back_to_default() {
    let fallback = "Paperclip cannot find this skill in the local runtime skills directory.";
    let entry_with_detail = missing_entry("a", "a", "/a", Some("custom"));
    assert_eq!(
        resolve_paperclip_skill_missing_detail(&entry_with_detail, fallback),
        "custom"
    );
    let entry_without = missing_entry("a", "a", "/a", None);
    assert_eq!(
        resolve_paperclip_skill_missing_detail(&entry_without, fallback),
        fallback
    );
    let entry_blank = missing_entry("a", "a", "/a", Some("   "));
    assert_eq!(
        resolve_paperclip_skill_missing_detail(&entry_blank, fallback),
        fallback
    );
}

// ---------------------------------------------------------------------------
// buildRuntimeMountedSkillSnapshot — end-to-end
// ---------------------------------------------------------------------------

#[test]
fn runtime_snapshot_end_to_end_supported_ephemeral() {
    let options = RuntimeMountedSkillSnapshotOptions {
        adapter_type: "codex-local".to_string(),
        available_entries: vec![
            entry(
                "paperclip/code-review",
                "code-review",
                "/skills/code-review",
            ),
            entry("paperclip/summarize", "summarize", "/skills/summarize"),
        ],
        desired_skills: vec!["paperclip/code-review".to_string()],
        configured_detail: SkillDetail::Static("applied at runtime".to_string()),
        ..Default::default()
    };
    let snapshot = build_runtime_mounted_skill_snapshot(&options);
    assert_eq!(snapshot.adapter_type, "codex-local");
    assert!(snapshot.supported);
    assert_eq!(snapshot.mode, AdapterSkillSyncMode::Ephemeral);
    assert_eq!(snapshot.desired_skills, vec!["paperclip/code-review"]);
    assert!(snapshot.warnings.is_empty());
    assert_eq!(snapshot.entries.len(), 2);
    let review = snapshot
        .entries
        .iter()
        .find(|entry| entry.key == "paperclip/code-review")
        .unwrap();
    assert_eq!(review.state, AdapterSkillState::Configured);
    assert!(review.desired);
    assert_eq!(review.detail, Some("applied at runtime".to_string()));
    let summarize = snapshot
        .entries
        .iter()
        .find(|entry| entry.key == "paperclip/summarize")
        .unwrap();
    assert_eq!(summarize.state, AdapterSkillState::Available);
    assert!(!summarize.desired);
    assert!(summarize.detail.is_none());
}

#[test]
fn runtime_snapshot_uses_dynamic_detail_for_desired_entries() {
    let closure: SkillDetail =
        SkillDetail::Dynamic(Arc::new(|entry| Some(format!("applied-{}", entry.key))));
    let options = RuntimeMountedSkillSnapshotOptions {
        adapter_type: "claude-local".to_string(),
        available_entries: vec![entry("alpha", "alpha", "/a")],
        desired_skills: vec!["alpha".to_string()],
        configured_detail: closure,
        ..Default::default()
    };
    let snapshot = build_runtime_mounted_skill_snapshot(&options);
    assert_eq!(
        detail_lookup(&snapshot.entries, "alpha"),
        Some("applied-alpha")
    );
}

#[test]
fn runtime_snapshot_warns_for_unavailable_desired_skills() {
    let options = RuntimeMountedSkillSnapshotOptions {
        adapter_type: "claude-local".to_string(),
        available_entries: vec![entry("alpha", "alpha", "/a")],
        desired_skills: vec!["alpha".to_string(), "ghost".to_string()],
        configured_detail: SkillDetail::Static("applied".to_string()),
        warnings: Some(vec!["existing warning".to_string()]),
        ..Default::default()
    };
    let snapshot = build_runtime_mounted_skill_snapshot(&options);
    assert_eq!(snapshot.warnings.len(), 2);
    assert_eq!(snapshot.warnings[0], "existing warning");
    assert!(snapshot.warnings[1].contains("ghost"));
    let ghost = snapshot.entries.iter().find(|e| e.key == "ghost").unwrap();
    assert_eq!(ghost.state, AdapterSkillState::Missing);
    assert_eq!(ghost.origin, Some(AdapterSkillOrigin::ExternalUnknown));
    assert_eq!(
        ghost.origin_label,
        Some("External or unavailable".to_string())
    );
}

#[test]
fn runtime_snapshot_includes_external_installed_with_trimmed_label() {
    let mut external = BTreeMap::new();
    external.insert(
        "user-skill".to_string(),
        installed(Some("/external/user-skill")),
    );
    let options = RuntimeMountedSkillSnapshotOptions {
        adapter_type: "gemini-local".to_string(),
        available_entries: vec![entry("alpha", "alpha", "/a")],
        desired_skills: vec!["alpha".to_string()],
        configured_detail: SkillDetail::Static("applied".to_string()),
        external_installed: Some(external),
        external_location_label: Some("  /external  ".to_string()),
        external_detail: Some("outside".to_string()),
        skills_home: Some("/skills/home".to_string()),
        ..Default::default()
    };
    let snapshot = build_runtime_mounted_skill_snapshot(&options);
    let external_entry = snapshot
        .entries
        .iter()
        .find(|e| e.key == "user-skill")
        .unwrap();
    assert_eq!(external_entry.state, AdapterSkillState::External);
    assert_eq!(external_entry.location_label, Some("/external".to_string()));
    assert_eq!(
        external_entry.origin,
        Some(AdapterSkillOrigin::UserInstalled)
    );
    assert!(external_entry.read_only);
    assert!(!external_entry.managed);
    assert_eq!(
        external_entry.target_path,
        Some("/external/user-skill".to_string())
    );
}

#[test]
fn runtime_snapshot_external_target_falls_back_to_skills_home() {
    // When `externalInstalled.targetPath` is missing and `skillsHome`
    // is supplied, the builder joins the home with the entry name.
    let mut external = BTreeMap::new();
    external.insert(
        "user-skill".to_string(),
        InstalledSkillTarget {
            target_path: None,
            kind: InstalledSkillTargetKind::Directory,
        },
    );
    let options = RuntimeMountedSkillSnapshotOptions {
        adapter_type: "test".to_string(),
        available_entries: vec![],
        desired_skills: vec![],
        configured_detail: SkillDetail::None,
        external_installed: Some(external),
        skills_home: Some("/skills/home".to_string()),
        external_detail: Some("outside".to_string()),
        ..Default::default()
    };
    let snapshot = build_runtime_mounted_skill_snapshot(&options);
    let external_entry = snapshot.entries.first().unwrap();
    assert_eq!(
        external_entry.target_path,
        Some("/skills/home/user-skill".to_string())
    );
}

#[test]
fn runtime_snapshot_skips_external_when_runtime_name_collides() {
    // Runtime name collision is resolved by skipping the external
    // entry — the available entry already covers the slot.
    let mut external = BTreeMap::new();
    external.insert("alpha".to_string(), installed(Some("/external/alpha")));
    let options = RuntimeMountedSkillSnapshotOptions {
        adapter_type: "test".to_string(),
        available_entries: vec![entry("alpha", "alpha", "/skills/alpha")],
        desired_skills: vec![],
        configured_detail: SkillDetail::None,
        external_installed: Some(external),
        ..Default::default()
    };
    let snapshot = build_runtime_mounted_skill_snapshot(&options);
    assert_eq!(snapshot.entries.len(), 1);
    assert_eq!(snapshot.entries[0].key, "alpha");
}

#[test]
fn runtime_snapshot_desired_skill_entries_preserve_order_and_version() {
    let mut code_review = entry(
        "paperclip/code-review",
        "code-review",
        "/skills/code-review",
    );
    code_review.version_id = Some("1.2.3".to_string());
    let mut summarize = entry("paperclip/summarize", "summarize", "/skills/summarize");
    summarize.version_id = Some("9.9".to_string());
    let options = RuntimeMountedSkillSnapshotOptions {
        adapter_type: "test".to_string(),
        available_entries: vec![code_review, summarize],
        desired_skills: vec![
            "paperclip/summarize".to_string(),
            "paperclip/code-review".to_string(),
        ],
        configured_detail: SkillDetail::Static("applied".to_string()),
        ..Default::default()
    };
    let snapshot = build_runtime_mounted_skill_snapshot(&options);
    assert_eq!(
        snapshot.desired_skill_entries,
        vec![
            pc_acpx::AdapterDesiredSkillEntry {
                key: "paperclip/summarize".to_string(),
                version_id: Some("9.9".to_string()),
            },
            pc_acpx::AdapterDesiredSkillEntry {
                key: "paperclip/code-review".to_string(),
                version_id: Some("1.2.3".to_string()),
            },
        ]
    );
}

// ---------------------------------------------------------------------------
// buildPersistentSkillSnapshot — end-to-end
// ---------------------------------------------------------------------------

#[test]
fn persistent_snapshot_installed_when_target_matches_source() {
    let mut installed_map = BTreeMap::new();
    installed_map.insert(
        "code-review".to_string(),
        installed(Some("/skills/code-review")),
    );
    let options = PersistentSkillSnapshotOptions {
        adapter_type: "codex-local".to_string(),
        available_entries: vec![entry(
            "paperclip/code-review",
            "code-review",
            "/skills/code-review",
        )],
        desired_skills: vec!["paperclip/code-review".to_string()],
        installed: installed_map,
        skills_home: "/skills/home".to_string(),
        missing_detail: "missing".to_string(),
        external_conflict_detail: "conflict".to_string(),
        external_detail: "external".to_string(),
        warnings: None,
        location_label: None,
        installed_detail: Some("symlink OK".to_string()),
    };
    let snapshot = build_persistent_skill_snapshot(&options);
    assert_eq!(snapshot.mode, AdapterSkillSyncMode::Persistent);
    assert!(snapshot.supported);
    let review = &snapshot.entries[0];
    assert_eq!(review.state, AdapterSkillState::Installed);
    assert!(review.managed);
    assert_eq!(review.detail, Some("symlink OK".to_string()));
    assert_eq!(
        review.target_path,
        Some("/skills/home/code-review".to_string())
    );
}

#[test]
fn persistent_snapshot_stale_when_installed_but_no_longer_desired() {
    let mut installed_map = BTreeMap::new();
    installed_map.insert(
        "code-review".to_string(),
        installed(Some("/skills/code-review")),
    );
    let options = PersistentSkillSnapshotOptions {
        adapter_type: "codex-local".to_string(),
        available_entries: vec![entry(
            "paperclip/code-review",
            "code-review",
            "/skills/code-review",
        )],
        desired_skills: vec![],
        installed: installed_map,
        skills_home: "/skills/home".to_string(),
        missing_detail: "missing".to_string(),
        external_conflict_detail: "conflict".to_string(),
        external_detail: "external".to_string(),
        warnings: None,
        location_label: None,
        installed_detail: None,
    };
    let snapshot = build_persistent_skill_snapshot(&options);
    assert_eq!(snapshot.entries[0].state, AdapterSkillState::Stale);
    assert!(snapshot.entries[0].managed);
    // `installedDetail ?? null` — when the caller leaves it unset,
    // the stale entry has no detail.
    assert!(snapshot.entries[0].detail.is_none());
}

#[test]
fn persistent_snapshot_external_when_target_path_differs() {
    let mut installed_map = BTreeMap::new();
    installed_map.insert(
        "code-review".to_string(),
        installed(Some("/elsewhere/code-review")),
    );
    let options = PersistentSkillSnapshotOptions {
        adapter_type: "codex-local".to_string(),
        available_entries: vec![entry(
            "paperclip/code-review",
            "code-review",
            "/skills/code-review",
        )],
        desired_skills: vec!["paperclip/code-review".to_string()],
        installed: installed_map,
        skills_home: "/skills/home".to_string(),
        missing_detail: "missing".to_string(),
        external_conflict_detail: "conflict-detail".to_string(),
        external_detail: "external-detail".to_string(),
        warnings: None,
        location_label: None,
        installed_detail: None,
    };
    let snapshot = build_persistent_skill_snapshot(&options);
    assert_eq!(snapshot.entries[0].state, AdapterSkillState::External);
    assert!(!snapshot.entries[0].managed);
    assert_eq!(
        snapshot.entries[0].detail,
        Some("conflict-detail".to_string())
    );
}

#[test]
fn persistent_snapshot_external_when_not_desired_uses_external_detail() {
    let mut installed_map = BTreeMap::new();
    installed_map.insert(
        "code-review".to_string(),
        installed(Some("/elsewhere/code-review")),
    );
    let options = PersistentSkillSnapshotOptions {
        adapter_type: "codex-local".to_string(),
        available_entries: vec![entry(
            "paperclip/code-review",
            "code-review",
            "/skills/code-review",
        )],
        desired_skills: vec![],
        installed: installed_map,
        skills_home: "/skills/home".to_string(),
        missing_detail: "missing".to_string(),
        external_conflict_detail: "conflict-detail".to_string(),
        external_detail: "external-detail".to_string(),
        warnings: None,
        location_label: None,
        installed_detail: None,
    };
    let snapshot = build_persistent_skill_snapshot(&options);
    assert_eq!(snapshot.entries[0].state, AdapterSkillState::External);
    assert_eq!(
        snapshot.entries[0].detail,
        Some("external-detail".to_string())
    );
}

#[test]
fn persistent_snapshot_missing_when_desired_but_not_installed() {
    let options = PersistentSkillSnapshotOptions {
        adapter_type: "codex-local".to_string(),
        available_entries: vec![entry(
            "paperclip/code-review",
            "code-review",
            "/skills/code-review",
        )],
        desired_skills: vec!["paperclip/code-review".to_string()],
        installed: BTreeMap::new(),
        skills_home: "/skills/home".to_string(),
        missing_detail: "missing-detail".to_string(),
        external_conflict_detail: "conflict".to_string(),
        external_detail: "external".to_string(),
        warnings: None,
        location_label: None,
        installed_detail: None,
    };
    let snapshot = build_persistent_skill_snapshot(&options);
    assert_eq!(snapshot.entries[0].state, AdapterSkillState::Missing);
    assert_eq!(
        snapshot.entries[0].detail,
        Some("missing-detail".to_string())
    );
}

#[test]
fn persistent_snapshot_external_installed_without_target_path_uses_skills_home() {
    let mut installed_map = BTreeMap::new();
    installed_map.insert(
        "user-skill".to_string(),
        InstalledSkillTarget {
            target_path: None,
            kind: InstalledSkillTargetKind::Directory,
        },
    );
    let options = PersistentSkillSnapshotOptions {
        adapter_type: "codex-local".to_string(),
        available_entries: vec![entry(
            "paperclip/code-review",
            "code-review",
            "/skills/code-review",
        )],
        desired_skills: vec![],
        installed: installed_map,
        skills_home: "/skills/home".to_string(),
        missing_detail: "missing".to_string(),
        external_conflict_detail: "conflict".to_string(),
        external_detail: "external-detail".to_string(),
        warnings: None,
        location_label: Some("  /external  ".to_string()),
        installed_detail: None,
    };
    let snapshot = build_persistent_skill_snapshot(&options);
    let external = snapshot
        .entries
        .iter()
        .find(|entry| entry.key == "user-skill")
        .unwrap();
    assert_eq!(external.state, AdapterSkillState::External);
    assert_eq!(
        external.target_path,
        Some("/skills/home/user-skill".to_string())
    );
    assert_eq!(external.detail, Some("external-detail".to_string()));
    assert_eq!(external.location_label, Some("/external".to_string()));
}

// ---------------------------------------------------------------------------
// resolveSkillDetail across variants
// ---------------------------------------------------------------------------

#[test]
fn resolve_skill_detail_static_and_closure_variants() {
    let entry = entry("alpha", "alpha", "/skills/alpha");
    assert_eq!(resolve_skill_detail(&SkillDetail::None, &entry), None);
    assert_eq!(
        resolve_skill_detail(&SkillDetail::Static("static".to_string()), &entry),
        Some("static".to_string())
    );
    let dynamic: SkillDetail =
        SkillDetail::Dynamic(Arc::new(|e| Some(format!("dyn-{}", e.runtime_name))));
    assert_eq!(
        resolve_skill_detail(&dynamic, &entry),
        Some("dyn-alpha".to_string())
    );
}

#[test]
fn skill_detail_supports_default_construction() {
    let none: SkillDetail = SkillDetail::default();
    let entry = entry("alpha", "alpha", "/a");
    assert_eq!(resolve_skill_detail(&none, &entry), None);
}
