//! Adapter models env parsing（1:1 port of Node `server/src/services/adapter-models-env.ts`，40 行）。
//!
//! 单一职责：从环境变量 `PAPERCLIP_ADAPTER_MODELS` 解析出 per-adapter model 列表，
//! 让 agent model picker 可以展示 server 无法 CLI 自动发现的模型（如 gateway models）。
//!
//! 格式（与 Node `parseAdapterModelsEnv` 1:1 对齐）：
//! - JSON object：`{ adapterType: [{ id, label? }, ...], ... }`
//! - 未设置 → `Ok(None)`
//! - JSON 格式错 / 字段错 → `Err(AdapterModelsEnvError)`
//!
//! 不持有状态；不依赖 IO（除读取传入的 env map）。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// 环境变量名（与 Node `env.PAPERCLIP_ADAPTER_MODELS` 1:1 对齐）。
pub const PAPERCLIP_ADAPTER_MODELS_ENV: &str = "PAPERCLIP_ADAPTER_MODELS";

/// 单个模型条目（与 Node `AdapterModelEntry` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterModelEntry {
    pub id: String,
    /// label 可选；未提供时降级使用 `id`（与 Node `label ?? id` 1:1 对齐）。
    pub label: String,
}

impl AdapterModelEntry {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
        }
    }
}

/// `PAPERCLIP_ADAPTER_MODELS` 解析错误（与 Node 抛 `Error` 1:1 对齐）。
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum AdapterModelsEnvError {
    #[error("PAPERCLIP_ADAPTER_MODELS must be valid JSON: {message}")]
    InvalidJson { message: String },
    #[error("PAPERCLIP_ADAPTER_MODELS must be a JSON object mapping adapterType to an array of {{id,label}}")]
    NotJsonObject,
    #[error("PAPERCLIP_ADAPTER_MODELS[{adapter_type}] must be an array")]
    NotArray { adapter_type: String },
    #[error("PAPERCLIP_ADAPTER_MODELS[{adapter_type}] entries require a non-empty string id")]
    InvalidEntry { adapter_type: String },
}

