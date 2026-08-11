//! Hermes 配置 schema（对齐 Node `config-schema.ts` 的 12 个字段）。
//!
//! Paperclip UI 用此 schema 渲染配置表单。Schema 是纯数据 — 字段定义可
//! 直接喂给前端表单组件。

use serde::{Deserialize, Serialize};

/// 表单字段类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Select,
    Number,
    Text,
    Textarea,
    Toggle,
}

/// 单个配置字段。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConfigField {
    pub key: String,
    pub label: String,
    pub r#type: FieldType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<ConfigOption>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// 下拉选项 `{ value, label }`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConfigOption {
    pub value: String,
    pub label: String,
}

/// AdapterConfigSchema（与 Paperclip UI 兼容）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AdapterConfigSchema {
    pub fields: Vec<ConfigField>,
}

/// 把 provider 名字转成 UI 标签（对齐 Node `providerLabel`）。
fn provider_label(provider: &str) -> String {
    match provider {
        "auto" => "Auto".to_string(),
        "openai-codex" => "OpenAI Codex".to_string(),
        "kimi-coding" => "Kimi Coding".to_string(),
        "minimax-cn" => "MiniMax China".to_string(),
        _ => provider
            .split('-')
            .map(|part| {
                let mut chars = part.chars();
                match chars.next() {
                    Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// 构造 Hermes 配置 schema（12 个字段）。
pub fn get_config_schema() -> AdapterConfigSchema {
    use crate::constants::VALID_PROVIDERS;
    AdapterConfigSchema {
        fields: vec![
            ConfigField {
                key: "provider".to_string(),
                label: "Provider".to_string(),
                r#type: FieldType::Select,
                default: Some(serde_json::json!("auto")),
                options: Some(
                    VALID_PROVIDERS
                        .iter()
                        .map(|provider| ConfigOption {
                            value: provider.to_string(),
                            label: provider_label(provider),
                        })
                        .collect(),
                ),
                hint: Some(
                    "Usually auto. Set this only when Hermes cannot infer the provider from the model or ~/.hermes/config.yaml.".to_string(),
                ),
            },
            ConfigField {
                key: "timeoutSec".to_string(),
                label: "Timeout seconds".to_string(),
                r#type: FieldType::Number,
                default: Some(serde_json::json!(crate::constants::DEFAULT_TIMEOUT_SEC)),
                options: None,
                hint: None,
            },
            ConfigField {
                key: "graceSec".to_string(),
                label: "Grace seconds".to_string(),
                r#type: FieldType::Number,
                default: Some(serde_json::json!(crate::constants::DEFAULT_GRACE_SEC)),
                options: None,
                hint: Some(
                    "Seconds to wait after SIGTERM before killing the Hermes process.".to_string(),
                ),
            },
            ConfigField {
                key: "maxTurnsPerRun".to_string(),
                label: "Max turns per run".to_string(),
                r#type: FieldType::Number,
                default: None,
                options: None,
                hint: Some(
                    "Optional Hermes --max-turns limit for tool-calling iterations.".to_string(),
                ),
            },
            ConfigField {
                key: "toolsets".to_string(),
                label: "Toolsets".to_string(),
                r#type: FieldType::Text,
                default: None,
                options: None,
                hint: Some(
                    "Optional comma-separated Hermes toolsets, such as terminal,file,web.".to_string(),
                ),
            },
            ConfigField {
                key: "persistSession".to_string(),
                label: "Persist session".to_string(),
                r#type: FieldType::Toggle,
                default: Some(serde_json::json!(true)),
                options: None,
                hint: Some(
                    "Resume Hermes sessions across Paperclip heartbeats.".to_string(),
                ),
            },
            ConfigField {
                key: "worktreeMode".to_string(),
                label: "Hermes worktree mode".to_string(),
                r#type: FieldType::Toggle,
                default: Some(serde_json::json!(false)),
                options: None,
                hint: Some("Pass Hermes --worktree.".to_string()),
            },
            ConfigField {
                key: "checkpoints".to_string(),
                label: "Checkpoints".to_string(),
                r#type: FieldType::Toggle,
                default: Some(serde_json::json!(false)),
                options: None,
                hint: Some("Pass Hermes --checkpoints.".to_string()),
            },
            ConfigField {
                key: "quiet".to_string(),
                label: "Quiet output".to_string(),
                r#type: FieldType::Toggle,
                default: Some(serde_json::json!(true)),
                options: None,
                hint: Some(
                    "Pass Hermes --quiet for cleaner Paperclip run transcripts.".to_string(),
                ),
            },
            ConfigField {
                key: "verbose".to_string(),
                label: "Verbose output".to_string(),
                r#type: FieldType::Toggle,
                default: Some(serde_json::json!(false)),
                options: None,
                hint: Some("Pass Hermes --verbose.".to_string()),
            },
            ConfigField {
                key: "paperclipApiUrl".to_string(),
                label: "Paperclip API URL".to_string(),
                r#type: FieldType::Text,
                default: None,
                options: None,
                hint: Some(
                    "Optional API base override. Defaults to PAPERCLIP_API_URL.".to_string(),
                ),
            },
            ConfigField {
                key: "promptTemplate".to_string(),
                label: "Prompt template".to_string(),
                r#type: FieldType::Textarea,
                default: None,
                options: None,
                hint: Some(
                    "Optional custom prompt template with {{variable}} placeholders.".to_string(),
                ),
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_has_12_fields() {
        let schema = get_config_schema();
        assert_eq!(schema.fields.len(), 12);
    }

    #[test]
    fn provider_options_match_valid_providers() {
        let schema = get_config_schema();
        let provider_field = schema
            .fields
            .iter()
            .find(|field| field.key == "provider")
            .expect("provider field");
        let options = provider_field.options.as_ref().expect("options");
        let values: Vec<&str> = options.iter().map(|opt| opt.value.as_str()).collect();
        for required in ["auto", "anthropic", "minimax", "minimax-cn"] {
            assert!(
                values.contains(&required),
                "missing provider option: {required}"
            );
        }
    }

    #[test]
    fn defaults_align_with_constants() {
        let schema = get_config_schema();
        let timeout_field = schema
            .fields
            .iter()
            .find(|field| field.key == "timeoutSec")
            .expect("timeoutSec field");
        assert_eq!(
            timeout_field.default,
            Some(serde_json::json!(crate::constants::DEFAULT_TIMEOUT_SEC))
        );
    }

    #[test]
    fn schema_serializes_to_json() {
        let schema = get_config_schema();
        let json = serde_json::to_value(&schema).expect("serialize");
        let fields = json
            .get("fields")
            .and_then(|v| v.as_array())
            .expect("fields");
        assert_eq!(fields.len(), 12);
    }

    #[test]
    fn provider_label_handles_special_cases() {
        assert_eq!(provider_label("auto"), "Auto");
        assert_eq!(provider_label("openai-codex"), "OpenAI Codex");
        assert_eq!(provider_label("kimi-coding"), "Kimi Coding");
        assert_eq!(provider_label("minimax-cn"), "MiniMax China");
        assert_eq!(provider_label("anthropic"), "Anthropic");
    }
}
