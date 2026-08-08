//! Round 407 - integration tests for `pc_acpx::server_utils` (Part 3: skill entries).
//!
//! Validates the cross-module composition of skill helpers ported in R407:
//!   - PaperclipSkillEntry / InstalledSkillTarget / AdapterSkillSnapshot wire shape
//!   - normalize_path_slashes / is_maintainer_only_skill_target path helpers
//!   - resolvePaperclipInstanceRootForAdapter home + instance resolution
//!   - readPaperclipSkillSyncPreference / writePaperclipSkillSyncPreference round-trip
//!   - resolvePaperclipDesiredSkillNames canonicalization (key / runtime / slug)
//!   - buildRuntimeMountedSkillSnapshot state matrix
//!   - buildPersistentSkillSnapshot installed/external/missing/stale states
//!   - normalizeConfiguredPaperclipRuntimeSkills config parsing

use std::collections::HashMap;

use pc_acpx::server_utils::{
    build_persistent_skill_snapshot, build_runtime_mounted_skill_snapshot,
    canonicalize_desired_paperclip_skill_reference, expand_home_prefix,
    is_maintainer_only_skill_target, is_paperclip_skill_source_missing,
    normalize_configured_paperclip_runtime_skills, normalize_path_slashes,
    read_paperclip_skill_sync_preference, resolve_paperclip_desired_skill_names,
    resolve_paperclip_instance_root_for_adapter, resolve_paperclip_skill_missing_detail,
    resolve_installed_entry_target, resolve_skill_detail, skill_location_label,
    write_paperclip_skill_sync_preference, AdapterSkillEntry, AdapterSkillOrigin,
    AdapterSkillSnapshot, AdapterSkillState, AdapterSkillSyncMode,
    AvailableSkillRef, InstalledSkillTarget, InstalledSkillTargetKind,
    PaperclipDesiredSkillEntry, PaperclipSkillEntry, PaperclipSkillSourceStatus,
    PersistentSkillSnapshotOptions, ResolveInstanceRootInput,
    RuntimeMountedSkillSnapshotOptions, SkillDetail, SkillSyncWrite,
};

// ===========================================================================
// Path / label helpers
// ===========================================================================

#[test]
fn path_helpers_normalize_windows_paths_and_detect_maintainer_root() {
    assert_eq!(normalize_path_slashes(r"C:\Users\agent\.paperclip\skills"), "C:/Users/agent/.paperclip/skills");
    // Already POSIX → unchanged.
    assert_eq!(normalize_path_slashes("/home/agent/.paperclip/skills"), "/home/agent/.paperclip/skills");
    // Maintainer-only detection on POSIX + Windows paths.
    assert!(is_maintainer_only_skill_target("/home/agent/.agents/skills/foo"));
    assert!(is_maintainer_only_skill_target(r"C:\home\.agents\skills\foo"));
    assert!(!is_maintainer_only_skill_target("/home/agent/.paperclip/skills/foo"));
}

#[test]
fn skill_location_label_round_trips_through_trim() {
    assert_eq!(skill_location_label(Some("/home/agent")), Some("/home/agent".to_string()));
    assert_eq!(skill_location_label(Some("  /home  ")), Some("/home".to_string()));
    assert_eq!(skill_location_label(None), None);
    assert_eq!(skill_location_label(Some("")), None);
}

#[test]
fn expand_home_prefix_supports_tilde_and_absolute_paths() {
    assert_eq!(expand_home_prefix("~", "/home/agent"), "/home/agent");
    assert_eq!(expand_home_prefix("~/skills", "/home/agent"), "/home/agent/skills");
    assert_eq!(expand_home_prefix("/abs/path", "/home/agent"), "/abs/path");
    assert_eq!(expand_home_prefix("relative", "/home/agent"), "relative");
    // Plain `~user` is not supported (Node behavior); passed through unchanged.
    assert_eq!(expand_home_prefix("~other", "/home/agent"), "~other");
}

// ===========================================================================
// resolvePaperclipInstanceRootForAdapter
// ===========================================================================

