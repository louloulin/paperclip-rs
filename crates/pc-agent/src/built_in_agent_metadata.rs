//! Built-in agent metadata marker (1:1 port of Node `server/src/services/built-in-agent-metadata.ts`, 45 行).
//!
//! 单一职责：在 agent metadata JSON 中读、写、比较 built-in agent 标记。
//!
//! - `BUILT_IN_AGENT_METADATA_KEY = "paperclipBuiltInAgent"` 是 metadata 中的固定 key
//! - `BuiltInAgentMarker { key, featureKeys }` 描述内置 agent 的身份和能力开关
//! - `read_built_in_agent_marker` 安全解析 metadata（任何错误形状都返回 `None`）
//! - `with_built_in_agent_marker` 不可变方式插入 marker
//! - `built_in_agent_markers_equal` 用于比较两个 marker（key + featureKeys 完全相同）
//!
//! 不持有任何状态；不依赖 IO。

use serde_json::{Map, Value};

/// metadata JSON 中的固定 key（与 Node `BUILT_IN_AGENT_METADATA_KEY` 1:1 对齐）。
pub const BUILT_IN_AGENT_METADATA_KEY: &str = "paperclipBuiltInAgent";

/// Built-in agent marker 解析后的结构。
///
/// 字段名用 Rust snake_case 以匹配仓库其它领域模型；
/// JSON 序列化时与 Node 同名（`key` / `featureKeys`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltInAgentMarker {
    pub key: String,
    pub feature_keys: Vec<String>,
}

impl BuiltInAgentMarker {
    pub fn new(key: impl Into<String>, feature_keys: Vec<String>) -> Self {
        Self {
            key: key.into(),
            feature_keys,
        }
    }
}

// ============================================================================
// Internal helpers
// ============================================================================

/// 是否为 plain object（对象且非数组）。对应 Node `isPlainRecord`。
fn is_plain_record(value: &Value) -> bool {
    value.is_object()
}

/// 规范化 feature keys：
/// - 必须是 `string[]`
/// - 每个元素必须是非空字符串（`trim().length > 0`）
/// - 任一条件不满足返回 `None`
///
/// 对应 Node `normalizeFeatureKeys`。
fn normalize_feature_keys(value: &Value) -> Option<Vec<String>> {
    let arr = value.as_array()?;
    let mut feature_keys: Vec<String> = Vec::with_capacity(arr.len());
    for entry in arr {
        let s = entry.as_str()?;
        if s.trim().is_empty() {
            return None;
        }
        feature_keys.push(s.to_string());
    }
    Some(feature_keys)
}

// ============================================================================
// Public API
// ============================================================================

/// 从 metadata JSON 中读取 built-in agent marker。
///
/// 行为（与 Node `readBuiltInAgentMarker` 1:1 对齐）：
/// 1. `metadata` 必须是 plain object
/// 2. `metadata[BUILT_IN_AGENT_METADATA_KEY]` 必须是 plain object
/// 3. `marker.key` 必须是非空 string（`trim().length > 0`）
/// 4. `marker.featureKeys` 必须可规范化为 `string[]`
///
/// 任一条件不满足返回 `None`。
#[must_use]
pub fn read_built_in_agent_marker(metadata: &Value) -> Option<BuiltInAgentMarker> {
    if !is_plain_record(metadata) {
        return None;
    }
    let marker = metadata.get(BUILT_IN_AGENT_METADATA_KEY)?;
    if !is_plain_record(marker) {
        return None;
    }
    let key = marker.get("key")?.as_str()?.trim();
    if key.is_empty() {
        return None;
    }
    let feature_keys_value = marker.get("featureKeys")?;
    let feature_keys = normalize_feature_keys(feature_keys_value)?;
    Some(BuiltInAgentMarker {
        key: key.to_string(),
        feature_keys,
    })
}

/// 在 metadata 上写入 built-in agent marker，返回新的 Map（不动原值）。
///
/// 行为（与 Node `withBuiltInAgentMarker` 1:1 对齐）：
/// - `metadata` 为 `None` → 起点为空对象
/// - 保留原 metadata 中所有其它字段
/// - `marker.featureKeys` 深拷贝（避免外部修改）
#[must_use]
pub fn with_built_in_agent_marker(
    metadata: Option<&Map<String, Value>>,
    marker: &BuiltInAgentMarker,
) -> Map<String, Value> {
    let mut out: Map<String, Value> = match metadata {
        Some(m) => m.clone(),
        None => Map::new(),
    };
    let mut marker_obj = Map::new();
    marker_obj.insert("key".into(), Value::String(marker.key.clone()));
    marker_obj.insert(
        "featureKeys".into(),
        Value::Array(marker.feature_keys.iter().map(|s| Value::String(s.clone())).collect()),
    );
    out.insert(BUILT_IN_AGENT_METADATA_KEY.into(), Value::Object(marker_obj));
    out
}

