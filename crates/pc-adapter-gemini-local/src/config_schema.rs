//! Gemini adapter 配置 schema（对齐 Node
//! `packages/adapters/gemini-local/src/server/config-schema.ts`）。
//!
//! 6 字段：engine / agentCommand / mode / nonInteractivePermissions /
//! stateDir / warmHandleIdleMs。所有 ACP 相关字段标 `acpVisible` meta
//! （UI 仅在 `engine ∈ {acp, auto}` 时显示）。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Select,
    Number,
    Text,
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
    pub meta: Option<FieldMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConfigOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FieldMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible_when: Option<VisibleWhen>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisibleWhen {
    pub key: String,
    pub values: Vec<String>,
}

/// 默认值（对齐 Node `DEFAULT_ACP_ENGINE_*`）。
pub const DEFAULT_ACP_ENGINE_MODE: &str = "persistent";
pub const DEFAULT_ACP_ENGINE_NON_INTERACTIVE_PERMISSIONS: &str = "deny";
pub const DEFAULT_ACP_ENGINE_WARM_HANDLE_IDLE_MS: u64 = 0;

/// UI 仅在 `engine ∈ {acp, auto}` 时显示的 meta 标记。
pub fn acp_visible() -> FieldMeta {
    FieldMeta {
        visible_when: Some(VisibleWhen {
            key: "engine".to_string(),
            values: vec!["acp".to_string(), "auto".to_string()],
        }),
    }
}

/// 构造 Gemini adapter 6 字段 schema。
pub fn get_config_schema() -> Vec<ConfigField> {
    vec![
        ConfigField {
            key: "engine".to_string(),
            label: "Execution engine".to_string(),
            r#type: FieldType::Select,
            default: Some(serde_json::json!("auto")),
            options: Some(vec![
                ConfigOption { value: "auto".to_string(), label: "Auto (ACP preferred)".to_string() },
                ConfigOption { value: "cli".to_string(), label: "Gemini CLI".to_string() },
                ConfigOption { value: "acp".to_string(), label: "ACP".to_string() },
            ]),
            hint: Some(
                "Auto uses ACP when prerequisites pass and falls back to Gemini CLI with diagnostics.".to_string(),
            ),
            meta: None,
        },
        ConfigField {
            key: "agentCommand".to_string(),
            label: "ACP server command".to_string(),
            r#type: FieldType::Text,
            default: None,
            options: None,
            hint: Some(
                "Optional override for the Gemini ACP server command. Defaults to gemini --acp.".to_string(),
            ),
            meta: Some(acp_visible()),
        },
        ConfigField {
            key: "mode".to_string(),
            label: "ACP session mode".to_string(),
            r#type: FieldType::Select,
            default: Some(serde_json::json!(DEFAULT_ACP_ENGINE_MODE)),
            options: Some(vec![
                ConfigOption { value: "persistent".to_string(), label: "Persistent".to_string() },
                ConfigOption { value: "oneshot".to_string(), label: "One-shot".to_string() },
            ]),
            hint: Some(
                "Persistent keeps ACP session state between runs. One-shot starts fresh each run.".to_string(),
            ),
            meta: Some(acp_visible()),
        },
        ConfigField {
            key: "nonInteractivePermissions".to_string(),
            label: "ACP non-interactive permissions".to_string(),
            r#type: FieldType::Select,
            default: Some(serde_json::json!(DEFAULT_ACP_ENGINE_NON_INTERACTIVE_PERMISSIONS)),
            options: Some(vec![
                ConfigOption { value: "deny".to_string(), label: "Deny".to_string() },
                ConfigOption { value: "fail".to_string(), label: "Fail".to_string() },
            ]),
            hint: Some(
                "Fallback if the ACP agent asks for input outside an interactive session.".to_string(),
            ),
            meta: Some(acp_visible()),
        },
        ConfigField {
            key: "stateDir".to_string(),
            label: "ACP state directory".to_string(),
            r#type: FieldType::Text,
            default: None,
            options: None,
            hint: Some(
                "Optional ACP session state directory. Defaults to Paperclip-managed company/agent scoped storage.".to_string(),
            ),
            meta: Some(acp_visible()),
        },
        ConfigField {
            key: "warmHandleIdleMs".to_string(),
            label: "ACP warm process idle ms".to_string(),
            r#type: FieldType::Number,
            default: Some(serde_json::json!(DEFAULT_ACP_ENGINE_WARM_HANDLE_IDLE_MS)),
            options: None,
            hint: Some(
                "Defaults to 0, which closes the ACP process after each run while retaining persistent session state.".to_string(),
            ),
            meta: Some(acp_visible()),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_has_six_fields() {
        let schema = get_config_schema();
        assert_eq!(schema.len(), 6);
    }

    #[test]
    fn engine_field_has_three_options() {
        let schema = get_config_schema();
        let engine = schema.iter().find(|f| f.key == "engine").expect("engine");
        let options = engine.options.as_ref().expect("options");
        let values: Vec<&str> = options.iter().map(|o| o.value.as_str()).collect();
        for required in ["auto", "cli", "acp"] {
            assert!(
                values.contains(&required),
                "missing engine option: {required}"
            );
        }
        assert_eq!(engine.meta, None);
    }

    #[test]
    fn acp_fields_carry_visible_when_meta() {
        let schema = get_config_schema();
        for field in schema.iter().filter(|f| f.key != "engine") {
            let meta = field
                .meta
                .as_ref()
                .expect(&format!("{} should have meta", field.key));
            let visible_when = meta.visible_when.as_ref().expect("visible_when");
            assert_eq!(visible_when.key, "engine");
            assert!(visible_when.values.contains(&"acp".to_string()));
            assert!(visible_when.values.contains(&"auto".to_string()));
        }
    }

    #[test]
    fn defaults_align_with_constants() {
        let schema = get_config_schema();
        let mode = schema.iter().find(|f| f.key == "mode").unwrap();
        assert_eq!(
            mode.default,
            Some(serde_json::json!(DEFAULT_ACP_ENGINE_MODE))
        );
        let perms = schema
            .iter()
            .find(|f| f.key == "nonInteractivePermissions")
            .unwrap();
        assert_eq!(
            perms.default,
            Some(serde_json::json!(
                DEFAULT_ACP_ENGINE_NON_INTERACTIVE_PERMISSIONS
            ))
        );
        let idle = schema.iter().find(|f| f.key == "warmHandleIdleMs").unwrap();
        assert_eq!(
            idle.default,
            Some(serde_json::json!(DEFAULT_ACP_ENGINE_WARM_HANDLE_IDLE_MS))
        );
    }

    #[test]
    fn schema_serializes_to_json() {
        let schema = get_config_schema();
        let json = serde_json::to_value(&schema).expect("serialize");
        let arr = json.as_array().expect("array");
        assert_eq!(arr.len(), 6);
        // All fields have required keys
        for field in arr {
            assert!(field.get("key").is_some());
            assert!(field.get("label").is_some());
            assert!(field.get("type").is_some());
        }
    }
}