#[test]
fn resolve_instance_root_priority_chain() {
    // 1. Caller-supplied home + instance → canonical path.
    let out = resolve_paperclip_instance_root_for_adapter(ResolveInstanceRootInput {
        home_dir: Some("/custom/home"),
        instance_id: Some("acpx-prod"),
        env: None,
        default_home_dir: "/home/agent",
    });
    assert_eq!(out, "/custom/home/instances/acpx-prod");

    // 2. Caller home only → instance defaults to "default".
    let out = resolve_paperclip_instance_root_for_adapter(ResolveInstanceRootInput {
        home_dir: Some("/custom/home"),
        instance_id: None,
        env: None,
        default_home_dir: "/home/agent",
    });
    assert_eq!(out, "/custom/home/instances/default");

    // 3. Env-supplied home (via PAPERCLIP_HOME) + caller instance.
    let mut env = HashMap::new();
    env.insert("PAPERCLIP_HOME".to_string(), "/env/home".to_string());
    let out = resolve_paperclip_instance_root_for_adapter(ResolveInstanceRootInput {
        home_dir: None,
        instance_id: Some("ws-1"),
        env: Some(&env),
        default_home_dir: "/home/agent",
    });
    assert_eq!(out, "/env/home/instances/ws-1");

    // 4. Nothing → fallback to default_home_dir/.paperclip/instances/default.
    let out = resolve_paperclip_instance_root_for_adapter(ResolveInstanceRootInput {
        home_dir: None,
        instance_id: None,
        env: None,
        default_home_dir: "/home/agent",
    });
    assert_eq!(out, "/home/agent/.paperclip/instances/default");
}

// ===========================================================================
// Skill sync preference (read / write / canonicalize)
// ===========================================================================

#[test]
fn skill_sync_preference_round_trip_preserves_desired_keys() {
    let cfg = serde_json::json!({
        "paperclipSkillSync": {
            "desiredSkills": [
                "k1",
                { "key": "k2", "versionId": "v2" }
            ]
        }
    });
    // Read parses back to two entries with explicit flag.
    let pref = read_paperclip_skill_sync_preference(&cfg);
    assert!(pref.explicit);
    assert_eq!(
        pref.desired_skills,
        vec!["k1".to_string(), "k2".to_string()]
    );

    // Write back yields the same desiredSkills structure.
    let new_cfg = serde_json::json!({});
    let out = write_paperclip_skill_sync_preference(
        &new_cfg,
        &[
            SkillSyncWrite::Key("k1"),
            SkillSyncWrite::Entry {
                key: "k2",
                version_id: Some("v2".to_string()),
            },
        ],
    );
    let desired = out
        .get("paperclipSkillSync")
        .and_then(|v| v.get("desiredSkills"))
        .unwrap();
    let arr = desired.as_array().expect("array");
    assert_eq!(arr.len(), 2);
}

#[test]
fn write_skill_sync_with_no_versions_emits_string_array() {
    let cfg = serde_json::json!({});
    let out = write_paperclip_skill_sync_preference(
        &cfg,
        &[SkillSyncWrite::Key("k1"), SkillSyncWrite::Key("k2")],
    );
    let desired = out
        .get("paperclipSkillSync")
        .and_then(|v| v.get("desiredSkills"))
        .unwrap();
    assert_eq!(desired, &serde_json::json!(["k1", "k2"]));
}

#[test]
fn write_skill_sync_preserves_other_config_keys() {
    let cfg = serde_json::json!({
        "agent": { "id": "a1" },
        "skillsHome": "/home/skills"
    });
    let out = write_paperclip_skill_sync_preference(
        &cfg,
        &[SkillSyncWrite::Key("k1")],
    );
    // Other top-level keys are untouched.
    assert_eq!(out.get("agent"), cfg.get("agent"));
    assert_eq!(out.get("skillsHome"), cfg.get("skillsHome"));
    // paperclipSkillSync block added.
    assert!(out.get("paperclipSkillSync").is_some());
}

