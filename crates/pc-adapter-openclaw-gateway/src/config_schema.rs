//! OpenClaw Gateway adapter config schema — 对齐 Node
//! `packages/adapters/openclaw-gateway/src/ui/build-config.ts`。
//!
//! 字段集合（按 Paperclip UI 表单顺序）：
//! - gatewayUrl                 — WebSocket URL（ws/wss/http/https）
//! - sessionKey                 — 显式 session key（fixed 策略使用）
//! - sessionKeyStrategy         — fixed / issue / run
//! - scopes                     — 逗号分隔的 OAuth scopes 列表
//! - clientId / clientMode / clientVersion / role — 客户端身份元数据
//! - allowInsecureRemoteHttp    — 远端 HTTP escape hatch
//! - deviceIdentityPath         — 可选配置文件路径（不在本 round 实现读取）
//! - requestTimeoutMs / connectTimeoutMs

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

use crate::constants::{
    ADAPTER_LABEL, ADAPTER_TYPE, DEFAULT_CLIENT_ID, DEFAULT_CLIENT_MODE, DEFAULT_CLIENT_VERSION,
    DEFAULT_CONNECT_TIMEOUT_MS, DEFAULT_REQUEST_TIMEOUT_MS, DEFAULT_ROLE, DEFAULT_SCOPES,
    DEFAULT_SESSION_KEY_STRATEGY,
};
use crate::host_security::ESCAPE_HATCH_KEY;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Select,
    Number,
    Text,
    LongText,
    Boolean,
}

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConfigOption {
    pub value: String,
    pub label: String,
}

/// Adapter 元数据描述符（Paperclip UI 用）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AdapterDescriptor {
    #[serde(rename = "adapterType")]
    pub adapter_type: String,
    pub label: String,
    pub schema: Vec<ConfigField>,
}

