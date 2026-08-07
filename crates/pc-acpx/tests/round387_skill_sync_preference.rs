//! R387 — Integration tests for `skill_sync_preference` (Node parity surface).
//!
//! Mirrors Node parity surface in `adapter-utils/src/server-utils.ts`:
//! - `readPaperclipSkillSyncPreference` (L2794-2834)
//! - `canonicalizeDesiredPaperclipSkillReference` (L2842-2857, internal)
//! - `resolvePaperclipDesiredSkillNames` (L2858-2869)
//! - `writePaperclipSkillSyncPreference` (L2870-2899)
//!
//! The unit tests inside `skill_sync_preference::tests` already exercise
//! every branch in isolation; the integration tests below focus on
//! cross-cutting behaviour:
//!
//! - end-to-end read → resolve → write round-trip
//! - Node-shape parity for the typed / string output variants
//! - explicit / implicit detection through `hasOwnProperty`-equivalent
//!   semantics on `Map.contains_key`
//! - dedup-by-first-seen-key ordering preserved across all three
//!   helpers.

use pc_acpx::{
    canonicalize_desired_paperclip_skill_reference, read_paperclip_skill_sync_preference,
    resolve_paperclip_desired_skill_names, write_paperclip_skill_sync_preference,
    AvailableSkillEntry, PaperclipDesiredSkillEntry, SkillSyncPreferenceInput,
};
use serde_json::{json, Map, Value};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_config(value: Value) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("paperclipSkillSync".to_string(), value);
    map
}

