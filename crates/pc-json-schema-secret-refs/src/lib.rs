#![forbid(unsafe_code)]
//! `pc-json-schema-secret-refs` —— JSON schema secret-ref 提取 + dot-path 读写。
//!
//! 对应 Node `server/src/services/json-schema-secret-refs.ts`（104 行）。
//!
//! 设计目标：1:1 复刻
//! - [`is_uuid_secret_ref`] —— 校验 UUID 格式
//! - [`parse_secret_ref_binding_object`] —— 解析 `{ type: "secret_ref", secretId, version }` binding
//! - [`collect_secret_ref_paths`] —— 递归遍历 JSON schema，收集所有 `format: "secret-ref"` 字段的 dot-path
//! - [`read_config_value_at_path`] / [`write_config_value_at_path`] —— dot-path 读写
//!
//! Pure logic，无 IO 依赖；可被 `pc-secrets` / `pc-tool-profile-binding-precedence` / `pc-plugin-manifest-validator` 等复用。

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

// ============================================================================
// UUID validation
// ============================================================================

/// 校验字符串是否为 UUID（与 Node `isUuidSecretRef` 1:1 对齐）。
pub fn is_uuid_secret_ref(value: &str) -> bool {
    uuid::Uuid::parse_str(value.trim()).is_ok()
}

// ============================================================================
// Binding object
// ============================================================================

/// Secret ref binding（与 Node `SecretRefBindingObject` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretRefBindingObject {
    pub secret_id: String,
    #[serde(default = "default_version")]
    pub version: SecretRefVersion,
}

fn default_version() -> SecretRefVersion {
    SecretRefVersion::Latest
}

/// Secret ref 版本（与 Node `version: "latest" | number` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SecretRefVersion {
    Latest,
    #[serde(untagged)]
    Number(u32),
}

/// 解析 `{ type: "secret_ref", secretId, version? }` binding。
///
/// 返回 `None`：raw value / 裸 secret-id 字符串 / 类型不匹配 / 格式错误。
pub fn parse_secret_ref_binding_object(value: &serde_json::Value) -> Option<SecretRefBindingObject> {
    let obj = value.as_object()?;
    if obj.get("type")?.as_str()? != "secret_ref" {
        return None;
    }
    let secret_id = obj.get("secretId")?.as_str()?.trim().to_string();
    if !is_uuid_secret_ref(&secret_id) {
        return None;
    }
    // version 字段可省略 / null / "latest" / 正整数
    match obj.get("version") {
        None => Some(SecretRefBindingObject {
            secret_id,
            version: SecretRefVersion::Latest,
        }),
        Some(serde_json::Value::Null) => Some(SecretRefBindingObject {
            secret_id,
            version: SecretRefVersion::Latest,
        }),
        Some(serde_json::Value::String(s)) if s == "latest" => Some(SecretRefBindingObject {
            secret_id,
            version: SecretRefVersion::Latest,
        }),
        Some(serde_json::Value::Number(n)) => {
            let v = n.as_u64()? as u32;
            if v == 0 {
                return None;
            }
            Some(SecretRefBindingObject {
                secret_id,
                version: SecretRefVersion::Number(v),
            })
        }
        _ => None,
    }
}

// ============================================================================
// Schema path collection
// ============================================================================

/// 递归遍历 JSON schema，收集所有 `format: "secret-ref"` 字段的 dot-path。
///
/// 遍历方式（与 Node 1:1）：
/// - 处理 `allOf` / `anyOf` / `oneOf` 分支
/// - 处理 `properties` 嵌套
/// - 路径用 `.` 连接
pub fn collect_secret_ref_paths(schema: &serde_json::Value) -> HashSet<String> {
    let mut paths = HashSet::new();
    walk(schema, "", &mut paths);
    paths
}