/// 构造 OpenClaw Gateway 配置 schema。
pub fn get_config_schema() -> Vec<ConfigField> {
    vec![
        ConfigField {
            key: "gatewayUrl".into(),
            label: "Gateway URL".into(),
            r#type: FieldType::Text,
            default: None,
            options: None,
            hint: Some(
                "WebSocket URL (ws://, wss://, http://, https://). Loopback allows any scheme; remote http requires allowInsecureRemoteHttp.".to_owned(),
            ),
            required: Some(true),
        },
        ConfigField {
            key: "sessionKey".into(),
            label: "Session key (fixed)".into(),
            r#type: FieldType::Text,
            default: Some(serde_json::json!("paperclip")),
            options: None,
            hint: Some(
                "Used only when sessionKeyStrategy = 'fixed'. The runtime can override via the agent's session id.".to_owned(),
            ),
            required: Some(false),
        },
        ConfigField {
            key: "sessionKeyStrategy".into(),
            label: "Session key strategy".into(),
            r#type: FieldType::Select,
            default: Some(serde_json::json!(DEFAULT_SESSION_KEY_STRATEGY)),
            options: Some(vec![
                ConfigOption { value: "fixed".into(), label: "Fixed (use sessionKey)".into() },
                ConfigOption { value: "issue".into(), label: "Issue (use issueId)".into() },
                ConfigOption { value: "run".into(), label: "Run (use runId)".into() },
            ]),
            hint: Some(
                "How to derive the session key sent to the gateway. 'fixed' uses the configured value; 'issue' uses issueId; 'run' uses runId.".to_owned(),
            ),
            required: Some(true),
        },
        ConfigField {
            key: "scopes".into(),
            label: "OAuth scopes".into(),
            r#type: FieldType::Text,
            default: Some(serde_json::json!(DEFAULT_SCOPES.join(","))),
            options: None,
            hint: Some(
                "Comma-separated OAuth scopes. Default 'operator.admin'. Pass empty to disable.".to_owned(),
            ),
            required: Some(false),
        },
        ConfigField {
            key: "clientId".into(),
            label: "Client ID".into(),
            r#type: FieldType::Text,
            default: Some(serde_json::json!(DEFAULT_CLIENT_ID)),
            options: None,
            hint: Some("Sent in the device.connect frame as the client identity.".to_owned()),
            required: Some(false),
        },
        ConfigField {
            key: "clientMode".into(),
            label: "Client mode".into(),
            r#type: FieldType::Text,
            default: Some(serde_json::json!(DEFAULT_CLIENT_MODE)),
            options: None,
            hint: Some("Free-form string describing the deployment mode (e.g. 'backend', 'cli').".to_owned()),
            required: Some(false),
        },
        ConfigField {
            key: "clientVersion".into(),
            label: "Client version".into(),
            r#type: FieldType::Text,
            default: Some(serde_json::json!(DEFAULT_CLIENT_VERSION)),
            options: None,
            hint: Some("Reported as 'clientVersion' on connect (e.g. 'paperclip').".to_owned()),
            required: Some(false),
        },
        ConfigField {
            key: "role".into(),
            label: "Role".into(),
            r#type: FieldType::Text,
            default: Some(serde_json::json!(DEFAULT_ROLE)),
            options: None,
            hint: Some("Optional role label for the connecting device.".to_owned()),
            required: Some(false),
        },
        ConfigField {
            key: "deviceIdentityPath".into(),
            label: "Device identity path".into(),
            r#type: FieldType::Text,
            default: None,
            options: None,
            hint: Some("Optional path to a JSON file with {deviceId, publicKeyRawBase64Url, privateKeyPem}. When absent, an ephemeral Ed25519 identity is generated.".to_owned()),
            required: Some(false),
        },
        ConfigField {
            key: ESCAPE_HATCH_KEY.into(),
            label: "Allow insecure remote http".into(),
            r#type: FieldType::Boolean,
            default: Some(serde_json::json!(false)),
            options: None,
            hint: Some(
                "Escape hatch: permit plain ws:// or http:// to a non-loopback host. Only enable for local testing.".to_owned(),
            ),
            required: Some(false),
        },
        ConfigField {
            key: "requestTimeoutMs".into(),
            label: "Request timeout (ms)".into(),
            r#type: FieldType::Number,
            default: Some(serde_json::json!(DEFAULT_REQUEST_TIMEOUT_MS)),
            options: None,
            hint: Some("Per-RPC deadline. Defaults to 30s.".to_owned()),
            required: Some(false),
        },
        ConfigField {
            key: "connectTimeoutMs".into(),
            label: "Connect timeout (ms)".into(),
            r#type: FieldType::Number,
            default: Some(serde_json::json!(DEFAULT_CONNECT_TIMEOUT_MS)),
            options: None,
            hint: Some("WebSocket handshake deadline. Defaults to 15s.".to_owned()),
            required: Some(false),
        },
    ]
}

pub fn describe_adapter() -> AdapterDescriptor {
    AdapterDescriptor {
        adapter_type: ADAPTER_TYPE.to_owned(),
        label: ADAPTER_LABEL.to_owned(),
        schema: get_config_schema(),
    }
}

/// 简易辅助 — 列出所有必填字段。
pub fn required_field_keys() -> &'static [&'static str] {
    &["gatewayUrl", "sessionKeyStrategy"]
}

