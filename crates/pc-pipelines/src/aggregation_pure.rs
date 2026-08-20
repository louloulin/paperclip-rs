#![forbid(unsafe_code)]

//! Pipeline aggregation pure helpers — 1:1 port of
//! paperclip/server/src/services/pipelines-aggregation.ts.
//!
//! Pure decision logic for limiting, parsing, and classifying pipeline data.
//! DB-bound query functions stay in pc-pipelines::service.

/// Default limit for `listPipelineAttention`.
pub const PIPELINE_ATTENTION_DEFAULT_LIMIT: u32 = 50;

/// Maximum limit for `listPipelineAttention`.
pub const PIPELINE_ATTENTION_MAX_LIMIT: u32 = 100;

/// Default limit for `listCompanyCaseEvents`.
pub const COMPANY_CASE_EVENTS_DEFAULT_LIMIT: u32 = 50;

/// Maximum limit for `listCompanyCaseEvents`.
pub const COMPANY_CASE_EVENTS_MAX_LIMIT: u32 = 100;

/// Maximum distinct event types accepted in one `listCompanyCaseEvents` request.
pub const COMPANY_CASE_EVENTS_MAX_TYPES: u32 = 10;

/// Maximum tree nodes returned by `getCaseChildrenTree`.
pub const CASE_CHILDREN_TREE_MAX_NODES: u32 = 1_000;

/// Maximum depth walked by `getCaseChildrenTree`.
pub const CASE_CHILDREN_TREE_MAX_DEPTH: u32 = 10;

/// Bound a pagination limit (Node `boundedLimit`).
///
/// `min(max, max(1, floor(limit ?? fallback)))`. Always returns ≥ 1.
pub fn bounded_limit(limit: Option<u32>, fallback: u32, max: u32) -> u32 {
    let mut l = limit.unwrap_or(fallback);
    if l == 0 {
        l = fallback;
    }
    if l > max {
        l = max;
    }
    if l < 1 {
        l = 1;
    }
    l
}

