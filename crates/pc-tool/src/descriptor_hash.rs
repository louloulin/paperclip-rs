#![forbid(unsafe_code)]

//! Tool descriptor stable hash.
//! R704: Direct port of tool-access.ts::descriptorHash + stableHash + flattenKeys.

use crate::{classify_risk, McpToolDescriptor};
use serde::Serialize;
use sha2::{Digest, Sha256};

/// 收集 value 中所有 key（递归对象 + 数组），输出 sorted vec。
/// 与 Node flattenKeys 1:1 parity。
pub fn flatten_keys(value: &serde_json::Value) -> Vec<String> {
    let mut keys: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    walk_keys(value, &mut keys);
    keys.into_iter().collect()
}

fn walk_keys(value: &serde_json::Value, keys: &mut std::collections::BTreeSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                keys.insert(k.clone());
                walk_keys(v, keys);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr { walk_keys(v, keys); }
        }
        _ => {}
    }
}

fn serialize_canonical(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => serde_json::to_string(s).unwrap(),
        serde_json::Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(serialize_canonical).collect();
            format!("[{}]", items.join(","))
        }
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let items: Vec<String> = keys
                .iter()
                .map(|k| format!("\"{}\":{}", k, serialize_canonical(&map[*k])))
                .collect();
            format!("{{{}}}", items.join(","))
        }
    }
}

/// Stable hash: SHA-256 hex of JSON.stringify(value, sortedKeys).
/// 与 Node 1:1 parity。
pub fn stable_hash<T: Serialize>(value: &T) -> String {
    let json = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
    let keys = flatten_keys(&json);
    let json_str = serialize_canonical(&json);
    let key_list = keys.join(",");
    let mut hasher = Sha256::new();
    hasher.update(json_str.as_bytes());
    hasher.update(key_list.as_bytes());
    let result = hasher.finalize();
    format!("{:x}", result)
}

/// Compute a stable hash for an McpToolDescriptor.
/// 与 Node  1:1 parity (含 classifyRisk)。
pub fn descriptor_hash(tool: &McpToolDescriptor) -> String {
    let risk = classify_risk(tool);
    let payload = serde_json::json!({
        "name": tool.name,
        "title": tool.title,
        "description": tool.description,
        "inputSchema": tool.input_schema,
        "annotations": tool.annotations,
        "riskLevel": risk.as_str(),
    });
    stable_hash(&payload)
}

#[cfg(test)]
mod internal_tests {
    use super::*;
    use crate::McpToolAnnotations;
    use serde_json::json;

    #[test]
    fn stable_hash_deterministic() {
        let v = json!({"a": 1, "b": 2});
        let h1 = stable_hash(&v);
        let h2 = stable_hash(&v);
        assert_eq!(h1, h2);
    }

    #[test]
    fn stable_hash_changes_with_value() {
        let v1 = json!({"a": 1});
        let v2 = json!({"a": 2});
        assert_ne!(stable_hash(&v1), stable_hash(&v2));
    }

    #[test]
    fn stable_hash_64_hex() {
        let v = json!({});
        let h = stable_hash(&v);
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn flatten_keys_collects_all() {
        let v = json!({"a": {"b": {"c": 1}, "d": 2}, "e": [{"f": 3}]});
        let keys = flatten_keys(&v);
        assert_eq!(keys, vec!["a".to_string(), "b".to_string(), "c".to_string(), "d".to_string(), "e".to_string(), "f".to_string()]);
    }

    #[test]
    fn flatten_keys_empty() {
        assert_eq!(flatten_keys(&json!(null)), Vec::<String>::new());
        assert_eq!(flatten_keys(&json!(1)), Vec::<String>::new());
        assert_eq!(flatten_keys(&json!("str")), Vec::<String>::new());
        assert_eq!(flatten_keys(&json!([])), Vec::<String>::new());
        assert_eq!(flatten_keys(&json!({})), Vec::<String>::new());
    }

    fn desc(name: &str) -> McpToolDescriptor {
        McpToolDescriptor { name: name.into(), title: None, description: None, input_schema: None, annotations: None }
    }

    #[test]
    fn descriptor_hash_changes_with_name() {
        let h1 = descriptor_hash(&desc("foo"));
        let h2 = descriptor_hash(&desc("bar"));
        assert_ne!(h1, h2);
    }

    #[test]
    fn descriptor_hash_changes_with_risk() {
        let mut read = desc("foo");
        read.title = None;
        let mut destructive = desc("foo");
        destructive.annotations = Some(McpToolAnnotations { destructive_hint: Some(true), ..Default::default() });
        assert_ne!(descriptor_hash(&read), descriptor_hash(&destructive));
    }

    #[test]
    fn descriptor_hash_changes_with_input_schema() {
        let mut a = desc("foo");
        a.input_schema = Some(json!({"type": "object"}));
        let mut b = desc("foo");
        b.input_schema = Some(json!({"type": "string"}));
        assert_ne!(descriptor_hash(&a), descriptor_hash(&b));
    }

    #[test]
    fn descriptor_hash_deterministic() {
        let h1 = descriptor_hash(&desc("foo"));
        let h2 = descriptor_hash(&desc("foo"));
        assert_eq!(h1, h2);
    }

    #[test]
    fn descriptor_hash_64_hex() {
        let h = descriptor_hash(&desc("foo"));
        assert_eq!(h.len(), 64);
    }

    // ---- Round 767: pc-tool::descriptor_hash 集成测试 ----

    /// descriptor_hash 包含 description。
    #[test]
    fn r767_descriptor_hash_includes_description() {
        let mut a = desc("foo");
        a.description = Some("first".into());
        let mut b = desc("foo");
        b.description = Some("second".into());
        assert_ne!(descriptor_hash(&a), descriptor_hash(&b));
    }

    /// descriptor_hash 包含 title。
    #[test]
    fn r767_descriptor_hash_includes_title() {
        let mut a = desc("foo");
        a.title = Some("A".into());
        let mut b = desc("foo");
        b.title = Some("B".into());
        assert_ne!(descriptor_hash(&a), descriptor_hash(&b));
    }

    /// flatten_keys: 数组嵌套对象 + null 叶子。
    #[test]
    fn r767_flatten_keys_nested_objects() {
        let v = json!({"a": [{"b": 1, "c": {"d": 2}}], "e": null});
        let keys = flatten_keys(&v);
        assert_eq!(keys, vec!["a".to_string(), "b".to_string(), "c".to_string(), "d".to_string(), "e".to_string()]);
    }

    /// stable_hash: key 顺序无关（BTreeSet）。
    #[test]
    fn r767_stable_hash_key_order_invariant() {
        let v1 = json!({"a": 1, "b": 2, "c": 3});
        let v2 = json!({"c": 3, "b": 2, "a": 1});
        assert_eq!(stable_hash(&v1), stable_hash(&v2), "stable_hash must be key-order independent");
    }
}