/// 从 env map 中读取 `PAPERCLIP_ADAPTER_MODELS` 并解析。
///
/// 行为（与 Node `parseAdapterModelsEnv` 1:1 对齐）：
/// 1. 读 `PAPERCLIP_ADAPTER_MODELS`，trim；空 / 未设置 → `Ok(None)`
/// 2. `JSON.parse` 失败 → `Err(InvalidJson)`
/// 3. 解析结果非 object / 是数组 → `Err(NotJsonObject)`
/// 4. 对每个 `[adapterType, list]`：list 非数组 → `Err(NotArray)`
/// 5. list 中每个 entry：`id` 必须是非空 string → `Err(InvalidEntry)`；否则 `{ id, label ?? id }`
#[must_use]
pub fn parse_adapter_models_env(
    env: &HashMap<String, String>,
) -> Result<Option<HashMap<String, Vec<AdapterModelEntry>>>, AdapterModelsEnvError> {
    let raw = env
        .get(PAPERCLIP_ADAPTER_MODELS_ENV)
        .map(String::as_str)
        .unwrap_or("")
        .trim();

    if raw.is_empty() {
        return Ok(None);
    }

    // 步骤 2：JSON 解析
    let parsed: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| AdapterModelsEnvError::InvalidJson {
            message: e.to_string(),
        })?;

    // 步骤 3：必须是 plain object（不是数组）
    let Some(obj) = parsed.as_object() else {
        return Err(AdapterModelsEnvError::NotJsonObject);
    };

    // 步骤 4 & 5：遍历每个 adapterType → array of {id, label?}
    let mut out: HashMap<String, Vec<AdapterModelEntry>> = HashMap::new();
    for (adapter_type, list_value) in obj {
        let Some(list) = list_value.as_array() else {
            return Err(AdapterModelsEnvError::NotArray {
                adapter_type: adapter_type.clone(),
            });
        };
        let entries: Vec<AdapterModelEntry> = list
            .iter()
            .map(|entry| {
                let entry_obj =
                    entry
                        .as_object()
                        .ok_or_else(|| AdapterModelsEnvError::InvalidEntry {
                            adapter_type: adapter_type.clone(),
                        })?;
                let id = entry_obj
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| AdapterModelsEnvError::InvalidEntry {
                        adapter_type: adapter_type.clone(),
                    })?;
                if id.is_empty() {
                    return Err(AdapterModelsEnvError::InvalidEntry {
                        adapter_type: adapter_type.clone(),
                    });
                }
                // label 可选；缺失或非 string 时降级使用 id
                let label = entry_obj
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap_or(id);
                Ok(AdapterModelEntry {
                    id: id.to_string(),
                    label: label.to_string(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        out.insert(adapter_type.clone(), entries);
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with(raw: &str) -> HashMap<String, String> {
        let mut h = HashMap::new();
        h.insert(PAPERCLIP_ADAPTER_MODELS_ENV.into(), raw.into());
        h
    }

    // ---- 常量 ----

    #[test]
    fn env_constant_matches_node() {
        assert_eq!(PAPERCLIP_ADAPTER_MODELS_ENV, "PAPERCLIP_ADAPTER_MODELS");
    }

    // ---- 空 / 未设置 ----

    #[test]
    fn missing_env_returns_none() {
        let env: HashMap<String, String> = HashMap::new();
        let out = parse_adapter_models_env(&env).unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn empty_value_returns_none() {
        let env = env_with("");
        let out = parse_adapter_models_env(&env).unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn whitespace_only_returns_none() {
        let env = env_with("   \t\n");
        let out = parse_adapter_models_env(&env).unwrap();
        assert!(out.is_none());
    }

    // ---- JSON 格式错误 ----

    #[test]
    fn invalid_json_returns_error() {
        let env = env_with("{not valid json");
        let err = parse_adapter_models_env(&env).unwrap_err();
        assert!(matches!(err, AdapterModelsEnvError::InvalidJson { .. }));
    }

    #[test]
    fn array_root_returns_not_object_error() {
        let env = env_with(r#"[1, 2, 3]"#);
        let err = parse_adapter_models_env(&env).unwrap_err();
        assert_eq!(err, AdapterModelsEnvError::NotJsonObject);
    }

    #[test]
    fn string_root_returns_not_object_error() {
        let env = env_with(r#""hello""#);
        let err = parse_adapter_models_env(&env).unwrap_err();
        assert_eq!(err, AdapterModelsEnvError::NotJsonObject);
    }

    #[test]
    fn null_root_returns_not_object_error() {
        let env = env_with("null");
        let err = parse_adapter_models_env(&env).unwrap_err();
        assert_eq!(err, AdapterModelsEnvError::NotJsonObject);
    }

    // ---- 字段错误 ----

    #[test]
    fn list_not_array_returns_not_array_error() {
        let env = env_with(r#"{"claude": "not an array"}"#);
        let err = parse_adapter_models_env(&env).unwrap_err();
        assert_eq!(
            err,
            AdapterModelsEnvError::NotArray {
                adapter_type: "claude".into()
            }
        );
    }

    #[test]
    fn missing_id_returns_invalid_entry_error() {
        let env = env_with(r#"{"claude": [{"label": "no id"}]}"#);
        let err = parse_adapter_models_env(&env).unwrap_err();
        assert_eq!(
            err,
            AdapterModelsEnvError::InvalidEntry {
                adapter_type: "claude".into()
            }
        );
    }

    #[test]
    fn empty_id_returns_invalid_entry_error() {
        let env = env_with(r#"{"claude": [{"id": ""}]}"#);
        let err = parse_adapter_models_env(&env).unwrap_err();
        assert_eq!(
            err,
            AdapterModelsEnvError::InvalidEntry {
                adapter_type: "claude".into()
            }
        );
    }

    #[test]
    fn entry_not_object_returns_invalid_entry_error() {
        let env = env_with(r#"{"claude": ["not an object"]}"#);
        let err = parse_adapter_models_env(&env).unwrap_err();
        assert_eq!(
            err,
            AdapterModelsEnvError::InvalidEntry {
                adapter_type: "claude".into()
            }
        );
    }

    // ---- 成功路径 ----

    #[test]
    fn simple_object_parses() {
        let env = env_with(r#"{"claude": [{"id": "claude-3"}]}"#);
        let out = parse_adapter_models_env(&env).unwrap().unwrap();
        let entries = out.get("claude").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "claude-3");
        // label 缺省时降级为 id
        assert_eq!(entries[0].label, "claude-3");
    }

    #[test]
    fn explicit_label_preserved() {
        let env = env_with(r#"{"claude": [{"id": "claude-3", "label": "Claude 3 Opus"}]}"#);
        let out = parse_adapter_models_env(&env).unwrap().unwrap();
        let entries = out.get("claude").unwrap();
        assert_eq!(entries[0].id, "claude-3");
        assert_eq!(entries[0].label, "Claude 3 Opus");
    }

    #[test]
    fn multiple_adapter_types() {
        let env = env_with(
            r#"{
                "claude": [{"id": "c1"}, {"id": "c2", "label": "C2"}],
                "codex": [{"id": "x1"}]
            }"#,
        );
        let out = parse_adapter_models_env(&env).unwrap().unwrap();
        assert_eq!(out.len(), 2);

        let claude = out.get("claude").unwrap();
        assert_eq!(claude.len(), 2);
        assert_eq!(claude[0].id, "c1");
        assert_eq!(claude[0].label, "c1"); // fallback
        assert_eq!(claude[1].id, "c2");
        assert_eq!(claude[1].label, "C2");

        let codex = out.get("codex").unwrap();
        assert_eq!(codex.len(), 1);
        assert_eq!(codex[0].id, "x1");
    }

    #[test]
    fn empty_object_returns_empty_map() {
        let env = env_with("{}");
        let out = parse_adapter_models_env(&env).unwrap().unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn empty_array_for_adapter_type() {
        let env = env_with(r#"{"claude": []}"#);
        let out = parse_adapter_models_env(&env).unwrap().unwrap();
        assert!(out.get("claude").unwrap().is_empty());
    }

    #[test]
    fn non_string_label_falls_back_to_id() {
        // Node 端：label 是非 string 时也用 id
        let env = env_with(r#"{"claude": [{"id": "c1", "label": 42}]}"#);
        let out = parse_adapter_models_env(&env).unwrap().unwrap();
        assert_eq!(out.get("claude").unwrap()[0].label, "c1");
    }

    // ---- AdapterModelEntry ----

    #[test]
    fn adapter_model_entry_new() {
        let e = AdapterModelEntry::new("id1", "Label 1");
        assert_eq!(e.id, "id1");
        assert_eq!(e.label, "Label 1");
    }

    // ---- Error Display ----

    #[test]
    fn error_messages_include_helpful_context() {
        let env = env_with(r#"{"x": "not array"}"#);
        let err = parse_adapter_models_env(&env).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("PAPERCLIP_ADAPTER_MODELS[x]"));
        assert!(msg.contains("array"));
    }
}