#[test]
fn resolve_desired_skill_names_canonicalizes_across_modes() {
    let cfg = serde_json::json!({
        "paperclipSkillSync": {
            "desiredSkills": ["OWNER/K1", "K2", "k3", "unknown-skill"]
        }
    });
    let avail = vec![
        AvailableSkillRef { key: "owner/k1", runtime_name: Some("k1") },
        AvailableSkillRef { key: "owner/k2", runtime_name: Some("k2") },
        AvailableSkillRef { key: "owner/k3", runtime_name: Some("k3") },
    ];
    let names = resolve_paperclip_desired_skill_names(&cfg, &avail);
    assert_eq!(
        names,
        vec![
            "owner/k1".to_string(),
            "owner/k2".to_string(),
            "owner/k3".to_string(),
            "unknown-skill".to_string()
        ]
    );
}

#[test]
fn resolve_desired_skill_names_returns_empty_when_not_explicit() {
    let cfg = serde_json::json!({ "other": "key" });
    let avail = vec![AvailableSkillRef { key: "owner/k1", runtime_name: Some("k1") }];
    assert!(resolve_paperclip_desired_skill_names(&cfg, &avail).is_empty());
}

// ===========================================================================
// isPaperclipSkillSourceMissing + resolvePaperclipSkillMissingDetail
// ===========================================================================

#[test]
fn source_missing_and_detail_fallback() {
    let mut entry = PaperclipSkillEntry {
        key: "k1".to_string(),
        runtime_name: "k1".to_string(),
        source: "/skills/k1".to_string(),
        version_id: None,
        current_version_id: None,
        source_status: Some(PaperclipSkillSourceStatus::Missing),
        missing_detail: Some("explicit reason".to_string()),
    };
    assert!(is_paperclip_skill_source_missing(&entry));
    assert_eq!(
        resolve_paperclip_skill_missing_detail(&entry, "fallback"),
        "explicit reason"
    );
    entry.missing_detail = Some("   ".to_string());
    assert_eq!(
        resolve_paperclip_skill_missing_detail(&entry, "fallback"),
        "fallback"
    );
    entry.source_status = Some(PaperclipSkillSourceStatus::Available);
    assert!(!is_paperclip_skill_source_missing(&entry));
}

// ===========================================================================
// resolve_skill_detail
// ===========================================================================

#[test]
fn resolve_skill_detail_literal_vs_callback() {
    let entry = PaperclipSkillEntry {
        key: "k1".to_string(),
        runtime_name: "k1".to_string(),
        source: "/skills/k1".to_string(),
        version_id: None,
        current_version_id: None,
        source_status: None,
        missing_detail: None,
    };
    let lit = SkillDetail::Literal("literal");
    assert_eq!(
        resolve_skill_detail(Some(&lit), &entry).as_deref(),
        Some("literal")
    );
    let cb: SkillDetail<'_> = SkillDetail::Callback(&|_e| Some("from-cb".to_string()));
    assert_eq!(
        resolve_skill_detail(Some(&cb), &entry).as_deref(),
        Some("from-cb")
    );
    assert_eq!(resolve_skill_detail(None, &entry), None);
}

// ===========================================================================
// resolveInstalledEntryTarget
// ===========================================================================

#[test]
fn resolve_installed_entry_target_symlink_resolves_relative_path() {
    let target = resolve_installed_entry_target(
        "/home/agent/skills",
        "my-skill",
        InstalledSkillTargetKind::Symlink,
        Some("../../actual/my-skill"),
    );
    assert_eq!(
        target.target_path.as_deref(),
        Some("/home/agent/skills/../../actual/my-skill")
    );
    assert_eq!(target.kind, InstalledSkillTargetKind::Symlink);

    let dir = resolve_installed_entry_target(
        "/home/agent/skills",
        "my-skill",
        InstalledSkillTargetKind::Directory,
        None,
    );
    assert_eq!(dir.target_path.as_deref(), Some("/home/agent/skills/my-skill"));
    assert_eq!(dir.kind, InstalledSkillTargetKind::Directory);
}

// ===========================================================================
// Snapshot builders
// ===========================================================================

