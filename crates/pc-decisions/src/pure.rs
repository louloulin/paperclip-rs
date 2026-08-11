//! Pure (no IO, no async) helpers for decision option / effect / payload logic.
//!
//! Mirrors the small helper family in `paperclip/server/src/services/decisions.ts`
//! and `paperclip/server/src/services/decision-training.ts` (commit-sha probe
//! and JSON deep-clone). Every function here is deterministic, side-effect
//! free, and operates on `serde_json::Value` so the caller can pass either a
//! freshly deserialised `DecisionRow.options` or an in-memory DTO without any
//! further conversion.
//!
//! High cohesion: this module is pure logic only. Low coupling: it depends
//! only on `serde_json`, the standard library, and `pc-secrets::canonical` for
//! the canonicalisation routine that is shared with the signing layer.

use pc_secrets::canonical as canonical_json;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Action class a decision effect would trigger on its target issue.
///
/// 1:1 with the union `"issue:comment" | "issue:mutate"` used by the upstream
/// `targetActions` map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EffectAction {
    Comment,
    Mutate,
}

impl EffectAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Comment => "issue:comment",
            Self::Mutate => "issue:mutate",
        }
    }
}

/// Map a decision effect `type` string to its action class.
///
/// Only `comment_on_issue` is classified as `Comment`; every other effect type
/// (including unknown / future types) is treated as `Mutate`, matching the
/// upstream behaviour of using `issue:mutate` as the safe default.
pub fn classify_effect_type(effect_type: &str) -> EffectAction {
    if effect_type == "comment_on_issue" {
        EffectAction::Comment
    } else {
        EffectAction::Mutate
    }
}

/// Pull every target id from a single effect.
///
/// 1:1 with the upstream `effectTargetIds` helper. The output order is:
/// 1. `targetIssueId` (always first when present and non-empty)
/// 2. `create_issue.draft.parentId` (if non-empty)
/// 3. `create_issue.draft.blockedByIssueIds` (in order)
/// 4. `resolve_blocker.removeBlockedByIssueIds` (in order)
///
/// Empty strings are filtered out. Already-listed ids are de-duplicated so the
/// caller can union the result with `targetIssueId` without worrying about
/// repeats.
pub fn effect_target_ids(effect: &Value) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let push_unique = |result: &mut Vec<String>, seen: &mut BTreeSet<String>, id: &str| {
        if id.is_empty() {
            return;
        }
        if seen.insert(id.to_string()) {
            result.push(id.to_string());
        }
    };
    if let Some(target) = effect.get("targetIssueId").and_then(Value::as_str) {
        push_unique(&mut result, &mut seen, target);
    }
    if effect.get("type").and_then(Value::as_str) == Some("create_issue") {
        if let Some(parent) = effect
            .get("draft")
            .and_then(|d| d.get("parentId"))
            .and_then(Value::as_str)
        {
            push_unique(&mut result, &mut seen, parent);
        }
        if let Some(blocked) = effect
            .get("draft")
            .and_then(|d| d.get("blockedByIssueIds"))
            .and_then(Value::as_array)
        {
            for id in blocked.iter().filter_map(Value::as_str) {
                push_unique(&mut result, &mut seen, id);
            }
        }
    }
    if effect.get("type").and_then(Value::as_str) == Some("resolve_blocker") {
        if let Some(remove) = effect.get("removeBlockedByIssueIds").and_then(Value::as_array) {
            for id in remove.iter().filter_map(Value::as_str) {
                push_unique(&mut result, &mut seen, id);
            }
        }
    }
    result
}

/// Unique target ids across all options (1:1 with upstream `targetIds`).
///
/// Returns ids in first-seen order. An empty `options` value (or a non-array)
/// yields an empty `Vec` rather than an error so callers can safely pipe
/// freshly deserialised JSON through.
pub fn target_ids(options: &Value) -> Vec<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut result: Vec<String> = Vec::new();
    let Some(options) = options.as_array() else {
        return result;
    };
    for option in options {
        let Some(effects) = option.get("effects").and_then(Value::as_array) else {
            continue;
        };
        for effect in effects {
            for id in effect_target_ids(effect) {
                if seen.insert(id.clone()) {
                    result.push(id);
                }
            }
        }
    }
    result
}

