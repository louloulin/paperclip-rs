#![forbid(unsafe_code)]
//! `pc-adapter-models-env` —— 解析 `PAPERCLIP_ADAPTER_MODELS` 环境变量。
//!
//! 对应 Node `server/src/services/adapter-models-env.ts`（40 行）。
//!
//! 设计目标：1:1 复刻 `parseAdapterModelsEnv` 的语义——
//! - 空字符串 / 未设置 → `None`
//! - 非 JSON → throw
//! - 非对象 / 数组 → throw
//! - 嵌套数组中每项必须有非空 string `id`，`label` 可选（默认用 `id`）

use std::collections::HashMap;

/// 单条 model entry。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AdapterModelEntry {
    pub id: String,
    pub label: String,
}

impl AdapterModelEntry {
    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into();
        Self { label: id.clone(), id }
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AdapterModelsEnvError {
    #[error("PAPERCLIP_ADAPTER_MODELS must be valid JSON: {0}")]
    InvalidJson(String),
    #[error("PAPERCLIP_ADAPTER_MODELS must be a JSON object mapping adapterType to an array of {{id,label}}")]
    NotObject,
    #[error("PAPERCLIP_ADAPTER_MODELS[{0}] must be an array")]
    AdapterListNotArray(String),
    #[error("PAPERCLIP_ADAPTER_MODELS[{0}] entries require a non-empty string id")]
    AdapterEntryMissingId(String),
}

/// 解析 env（默认用 `process.env`-like HashMap）。
///
/// 与 Node `parseAdapterModelsEnv(env)` 1:1 对齐。
pub fn parse_adapter_models_env(
    env: &HashMap<String, String>,
) -> Result<Option<HashMap<String, Vec<AdapterModelEntry>>>, AdapterModelsEnvError> {
    let raw = env
        .get("PAPERCLIP_ADAPTER_MODELS")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let Some(raw) = raw else {
        return Ok(None);
    };

    let parsed: serde_json::Value = serde_json::from_str(raw)
        .map_err(|e| AdapterModelsEnvError::InvalidJson(e.to_string()))?;
    let Some(map) = parsed.as_object() else {
        return Err(AdapterModelsEnvError::NotObject);
    };

    let mut out: HashMap<String, Vec<AdapterModelEntry>> = HashMap::new();
    for (adapter_type, list) in map {
        let Some(list) = list.as_array() else {
            return Err(AdapterModelsEnvError::AdapterListNotArray(adapter_type.clone()));
        };
        let mut entries = Vec::with_capacity(list.len());
        for item in list {
            let Some(obj) = item.as_object() else {
                return Err(AdapterModelsEnvError::AdapterEntryMissingId(adapter_type.clone()));
            };
            let id = obj
                .get("id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| AdapterModelsEnvError::AdapterEntryMissingId(adapter_type.clone()))?
                .to_string();
            let label = obj
                .get("label")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| id.clone());
            entries.push(AdapterModelEntry { id, label });
        }
        out.insert(adapter_type.clone(), entries);
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn r692_unset_returns_none() {
        let env = env_of(&[]);
        assert!(parse_adapter_models_env(&env).unwrap().is_none());
    }

    #[test]
    fn r692_empty_string_returns_none() {
        let env = env_of(&[("PAPERCLIP_ADAPTER_MODELS", "")]);
        assert!(parse_adapter_models_env(&env).unwrap().is_none());
    }

    #[test]
    fn r692_whitespace_only_returns_none() {
        let env = env_of(&[("PAPERCLIP_ADAPTER_MODELS", "   \n\t")]);
        assert!(parse_adapter_models_env(&env).unwrap().is_none());
    }

    #[test]
    fn r692_invalid_json_errors() {
        let env = env_of(&[("PAPERCLIP_ADAPTER_MODELS", "{not json")]);
        let err = parse_adapter_models_env(&env).unwrap_err();
        assert!(matches!(err, AdapterModelsEnvError::InvalidJson(_)));
    }

    #[test]
    fn r692_top_level_array_errors() {
        let env = env_of(&[("PAPERCLIP_ADAPTER_MODELS", "[]")]);
        let err = parse_adapter_models_env(&env).unwrap_err();
        assert!(matches!(err, AdapterModelsEnvError::NotObject));
    }

    #[test]
    fn r692_adapter_list_not_array() {
        let env = env_of(&[("PAPERCLIP_ADAPTER_MODELS", r#"{"claude": "abc"}"#)]);
        let err = parse_adapter_models_env(&env).unwrap_err();
        assert!(matches!(err, AdapterModelsEnvError::AdapterListNotArray(ref t) if t == "claude"));
    }

    #[test]
    fn r692_entry_missing_id() {
        let env = env_of(&[("PAPERCLIP_ADAPTER_MODELS", r#"{"claude": [{}]}"#)]);
        let err = parse_adapter_models_env(&env).unwrap_err();
        assert!(matches!(err, AdapterModelsEnvError::AdapterEntryMissingId(ref t) if t == "claude"));
    }

    #[test]
    fn r692_entry_id_must_be_non_empty() {
        let env = env_of(&[("PAPERCLIP_ADAPTER_MODELS", r#"{"claude": [{"id": ""}]}"#)]);
        let err = parse_adapter_models_env(&env).unwrap_err();
        assert!(matches!(err, AdapterModelsEnvError::AdapterEntryMissingId(_)));
    }

    #[test]
    fn r692_parses_minimal_object() {
        let env = env_of(&[(
            "PAPERCLIP_ADAPTER_MODELS",
            r#"{"claude": [{"id": "claude-opus-4"}]}"#,
        )]);
        let out = parse_adapter_models_env(&env).unwrap().unwrap();
        assert_eq!(out.len(), 1);
        let claude = out.get("claude").unwrap();
        assert_eq!(claude.len(), 1);
        assert_eq!(claude[0].id, "claude-opus-4");
        // 没有 label 时默认等于 id
        assert_eq!(claude[0].label, "claude-opus-4");
    }

    #[test]
    fn r692_parses_with_label() {
        let env = env_of(&[(
            "PAPERCLIP_ADAPTER_MODELS",
            r#"{"claude": [{"id": "opus-4", "label": "Opus 4"}]}"#,
        )]);
        let out = parse_adapter_models_env(&env).unwrap().unwrap();
        assert_eq!(out.get("claude").unwrap()[0].label, "Opus 4");
    }

    #[test]
    fn r692_parses_multiple_adapters() {
        let env = env_of(&[(
            "PAPERCLIP_ADAPTER_MODELS",
            r#"{
                "claude": [{"id": "opus-4", "label": "Opus 4"}],
                "codex": [{"id": "codex-1"}]
            }"#,
        )]);
        let out = parse_adapter_models_env(&env).unwrap().unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out.get("claude").unwrap()[0].id, "opus-4");
        assert_eq!(out.get("codex").unwrap()[0].label, "codex-1");
    }

    #[test]
    fn r692_label_non_string_falls_back_to_id() {
        let env = env_of(&[(
            "PAPERCLIP_ADAPTER_MODELS",
            r#"{"claude": [{"id": "x", "label": 123}]}"#,
        )]);
        let out = parse_adapter_models_env(&env).unwrap().unwrap();
        // 非 string label → 用 id 兜底
        assert_eq!(out.get("claude").unwrap()[0].label, "x");
    }

    #[test]
    fn r692_entry_not_object_errors() {
        let env = env_of(&[("PAPERCLIP_ADAPTER_MODELS", r#"{"claude": ["not-an-object"]}"#)]);
        let err = parse_adapter_models_env(&env).unwrap_err();
        assert!(matches!(err, AdapterModelsEnvError::AdapterEntryMissingId(_)));
    }

    #[test]
    fn r692_empty_object_parses_to_empty_map() {
        let env = env_of(&[("PAPERCLIP_ADAPTER_MODELS", "{}")]);
        let out = parse_adapter_models_env(&env).unwrap().unwrap();
        assert!(out.is_empty());
    }
}
