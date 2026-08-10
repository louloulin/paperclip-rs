#![forbid(unsafe_code)]
//! `pc-built-in-agent-metadata` —— 内置 agent 元数据标记（marker）。
//!
//! 对应 Node `server/src/services/built-in-agent-metadata.ts`（45 行）。
//!
//! 设计目标：1:1 复刻 `readBuiltInAgentMarker` / `withBuiltInAgentMarker` /
//! `builtInAgentMarkersEqual` 的语义。

/// metadata key —— 与 Node `BUILT_IN_AGENT_METADATA_KEY` 1:1。
pub const BUILT_IN_AGENT_METADATA_KEY: &str = "paperclipBuiltInAgent";

/// 内置 agent marker。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BuiltInAgentMarker {
    pub key: String,
    pub feature_keys: Vec<String>,
}

/// 判断 value 是否为"plain record"（对象、不是 null、不是数组）。
fn is_plain_record(value: &serde_json::Value) -> bool {
    value.is_object()
}

/// 把 feature keys 标准化为 `string[]`；非 string 或空字符串会被剔除。
/// 若有任何非字符串项，返回 `None`（调用方判定为 invalid）。
fn normalize_feature_keys(value: &serde_json::Value) -> Option<Vec<String>> {
    let arr = value.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for entry in arr {
        let s = entry.as_str().filter(|s| !s.trim().is_empty())?;
        out.push(s.to_string());
    }
    Some(out)
}

/// 从任意 metadata JSON 中读取 built-in agent marker。
///
/// 与 Node `readBuiltInAgentMarker` 1:1 对齐：
/// - metadata 不是 plain object → None
/// - 没有 `paperclipBuiltInAgent` 字段 → None
/// - marker 不是 plain object → None
/// - `key` 不是非空 string → None
/// - `featureKeys` 不是 string[]（或含空字符串） → None
pub fn read_built_in_agent_marker(metadata: &serde_json::Value) -> Option<BuiltInAgentMarker> {
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
    let feature_keys = marker
        .get("featureKeys")
        .and_then(|v| normalize_feature_keys(v))?;
    Some(BuiltInAgentMarker {
        key: key.to_string(),
        feature_keys,
    })
}

/// 把 marker 注入 metadata（保留其它字段）。
pub fn with_built_in_agent_marker(
    metadata: Option<serde_json::Map<String, serde_json::Value>>,
    marker: &BuiltInAgentMarker,
) -> serde_json::Map<String, serde_json::Value> {
    let mut base = metadata.unwrap_or_default();
    base.insert(
        BUILT_IN_AGENT_METADATA_KEY.to_string(),
        serde_json::json!({
            "key": marker.key,
            "featureKeys": marker.feature_keys,
        }),
    );
    base
}

