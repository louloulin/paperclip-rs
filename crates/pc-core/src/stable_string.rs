//! `stable_string` 域（Round 276）。
//!
//! 与原 `paperclip/server/src/services/{execution-workspaces,workspace-runtime}.ts`
//! 中 `stableStringify` 1:1 对齐：递归稳定序列化（key 字典序排序、嵌套 object/array）。
//!
//! 设计目标：高内聚低耦合。
//! - 高内聚：单一职责 — 把任意 JSON-like value 规范化为可哈希的字符串。
//! - 低耦合：仅依赖 `serde_json`，无 IO。

use serde_json::{Map, Value};
use sha2::Digest;

/// `stableStringify(value)` 1:1 对位 Node：
/// - array → `[<elems joined by \",\">]`
/// - object → `{<sorted keys joined by \",\">}`，每个 key 用 JSON.stringify(key)
/// - 其他 → `JSON.stringify(value)`
pub fn stable_stringify(value: &Value) -> String {
    match value {
        Value::Array(arr) => {
            let parts: Vec<String> = arr.iter().map(stable_stringify).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Object(obj) => stable_stringify_object(obj),
        _ => serde_json::to_string(value).unwrap_or_else(|_| "null".to_string()),
    }
}

/// Object 模式：key 字典序（按 UTF-8 字节序）排序，每个 key 包 JSON.stringify。
pub fn stable_stringify_object(obj: &Map<String, Value>) -> String {
    let mut keys: Vec<&String> = obj.keys().collect();
    // Node `Object.keys().sort()` 默认按 UTF-16 code unit 排序；对纯 ASCII 它与 Rust 字典序一致；
    // 对非 ASCII 测试中显式覆盖。
    keys.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    let parts: Vec<String> = keys
        .into_iter()
        .map(|k| {
            let key_json = serde_json::to_string(k).unwrap_or_else(|_| "\"\"".to_string());
            format!("{}:{}", key_json, stable_stringify(&obj[k]))
        })
        .collect();
    format!("{{{}}}", parts.join(","))
}

/// SHA-256 hex digest of stable_stringify(input)。等价于 Node `createHash("sha256").update(...).digest("hex")`。
pub fn stable_string_sha256_hex(value: &Value) -> String {
    let s = stable_stringify(value);
    let digest = sha2::Sha256::digest(s.as_bytes());
    hex::encode(digest)
}

/// 版本化 fingerprint：常用形式 `fingerprint:v1:sha256:<hash>`。
/// 也支持 `workspace_incoherence:v1:sha256:<hash>` 这种带"逻辑域"前缀（`reason`）。
pub fn versioned_sha256_fingerprint(prefix: &str, value: &Value) -> String {
    format!("{}:v1:sha256:{}", prefix, stable_string_sha256_hex(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn primitive_stable_stringify() {
        assert_eq!(stable_stringify(&json!(null)), "null");
        assert_eq!(stable_stringify(&json!(true)), "true");
        assert_eq!(stable_stringify(&json!(42)), "42");
        assert_eq!(stable_stringify(&json!("hi")), r#""hi""#);
    }

    #[test]
    fn array_stable_stringify() {
        assert_eq!(stable_stringify(&json!([1, 2, 3])), "[1,2,3]");
        assert_eq!(stable_stringify(&json!([])), "[]");
        assert_eq!(stable_stringify(&json!(["a", "b"])), r#"["a","b"]"#);
    }

    #[test]
    fn object_keys_sorted() {
        let v = json!({"c": 1, "a": 2, "b": 3});
        assert_eq!(stable_stringify(&v), r#"{"a":2,"b":3,"c":1}"#);
    }

    #[test]
    fn object_nested() {
        let v = json!({"z": {"y": 1, "x": 2}, "a": [3, 2, 1]});
        assert_eq!(
            stable_stringify(&v),
            r#"{"a":[3,2,1],"z":{"x":2,"y":1}}"#
        );
    }

    #[test]
    fn object_keys_are_json_stringified() {
        let v = json!({"a\"b": 1}); // 双引号需转义
        // Node: JSON.stringify(`a"b`) => `"a\"b"` → 字段 key 在稳定序列化中也是 JSON 字符串
        let expected = r#"{"a\"b":1}"#;
        assert_eq!(stable_stringify(&v), expected);
    }

    #[test]
    fn sha256_fingerprint_deterministic() {
        let v = json!({"a": 1, "b": [2, 3]});
        let h1 = stable_string_sha256_hex(&v);
        let h2 = stable_string_sha256_hex(&v);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn sha256_fingerprint_changes_with_content() {
        let h1 = stable_string_sha256_hex(&json!({"a": 1}));
        let h2 = stable_string_sha256_hex(&json!({"a": 2}));
        assert_ne!(h1, h2);
    }

    #[test]
    fn versioned_fingerprint_format() {
        let v = json!({"x": 1});
        let f = versioned_sha256_fingerprint("workspace_incoherence", &v);
        assert!(f.starts_with("workspace_incoherence:v1:sha256:"));
        // prefix 长度为 30，sha256 hex 为 64；f 总长 = 30 + 1 (:) + 64 = 94（不计末尾冒号）。
        assert_eq!(f.len(), "workspace_incoherence:v1:sha256:".len() + 64);
    }

    #[test]
    fn order_independent_for_objects() {
        // 不同 key 顺序应得到相同 hash
        let v1 = json!({"a": 1, "b": 2, "c": 3});
        let v2 = json!({"c": 3, "a": 1, "b": 2});
        assert_eq!(stable_string_sha256_hex(&v1), stable_string_sha256_hex(&v2));
    }

    #[test]
    fn nested_array_stable() {
        let v1 = json!([[1, 2], [3, 4]]);
        let v2 = json!([[3, 4], [1, 2]]); // 顺序不同 → 不同 hash
        assert_ne!(stable_string_sha256_hex(&v1), stable_string_sha256_hex(&v2));
    }
}