#[test]
fn runtime_mounted_snapshot_canonical_state_matrix() {
    let avail = vec![
        PaperclipSkillEntry {
            key: "owner/k1".to_string(),
            runtime_name: "k1".to_string(),
            source: "/skills/k1".to_string(),
            version_id: Some("v1".to_string()),
            current_version_id: None,
            source_status: Some(PaperclipSkillSourceStatus::Available),
            missing_detail: None,
        },
        PaperclipSkillEntry {
            key: "owner/k2".to_string(),
            runtime_name: "k2".to_string(),
            source: "/skills/k2".to_string(),
            version_id: None,
            current_version_id: None,
            source_status: Some(PaperclipSkillSourceStatus::Missing),
            missing_detail: Some("k2 is gone".to_string()),
        },
    ];
    let desired = vec!["owner/k1".to_string(), "owner/missing".to_string()];
    let snap = build_runtime_mounted_skill_snapshot(RuntimeMountedSkillSnapshotOptions {
        adapter_type: "acpx",
        available_entries: &avail,
        desired_skills: &desired,
        configured_detail: SkillDetail::Literal("k1 configured"),
        missing_detail: Some("generic missing"),
        mode: Some(AdapterSkillSyncMode::Ephemeral),
        supported: None,
        unsupported_detail: None,
        warnings: None,
        external_installed: None,
        external_location_label: None,
        external_detail: None,
        skills_home: None,
    });
    assert!(snap.supported);
    assert_eq!(snap.mode, AdapterSkillSyncMode::Ephemeral);
    // Sorted by key: k1, k2, missing.
    assert_eq!(snap.entries.len(), 3);
    assert_eq!(snap.entries[0].key, "owner/k1");
    assert_eq!(snap.entries[0].state, AdapterSkillState::Configured);
    assert_eq!(snap.entries[0].detail.as_deref(), Some("k1 configured"));
    assert_eq!(snap.entries[1].key, "owner/k2");
    assert_eq!(snap.entries[1].state, AdapterSkillState::Missing);
    assert_eq!(snap.entries[1].detail.as_deref(), Some("k2 is gone"));
    // The unavailable-desired entry becomes "missing" with the
    // fallback detail.
    assert_eq!(snap.entries[2].key, "owner/missing");
    assert_eq!(snap.entries[2].state, AdapterSkillState::Missing);
    assert_eq!(snap.entries[2].detail.as_deref(), Some("generic missing"));
    assert!(snap.warnings.iter().any(|w| w.contains("owner/missing")));
    // desiredSkillEntries preserves the requested order.
    assert_eq!(
        snap.desired_skill_entries.as_ref().unwrap(),
        &vec![
            PaperclipDesiredSkillEntry {
                key: "owner/k1".to_string(),
                version_id: Some("v1".to_string()),
            },
            PaperclipDesiredSkillEntry {
                key: "owner/missing".to_string(),
                version_id: None,
            },
        ]
    );
}