fn walk(node: &serde_json::Value, prefix: &str, paths: &mut HashSet<String>) {
    let Some(obj) = node.as_object() else {
        return;
    };

    // allOf / anyOf / oneOf
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(branches) = obj.get(keyword).and_then(|v| v.as_array()) {
            for branch in branches {
                if branch.is_object() {
                    walk(branch, prefix, paths);
                }
            }
        }
    }

    // properties
    let Some(props) = obj.get("properties").and_then(|v| v.as_object()) else {
        return;
    };
    for (key, property_schema) in props {
        let Some(prop_obj) = property_schema.as_object() else {
            continue;
        };
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        if prop_obj.get("format").and_then(|v| v.as_str()) == Some("secret-ref") {
            paths.insert(path.clone());
        }
        walk(property_schema, &path, paths);
    }
}

// ============================================================================
// Dot-path config read/write
// ============================================================================

/// 读 dot-path 对应的值（与 Node `readConfigValueAtPath` 1:1 对齐）。
///
/// 路径不存在 / 中间节点非 object → `None`。
pub fn read_config_value_at_path<'a>(config: &'a serde_json::Value, dot_path: &str) -> Option<&'a serde_json::Value> {
    let mut current = config;
    for key in dot_path.split('.') {
        let obj = current.as_object()?;
        current = obj.get(key)?;
    }
    Some(current)
}

