use std::collections::BTreeSet;

use serde_json::Value;
use uuid::Uuid;

use super::keys::legacy_target_keys;

pub(super) fn normalize_target_keys(target_keys: &[String]) -> Vec<String> {
    target_keys
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub(super) fn expand_target_keys(target_keys: &[String]) -> Vec<String> {
    let mut expanded = BTreeSet::new();
    for key in target_keys {
        expanded.insert(key.clone());
        expanded.extend(legacy_target_keys(key));
    }
    expanded.into_iter().collect()
}

pub(super) fn payload_has_displayed_diff(payload: &Value) -> bool {
    let Some(details) = payload
        .get("detailsMarkdown")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return false;
    };
    let lower = details.to_ascii_lowercase();
    let fenced_diff = lower.match_indices("```diff").any(|(index, _)| {
        lower[index + 7..]
            .chars()
            .next()
            .is_none_or(|value| !value.is_ascii_alphanumeric() && value != '_')
    });
    fenced_diff
        || details
            .lines()
            .any(|line| line.starts_with('+') || line.starts_with('-'))
}

pub(super) fn result_consumed(result: &Value) -> bool {
    ["consumedByRunId", "consumedAt"].iter().any(|key| {
        result
            .get(*key)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
    })
}

pub(super) fn row_is_eligible(
    source_run_id: Option<Uuid>,
    payload: &Value,
    result: &Value,
    actor_run_id: Uuid,
    target_keys: &[String],
) -> bool {
    source_run_id.is_some_and(|value| value != actor_run_id)
        && payload
            .get("target")
            .and_then(|target| target.get("type"))
            .and_then(Value::as_str)
            == Some("custom")
        && payload
            .get("target")
            .and_then(|target| target.get("key"))
            .and_then(Value::as_str)
            .is_some_and(|key| target_keys.iter().any(|candidate| candidate == key))
        && result.get("outcome").and_then(Value::as_str) == Some("accepted")
        && !result_consumed(result)
        && payload_has_displayed_diff(payload)
}