#[test]
fn persistent_snapshot_marks_installed_external_stale_and_missing() {
    let avail = vec![
        PaperclipSkillEntry {
            key: "owner/k1".to_string(),
            runtime_name: "k1".to_string(),
            source: "/skills/k1".to_string(), // matches installed target
            version_id: None,
            current_version_id: None,
            source_status: Some(PaperclipSkillSourceStatus::Available),
            missing_detail: None,
        },
        PaperclipSkillEntry {
            key: "owner/k2".to_string(),
            runtime_name: "k2".to_string(),
            source: "/skills/k2".to_string(), // mismatched install
            version_id: None,
            current_version_id: None,
            source_status: Some(PaperclipSkillSourceStatus::Available),
            missing_detail: None,
        },
        PaperclipSkillEntry {
            key: "owner/k3".to_string(),
            runtime_name: "k3".to_string(),
            source: "/skills/k3".to_string(),
            version_id: None,
            current_version_id: None,
            source_status: Some(PaperclipSkillSourceStatus::Available),
            missing_detail: None,
        },
    ];
    let mut installed = HashMap::new();
    installed.insert(
        "k1".to_string(),
        InstalledSkillTarget {
            target_path: Some("/skills/k1".to_string()),
            kind: InstalledSkillTargetKind::Symlink,
        },
    );
    installed.insert(
        "k2".to_string(),
        InstalledSkillTarget {
            target_path: Some("/other/path".to_string()),
            kind: InstalledSkillTargetKind::Symlink,
        },
    );
    installed.insert(
        "external".to_string(),
        InstalledSkillTarget {
            target_path: Some("/home/external".to_string()),
            kind: InstalledSkillTargetKind::Directory,
        },
    );
    let desired = vec!["owner/k1".to_string(), "owner/k2".to_string(), "owner/k3".to_string()];
    let snap = build_persistent_skill_snapshot(PersistentSkillSnapshotOptions {
        adapter_type: "acpx",
        available_entries: &avail,
        desired_skills: &desired,
        installed: Some(&installed),
        skills_home: "/skills",
        location_label: None,
        installed_detail: Some("installed OK"),
        missing_detail: "not installed",
        external_conflict_detail: "conflict with non-managed install",
        external_detail: "non-managed install",
        warnings: None,
    });
    assert!(snap.supported);
    assert_eq!(snap.mode, AdapterSkillSyncMode::Persistent);
    let by_key: HashMap<String, AdapterSkillEntry> = snap
        .entries
        .iter()
        .map(|e| (e.key.clone(), e.clone()))
        .collect();
    // k1: installed + desired → Installed + managed.
    assert_eq!(by_key["owner/k1"].state, AdapterSkillState::Installed);
    assert!(by_key["owner/k1"].managed);
    // k2: installed elsewhere → External.
    assert_eq!(by_key["owner/k2"].state, AdapterSkillState::External);
    assert!(!by_key["owner/k2"].managed);
    // k3: desired + not installed → Missing.
    assert_eq!(by_key["owner/k3"].state, AdapterSkillState::Missing);
    // External entry (not in available_entries) appears as external.
    let external_entry = snap
        .entries
        .iter()
        .find(|e| e.key == "external")
        .expect("external entry");
    assert_eq!(external_entry.state, AdapterSkillState::External);
    assert!(!external_entry.managed);
    assert_eq!(
        external_entry.origin,
        Some(AdapterSkillOrigin::UserInstalled)
    );
}

// ===========================================================================
// normalizeConfiguredPaperclipRuntimeSkills
// ===========================================================================

#[test]
fn normalize_configured_runtime_skills_handles_alternate_field_names() {
    let cfg = serde_json::json!([
        { "key": "owner/k1", "runtimeName": "k1", "source": "/skills/k1", "versionId": "v1" },
        { "key": "owner/k2", "name": "k2", "source": "/skills/k2" }, // via `name`
        { "key": "", "runtimeName": "x", "source": "/x" }, // empty key → skip
        { "key": "owner/k3", "runtimeName": "", "source": "/skills/k3" },
        { "key": "owner/k4", "runtimeName": "k4", "source": "" },
        { "not-an-object": true },
        {
            "key": "owner/k5",
            "runtimeName": "k5",
            "source": "/skills/k5",
            "versionId": "v5",
            "currentVersionId": "v5-current",
            "sourceStatus": "missing",
            "missingDetail": "explicit"
        }
    ]);
    let entries = normalize_configured_paperclip_runtime_skills(&cfg);
    assert_eq!(entries.len(), 3);
    let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
    assert_eq!(keys, vec!["owner/k1", "owner/k2", "owner/k5"]);
    assert_eq!(entries[2].source_status, Some(PaperclipSkillSourceStatus::Missing));
    assert_eq!(entries[2].missing_detail.as_deref(), Some("explicit"));
    assert_eq!(entries[2].current_version_id.as_deref(), Some("v5-current"));
}

#[test]
fn normalize_configured_runtime_skills_handles_non_array() {
    let cfg = serde_json::json!({"key": "owner/k1"});
    let entries = normalize_configured_paperclip_runtime_skills(&cfg);
    assert!(entries.is_empty());
}

// ===========================================================================
// canonicalizeDesiredPaperclipSkillReference edge cases
// ===========================================================================