/// 解析逗号分隔的 scopes 字符串。
///
/// trim 跳过空段；保留合法非空字符串。
pub fn parse_scopes(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_has_twelve_fields() {
        let schema = get_config_schema();
        assert_eq!(schema.len(), 12);
    }

    #[test]
    fn schema_required_fields_are_listed() {
        let keys = required_field_keys();
        assert!(keys.contains(&"gatewayUrl"));
        assert!(keys.contains(&"sessionKeyStrategy"));
    }

    #[test]
    fn schema_required_marker_serializes_correctly() {
        let schema = get_config_schema();
        let json = serde_json::to_value(&schema).unwrap();
        let arr = json.as_array().unwrap();
        let url_entry = arr.iter().find(|v| v["key"] == "gatewayUrl").unwrap();
        assert_eq!(url_entry["required"], serde_json::json!(true));
        let strategy_entry = arr
            .iter()
            .find(|v| v["key"] == "sessionKeyStrategy")
            .unwrap();
        assert_eq!(strategy_entry["required"], serde_json::json!(true));
    }

    #[test]
    fn schema_optional_fields_default_false() {
        let schema = get_config_schema();
        let json = serde_json::to_value(&schema).unwrap();
        let arr = json.as_array().unwrap();
        let client_id_entry = arr.iter().find(|v| v["key"] == "clientId").unwrap();
        assert_eq!(client_id_entry["required"], serde_json::json!(false));
    }

    #[test]
    fn schema_session_key_strategy_options_match_constants() {
        let schema = get_config_schema();
        let strategy_field = schema
            .iter()
            .find(|f| f.key == "sessionKeyStrategy")
            .unwrap();
        let options = strategy_field.options.as_ref().unwrap();
        let values: Vec<&str> = options.iter().map(|o| o.value.as_str()).collect();
        assert_eq!(values, vec!["fixed", "issue", "run"]);
    }

    #[test]
    fn schema_defaults_align_with_constants() {
        let schema = get_config_schema();
        let str_cases: &[(&str, &str)] = &[
            ("sessionKey", "paperclip"),
            ("sessionKeyStrategy", DEFAULT_SESSION_KEY_STRATEGY),
            ("scopes", &DEFAULT_SCOPES.join(",")),
            ("clientId", DEFAULT_CLIENT_ID),
            ("clientMode", DEFAULT_CLIENT_MODE),
            ("clientVersion", DEFAULT_CLIENT_VERSION),
            ("role", DEFAULT_ROLE),
        ];
        for (key, expected) in str_cases.iter().copied() {
            let f = schema.iter().find(|f| f.key == key).unwrap();
            let actual = f.default.as_ref().unwrap();
            assert_eq!(actual, &serde_json::json!(expected), "{key}");
        }
        let num_cases: &[(&str, u64)] = &[
            ("requestTimeoutMs", DEFAULT_REQUEST_TIMEOUT_MS),
            ("connectTimeoutMs", DEFAULT_CONNECT_TIMEOUT_MS),
        ];
        for (key, expected) in num_cases.iter().copied() {
            let f = schema.iter().find(|f| f.key == key).unwrap();
            let actual = f.default.as_ref().unwrap();
            assert_eq!(actual, &serde_json::json!(expected), "{key}");
        }
    }

    #[test]
    fn describe_adapter_serializes_with_canonical_metadata() {
        let desc = describe_adapter();
        let json = serde_json::to_value(&desc).unwrap();
        assert_eq!(json["adapterType"], ADAPTER_TYPE);
        assert_eq!(json["label"], ADAPTER_LABEL);
        assert_eq!(json["schema"].as_array().unwrap().len(), 12);
    }

    #[test]
    fn escape_hatch_field_is_boolean() {
        let schema = get_config_schema();
        let f = schema.iter().find(|f| f.key == ESCAPE_HATCH_KEY).unwrap();
        assert_eq!(f.r#type, FieldType::Boolean);
        assert_eq!(f.default, Some(serde_json::json!(false)));
    }

    #[test]
    fn parse_scopes_trims_and_drops_empty_segments() {
        assert_eq!(parse_scopes("a,b,c"), vec!["a", "b", "c"]);
        assert_eq!(parse_scopes("a,,b,"), vec!["a", "b"]);
        assert_eq!(parse_scopes("  a  ,\tb\t,c "), vec!["a", "b", "c"]);
        assert_eq!(parse_scopes(""), Vec::<String>::new());
        assert_eq!(parse_scopes(" , , "), Vec::<String>::new());
    }

    #[test]
    fn parse_scopes_default_input_matches_constants() {
        let text = DEFAULT_SCOPES.join(",");
        let parsed = parse_scopes(&text);
        let expected: Vec<String> = DEFAULT_SCOPES.iter().map(|s| s.to_string()).collect();
        assert_eq!(parsed, expected);
    }
}
