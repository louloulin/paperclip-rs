//! `pc-acpx` skill-sync preference helpers — pure config-shape adapters
//! between `config.paperclipSkillSync.desiredSkills` and the rest of
//! the adapter runtime.
//!
//! Rust port of Node `packages/adapter-utils/src/server-utils.ts`:
//! - `readPaperclipSkillSyncPreference` (L2794-2834)
//! - `canonicalizeDesiredPaperclipSkillReference` (L2842-2857, internal)
//! - `resolvePaperclipDesiredSkillNames` (L2858-2869)
//! - `writePaperclipSkillSyncPreference` (L2870-2899)
//!
//! All helpers operate on a borrowed `serde_json::Map<String, Value>` so
//! they slot into the existing `serde_json::Value`-shaped config objects
//! already used across `pc-acpx` (`build_runtime`, `build_prompt`,
//! `startup_timing`, `normalize`, `transcript`, …).
//!
//! The helpers are pure: no I/O, no async, no global state. They mirror
//! the Node reference behaviour 1:1 — including the dedup-by-first-key
//! rule, the `explicit` flag derived from `Object.prototype.hasOwnProperty`,
//! the dual output shape (`desiredSkills: string[]` vs the typed
//! `desiredSkillEntries` list), and the canonical reference resolution
//! strategy (exact key → single runtimeName → single slug → fallback to
//! the normalised reference).

use serde_json::{Map, Value};

// ============================================================================
// Types
// ============================================================================

/// A single entry in the `desiredSkills` list. Mirrors Node
/// `PaperclipDesiredSkillEntry` (L2794-2834).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaperclipDesiredSkillEntry {
    /// Skill key (the canonical lookup key used by the runtime).
    pub key: String,
    /// Optional pinned version id. `None` means "track latest".
    pub version_id: Option<String>,
}

/// Result of [`read_paperclip_skill_sync_preference`]. Mirrors the
/// `{ explicit, desiredSkills, desiredSkillEntries }` shape returned by
/// Node `readPaperclipSkillSyncPreference` (L2794-2834).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SkillSyncPreference {
    /// `true` when the caller explicitly set `desiredSkills` on the
    /// `paperclipSkillSync` block (even to `[]`); `false` when the
    /// block is absent, non-object, or missing the field.
    pub explicit: bool,
    /// Deduped list of skill keys in the order they were first seen.
    pub desired_skills: Vec<String>,
    /// Deduped list of typed entries. Preserves order and `versionId`
    /// values that the string-only projection drops.
    pub desired_skill_entries: Vec<PaperclipDesiredSkillEntry>,
}

/// One entry from the runtime's available-skills table. Mirrors the
/// structural type consumed by `resolvePaperclipDesiredSkillNames`
/// (L2858-2869) and `canonicalizeDesiredPaperclipSkillReference`
/// (L2842-2857).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableSkillEntry {
    /// Canonical skill key (must match the runtime's lookup table).
    pub key: String,
    /// Optional runtime-side display name (e.g. for Codex-style
    /// `name` → `runtimeName` aliases).
    pub runtime_name: Option<String>,
}

/// Input variant accepted by [`write_paperclip_skill_sync_preference`].
/// Mirrors the union `Array<string | PaperclipDesiredSkillEntry>` at
/// Node L2870-2899.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSyncPreferenceInput {
    /// Plain string skill key (mirrors a `desiredSkills: ["name"]`).
    Key(String),
    /// Typed entry (mirrors `desiredSkills: [{ key, versionId }]`).
    Entry(PaperclipDesiredSkillEntry),
}

// ============================================================================
// readPaperclipSkillSyncPreference
// ============================================================================