/// 写 dot-path 对应的值（与 Node `writeConfigValueAtPath` 1:1 对齐）。
///
/// - 不修改原 config（deep clone）
/// - 中间节点不存在 → 自动创建空 object
/// - `value = None` → 删除 leaf key
/// - 路径非法（中间节点是 array / 标量）→ 替换为 object
///
/// 返回新 `serde_json::Value`。
pub fn write_config_value_at_path(
    config: &serde_json::Value,
    dot_path: &str,
    value: Option<&serde_json::Value>,
) -> serde_json::Value {
    let mut result = config.clone();
    if dot_path.is_empty() {
        return result;
    }
    let keys: Vec<&str> = dot_path.split('.').collect();
    if keys.is_empty() {
        return result;
    }

    // 确保 result 是 object
    if !result.is_object() {
        result = serde_json::Value::Object(serde_json::Map::new());
    }

    let mut cursor = result.as_object_mut().unwrap();
    for key in &keys[..keys.len() - 1] {
        let needs_new = !cursor
            .get(*key)
            .map(|v| v.is_object())
            .unwrap_or(false);
        if needs_new {
            cursor.insert((*key).to_string(), serde_json::Value::Object(serde_json::Map::new()));
        }
        cursor = cursor
            .get_mut(*key)
            .and_then(|v| v.as_object_mut())
            .expect("just ensured it's an object");
    }

    let leaf_key = keys[keys.len() - 1];
    match value {
        None => {
            cursor.remove(leaf_key);
        }
        Some(v) => {
            cursor.insert(leaf_key.to_string(), v.clone());
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ----- isUuidSecretRef -----

    #[test]
    fn r715_is_uuid_valid() {
        assert!(is_uuid_secret_ref("123e4567-e89b-12d3-a456-426614174000"));
        assert!(is_uuid_secret_ref("  123E4567-E89B-12D3-A456-426614174000  "));
    }

    #[test]
    fn r715_is_uuid_invalid() {
        assert!(!is_uuid_secret_ref("not-a-uuid"));
        assert!(!is_uuid_secret_ref(""));
        assert!(!is_uuid_secret_ref("123"));
    }

    // ----- parseSecretRefBindingObject -----

    #[test]
    fn r715_parse_binding_minimal() {
        let v = json!({
            "type": "secret_ref",
            "secretId": "123e4567-e89b-12d3-a456-426614174000"
        });
        let r = parse_secret_ref_binding_object(&v).unwrap();
        assert_eq!(r.secret_id, "123e4567-e89b-12d3-a456-426614174000");
        assert_eq!(r.version, SecretRefVersion::Latest);
    }

    #[test]
    fn r715_parse_binding_with_version() {
        let v = json!({
            "type": "secret_ref",
            "secretId": "123e4567-e89b-12d3-a456-426614174000",
            "version": 3
        });
        let r = parse_secret_ref_binding_object(&v).unwrap();
        assert_eq!(r.version, SecretRefVersion::Number(3));
    }

    #[test]
    fn r715_parse_binding_latest_explicit() {
        let v = json!({
            "type": "secret_ref",
            "secretId": "123e4567-e89b-12d3-a456-426614174000",
            "version": "latest"
        });
        let r = parse_secret_ref_binding_object(&v).unwrap();
        assert_eq!(r.version, SecretRefVersion::Latest);
    }

    #[test]
    fn r715_parse_binding_null_version() {
        let v = json!({
            "type": "secret_ref",
            "secretId": "123e4567-e89b-12d3-a456-426614174000",
            "version": null
        });
        let r = parse_secret_ref_binding_object(&v).unwrap();
        assert_eq!(r.version, SecretRefVersion::Latest);
    }

    #[test]
    fn r715_parse_binding_rejects_raw_string() {
        let v = json!("123e4567-e89b-12d3-a456-426614174000");
        assert!(parse_secret_ref_binding_object(&v).is_none());
    }

    #[test]
    fn r715_parse_binding_rejects_wrong_type() {
        let v = json!({
            "type": "value",
            "secretId": "123e4567-e89b-12d3-a456-426614174000"
        });
        assert!(parse_secret_ref_binding_object(&v).is_none());
    }

    #[test]
    fn r715_parse_binding_rejects_invalid_uuid() {
        let v = json!({
            "type": "secret_ref",
            "secretId": "not-a-uuid"
        });
        assert!(parse_secret_ref_binding_object(&v).is_none());
    }

    #[test]
    fn r715_parse_binding_rejects_zero_version() {
        let v = json!({
            "type": "secret_ref",
            "secretId": "123e4567-e89b-12d3-a456-426614174000",
            "version": 0
        });
        assert!(parse_secret_ref_binding_object(&v).is_none());
    }

    #[test]
    fn r715_parse_binding_rejects_negative_version() {
        let v = json!({
            "type": "secret_ref",
            "secretId": "123e4567-e89b-12d3-a456-426614174000",
            "version": -1
        });
        assert!(parse_secret_ref_binding_object(&v).is_none());
    }

    #[test]
    fn r715_parse_binding_rejects_non_integer_version() {
        let v = json!({
            "type": "secret_ref",
            "secretId": "123e4567-e89b-12d3-a456-426614174000",
            "version": 1.5
        });
        assert!(parse_secret_ref_binding_object(&v).is_none());
    }

    #[test]
    fn r715_parse_binding_rejects_array() {
        let v = json!([{"type": "secret_ref", "secretId": "x"}]);
        assert!(parse_secret_ref_binding_object(&v).is_none());
    }

    // ----- collectSecretRefPaths -----

    #[test]
    fn r715_collect_paths_simple() {
        let schema = json!({
            "type": "object",
            "properties": {
                "apiKey": {"type": "string", "format": "secret-ref"},
                "name": {"type": "string"}
            }
        });
        let paths = collect_secret_ref_paths(&schema);
        assert!(paths.contains("apiKey"));
        assert!(!paths.contains("name"));
    }

    #[test]
    fn r715_collect_paths_nested() {
        let schema = json!({
            "type": "object",
            "properties": {
                "outer": {
                    "type": "object",
                    "properties": {
                        "inner": {"type": "string", "format": "secret-ref"}
                    }
                }
            }
        });
        let paths = collect_secret_ref_paths(&schema);
        assert!(paths.contains("outer.inner"));
    }

    #[test]
    fn r715_collect_paths_all_of() {
        let schema = json!({
            "allOf": [
                {
                    "type": "object",
                    "properties": {
                        "a": {"type": "string", "format": "secret-ref"}
                    }
                },
                {
                    "type": "object",
                    "properties": {
                        "b": {"type": "string", "format": "secret-ref"}
                    }
                }
            ]
        });
        let paths = collect_secret_ref_paths(&schema);
        assert!(paths.contains("a"));
        assert!(paths.contains("b"));
    }

    #[test]
    fn r715_collect_paths_any_of() {
        let schema = json!({
            "anyOf": [
                {"type": "object", "properties": {"x": {"format": "secret-ref"}}},
                {"type": "object", "properties": {"y": {"format": "secret-ref"}}}
            ]
        });
        let paths = collect_secret_ref_paths(&schema);
        assert!(paths.contains("x"));
        assert!(paths.contains("y"));
    }

    #[test]
    fn r715_collect_paths_one_of() {
        let schema = json!({
            "oneOf": [
                {"type": "object", "properties": {"p": {"format": "secret-ref"}}}
            ]
        });
        let paths = collect_secret_ref_paths(&schema);
        assert!(paths.contains("p"));
    }

    #[test]
    fn r715_collect_paths_empty_schema() {
        let paths = collect_secret_ref_paths(&json!({}));
        assert!(paths.is_empty());
    }

    #[test]
    fn r715_collect_paths_null_schema() {
        let paths = collect_secret_ref_paths(&json!(null));
        assert!(paths.is_empty());
    }

    // ----- readConfigValueAtPath -----

    #[test]
    fn r715_read_top_level() {
        let config = json!({"a": 1, "b": "x"});
        assert_eq!(read_config_value_at_path(&config, "a"), Some(&json!(1)));
    }

    #[test]
    fn r715_read_nested() {
        let config = json!({"a": {"b": {"c": 42}}});
        assert_eq!(read_config_value_at_path(&config, "a.b.c"), Some(&json!(42)));
    }

    #[test]
    fn r715_read_missing_path() {
        let config = json!({"a": 1});
        assert_eq!(read_config_value_at_path(&config, "b"), None);
        assert_eq!(read_config_value_at_path(&config, "a.b"), None);
    }

    #[test]
    fn r715_read_through_array_returns_none() {
        let config = json!({"a": [1, 2, 3]});
        assert_eq!(read_config_value_at_path(&config, "a.0"), None);
    }

    // ----- writeConfigValueAtPath -----

    #[test]
    fn r715_write_top_level() {
        let config = json!({});
        let new = write_config_value_at_path(&config, "a", Some(&json!(1)));
        assert_eq!(new, json!({"a": 1}));
        // 原 config 不被修改
        assert_eq!(config, json!({}));
    }

    #[test]
    fn r715_write_nested_creates_intermediate() {
        let config = json!({});
        let new = write_config_value_at_path(&config, "a.b.c", Some(&json!("v")));
        assert_eq!(new, json!({"a": {"b": {"c": "v"}}}));
    }

    #[test]
    fn r715_write_replaces_existing() {
        let config = json!({"a": {"b": 1}});
        let new = write_config_value_at_path(&config, "a.b", Some(&json!(2)));
        assert_eq!(new, json!({"a": {"b": 2}}));
    }

    #[test]
    fn r715_write_value_none_deletes() {
        let config = json!({"a": 1, "b": 2});
        let new = write_config_value_at_path(&config, "a", None);
        assert_eq!(new, json!({"b": 2}));
    }

    #[test]
    fn r715_write_through_array_creates_object() {
        let config = json!({"a": [1, 2, 3]});
        let new = write_config_value_at_path(&config, "a.b", Some(&json!("v")));
        // 原 array 被替换为 object（与 Node 1:1）
        assert_eq!(new, json!({"a": {"b": "v"}}));
    }

    #[test]
    fn r715_write_empty_path_noop() {
        let config = json!({"a": 1});
        let new = write_config_value_at_path(&config, "", Some(&json!("v")));
        assert_eq!(new, config);
    }

    // ----- integration -----

    #[test]
    fn r715_collect_then_read() {
        let schema = json!({
            "type": "object",
            "properties": {
                "auth": {
                    "type": "object",
                    "properties": {
                        "apiKey": {"type": "string", "format": "secret-ref"},
                        "endpoint": {"type": "string"}
                    }
                }
            }
        });
        let paths = collect_secret_ref_paths(&schema);
        let config = json!({
            "auth": {
                "apiKey": {"type": "secret_ref", "secretId": "123e4567-e89b-12d3-a456-426614174000"},
                "endpoint": "https://api.example.com"
            }
        });
        for path in &paths {
            let v = read_config_value_at_path(&config, path).unwrap();
            let binding = parse_secret_ref_binding_object(v)
                .expect("path values should be valid secret_ref bindings");
            assert!(is_uuid_secret_ref(&binding.secret_id));
        }
    }

    #[test]
    fn r715_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SecretRefBindingObject>();
        assert_send_sync::<SecretRefVersion>();
    }
}