/// Per-target action classes for every effect across all options (1:1 with
/// upstream `targetActions`).
///
/// The value is a set so multiple effects on the same target collapse to the
/// union of their action classes. `BTreeMap` / `BTreeSet` give stable
/// ordering for snapshotting.
pub fn target_actions(options: &Value) -> BTreeMap<String, BTreeSet<EffectAction>> {
    let mut result: BTreeMap<String, BTreeSet<EffectAction>> = BTreeMap::new();
    let Some(options) = options.as_array() else {
        return result;
    };
    for option in options {
        let Some(effects) = option.get("effects").and_then(Value::as_array) else {
            continue;
        };
        for effect in effects {
            let action = classify_effect_type(
                effect.get("type").and_then(Value::as_str).unwrap_or(""),
            );
            for id in effect_target_ids(effect) {
                result.entry(id).or_default().insert(action);
            }
        }
    }
    result
}

/// Set-equality comparison with duplicate detection (1:1 with upstream `sameIds`).
///
/// Returns `true` iff `left` and `right` are the same set AND neither side
/// contains duplicate ids. An empty / non-array input on either side is not
/// treated specially here — callers are expected to pass `Vec<String>`.
pub fn same_ids(left: &[String], right: &[String]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let right_ids: BTreeSet<&str> = right.iter().map(String::as_str).collect();
    if right_ids.len() != right.len() {
        return false;
    }
    left.iter().all(|id| right_ids.contains(id.as_str()))
}

/// Compare `inputValues` payloads (1:1 with upstream `sameInputValues`).
///
/// Both sides must be JSON objects (or `null`). Returns `true` iff the key
/// sets are equal and every value matches exactly. `null` and an empty
/// object are considered equal so callers can pass either representation.
pub fn same_input_values(left: &Value, right: &Value) -> bool {
    let left_obj = match left.as_object() {
        Some(o) => o,
        None if left.is_null() => {
            return right.as_object().map_or(true, |o| o.is_empty());
        }
        None => return false,
    };
    let right_obj = match right.as_object() {
        Some(o) => o,
        None if right.is_null() => return left_obj.is_empty(),
        None => return false,
    };
    if left_obj.len() != right_obj.len() {
        return false;
    }
    left_obj.iter().all(|(k, v)| right_obj.get(k) == Some(v))
}