/// Extract a non-empty trimmed string field from a JSON object (Node `payloadString`).
///
/// Returns `None` if `value` is not an object, the key is missing, or the value
/// is not a non-empty trimmed string.
pub fn payload_string(value: Option<&serde_json::Value>, key: &str) -> Option<String> {
    let obj = value?.as_object()?;
    let raw = obj.get(key)?;
    raw.as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Decide whether a list of event types exceeds the max-types guard.
pub fn exceeds_max_event_types(types: &[String]) -> bool {
    types.len() > COMPANY_CASE_EVENTS_MAX_TYPES as usize
}

/// Clamp a list of event types to the max-types guard.
pub fn clamp_event_types(types: Vec<String>) -> Vec<String> {
    if types.len() <= COMPANY_CASE_EVENTS_MAX_TYPES as usize {
        types
    } else {
        types.into_iter().take(COMPANY_CASE_EVENTS_MAX_TYPES as usize).collect()
    }
}

/// Decide whether a node count exceeds the tree max-nodes guard.
pub fn exceeds_max_tree_nodes(count: usize) -> bool {
    count > CASE_CHILDREN_TREE_MAX_NODES as usize
}

/// Decide whether a depth exceeds the tree max-depth guard.
pub fn exceeds_max_tree_depth(depth: usize) -> bool {
    depth > CASE_CHILDREN_TREE_MAX_DEPTH as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bounded_limit_uses_fallback_for_none() {
        assert_eq!(bounded_limit(None, 50, 100), 50);
    }

    #[test]
    fn bounded_limit_uses_fallback_for_zero() {
        assert_eq!(bounded_limit(Some(0), 50, 100), 50);
    }

    #[test]
    fn bounded_limit_caps_at_max() {
        assert_eq!(bounded_limit(Some(500), 50, 100), 100);
        assert_eq!(bounded_limit(Some(100), 50, 100), 100);
    }

    #[test]
    fn bounded_limit_floors_at_one() {
        assert_eq!(bounded_limit(Some(1), 50, 100), 1);
    }

    #[test]
    fn bounded_limit_passes_through_normal() {
        assert_eq!(bounded_limit(Some(25), 50, 100), 25);
    }

    #[test]
    fn payload_string_extracts_trimmed() {
        let v = json!({ "wakeReason": "  hello  " });
        assert_eq!(payload_string(Some(&v), "wakeReason").as_deref(), Some("hello"));
    }

    #[test]
    fn payload_string_returns_none_for_missing_key() {
        let v = json!({ "other": "x" });
        assert_eq!(payload_string(Some(&v), "wakeReason"), None);
    }

    #[test]
    fn payload_string_returns_none_for_empty_string() {
        let v = json!({ "wakeReason": "   " });
        assert_eq!(payload_string(Some(&v), "wakeReason"), None);
    }

    #[test]
    fn payload_string_returns_none_for_non_object() {
        assert_eq!(payload_string(Some(&json!("string")), "key"), None);
        assert_eq!(payload_string(Some(&json!([])), "key"), None);
        assert_eq!(payload_string(None, "key"), None);
    }

    #[test]
    fn payload_string_handles_non_string_value() {
        let v = json!({ "wakeReason": 123 });
        assert_eq!(payload_string(Some(&v), "wakeReason"), None);
    }

    #[test]
    fn event_types_within_limit() {
        let types: Vec<String> = (0..5).map(|i| format!("type{i}")).collect();
        assert!(!exceeds_max_event_types(&types));
    }

    #[test]
    fn event_types_at_limit() {
        let types: Vec<String> = (0..10).map(|i| format!("type{i}")).collect();
        assert!(!exceeds_max_event_types(&types));
    }

    #[test]
    fn event_types_exceeds_limit() {
        let types: Vec<String> = (0..11).map(|i| format!("type{i}")).collect();
        assert!(exceeds_max_event_types(&types));
    }

    #[test]
    fn clamp_event_types_under_limit() {
        let types: Vec<String> = vec!["a".into(), "b".into()];
        assert_eq!(clamp_event_types(types), vec!["a", "b"]);
    }

    #[test]
    fn clamp_event_types_over_limit() {
        let types: Vec<String> = (0..15).map(|i| format!("t{i}")).collect();
        let clamped = clamp_event_types(types);
        assert_eq!(clamped.len(), 10);
    }

    #[test]
    fn tree_nodes_under_max() {
        assert!(!exceeds_max_tree_nodes(500));
        assert!(!exceeds_max_tree_nodes(1000));
    }

    #[test]
    fn tree_nodes_exceeds_max() {
        assert!(exceeds_max_tree_nodes(1001));
        assert!(exceeds_max_tree_nodes(5000));
    }

    #[test]
    fn tree_depth_under_max() {
        assert!(!exceeds_max_tree_depth(5));
        assert!(!exceeds_max_tree_depth(10));
    }

    #[test]
    fn tree_depth_exceeds_max() {
        assert!(exceeds_max_tree_depth(11));
        assert!(exceeds_max_tree_depth(20));
    }

    #[test]
    fn constants_match_node_upstream() {
        assert_eq!(PIPELINE_ATTENTION_DEFAULT_LIMIT, 50);
        assert_eq!(PIPELINE_ATTENTION_MAX_LIMIT, 100);
        assert_eq!(COMPANY_CASE_EVENTS_DEFAULT_LIMIT, 50);
        assert_eq!(COMPANY_CASE_EVENTS_MAX_LIMIT, 100);
        assert_eq!(COMPANY_CASE_EVENTS_MAX_TYPES, 10);
        assert_eq!(CASE_CHILDREN_TREE_MAX_NODES, 1_000);
        assert_eq!(CASE_CHILDREN_TREE_MAX_DEPTH, 10);
    }
}