#[test]
fn canonicalize_handles_ambiguous_runtime_names_by_falling_through() {
    // Two entries with the same runtime name → ambiguity, fall through
    // to slug matching.
    let avail = vec![
        AvailableSkillRef { key: "owner/x", runtime_name: Some("shared") },
        AvailableSkillRef { key: "owner/y", runtime_name: Some("shared") },
    ];
    // Exact key match wins.
    assert_eq!(
        canonicalize_desired_paperclip_skill_reference("owner/x", &avail),
        "owner/x"
    );
    // Runtime-name ambiguous → fall through → no slug match → pass-through.
    assert_eq!(
        canonicalize_desired_paperclip_skill_reference("shared", &avail),
        "shared"
    );
}

#[test]
fn canonicalize_unique_slug_match_wins() {
    // Single entry, runtime name differs from slug — slug match must win
    // when no exact key + no exact runtime-name hit.
    let avail = vec![AvailableSkillRef {
        key: "owner/unique-slug",
        runtime_name: Some("different-runtime-name"),
    }];
    assert_eq!(
        canonicalize_desired_paperclip_skill_reference("unique-slug", &avail),
        "owner/unique-slug"
    );
}

// ===========================================================================
// Wire format: serialize AdapterSkillSnapshot as JSON
// ===========================================================================

#[test]
fn adapter_skill_snapshot_serializes_to_camelcase_wire_shape() {
    let snap = AdapterSkillSnapshot {
        adapter_type: "test-adapter".to_string(),
        supported: true,
        mode: AdapterSkillSyncMode::Ephemeral,
        desired_skills: vec!["k1".to_string()],
        desired_skill_entries: Some(vec![PaperclipDesiredSkillEntry {
            key: "k1".to_string(),
            version_id: Some("v1".to_string()),
        }]),
        entries: vec![AdapterSkillEntry {
            key: "k1".to_string(),
            runtime_name: Some("k1".to_string()),
            version_id: Some("v1".to_string()),
            current_version_id: None,
            desired: true,
            managed: true,
            state: AdapterSkillState::Configured,
            origin: Some(AdapterSkillOrigin::CompanyManaged),
            origin_label: Some("Managed by Paperclip".to_string()),
            location_label: None,
            read_only: Some(false),
            source_path: Some("/skills/k1".to_string()),
            target_path: None,
            detail: Some("ready".to_string()),
        }],
        warnings: vec![],
    };
    let j = serde_json::to_value(&snap).expect("serialize");
    assert_eq!(j["adapterType"], "test-adapter");
    assert_eq!(j["supported"], true);
    assert_eq!(j["mode"], "ephemeral");
    assert_eq!(j["desiredSkills"], serde_json::json!(["k1"]));
    assert_eq!(j["desiredSkillEntries"][0]["key"], "k1");
    assert_eq!(j["desiredSkillEntries"][0]["versionId"], "v1");
    assert_eq!(j["entries"][0]["state"], "configured");
    assert_eq!(j["entries"][0]["origin"], "company_managed");
    assert_eq!(j["entries"][0]["originLabel"], "Managed by Paperclip");
    assert_eq!(j["entries"][0]["readOnly"], false);
    // Optional null fields are skipped.
    assert!(j["entries"][0].get("currentVersionId").is_none());
    assert!(j["entries"][0].get("locationLabel").is_none());
    assert!(j["entries"][0].get("targetPath").is_none());
}

// ===========================================================================
// Constants parity
// ===========================================================================

#[test]
fn skill_constants_match_node() {
    assert_eq!(MATERIALIZED_SKILL_SENTINEL, ".paperclip-materialized-skill.json");
    assert_eq!(MATERIALIZED_SKILL_LOCK_OWNER, "owner.json");
    assert_eq!(MATERIALIZED_SKILL_LOCK_STALE_MS, 30_000);
    assert_eq!(
        PAPERCLIP_SKILL_ROOT_RELATIVE_CANDIDATES,
        &["../../skills", "../../../../../skills"]
    );
}

// Re-export the constants for the parity test
use pc_acpx::server_utils::{
    MATERIALIZED_SKILL_LOCK_OWNER, MATERIALIZED_SKILL_LOCK_STALE_MS,
    MATERIALIZED_SKILL_SENTINEL, PAPERCLIP_SKILL_ROOT_RELATIVE_CANDIDATES,
};
