//! Hermes gateway 配置 schema（对齐 Node
//! `packages/adapters/hermes/src/gateway/server/config-schema.ts`）。
//!
//! 9 字段：apiBaseUrl / apiKey / escape-hatch / sessionKeyStrategy /
//! timeoutSec / eventReconnectMs / paperclipApiUrl / headers / instructions。

use serde::{Deserialize, Serialize};

use crate::constants::{DEFAULT_EVENT_RECONNECT_MS, DEFAULT_TIMEOUT_SEC};
use crate::transport_security::INSECURE_REMOTE_HTTP_ESCAPE_HATCH;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Select,
    Number,
    Text,
    Textarea,
    Toggle,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConfigField {
    pub key: String,
    pub label: String,
    pub r#type: FieldType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<ConfigOption>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConfigOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AdapterConfigSchema {
    pub fields: Vec<ConfigField>,
}

/// 构造 Hermes gateway 配置 schema（9 字段）。
pub fn get_config_schema() -> AdapterConfigSchema {
    AdapterConfigSchema {
        fields: vec![
            ConfigField {
                key: "apiBaseUrl".to_string(),
                label: "API base URL".to_string(),
                r#type: FieldType::Text,
                default: None,
                required: Some(true),
                options: None,
                hint: Some(
                    "Hermes API server base URL, such as http://127.0.0.1:8642 or a private HTTPS URL. The default dashboard root or chat URL, such as http://127.0.0.1:9119/chat, is accepted and maps to /api.".to_string(),
                ),
            },
            ConfigField {
                key: "apiKey".to_string(),
                label: "API key".to_string(),
                r#type: FieldType::Text,
                default: None,
                required: Some(true),
                options: None,
                hint: Some(
                    "Hermes API_SERVER_KEY, not PAPERCLIP_API_KEY. Stored as a Paperclip secret reference.".to_string(),
                ),
            },
            ConfigField {
                key: INSECURE_REMOTE_HTTP_ESCAPE_HATCH.to_string(),
                label: "Dangerously allow remote HTTP".to_string(),
                r#type: FieldType::Toggle,
                default: Some(serde_json::json!(false)),
                required: None,
                options: None,
                hint: Some(
                    "Unsafe dev-only escape hatch. Remote Hermes gateways should use HTTPS; loopback HTTP remains allowed.".to_string(),
                ),
            },
            ConfigField {
                key: "sessionKeyStrategy".to_string(),
                label: "Session key strategy".to_string(),
                r#type: FieldType::Select,
                default: Some(serde_json::json!("issue")),
                required: None,
                options: Some(vec![
                    ConfigOption { value: "issue".to_string(), label: "Issue scoped".to_string() },
                    ConfigOption { value: "agent".to_string(), label: "Agent scoped".to_string() },
                    ConfigOption { value: "run".to_string(), label: "Run scoped".to_string() },
                    ConfigOption { value: "none".to_string(), label: "None".to_string() },
                ]),
                hint: Some(
                    "Controls X-Hermes-Session-Key. Issue scoped prevents cross-task memory bleed by default.".to_string(),
                ),
            },
            ConfigField {
                key: "timeoutSec".to_string(),
                label: "Timeout seconds".to_string(),
                r#type: FieldType::Number,
                default: Some(serde_json::json!(DEFAULT_TIMEOUT_SEC)),
                required: None,
                options: None,
                hint: None,
            },
            ConfigField {
                key: "eventReconnectMs".to_string(),
                label: "Event reconnect ms".to_string(),
                r#type: FieldType::Number,
                default: Some(serde_json::json!(DEFAULT_EVENT_RECONNECT_MS)),
                required: None,
                options: None,
                hint: Some(
                    "Delay before reconnecting the Hermes SSE events stream after a nonterminal disconnect.".to_string(),
                ),
            },
            ConfigField {
                key: "paperclipApiUrl".to_string(),
                label: "Paperclip API URL".to_string(),
                r#type: FieldType::Text,
                default: None,
                required: None,
                options: None,
                hint: Some(
                    "Optional Paperclip API URL reachable by the remote Hermes host. This is not a credential.".to_string(),
                ),
            },
            ConfigField {
                key: "headers".to_string(),
                label: "Extra headers".to_string(),
                r#type: FieldType::Textarea,
                default: None,
                required: None,
                options: None,
                hint: Some(
                    "Optional JSON object of extra nonsecret headers. Security-critical headers are generated by the adapter.".to_string(),
                ),
            },
            ConfigField {
                key: "instructions".to_string(),
                label: "Instructions".to_string(),
                r#type: FieldType::Textarea,
                default: None,
                required: None,
                options: None,
                hint: Some(
                    "Optional stable Hermes instructions sent separately from the wake input.".to_string(),
                ),
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_has_nine_fields() {
        let schema = get_config_schema();
        assert_eq!(schema.fields.len(), 9);
    }

    #[test]
    fn api_base_url_is_required_text() {
        let schema = get_config_schema();
        let field = schema
            .fields
            .iter()
            .find(|f| f.key == "apiBaseUrl")
            .expect("apiBaseUrl");
        assert_eq!(field.r#type, FieldType::Text);
        assert_eq!(field.required, Some(true));
    }

    #[test]
    fn api_key_is_required_text() {
        let schema = get_config_schema();
        let field = schema
            .fields
            .iter()
            .find(|f| f.key == "apiKey")
            .expect("apiKey");
        assert_eq!(field.r#type, FieldType::Text);
        assert_eq!(field.required, Some(true));
    }

    #[test]
    fn session_key_strategy_lists_four_options() {
        let schema = get_config_schema();
        let field = schema
            .fields
            .iter()
            .find(|f| f.key == "sessionKeyStrategy")
            .expect("sessionKeyStrategy");
        let options = field.options.as_ref().expect("options");
        let values: Vec<&str> = options.iter().map(|o| o.value.as_str()).collect();
        for required in ["issue", "agent", "run", "none"] {
            assert!(values.contains(&required), "missing strategy: {required}");
        }
    }

    #[test]
    fn defaults_align_with_constants() {
        let schema = get_config_schema();
        let timeout = schema
            .fields
            .iter()
            .find(|f| f.key == "timeoutSec")
            .unwrap();
        assert_eq!(
            timeout.default,
            Some(serde_json::json!(DEFAULT_TIMEOUT_SEC))
        );
        let reconnect = schema
            .fields
            .iter()
            .find(|f| f.key == "eventReconnectMs")
            .unwrap();
        assert_eq!(
            reconnect.default,
            Some(serde_json::json!(DEFAULT_EVENT_RECONNECT_MS))
        );
    }
}
