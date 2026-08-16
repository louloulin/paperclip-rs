#![allow(clippy::all)]
// SPDX-License-Identifier: MIT
//
// R682 parity: `json-schema-secret-refs.ts` pure helpers.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// isUuidSecretRef
// ---------------------------------------------------------------------------

pub fn is_uuid_secret_ref(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    let dash_positions = [8usize, 13, 18, 23];
    for (i, b) in bytes.iter().enumerate() {
        let is_dash_pos = dash_positions.contains(&i);
        if is_dash_pos {
            if *b != 0x2D {
                return false;
            }
        } else if !b.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// SecretRefBindingObject
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretRefBindingObject {
    #[serde(rename = "secretId")]
    pub secret_id: String,
    #[serde(default)]
    pub version: SecretRefBindingVersion,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum SecretRefBindingVersion {
    Latest,
    Number(u64),
}

impl Default for SecretRefBindingVersion {
    fn default() -> Self {
        Self::Latest
    }
}

// ---------------------------------------------------------------------------
// parseSecretRefBindingObject
// ---------------------------------------------------------------------------

pub fn parse_secret_ref_binding_object(value: &serde_json::Value) -> Option<SecretRefBindingObject> {
    let obj = value.as_object()?;
    if obj.get("type").and_then(|v| v.as_str()) != Some("secret_ref") {
        return None;
    }
    let raw_secret_id = obj.get("secretId")?.as_str()?;
    let trimmed = raw_secret_id.trim();
    if !is_uuid_secret_ref(trimmed) {
        return None;
    }

    let version_value = obj.get("version");
    let version = match version_value {
        None | Some(serde_json::Value::Null) => SecretRefBindingVersion::Latest,
        Some(serde_json::Value::String(s)) if s == "latest" => SecretRefBindingVersion::Latest,
        Some(serde_json::Value::Number(n)) => {
            let v = n.as_i64()?;
            if v <= 0 {
                return None;
            }
            SecretRefBindingVersion::Number(v as u64)
        }
        _ => return None,
    };

    Some(SecretRefBindingObject {
        secret_id: trimmed.to_string(),
        version,
    })
}

// ---------------------------------------------------------------------------
// collectSecretRefPaths
// ---------------------------------------------------------------------------

pub fn collect_secret_ref_paths(schema: Option<&serde_json::Value>) -> HashSet<String> {
    let mut paths: HashSet<String> = HashSet::new();
    let Some(s) = schema else { return paths; };
    let Some(obj) = s.as_object() else { return paths; };
    walk_schema(obj, "", &mut paths);
    paths
}

fn walk_schema(
    node: &serde_json::Map<String, serde_json::Value>,
    prefix: &str,
    paths: &mut HashSet<String>,
) {
    for keyword in ["allOf", "anyOf", "oneOf"] {
        let Some(branches) = node.get(keyword).and_then(|v| v.as_array()) else { continue; };
        for branch in branches {
            let Some(branch_obj) = branch.as_object() else { continue; };
            walk_schema(branch_obj, prefix, paths);
        }
    }

    let Some(properties) = node.get("properties").and_then(|v| v.as_object()) else { return; };
    for (key, property_schema) in properties {
        let Some(property_obj) = property_schema.as_object() else { continue; };
        let path = if prefix.is_empty() { key.clone() } else { format!("{}.{}", prefix, key) };
        if property_obj.get("format").and_then(|v| v.as_str()) == Some("secret-ref") {
            paths.insert(path.clone());
        }
        walk_schema(property_obj, &path, paths);
    }
}

// ---------------------------------------------------------------------------
// readConfigValueAtPath
// ---------------------------------------------------------------------------

pub fn read_config_value_at_path<'a>(config: &'a serde_json::Value, dot_path: &str) -> Option<&'a serde_json::Value> {
    let mut current = config;
    for key in dot_path.split('.') {
        let obj = current.as_object()?;
        current = obj.get(key)?;
    }
    Some(current)
}

// ---------------------------------------------------------------------------
// writeConfigValueAtPath
// ---------------------------------------------------------------------------

pub fn write_config_value_at_path(
    config: &serde_json::Value,
    dot_path: &str,
    value: Option<&serde_json::Value>,
) -> serde_json::Value {
    let mut result = config.clone();
    let keys: Vec<&str> = dot_path.split('.').collect();
    if keys.is_empty() {
        return result;
    }

    let leaf_idx = keys.len() - 1;
    let mut cursor: &mut serde_json::Value = &mut result;
    for i in 0..leaf_idx {
        let key = keys[i];
        let needs_new = !cursor.is_object()
            || cursor.as_object().and_then(|o| o.get(key)).map(|v| !v.is_object()).unwrap_or(true);
        if needs_new {
            if !cursor.is_object() {
                *cursor = serde_json::Value::Object(serde_json::Map::new());
            }
            cursor.as_object_mut().unwrap().insert(
                key.to_string(),
                serde_json::Value::Object(serde_json::Map::new()),
            );
        }
        cursor = cursor.as_object_mut().unwrap().get_mut(key).unwrap();
    }

    let leaf_key = keys[leaf_idx];
    let obj = cursor.as_object_mut().unwrap();
    match value {
        None => { obj.remove(leaf_key); }
        Some(v) => { obj.insert(leaf_key.to_string(), v.clone()); }
    }
    result
}