/// Read the `paperclipSkillSync` block from a config map and produce a
/// normalised [`SkillSyncPreference`]. Mirrors
/// `readPaperclipSkillSyncPreference` (L2794-2834).
///
/// The `explicit` flag uses `Map.contains_key("desiredSkills")` which is
/// the Rust analogue of `Object.prototype.hasOwnProperty.call(raw,
/// "desiredSkills")`: the key counts as explicit when present, even when
/// its value is `null` or an empty list.
pub fn read_paperclip_skill_sync_preference(config: &Map<String, Value>) -> SkillSyncPreference {
    let Some(raw) = config.get("paperclipSkillSync") else {
        return SkillSyncPreference::default();
    };
    if !raw.is_object() || raw.is_array() {
        return SkillSyncPreference::default();
    }
    let sync_config = raw
        .as_object()
        .expect("non-array object checked above")
        .clone();
    let explicit = sync_config.contains_key("desiredSkills");
    let desired: Vec<Value> = match sync_config.get("desiredSkills") {
        Some(Value::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    let entries = parse_desired_skill_list(&desired);
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut desired_skill_entries: Vec<PaperclipDesiredSkillEntry> = Vec::new();
    for entry in entries {
        if seen.insert(entry.key.clone()) {
            desired_skill_entries.push(entry);
        }
    }
    let desired_skills = desired_skill_entries
        .iter()
        .map(|entry| entry.key.clone())
        .collect();
    SkillSyncPreference {
        explicit,
        desired_skills,
        desired_skill_entries,
    }
}

fn parse_desired_skill_list(items: &[Value]) -> Vec<PaperclipDesiredSkillEntry> {
    let mut out: Vec<PaperclipDesiredSkillEntry> = Vec::new();
    for value in items {
        if let Some(entry) = parse_single_desired_skill(value) {
            out.push(entry);
        }
    }
    out
}

fn parse_single_desired_skill(value: &Value) -> Option<PaperclipDesiredSkillEntry> {
    match value {
        Value::String(s) => {
            let key = s.trim();
            if key.is_empty() {
                None
            } else {
                Some(PaperclipDesiredSkillEntry {
                    key: key.to_string(),
                    version_id: None,
                })
            }
        }
        Value::Object(record) => {
            let key = record.get("key").and_then(Value::as_str)?.trim();
            if key.is_empty() {
                return None;
            }
            let version_id = match record.get("versionId") {
                Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
                _ => None,
            };
            Some(PaperclipDesiredSkillEntry {
                key: key.to_string(),
                version_id,
            })
        }
        _ => None,
    }
}

// ============================================================================
// canonicalizeDesiredPaperclipSkillReference
// ============================================================================

/// Canonicalise a single desired-skill reference against the runtime's
/// available-skills table. Mirrors
/// `canonicalizeDesiredPaperclipSkillReference` (L2842-2857).
///
/// Resolution order:
/// 1. Exact key match (case-insensitive) → that entry's key.
/// 2. Single runtime-name match (case-insensitive) → that entry's key.
/// 3. Single slug match — the trailing path segment of each key
///    (case-insensitive) → that entry's key.
/// 4. Otherwise, the trimmed-lowercased reference (no resolution).
pub fn canonicalize_desired_paperclip_skill_reference(
    reference: &str,
    available_entries: &[AvailableSkillEntry],
) -> String {
    let normalized_reference = reference.trim().to_lowercase();
    if normalized_reference.is_empty() {
        return String::new();
    }

    let exact_key = available_entries
        .iter()
        .find(|entry| entry.key.trim().to_lowercase() == normalized_reference);
    if let Some(entry) = exact_key {
        return entry.key.clone();
    }

    let mut by_runtime_name: Vec<&AvailableSkillEntry> = available_entries
        .iter()
        .filter(|entry| {
            entry
                .runtime_name
                .as_deref()
                .map(|name| name.trim().to_lowercase() == normalized_reference)
                .unwrap_or(false)
        })
        .collect();
    if by_runtime_name.len() == 1 {
        return by_runtime_name[0].key.clone();
    }
    // Mirror the Node fallback that returns `byRuntimeName[0]` when
    // there is exactly one match — the second-element access on a
    // single-element slice is a no-op safety net.
    let _ = &mut by_runtime_name;

    let slug_matches: Vec<&AvailableSkillEntry> = available_entries
        .iter()
        .filter(|entry| {
            entry
                .key
                .trim()
                .to_lowercase()
                .rsplit('/')
                .next()
                .unwrap_or("")
                == normalized_reference
        })
        .collect();
    if slug_matches.len() == 1 {
        return slug_matches[0].key.clone();
    }

    normalized_reference
}

// ============================================================================
// resolvePaperclipDesiredSkillNames
// ============================================================================

/// Resolve the canonical skill-key list for a config against the
/// runtime's available-skills table. Mirrors
/// `resolvePaperclipDesiredSkillNames` (L2858-2869).
///
/// - When the caller did not explicitly set `desiredSkills` (no
///   `paperclipSkillSync.desiredSkills` key), returns an empty list.
/// - References that fail to resolve are dropped (empty after
///   canonicalisation).
/// - The returned list is deduped while preserving first-seen order.
pub fn resolve_paperclip_desired_skill_names(
    config: &Map<String, Value>,
    available_entries: &[AvailableSkillEntry],
) -> Vec<String> {
    let preference = read_paperclip_skill_sync_preference(config);
    if !preference.explicit {
        return Vec::new();
    }
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for reference in &preference.desired_skills {
        let resolved = canonicalize_desired_paperclip_skill_reference(reference, available_entries);
        if resolved.is_empty() {
            continue;
        }
        if seen.insert(resolved.clone()) {
            out.push(resolved);
        }
    }
    out
}

// ============================================================================
// writePaperclipSkillSyncPreference
// ============================================================================

/// Write the desired-skill list into the supplied config, returning a
/// new `Map`. Mirrors `writePaperclipSkillSyncPreference`
/// (L2870-2899).
///
/// - The input config is not mutated; a shallow clone is returned.
/// - When `desiredSkills` already exists as a non-array object, the
///   existing keys are preserved (only `desiredSkills` is overwritten).
/// - The output shape is the compact string list when no entry carries
///   a `versionId`; otherwise the typed entries are emitted.
pub fn write_paperclip_skill_sync_preference(
    config: &Map<String, Value>,
    desired_skills: &[SkillSyncPreferenceInput],
) -> Map<String, Value> {
    let mut next = config.clone();
    let current_object = match next.get("paperclipSkillSync") {
        Some(Value::Object(existing)) => existing.clone(),
        _ => Map::new(),
    };
    let mut next_sync: Map<String, Value> = current_object;

    let entries: Vec<PaperclipDesiredSkillEntry> = desired_skills
        .iter()
        .filter_map(desired_skill_input_to_entry)
        .collect();

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut normalized: Vec<PaperclipDesiredSkillEntry> = Vec::new();
    for entry in entries {
        if seen.insert(entry.key.clone()) {
            normalized.push(entry);
        }
    }

    let has_version_id = normalized.iter().any(|entry| entry.version_id.is_some());
    let desired_skills_value: Value = if has_version_id {
        // When any entry carries a version id we emit the typed shape
        // for *every* entry (matches Node: `normalized` is emitted
        // verbatim even when individual entries have `versionId: null`).
        Value::Array(
            normalized
                .iter()
                .map(|entry| {
                    let mut object = Map::new();
                    object.insert("key".to_string(), Value::String(entry.key.clone()));
                    match &entry.version_id {
                        Some(version_id) => {
                            object
                                .insert("versionId".to_string(), Value::String(version_id.clone()));
                        }
                        None => {
                            object.insert("versionId".to_string(), Value::Null);
                        }
                    }
                    Value::Object(object)
                })
                .collect(),
        )
    } else {
        Value::Array(
            normalized
                .iter()
                .map(|entry| Value::String(entry.key.clone()))
                .collect(),
        )
    };

    next_sync.insert("desiredSkills".to_string(), desired_skills_value);
    next.insert("paperclipSkillSync".to_string(), Value::Object(next_sync));
    next
}

fn desired_skill_input_to_entry(
    value: &SkillSyncPreferenceInput,
) -> Option<PaperclipDesiredSkillEntry> {
    match value {
        SkillSyncPreferenceInput::Key(s) => {
            let key = s.trim();
            if key.is_empty() {
                None
            } else {
                Some(PaperclipDesiredSkillEntry {
                    key: key.to_string(),
                    version_id: None,
                })
            }
        }
        SkillSyncPreferenceInput::Entry(entry) => {
            let key = entry.key.trim();
            if key.is_empty() {
                None
            } else {
                Some(PaperclipDesiredSkillEntry {
                    key: key.to_string(),
                    version_id: entry
                        .version_id
                        .as_deref()
                        .and_then(non_empty_trimmed)
                        .map(str::to_string),
                })
            }
        }
    }
}

fn non_empty_trimmed(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Build a config map containing a `paperclipSkillSync` entry whose
    /// value is the supplied `Value` (typically an object). This
    /// mirrors the way tests construct `config.paperclipSkillSync` in
    /// production code.
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

    // ----- readPaperclipSkillSyncPreference -----

    #[test]
    fn read_returns_default_when_block_missing() {
        let config: Map<String, Value> = Map::new();
        let pref = read_paperclip_skill_sync_preference(&config);
        assert!(!pref.explicit);
        assert!(pref.desired_skills.is_empty());
        assert!(pref.desired_skill_entries.is_empty());
    }

    #[test]
    fn read_returns_default_when_block_is_non_object() {
        for bad in [
            json!("not-an-object"),
            json!(["array"]),
            json!(42),
            json!(true),
        ] {
            let config = make_config(bad.clone());
            let pref = read_paperclip_skill_sync_preference(&config);
            assert!(
                !pref.explicit,
                "non-object block {bad} must yield explicit=false"
            );
            assert!(pref.desired_skills.is_empty());
        }
    }

    #[test]
    fn read_returns_explicit_when_desired_skills_present_even_empty() {
        let config = make_config(json!({ "desiredSkills": [] }));
        let pref = read_paperclip_skill_sync_preference(&config);
        assert!(pref.explicit);
        assert!(pref.desired_skills.is_empty());
        assert!(pref.desired_skill_entries.is_empty());
    }

    #[test]
    fn read_parses_string_desired_skills() {
        let config = make_config(json!({
            "desiredSkills": ["alpha", " beta ", ""]
        }));
        let pref = read_paperclip_skill_sync_preference(&config);
        assert!(pref.explicit);
        assert_eq!(pref.desired_skills, vec!["alpha", "beta"]);
        assert_eq!(
            pref.desired_skill_entries,
            vec![
                PaperclipDesiredSkillEntry {
                    key: "alpha".to_string(),
                    version_id: None
                },
                PaperclipDesiredSkillEntry {
                    key: "beta".to_string(),
                    version_id: None
                },
            ]
        );
    }

    #[test]
    fn read_parses_typed_desired_skill_entries() {
        let config = make_config(json!({
            "desiredSkills": [
                { "key": "alpha", "versionId": "1.2.3" },
                { "key": "beta" },
                { "key": " gamma ", "versionId": "  v2  " },
                { "key": "delta", "versionId": "" },
                { "key": "epsilon", "versionId": null },
                { "key": "" },
                { "key": 42 },
            ]
        }));
        let pref = read_paperclip_skill_sync_preference(&config);
        assert!(pref.explicit);
        assert_eq!(
            pref.desired_skill_entries,
            vec![
                PaperclipDesiredSkillEntry {
                    key: "alpha".to_string(),
                    version_id: Some("1.2.3".to_string())
                },
                PaperclipDesiredSkillEntry {
                    key: "beta".to_string(),
                    version_id: None
                },
                PaperclipDesiredSkillEntry {
                    key: "gamma".to_string(),
                    version_id: Some("v2".to_string())
                },
                PaperclipDesiredSkillEntry {
                    key: "delta".to_string(),
                    version_id: None
                },
                PaperclipDesiredSkillEntry {
                    key: "epsilon".to_string(),
                    version_id: None
                },
            ]
        );
        assert_eq!(
            pref.desired_skills,
            vec!["alpha", "beta", "gamma", "delta", "epsilon"]
        );
    }

    #[test]
    fn read_dedupes_by_first_seen_key() {
        let config = make_config(json!({
            "desiredSkills": [
                { "key": "alpha", "versionId": "1" },
                { "key": "alpha", "versionId": "2" },
                "alpha",
                "beta",
            ]
        }));
        let pref = read_paperclip_skill_sync_preference(&config);
        assert_eq!(pref.desired_skills, vec!["alpha", "beta"]);
        assert_eq!(
            pref.desired_skill_entries,
            vec![
                PaperclipDesiredSkillEntry {
                    key: "alpha".to_string(),
                    version_id: Some("1".to_string())
                },
                PaperclipDesiredSkillEntry {
                    key: "beta".to_string(),
                    version_id: None
                },
            ]
        );
    }

    #[test]
    fn read_ignores_non_string_non_object_items() {
        let config = make_config(json!({
            "desiredSkills": [42, null, true, "alpha"]
        }));
        let pref = read_paperclip_skill_sync_preference(&config);
        assert_eq!(pref.desired_skills, vec!["alpha"]);
    }

    // ----- canonicalizeDesiredPaperclipSkillReference -----

    #[test]
    fn canonicalize_returns_empty_for_blank_reference() {
        let avail = available(&[("paperclip/code-review", None)]);
        assert_eq!(
            canonicalize_desired_paperclip_skill_reference("", &avail),
            ""
        );
        assert_eq!(
            canonicalize_desired_paperclip_skill_reference("   ", &avail),
            ""
        );
    }

    #[test]
    fn canonicalize_prefers_exact_key_case_insensitive() {
        let avail = available(&[
            ("paperclip/code-review", Some("Code Review")),
            ("paperclip/summarize", None),
        ]);
        assert_eq!(
            canonicalize_desired_paperclip_skill_reference("Paperclip/Code-Review", &avail),
            "paperclip/code-review"
        );
    }

    #[test]
    fn canonicalize_resolves_single_runtime_name_match() {
        let avail = available(&[
            ("paperclip/code-review", Some("Code Review")),
            ("paperclip/summarize", Some("Summarize")),
        ]);
        assert_eq!(
            canonicalize_desired_paperclip_skill_reference("summarize", &avail),
            "paperclip/summarize"
        );
    }

    #[test]
    fn canonicalize_skips_ambiguous_runtime_name_matches() {
        let avail = available(&[
            ("paperclip/code-review", Some("Review")),
            ("paperclip/pr-review", Some("review")),
        ]);
        // Both entries match `review` case-insensitively; the resolver
        // falls through to slug matching and finally returns the
        // normalised reference itself.
        assert_eq!(
            canonicalize_desired_paperclip_skill_reference("review", &avail),
            "review"
        );
    }

    #[test]
    fn canonicalize_resolves_single_slug_match() {
        let avail = available(&[
            ("paperclip/code-review", None),
            ("paperclip/summarize", None),
            ("paperclip/pr-review", None),
        ]);
        assert_eq!(
            canonicalize_desired_paperclip_skill_reference("summarize", &avail),
            "paperclip/summarize"
        );
    }

    #[test]
    fn canonicalize_returns_normalized_reference_when_unresolved() {
        let avail = available(&[("paperclip/code-review", None)]);
        assert_eq!(
            canonicalize_desired_paperclip_skill_reference("MYSTERY", &avail),
            "mystery"
        );
    }

    #[test]
    fn canonicalize_trims_whitespace_before_normalization() {
        let avail = available(&[("paperclip/code-review", None)]);
        assert_eq!(
            canonicalize_desired_paperclip_skill_reference("  Paperclip/Code-Review  ", &avail),
            "paperclip/code-review"
        );
    }

    // ----- resolvePaperclipDesiredSkillNames -----

    #[test]
    fn resolve_returns_empty_when_not_explicit() {
        let config: Map<String, Value> = Map::new();
        let avail = available(&[("paperclip/code-review", None)]);
        assert!(resolve_paperclip_desired_skill_names(&config, &avail).is_empty());
    }

    #[test]
    fn resolve_returns_empty_when_explicit_but_empty() {
        let config = make_config(json!({ "desiredSkills": [] }));
        let avail = available(&[("paperclip/code-review", None)]);
        assert!(resolve_paperclip_desired_skill_names(&config, &avail).is_empty());
    }

    #[test]
    fn resolve_canonicalizes_against_available_entries() {
        let config = make_config(json!({
            "desiredSkills": ["Paperclip/Code-Review", "summarize", "MYSTERY"]
        }));
        let avail = available(&[
            ("paperclip/code-review", None),
            ("paperclip/summarize", None),
        ]);
        // `MYSTERY` is preserved with its lowercased form (matches Node
        // `return normalizedReference` when no canonical entry is
        // found — the resolver does *not* drop unmatched references).
        assert_eq!(
            resolve_paperclip_desired_skill_names(&config, &avail),
            vec!["paperclip/code-review", "paperclip/summarize", "mystery"]
        );
    }

    #[test]
    fn resolve_dedupes_after_canonicalization() {
        let config = make_config(json!({
            "desiredSkills": ["summarize", "Paperclip/Summarize"]
        }));
        let avail = available(&[("paperclip/summarize", None)]);
        assert_eq!(
            resolve_paperclip_desired_skill_names(&config, &avail),
            vec!["paperclip/summarize"]
        );
    }

    // ----- writePaperclipSkillSyncPreference -----

    #[test]
    fn write_inserts_block_when_missing() {
        let config: Map<String, Value> = Map::new();
        let desired = vec![SkillSyncPreferenceInput::Key("alpha".to_string())];
        let next = write_paperclip_skill_sync_preference(&config, &desired);
        assert_eq!(
            next.get("paperclipSkillSync"),
            Some(&json!({ "desiredSkills": ["alpha"] }))
        );
    }

    #[test]
    fn write_preserves_existing_sync_block_keys() {
        let mut sync = Map::new();
        sync.insert("mode".to_string(), json!("strict"));
        let mut config = Map::new();
        config.insert("paperclipSkillSync".to_string(), Value::Object(sync));
        let desired = vec![SkillSyncPreferenceInput::Key("alpha".to_string())];
        let next = write_paperclip_skill_sync_preference(&config, &desired);
        assert_eq!(
            next.get("paperclipSkillSync"),
            Some(&json!({
                "mode": "strict",
                "desiredSkills": ["alpha"],
            }))
        );
    }

    #[test]
    fn write_emits_string_list_when_no_version_id() {
        let mut config = Map::new();
        config.insert("unrelated".to_string(), json!("preserved"));
        let desired = vec![
            SkillSyncPreferenceInput::Key("alpha".to_string()),
            SkillSyncPreferenceInput::Key("beta".to_string()),
        ];
        let next = write_paperclip_skill_sync_preference(&config, &desired);
        assert_eq!(
            next.get("paperclipSkillSync"),
            Some(&json!({ "desiredSkills": ["alpha", "beta"] }))
        );
        assert_eq!(next.get("unrelated"), Some(&json!("preserved")));
    }

    #[test]
    fn write_emits_typed_entries_when_any_has_version_id() {
        let config: Map<String, Value> = Map::new();
        let desired = vec![
            SkillSyncPreferenceInput::Entry(PaperclipDesiredSkillEntry {
                key: "alpha".to_string(),
                version_id: Some("1.2.3".to_string()),
            }),
            SkillSyncPreferenceInput::Key("beta".to_string()),
        ];
        let next = write_paperclip_skill_sync_preference(&config, &desired);
        // Node emits the typed shape verbatim for *every* entry once any
        // entry carries a versionId (even null), matching
        // `PaperclipDesiredSkillEntry = { key, versionId }`.
        assert_eq!(
            next.get("paperclipSkillSync"),
            Some(&json!({
                "desiredSkills": [
                    { "key": "alpha", "versionId": "1.2.3" },
                    { "key": "beta", "versionId": null },
                ]
            }))
        );
    }

    #[test]
    fn write_dedupes_by_first_seen_key_and_skips_blank_inputs() {
        let config: Map<String, Value> = Map::new();
        let desired = vec![
            SkillSyncPreferenceInput::Key("alpha".to_string()),
            SkillSyncPreferenceInput::Key("  ".to_string()),
            SkillSyncPreferenceInput::Entry(PaperclipDesiredSkillEntry {
                key: "alpha".to_string(),
                version_id: Some("ignored".to_string()),
            }),
            SkillSyncPreferenceInput::Key("beta".to_string()),
            SkillSyncPreferenceInput::Entry(PaperclipDesiredSkillEntry {
                key: "".to_string(),
                version_id: None,
            }),
        ];
        let next = write_paperclip_skill_sync_preference(&config, &desired);
        // First-seen wins on dedup, so the `Key("alpha")` form
        // (`versionId: null`) clobbers the later typed entry
        // (`versionId: "ignored"`). With no remaining versionId left in
        // `normalized`, the writer emits the compact string list.
        assert_eq!(
            next.get("paperclipSkillSync"),
            Some(&json!({ "desiredSkills": ["alpha", "beta"] }))
        );
    }

    #[test]
    fn write_trims_keys_and_version_ids() {
        let config: Map<String, Value> = Map::new();
        let desired = vec![SkillSyncPreferenceInput::Entry(
            PaperclipDesiredSkillEntry {
                key: "  alpha  ".to_string(),
                version_id: Some("  v1  ".to_string()),
            },
        )];
        let next = write_paperclip_skill_sync_preference(&config, &desired);
        assert_eq!(
            next.get("paperclipSkillSync"),
            Some(&json!({
                "desiredSkills": [{ "key": "alpha", "versionId": "v1" }]
            }))
        );
    }

    #[test]
    fn write_returns_compact_string_when_all_version_ids_blank() {
        let config: Map<String, Value> = Map::new();
        let desired = vec![
            SkillSyncPreferenceInput::Entry(PaperclipDesiredSkillEntry {
                key: "alpha".to_string(),
                version_id: Some("".to_string()),
            }),
            SkillSyncPreferenceInput::Key("beta".to_string()),
        ];
        let next = write_paperclip_skill_sync_preference(&config, &desired);
        assert_eq!(
            next.get("paperclipSkillSync"),
            Some(&json!({ "desiredSkills": ["alpha", "beta"] }))
        );
    }

    #[test]
    fn write_does_not_mutate_input_config() {
        let mut sync = Map::new();
        sync.insert("mode".to_string(), json!("strict"));
        let mut config = Map::new();
        config.insert("paperclipSkillSync".to_string(), Value::Object(sync));
        let original = config.clone();
        let desired = vec![SkillSyncPreferenceInput::Key("alpha".to_string())];
        let _next = write_paperclip_skill_sync_preference(&config, &desired);
        assert_eq!(config, original);
    }
}