/// 比较两个 built-in agent marker 是否相等。
///
/// 行为（与 Node `builtInAgentMarkersEqual` 1:1 对齐）：
/// - 都是 `None` → `true`
/// - 一个 `None` 一个非 `None` → `false`
/// - 都是 `Some`：`key` 完全相等 **且** `featureKeys` 序列化后相等
#[must_use]
pub fn built_in_agent_markers_equal(
    left: Option<&BuiltInAgentMarker>,
    right: Option<&BuiltInAgentMarker>,
) -> bool {
    match (left, right) {
        (None, None) => true,
        (None, Some(_)) | (Some(_), None) => false,
        (Some(l), Some(r)) => {
            l.key == r.key && serde_json::to_string(&l.feature_keys).ok()
                == serde_json::to_string(&r.feature_keys).ok()
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- BUILT_IN_AGENT_METADATA_KEY ----

    #[test]
    fn metadata_key_constant_matches_node() {
        assert_eq!(BUILT_IN_AGENT_METADATA_KEY, "paperclipBuiltInAgent");
    }

    // ---- is_plain_record / normalize_feature_keys (indirect) ----

    #[test]
    fn read_marker_returns_none_for_non_object_metadata() {
        assert!(read_built_in_agent_marker(&json!(null)).is_none());
        assert!(read_built_in_agent_marker(&json!("str")).is_none());
        assert!(read_built_in_agent_marker(&json!(42)).is_none());
        assert!(read_built_in_agent_marker(&json!([1, 2, 3])).is_none());
    }

    #[test]
    fn read_marker_returns_none_when_key_missing() {
        let metadata = json!({ "other": "x" });
        assert!(read_built_in_agent_marker(&metadata).is_none());
    }

    #[test]
    fn read_marker_returns_none_when_marker_not_object() {
        let metadata = json!({ "paperclipBuiltInAgent": "not an object" });
        assert!(read_built_in_agent_marker(&metadata).is_none());

        let metadata = json!({ "paperclipBuiltInAgent": ["array", "instead"] });
        assert!(read_built_in_agent_marker(&metadata).is_none());
    }

    #[test]
    fn read_marker_returns_none_when_key_empty_or_not_string() {
        let metadata = json!({
            "paperclipBuiltInAgent": { "key": "", "featureKeys": [] }
        });
        assert!(read_built_in_agent_marker(&metadata).is_none());

        let metadata = json!({
            "paperclipBuiltInAgent": { "key": 42, "featureKeys": [] }
        });
        assert!(read_built_in_agent_marker(&metadata).is_none());

        let metadata = json!({
            "paperclipBuiltInAgent": { "key": "   ", "featureKeys": [] }
        });
        assert!(read_built_in_agent_marker(&metadata).is_none());
    }

    #[test]
    fn read_marker_returns_none_when_feature_keys_invalid() {
        // featureKeys 不是数组
        let metadata = json!({
            "paperclipBuiltInAgent": { "key": "k", "featureKeys": "not an array" }
        });
        assert!(read_built_in_agent_marker(&metadata).is_none());

        // featureKeys 含非字符串
        let metadata = json!({
            "paperclipBuiltInAgent": { "key": "k", "featureKeys": ["ok", 42] }
        });
        assert!(read_built_in_agent_marker(&metadata).is_none());

        // featureKeys 含空字符串
        let metadata = json!({
            "paperclipBuiltInAgent": { "key": "k", "featureKeys": ["ok", "   "] }
        });
        assert!(read_built_in_agent_marker(&metadata).is_none());
    }

    #[test]
    fn read_marker_returns_marker_on_valid_input() {
        let metadata = json!({
            "other": "ignored",
            "paperclipBuiltInAgent": {
                "key": "ceo",
                "featureKeys": ["feature_a", "feature_b"],
            }
        });
        let m = read_built_in_agent_marker(&metadata).unwrap();
        assert_eq!(m.key, "ceo");
        assert_eq!(m.feature_keys, vec!["feature_a", "feature_b"]);
    }

    #[test]
    fn read_marker_accepts_empty_feature_keys_array() {
        let metadata = json!({
            "paperclipBuiltInAgent": { "key": "k", "featureKeys": [] }
        });
        let m = read_built_in_agent_marker(&metadata).unwrap();
        assert_eq!(m.key, "k");
        assert!(m.feature_keys.is_empty());
    }

    #[test]
    fn read_marker_trims_key_whitespace() {
        // Node: `key.trim().length === 0` 判断空，
        // 但保留 trim 后的实际 key（`key.trim()`）作为 marker.key
        let metadata = json!({
            "paperclipBuiltInAgent": { "key": "  ceo  ", "featureKeys": [] }
        });
        let m = read_built_in_agent_marker(&metadata).unwrap();
        assert_eq!(m.key, "ceo");
    }

    // ---- with_built_in_agent_marker ----

    #[test]
    fn with_marker_on_none_metadata_creates_marker_only() {
        let marker = BuiltInAgentMarker::new("ceo", vec!["f1".into()]);
        let out = with_built_in_agent_marker(None, &marker);
        assert_eq!(out.len(), 1);
        let inner = out.get(BUILT_IN_AGENT_METADATA_KEY).unwrap();
        assert_eq!(inner.get("key"), Some(&json!("ceo")));
        assert_eq!(inner.get("featureKeys"), Some(&json!(["f1"])));
    }

    #[test]
    fn with_marker_preserves_existing_fields() {
        let mut existing = Map::new();
        existing.insert("other".into(), json!("kept"));
        let marker = BuiltInAgentMarker::new("k", vec![]);
        let out = with_built_in_agent_marker(Some(&existing), &marker);
        assert_eq!(out.get("other"), Some(&json!("kept")));
        assert!(out.contains_key(BUILT_IN_AGENT_METADATA_KEY));
    }

    #[test]
    fn with_marker_replaces_previous_marker() {
        let mut existing = Map::new();
        let mut prev_marker = Map::new();
        prev_marker.insert("key".into(), json!("old"));
        prev_marker.insert("featureKeys".into(), json!([]));
        existing.insert(BUILT_IN_AGENT_METADATA_KEY.into(), Value::Object(prev_marker));

        let marker = BuiltInAgentMarker::new("new", vec!["f".into()]);
        let out = with_built_in_agent_marker(Some(&existing), &marker);
        let inner = out.get(BUILT_IN_AGENT_METADATA_KEY).unwrap();
        assert_eq!(inner.get("key"), Some(&json!("new")));
        assert_eq!(inner.get("featureKeys"), Some(&json!(["f"])));
    }

    #[test]
    fn with_marker_copies_feature_keys_array() {
        let mut existing = Map::new();
        let marker = BuiltInAgentMarker::new("k", vec!["a".into(), "b".into()]);
        let out = with_built_in_agent_marker(Some(&existing), &marker);
        // 验证内部数组是新分配的（修改 marker 不会影响结果）
        let mut marker2 = BuiltInAgentMarker::new("k", vec!["c".into()]);
        let out2 = with_built_in_agent_marker(Some(&existing), &marker2);
        assert_eq!(
            out.get(BUILT_IN_AGENT_METADATA_KEY).unwrap().get("featureKeys"),
            Some(&json!(["a", "b"]))
        );
        assert_eq!(
            out2.get(BUILT_IN_AGENT_METADATA_KEY).unwrap().get("featureKeys"),
            Some(&json!(["c"]))
        );
        // 修改 marker.feature_keys 不会影响已生成的 out
        marker2.feature_keys.push("d".into());
        assert_eq!(
            out2.get(BUILT_IN_AGENT_METADATA_KEY).unwrap().get("featureKeys"),
            Some(&json!(["c"]))
        );
    }

    // ---- built_in_agent_markers_equal ----

    #[test]
    fn equal_handles_both_none() {
        assert!(built_in_agent_markers_equal(None, None));
    }

    #[test]
    fn equal_handles_one_none() {
        let m = BuiltInAgentMarker::new("k", vec![]);
        assert!(!built_in_agent_markers_equal(None, Some(&m)));
        assert!(!built_in_agent_markers_equal(Some(&m), None));
    }

    #[test]
    fn equal_keys_must_match() {
        let a = BuiltInAgentMarker::new("a", vec!["f".into()]);
        let b = BuiltInAgentMarker::new("b", vec!["f".into()]);
        assert!(!built_in_agent_markers_equal(Some(&a), Some(&b)));
    }

    #[test]
    fn equal_feature_keys_must_match_value_and_order() {
        let a = BuiltInAgentMarker::new("k", vec!["x".into(), "y".into()]);
        let b = BuiltInAgentMarker::new("k", vec!["x".into(), "y".into()]);
        assert!(built_in_agent_markers_equal(Some(&a), Some(&b)));

        let c = BuiltInAgentMarker::new("k", vec!["y".into(), "x".into()]);
        // Node: JSON.stringify 比较 → 顺序敏感
        assert!(!built_in_agent_markers_equal(Some(&a), Some(&c)));
    }

    #[test]
    fn equal_feature_keys_must_match_length() {
        let a = BuiltInAgentMarker::new("k", vec!["x".into()]);
        let b = BuiltInAgentMarker::new("k", vec!["x".into(), "y".into()]);
        assert!(!built_in_agent_markers_equal(Some(&a), Some(&b)));
    }

    #[test]
    fn round_trip_marker_via_metadata() {
        let original = BuiltInAgentMarker::new("ceo", vec!["f1".into(), "f2".into()]);
        let metadata = with_built_in_agent_marker(None, &original);
        let value = Value::Object(metadata);
        let recovered = read_built_in_agent_marker(&value).unwrap();
        assert!(built_in_agent_markers_equal(Some(&original), Some(&recovered)));
    }
}