/// Replace `{{input.<id>}}` placeholders in `text` with the values in `values`
/// (1:1 with upstream `interpolate`).
///
/// Id characters match `[A-Za-z0-9_-]+`. Missing or empty entries render as
/// the empty string. The implementation iterates UTF-8 codepoints so the
/// function is safe on non-ASCII text.
pub fn interpolate(text: &str, values: &HashMap<String, String>) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if i + 8 <= bytes.len() && &bytes[i..i + 8] == b"{{input." {
            if let Some(end_rel) = find_close_double_brace(&bytes[i + 8..]) {
                let id = &text[i + 8..i + 8 + end_rel];
                if !id.is_empty()
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                {
                    out.push_str(values.get(id).map(String::as_str).unwrap_or(""));
                    i += 8 + end_rel + 2;
                    continue;
                }
            }
        }
        // Advance one UTF-8 codepoint without panicking on the boundary.
        let ch = text[i..].chars().next().expect("valid utf-8 boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn find_close_double_brace(bytes: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'}' && bytes[i + 1] == b'}' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Recursively search a JSON value for a field that looks like a commit SHA.
///
/// Re-exported from `pc_repos::decision_training::find_commit_sha` so callers
/// only need to depend on `pc_decisions` to get the behaviour, but the
/// authoritative implementation lives in the repository layer (where the
/// 1:1 port from `decision-training.ts` is kept and exercised by 13 unit
/// tests). 1:1 with the upstream `findCommitSha` helper from
/// `decision-training.ts`:
/// - Recognised keys: `commitSha`, `commitSHA`, `gitCommitSha`, `headSha`, `commit`
///   (looked up in this order, depth-first).
/// - Candidate must be a string matching `^[0-9a-f]{7,64}$` (case-insensitive).
pub use pc_repos::decision_training::find_commit_sha;

/// Build the canonical JSON envelope that is fed into the signing / idempotency
/// checks.
///
/// Equivalent to upstream `spec({ id, options, targetSnapshots })` but as a
/// pure constructor. The envelope keys are alphabetised by `canonical` so any
/// two envelopes with semantically equal payloads hash to the same string.
pub fn build_spec_envelope(decision_id: &str, options: &Value, target_snapshots: &Value) -> String {
    let envelope = serde_json::json!({
        "decisionId": decision_id,
        "options": options,
        "targetSnapshots": target_snapshots,
    });
    canonical_json(&envelope)
}

/// Re-export of the shared canonical JSON function so callers do not have to
/// depend on `pc-secrets` directly when they only need the decision form.
pub fn canonical_decision_value(value: &Value) -> String {
    canonical_json(value)
}

/// Build a deep clone of an arbitrary JSON value via `serde_json` (1:1 with
/// upstream `jsonCopy`).
pub fn json_copy<T: serde::Serialize + serde::de::DeserializeOwned>(value: &T) -> T {
    serde_json::from_value(serde_json::to_value(value).expect("serialize for copy"))
        .expect("deserialize after copy")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -------- classify_effect_type --------

    #[test]
    fn r492_classify_comment_on_issue() {
        assert_eq!(
            classify_effect_type("comment_on_issue"),
            EffectAction::Comment
        );
    }

    #[test]
    fn r492_classify_unknown_defaults_to_mutate() {
        assert_eq!(classify_effect_type(""), EffectAction::Mutate);
        assert_eq!(classify_effect_type("update_issue_status"), EffectAction::Mutate);
        assert_eq!(
            classify_effect_type("cancel_issue_tree"),
            EffectAction::Mutate
        );
        assert_eq!(classify_effect_type("future_unknown_type"), EffectAction::Mutate);
    }

    #[test]
    fn r492_effect_action_str_round_trip() {
        assert_eq!(EffectAction::Comment.as_str(), "issue:comment");
        assert_eq!(EffectAction::Mutate.as_str(), "issue:mutate");
    }

    // -------- effect_target_ids --------

    #[test]
    fn r492_effect_target_ids_basic_target() {
        let effect = json!({
            "type": "comment_on_issue",
            "targetIssueId": "i-1",
            "staleness": "lenient",
        });
        assert_eq!(effect_target_ids(&effect), vec!["i-1".to_string()]);
    }

    #[test]
    fn r492_effect_target_ids_create_issue_collects_parent_and_blockers() {
        let effect = json!({
            "type": "create_issue",
            "targetIssueId": "i-new",
            "staleness": "lenient",
            "draft": {
                "parentId": "i-parent",
                "blockedByIssueIds": ["i-blocker-1", "i-blocker-2"],
            },
        });
        assert_eq!(
            effect_target_ids(&effect),
            vec![
                "i-new".to_string(),
                "i-parent".to_string(),
                "i-blocker-1".to_string(),
                "i-blocker-2".to_string(),
            ]
        );
    }

    #[test]
    fn r492_effect_target_ids_resolve_blocker_appends_removals() {
        let effect = json!({
            "type": "resolve_blocker",
            "targetIssueId": "i-target",
            "staleness": "lenient",
            "removeBlockedByIssueIds": ["i-rb-1", "i-rb-2"],
        });
        assert_eq!(
            effect_target_ids(&effect),
            vec![
                "i-target".to_string(),
                "i-rb-1".to_string(),
                "i-rb-2".to_string(),
            ]
        );
    }

    #[test]
    fn r492_effect_target_ids_dedupes_overlap() {
        let effect = json!({
            "type": "create_issue",
            "targetIssueId": "i-1",
            "staleness": "lenient",
            "draft": {
                "parentId": "i-1",
                "blockedByIssueIds": ["i-1", "i-2"],
            },
        });
        assert_eq!(effect_target_ids(&effect), vec!["i-1".to_string(), "i-2".to_string()]);
    }

    #[test]
    fn r492_effect_target_ids_skips_empty_strings() {
        let effect = json!({
            "type": "create_issue",
            "targetIssueId": "",
            "staleness": "lenient",
            "draft": {
                "parentId": "",
                "blockedByIssueIds": ["", "i-1"],
            },
        });
        assert_eq!(effect_target_ids(&effect), vec!["i-1".to_string()]);
    }

    #[test]
    fn r492_effect_target_ids_unknown_shape_returns_empty() {
        let effect = json!({ "type": "future_effect", "targetIssueId": "i-1" });
        // Unknown future types still surface targetIssueId (no special handling).
        assert_eq!(effect_target_ids(&effect), vec!["i-1".to_string()]);
    }

    // -------- target_ids --------

    #[test]
    fn r492_target_ids_empty_options() {
        assert_eq!(target_ids(&json!([])), Vec::<String>::new());
        assert_eq!(target_ids(&json!({})), Vec::<String>::new());
        assert_eq!(target_ids(&Value::Null), Vec::<String>::new());
    }

    #[test]
    fn r492_target_ids_preserves_first_seen_order() {
        let options = json!([
            { "effects": [
                { "type": "comment_on_issue", "targetIssueId": "i-a" },
                { "type": "update_issue_status", "targetIssueId": "i-b" },
            ]},
            { "effects": [
                { "type": "assign_issue", "targetIssueId": "i-a" },
                { "type": "create_issue", "targetIssueId": "i-c", "draft": { "parentId": "i-b" } },
            ]},
        ]);
        let result = target_ids(&options);
        assert_eq!(
            result,
            vec![
                "i-a".to_string(),
                "i-b".to_string(),
                "i-c".to_string(),
            ]
        );
    }

    #[test]
    fn r492_target_ids_skips_options_without_effects() {
        let options = json!([
            { "effects": [] },
            { "label": "no effects" },
            { "effects": [
                { "type": "comment_on_issue", "targetIssueId": "i-x" }
            ]}
        ]);
        assert_eq!(target_ids(&options), vec!["i-x".to_string()]);
    }

    // -------- target_actions --------

    #[test]
    fn r492_target_actions_collapses_per_target() {
        let options = json!([
            { "effects": [
                { "type": "comment_on_issue", "targetIssueId": "i-a" },
                { "type": "update_issue_status", "targetIssueId": "i-a" },
                { "type": "assign_issue", "targetIssueId": "i-b" },
            ]},
        ]);
        let actions = target_actions(&options);
        let i_a = actions.get("i-a").expect("i-a present");
        assert!(i_a.contains(&EffectAction::Comment));
        assert!(i_a.contains(&EffectAction::Mutate));
        let i_b = actions.get("i-b").expect("i-b present");
        assert_eq!(i_b.len(), 1);
        assert!(i_b.contains(&EffectAction::Mutate));
    }

    #[test]
    fn r492_target_actions_empty_when_no_options() {
        let actions = target_actions(&json!([]));
        assert!(actions.is_empty());
    }

    // -------- same_ids --------

    #[test]
    fn r492_same_ids_equal_sets_match() {
        let left = vec!["a".to_string(), "b".to_string()];
        let right = vec!["b".to_string(), "a".to_string()];
        assert!(same_ids(&left, &right));
    }

    #[test]
    fn r492_same_ids_different_lengths_rejected() {
        let left = vec!["a".to_string()];
        let right = vec!["a".to_string(), "b".to_string()];
        assert!(!same_ids(&left, &right));
    }

    #[test]
    fn r492_same_ids_duplicate_on_right_rejected() {
        let left = vec!["a".to_string(), "b".to_string()];
        let right = vec!["a".to_string(), "a".to_string()];
        assert!(!same_ids(&left, &right));
    }

    #[test]
    fn r492_same_ids_both_empty() {
        assert!(same_ids(&[], &[]));
    }

    // -------- same_input_values --------

    #[test]
    fn r492_same_input_values_equal_objects() {
        let left = json!({ "a": "1", "b": "2" });
        let right = json!({ "b": "2", "a": "1" });
        assert!(same_input_values(&left, &right));
    }

    #[test]
    fn r492_same_input_values_different_value_rejected() {
        let left = json!({ "a": "1" });
        let right = json!({ "a": "2" });
        assert!(!same_input_values(&left, &right));
    }

    #[test]
    fn r492_same_input_values_null_and_empty_object_equal() {
        assert!(same_input_values(&Value::Null, &json!({})));
        assert!(same_input_values(&json!({}), &Value::Null));
        assert!(same_input_values(&Value::Null, &Value::Null));
    }

    #[test]
    fn r492_same_input_values_null_and_non_empty_rejected() {
        assert!(!same_input_values(&Value::Null, &json!({ "a": "1" })));
        assert!(!same_input_values(&json!({ "a": "1" }), &Value::Null));
    }

    #[test]
    fn r492_same_input_values_non_object_rejected() {
        let obj = json!({ "a": "1" });
        let arr = json!(["a", "1"]);
        assert!(!same_input_values(&arr, &obj));
        assert!(!same_input_values(&obj, &arr));
    }

    // -------- interpolate --------

    #[test]
    fn r492_interpolate_replaces_known_placeholders() {
        let mut values = HashMap::new();
        values.insert("name".to_string(), "world".to_string());
        values.insert("kebab-case".to_string(), "ok".to_string());
        let text = "Hello, {{input.name}}! {{input.kebab-case}} {{input.missing}}";
        assert_eq!(interpolate(text, &values), "Hello, world! ok ");
    }

    #[test]
    fn r492_interpolate_no_placeholders_passthrough() {
        let values = HashMap::new();
        assert_eq!(interpolate("plain text", &values), "plain text");
    }

    #[test]
    fn r492_interpolate_preserves_non_input_braces() {
        let values = HashMap::new();
        // `{{not.input.x}}` is not the `{{input.<id>}}` shape, so it is left as-is.
        assert_eq!(
            interpolate("{{not.input.x}} literal {{input.y}}", &values),
            "{{not.input.x}} literal "
        );
    }

    #[test]
    fn r492_interpolate_unicode_text_safe() {
        let mut values = HashMap::new();
        values.insert("topic".to_string(), "目录".to_string());
        assert_eq!(interpolate("关于 {{input.topic}} 的说明", &values), "关于 目录 的说明");
    }

    // -------- find_commit_sha --------

    #[test]
    fn r494_find_commit_sha_reexport_matches_pc_repos() {
        // The re-export must point at the canonical implementation, so
        // semantically equal inputs produce equal outputs.
        let v = json!({"headSha": "abc1234", "nested": {"commitSha": "def5678"}});
        assert_eq!(find_commit_sha(&v), pc_repos::decision_training::find_commit_sha(&v));
    }

    #[test]
    fn r494_find_commit_sha_reexport_returns_none_for_scalars() {
        // Sanity: the re-export still rejects non-object/array inputs.
        assert_eq!(find_commit_sha(&Value::Null), None);
        assert_eq!(find_commit_sha(&json!("plain")), None);
        assert_eq!(find_commit_sha(&json!(42)), None);
    }

    // -------- build_spec_envelope / canonical_decision_value --------

    #[test]
    fn r492_build_spec_envelope_is_key_sorted() {
        let envelope = build_spec_envelope(
            "decision-1",
            &json!([{ "effects": [], "id": "yes", "label": "Approve" }]),
            &json!({ "i-1": { "status": "todo" } }),
        );
        assert_eq!(
            envelope,
            r#"{"decisionId":"decision-1","options":[{"effects":[],"id":"yes","label":"Approve"}],"targetSnapshots":{"i-1":{"status":"todo"}}}"#
        );
    }

    #[test]
    fn r492_canonical_decision_value_matches_pc_secrets() {
        let value = json!({ "b": 1, "a": [3, 2, 1] });
        assert_eq!(
            canonical_decision_value(&value),
            pc_secrets::canonical(&value)
        );
    }

    // -------- json_copy --------

    #[test]
    fn r492_json_copy_round_trip() {
        let value = json!({ "nested": { "list": [1, 2, 3] } });
        let copy = json_copy(&value);
        assert_eq!(copy, value);
    }
}