fn available(entries: &[(&str, Option<&str>)]) -> Vec<AvailableSkillEntry> {
    entries
        .iter()
        .map(|(key, runtime_name)| AvailableSkillEntry {
            key: (*key).to_string(),
            runtime_name: runtime_name.map(|s| s.to_string()),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// explicit / implicit semantics
// ---------------------------------------------------------------------------

#[test]
fn read_marks_explicit_when_field_is_present_even_null() {
    // Mirrors `Object.prototype.hasOwnProperty.call(raw, "desiredSkills")`:
    // the field counts as explicit when present in the map, regardless
    // of its value (null, empty array, malformed entries).
    let config = make_config(json!({ "desiredSkills": null }));
    let pref = read_paperclip_skill_sync_preference(&config);
    assert!(pref.explicit);
    assert!(pref.desired_skills.is_empty());
    assert!(pref.desired_skill_entries.is_empty());
}

#[test]
fn read_marks_implicit_when_block_absent() {
    let config: Map<String, Value> = Map::new();
    let pref = read_paperclip_skill_sync_preference(&config);
    assert!(!pref.explicit);
}

#[test]
fn read_marks_implicit_when_field_missing_from_sync_block() {
    // `desiredSkills` is missing but the `paperclipSkillSync` block is
    // present. Explicit must be false in this case.
    let config = make_config(json!({ "other": "value" }));
    let pref = read_paperclip_skill_sync_preference(&config);
    assert!(!pref.explicit);
    assert!(pref.desired_skills.is_empty());
}

#[test]
fn resolve_returns_empty_when_not_explicit() {
    // This is the contract: when the caller has not opted in, the
    // resolver returns `[]` (no implicit skills).
    let config: Map<String, Value> = Map::new();
    let avail = available(&[("paperclip/code-review", None)]);
    assert!(resolve_paperclip_desired_skill_names(&config, &avail).is_empty());
}

// ---------------------------------------------------------------------------
// End-to-end round-trip
// ---------------------------------------------------------------------------

#[test]
fn end_to_end_read_resolve_write_round_trip() {
    // 1. Read a config that contains a string-list `desiredSkills`.
    let read_config = make_config(json!({
        "desiredSkills": ["paperclip/code-review", "paperclip/summarize"]
    }));
    let preference = read_paperclip_skill_sync_preference(&read_config);
    assert!(preference.explicit);
    assert_eq!(preference.desired_skills.len(), 2);

    // 2. Resolve against an available-skills table that matches one of
    //    them exactly and one via runtime-name.
    let avail = available(&[
        ("paperclip/code-review", Some("Code Review")),
        ("paperclip/summarize", Some("Summarize")),
        ("paperclip/unrelated", None),
    ]);
    let resolved = resolve_paperclip_desired_skill_names(&read_config, &avail);
    assert_eq!(
        resolved,
        vec!["paperclip/code-review", "paperclip/summarize"]
    );

    // 3. Write the resolved list back into a fresh config.
    let write_config: Map<String, Value> = Map::new();
    let desired_inputs: Vec<SkillSyncPreferenceInput> = resolved
        .iter()
        .map(|key| SkillSyncPreferenceInput::Key(key.clone()))
        .collect();
    let written = write_paperclip_skill_sync_preference(&write_config, &desired_inputs);
    assert_eq!(
        written.get("paperclipSkillSync"),
        Some(&json!({ "desiredSkills": ["paperclip/code-review", "paperclip/summarize"] }))
    );

    // 4. Reading the written config back must be a no-op (explicit
    //    flag stays true, ordering preserved, no entries lost).
    let reread = read_paperclip_skill_sync_preference(&written);
    assert!(reread.explicit);
    assert_eq!(
        reread.desired_skills,
        vec!["paperclip/code-review", "paperclip/summarize"]
    );
}

#[test]
fn end_to_end_round_trip_with_version_id_promotes_to_typed_shape() {
    // A write that mixes key + typed inputs must emit the typed shape
    // for *every* entry (matching Node: when *any* entry has a
    // versionId, the whole list becomes typed).
    let config: Map<String, Value> = Map::new();
    let desired = vec![
        SkillSyncPreferenceInput::Key("alpha".to_string()),
        SkillSyncPreferenceInput::Entry(PaperclipDesiredSkillEntry {
            key: "beta".to_string(),
            version_id: Some("2.0".to_string()),
        }),
        SkillSyncPreferenceInput::Key("gamma".to_string()),
    ];
    let written = write_paperclip_skill_sync_preference(&config, &desired);
    assert_eq!(
        written.get("paperclipSkillSync"),
        Some(&json!({
            "desiredSkills": [
                { "key": "alpha", "versionId": null },
                { "key": "beta", "versionId": "2.0" },
                { "key": "gamma", "versionId": null },
            ]
        }))
    );

    let reread = read_paperclip_skill_sync_preference(&written);
    assert_eq!(reread.desired_skill_entries.len(), 3);
    assert_eq!(reread.desired_skill_entries[0].key, "alpha");
    assert_eq!(reread.desired_skill_entries[0].version_id, None);
    assert_eq!(
        reread.desired_skill_entries[1].version_id,
        Some("2.0".to_string())
    );
}

// ---------------------------------------------------------------------------
// Canonicalisation rules
// ---------------------------------------------------------------------------

#[test]
fn canonicalize_prefers_exact_key_over_runtime_name() {
    let avail = available(&[
        ("paperclip/code-review", Some("Code Review")),
        ("paperclip/summarize", None),
    ]);
    assert_eq!(
        canonicalize_desired_paperclip_skill_reference("paperclip/code-review", &avail),
        "paperclip/code-review"
    );
}

#[test]
fn canonicalize_returns_normalised_reference_when_no_match() {
    // When no exact key, runtime-name, or slug matches, the helper
    // returns the trimmed-lowercased reference verbatim — matching
    // Node `return normalizedReference` (L2856). This is what allows
    // `resolve_paperclip_desired_skill_names` to surface unknown
    // skills for the runtime to report.
    let avail = available(&[("paperclip/code-review", None)]);
    assert_eq!(
        canonicalize_desired_paperclip_skill_reference("My-Skill", &avail),
        "my-skill"
    );
}

#[test]
fn canonicalize_uses_trailing_slug_only_when_key_has_slashes() {
    // Node `key.trim().toLowerCase().split("/").pop()` returns the
    // last path segment. Mirroring with `rsplit('/').next()` works
    // for any depth of nesting.
    let avail = available(&[
        ("team-a/paperclip/code-review", None),
        ("team-b/paperclip/code-review", None),
    ]);
    // Ambiguous slug (two matches) → falls through to the normalised
    // reference.
    assert_eq!(
        canonicalize_desired_paperclip_skill_reference("code-review", &avail),
        "code-review"
    );

    // Unique slug → resolves to that entry.
    let avail2 = available(&[
        ("team-a/paperclip/code-review", None),
        ("team-b/paperclip/summarize", None),
    ]);
    assert_eq!(
        canonicalize_desired_paperclip_skill_reference("summarize", &avail2),
        "team-b/paperclip/summarize"
    );
}

#[test]
fn resolve_preserves_unknown_skills_in_output() {
    // The resolver is *not* a filter — it surfaces unknown skill
    // names so the runtime can complain about them downstream. This
    // matches Node, which only filters out `""` from the canonical
    // output.
    let config = make_config(json!({
        "desiredSkills": ["alpha", "BETA"]
    }));
    let avail = available(&[("alpha", None)]);
    let resolved = resolve_paperclip_desired_skill_names(&config, &avail);
    assert_eq!(resolved, vec!["alpha", "beta"]);
}

// ---------------------------------------------------------------------------
// Dedup ordering invariants
// ---------------------------------------------------------------------------

#[test]
fn read_dedup_keeps_first_seen_version_id() {
    // When the same key appears twice with different versionIds, the
    // first-seen entry wins (matches `byKey.has(entry.key) ? skip :
    // set`).
    let config = make_config(json!({
        "desiredSkills": [
            { "key": "alpha", "versionId": "1" },
            { "key": "alpha", "versionId": "2" },
            { "key": "beta" },
        ]
    }));
    let pref = read_paperclip_skill_sync_preference(&config);
    assert_eq!(pref.desired_skills, vec!["alpha", "beta"]);
    assert_eq!(
        pref.desired_skill_entries[0].version_id,
        Some("1".to_string())
    );
}

#[test]
fn write_dedup_keeps_first_seen_version_id() {
    let config: Map<String, Value> = Map::new();
    let desired = vec![
        SkillSyncPreferenceInput::Entry(PaperclipDesiredSkillEntry {
            key: "alpha".to_string(),
            version_id: Some("1".to_string()),
        }),
        SkillSyncPreferenceInput::Entry(PaperclipDesiredSkillEntry {
            key: "alpha".to_string(),
            version_id: Some("2".to_string()),
        }),
    ];
    let written = write_paperclip_skill_sync_preference(&config, &desired);
    assert_eq!(
        written.get("paperclipSkillSync"),
        Some(&json!({
            "desiredSkills": [{ "key": "alpha", "versionId": "1" }]
        }))
    );
}