/// 判断两个 marker 是否相等（feature keys 用 JSON 序列化比对 —— 与 Node `JSON.stringify` 1:1）。
pub fn built_in_agent_markers_equal(
    left: Option<&BuiltInAgentMarker>,
    right: Option<&BuiltInAgentMarker>,
) -> bool {
    match (left, right) {
        (None, None) => true,
        (None, Some(_)) | (Some(_), None) => false,
        (Some(l), Some(r)) => {
            l.key == r.key
                && serde_json::to_string(&l.feature_keys).ok()
                    == serde_json::to_string(&r.feature_keys).ok()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn r694_metadata_key_constant_matches_node() {
        assert_eq!(BUILT_IN_AGENT_METADATA_KEY, "paperclipBuiltInAgent");
    }

    #[test]
    fn r694_read_marker_minimal() {
        let md = json!({
            "paperclipBuiltInAgent": {"key": "ceo", "featureKeys": ["finance"]}
        });
        let m = read_built_in_agent_marker(&md).unwrap();
        assert_eq!(m.key, "ceo");
        assert_eq!(m.feature_keys, vec!["finance"]);
    }

    #[test]
    fn r694_read_marker_non_object_metadata() {
        assert!(read_built_in_agent_marker(&json!("not-object")).is_none());
        assert!(read_built_in_agent_marker(&json!(null)).is_none());
        assert!(read_built_in_agent_marker(&json!([])).is_none());
    }

    #[test]
    fn r694_read_marker_missing_key_field() {
        let md = json!({"paperclipBuiltInAgent": {"featureKeys": ["x"]}});
        assert!(read_built_in_agent_marker(&md).is_none());
    }

    #[test]
    fn r694_read_marker_empty_key() {
        let md = json!({"paperclipBuiltInAgent": {"key": "  ", "featureKeys": ["x"]}});
        assert!(read_built_in_agent_marker(&md).is_none());
    }

    #[test]
    fn r694_read_marker_feature_keys_with_non_string() {
        let md = json!({"paperclipBuiltInAgent": {"key": "k", "featureKeys": ["ok", 123]}});
        assert!(read_built_in_agent_marker(&md).is_none());
    }

    #[test]
    fn r694_read_marker_feature_keys_with_empty_string() {
        let md = json!({"paperclipBuiltInAgent": {"key": "k", "featureKeys": ["ok", ""]}});
        assert!(read_built_in_agent_marker(&md).is_none());
    }

    #[test]
    fn r694_read_marker_feature_keys_not_array() {
        let md = json!({"paperclipBuiltInAgent": {"key": "k", "featureKeys": "not-array"}});
        assert!(read_built_in_agent_marker(&md).is_none());
    }

    #[test]
    fn r694_read_marker_not_object_inside() {
        let md = json!({"paperclipBuiltInAgent": "string"});
        assert!(read_built_in_agent_marker(&md).is_none());
        let md2 = json!({"paperclipBuiltInAgent": null});
        assert!(read_built_in_agent_marker(&md2).is_none());
        let md3 = json!({"paperclipBuiltInAgent": []});
        assert!(read_built_in_agent_marker(&md3).is_none());
    }

    #[test]
    fn r694_with_marker_preserves_existing_metadata() {
        let mut md = serde_json::Map::new();
        md.insert("name".into(), json!("agent-x"));
        md.insert("tag".into(), json!("primary"));
        let m = BuiltInAgentMarker {
            key: "ceo".into(),
            feature_keys: vec!["finance".into()],
        };
        let out = with_built_in_agent_marker(Some(md), &m);
        assert_eq!(out.get("name").unwrap(), &json!("agent-x"));
        assert_eq!(out.get("tag").unwrap(), &json!("primary"));
        assert_eq!(
            out.get("paperclipBuiltInAgent").unwrap(),
            &json!({"key": "ceo", "featureKeys": ["finance"]})
        );
    }

    #[test]
    fn r694_with_marker_from_none() {
        let m = BuiltInAgentMarker {
            key: "k".into(),
            feature_keys: vec![],
        };
        let out = with_built_in_agent_marker(None, &m);
        assert_eq!(out.len(), 1);
        assert!(out.contains_key("paperclipBuiltInAgent"));
    }

    #[test]
    fn r694_with_marker_copies_feature_keys_array() {
        let m = BuiltInAgentMarker {
            key: "k".into(),
            feature_keys: vec!["a".into(), "b".into()],
        };
        let mut md = serde_json::Map::new();
        md.insert("old".into(), json!(1));
        let out = with_built_in_agent_marker(Some(md.clone()), &m);
        let inserted = out.get("paperclipBuiltInAgent").unwrap();
        let inserted_keys = inserted.get("featureKeys").unwrap().as_array().unwrap();
        assert_eq!(inserted_keys.len(), 2);
        // 验证确实复制（mutating 原始 marker 不会影响输出）
        drop(m);
        let inserted_keys2 = out.get("paperclipBuiltInAgent").unwrap();
        assert_eq!(inserted_keys2.get("featureKeys").unwrap().as_array().unwrap().len(), 2);
    }

    #[test]
    fn r694_markers_equal_both_none() {
        assert!(built_in_agent_markers_equal(None, None));
    }

    #[test]
    fn r694_markers_equal_one_none() {
        let m = BuiltInAgentMarker {
            key: "k".into(),
            feature_keys: vec![],
        };
        assert!(!built_in_agent_markers_equal(Some(&m), None));
        assert!(!built_in_agent_markers_equal(None, Some(&m)));
    }

    #[test]
    fn r694_markers_equal_same_key_and_features() {
        let a = BuiltInAgentMarker {
            key: "k".into(),
            feature_keys: vec!["a".into(), "b".into()],
        };
        let b = BuiltInAgentMarker {
            key: "k".into(),
            feature_keys: vec!["a".into(), "b".into()],
        };
        assert!(built_in_agent_markers_equal(Some(&a), Some(&b)));
    }

    #[test]
    fn r694_markers_equal_different_key() {
        let a = BuiltInAgentMarker {
            key: "k1".into(),
            feature_keys: vec![],
        };
        let b = BuiltInAgentMarker {
            key: "k2".into(),
            feature_keys: vec![],
        };
        assert!(!built_in_agent_markers_equal(Some(&a), Some(&b)));
    }

    #[test]
    fn r694_markers_equal_different_feature_order() {
        // Node `JSON.stringify` 顺序敏感：["a","b"] vs ["b","a"] 不等
        let a = BuiltInAgentMarker {
            key: "k".into(),
            feature_keys: vec!["a".into(), "b".into()],
        };
        let b = BuiltInAgentMarker {
            key: "k".into(),
            feature_keys: vec!["b".into(), "a".into()],
        };
        assert!(!built_in_agent_markers_equal(Some(&a), Some(&b)));
    }
}
