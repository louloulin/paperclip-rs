//! Codex adapter config schema（控制台表单字段定义）。
//!
//! 严格对齐 Node `packages/adapters/codex-local/src/server/config-schema.ts`：
//!   - `getConfigSchema` 返回 `AdapterConfigSchema`
//!   - `acpVisible` 元数据：仅当 `engine == "acp"` 时显示的字段
//!
//! 字段列表（与 Node 版完全一致）：
//!   1. `engine` (select)
//!   2. `agentCommand` (text, 仅 acp)
//!   3. `mode` (select, 仅 acp)
//!   4. `nonInteractivePermissions` (select, 仅 acp)
//!   5. `stateDir` (text, 仅 acp)
//!   6. `warmHandleIdleMs` (number, 仅 acp)

use pc_acpx::constants::{
    DEFAULT_ACP_ENGINE_MODE, DEFAULT_ACP_ENGINE_NON_INTERACTIVE_PERMISSIONS,
    DEFAULT_ACP_ENGINE_WARM_HANDLE_IDLE_MS,
};
use pc_adapter_api::{AdapterConfigSchema, ConfigFieldOption, ConfigFieldSchema, ConfigFieldType};
use serde_json::json;

/// 仅当 `engine == "acp"` 时显示的字段元数据（对齐 Node `acpVisible`）。
fn acp_visible() -> serde_json::Value {
    json!({ "visibleWhen": { "key": "engine", "values": ["acp"] } })
}

/// 返回 Codex adapter 的 config schema（对齐 Node `getConfigSchema`）。
#[must_use]
pub fn get_config_schema() -> AdapterConfigSchema {
    AdapterConfigSchema::new(vec![
        ConfigFieldSchema {
            key: "engine".to_string(),
            label: "Execution engine".to_string(),
            field_type: ConfigFieldType::Select,
            options: Some(vec![
                ConfigFieldOption {
                    label: "Auto (ACP preferred)".to_string(),
                    value: "auto".to_string(),
                    group: None,
                },
                ConfigFieldOption {
                    label: "Codex CLI".to_string(),
                    value: "cli".to_string(),
                    group: None,
                },
                ConfigFieldOption {
                    label: "ACP".to_string(),
                    value: "acp".to_string(),
                    group: None,
                },
            ]),
            default: Some(json!("auto")),
            hint: Some(
                "Auto uses ACP when prerequisites pass and falls back to Codex CLI with diagnostics."
                    .to_string(),
            ),
            required: None,
            group: None,
            meta: None,
        },
        ConfigFieldSchema {
            key: "agentCommand".to_string(),
            label: "ACP server command".to_string(),
            field_type: ConfigFieldType::Text,
            options: None,
            default: None,
            hint: Some(
                "Optional override for the Codex ACP server command. Defaults to the package-local codex-acp binary."
                    .to_string(),
            ),
            required: None,
            group: None,
            meta: Some(acp_visible()),
        },
        ConfigFieldSchema {
            key: "mode".to_string(),
            label: "ACP session mode".to_string(),
            field_type: ConfigFieldType::Select,
            options: Some(vec![
                ConfigFieldOption {
                    label: "Persistent".to_string(),
                    value: "persistent".to_string(),
                    group: None,
                },
                ConfigFieldOption {
                    label: "One-shot".to_string(),
                    value: "oneshot".to_string(),
                    group: None,
                },
            ]),
            default: Some(json!(DEFAULT_ACP_ENGINE_MODE)),
            hint: Some(
                "Persistent keeps ACP session state between runs. One-shot starts fresh each run."
                    .to_string(),
            ),
            required: None,
            group: None,
            meta: Some(acp_visible()),
        },
        ConfigFieldSchema {
            key: "nonInteractivePermissions".to_string(),
            label: "ACP non-interactive permissions".to_string(),
            field_type: ConfigFieldType::Select,
            options: Some(vec![
                ConfigFieldOption {
                    label: "Deny".to_string(),
                    value: "deny".to_string(),
                    group: None,
                },
                ConfigFieldOption {
                    label: "Fail".to_string(),
                    value: "fail".to_string(),
                    group: None,
                },
            ]),
            default: Some(json!(DEFAULT_ACP_ENGINE_NON_INTERACTIVE_PERMISSIONS)),
            hint: Some(
                "Fallback if the ACP agent asks for input outside an interactive session."
                    .to_string(),
            ),
            required: None,
            group: None,
            meta: Some(acp_visible()),
        },
        ConfigFieldSchema {
            key: "stateDir".to_string(),
            label: "ACP state directory".to_string(),
            field_type: ConfigFieldType::Text,
            options: None,
            default: None,
            hint: Some(
                "Optional ACP session state directory. Defaults to Paperclip-managed company/agent scoped storage."
                    .to_string(),
            ),
            required: None,
            group: None,
            meta: Some(acp_visible()),
        },
        ConfigFieldSchema {
            key: "warmHandleIdleMs".to_string(),
            label: "ACP warm process idle ms".to_string(),
            field_type: ConfigFieldType::Number,
            options: None,
            default: Some(json!(DEFAULT_ACP_ENGINE_WARM_HANDLE_IDLE_MS)),
            hint: Some(
                "Defaults to 0, which closes the ACP process after each run while retaining persistent session state."
                    .to_string(),
            ),
            required: None,
            group: None,
            meta: Some(acp_visible()),
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use pc_adapter_api::ConfigFieldType;

    #[test]
    fn schema_has_six_fields() {
        let schema = get_config_schema();
        assert_eq!(schema.fields.len(), 6);
    }

    #[test]
    fn engine_field_has_three_options() {
        let schema = get_config_schema();
        let engine = schema
            .fields
            .iter()
            .find(|f| f.key == "engine")
            .expect("engine field present");
        assert_eq!(engine.field_type, ConfigFieldType::Select);
        let opts = engine.options.as_ref().expect("options present");
        let labels: Vec<&str> = opts.iter().map(|o| o.value.as_str()).collect();
        assert_eq!(labels, vec!["auto", "cli", "acp"]);
        assert_eq!(
            engine.default.as_ref().and_then(|v| v.as_str()),
            Some("auto")
        );
    }

    #[test]
    fn acp_only_fields_have_visibility_meta() {
        let schema = get_config_schema();
        for field in schema.fields.iter().filter(|f| f.key != "engine") {
            let meta = field.meta.as_ref().expect("acp field meta present");
            let visible_when = meta.get("visibleWhen").expect("visibleWhen present");
            assert_eq!(
                visible_when.get("key").and_then(|v| v.as_str()),
                Some("engine")
            );
            let values = visible_when
                .get("values")
                .and_then(|v| v.as_array())
                .expect("values array present");
            assert_eq!(values.len(), 1);
            assert_eq!(values[0].as_str(), Some("acp"));
        }
    }

    #[test]
    fn mode_field_persistent_default() {
        let schema = get_config_schema();
        let mode = schema
            .fields
            .iter()
            .find(|f| f.key == "mode")
            .expect("mode field present");
        assert_eq!(
            mode.default.as_ref().and_then(|v| v.as_str()),
            Some(DEFAULT_ACP_ENGINE_MODE)
        );
        assert_eq!(mode.field_type, ConfigFieldType::Select);
    }

    #[test]
    fn warm_handle_idle_default_zero() {
        let schema = get_config_schema();
        let field = schema
            .fields
            .iter()
            .find(|f| f.key == "warmHandleIdleMs")
            .expect("warmHandleIdleMs field present");
        assert_eq!(
            field.default.as_ref().and_then(|v| v.as_u64()),
            Some(DEFAULT_ACP_ENGINE_WARM_HANDLE_IDLE_MS)
        );
        assert_eq!(field.field_type, ConfigFieldType::Number);
    }

    #[test]
    fn non_interactive_permissions_default_deny() {
        let schema = get_config_schema();
        let field = schema
            .fields
            .iter()
            .find(|f| f.key == "nonInteractivePermissions")
            .expect("nonInteractivePermissions field present");
        assert_eq!(
            field.default.as_ref().and_then(|v| v.as_str()),
            Some(DEFAULT_ACP_ENGINE_NON_INTERACTIVE_PERMISSIONS)
        );
        let options = field.options.as_ref().expect("options present");
        let values: Vec<&str> = options.iter().map(|o| o.value.as_str()).collect();
        assert_eq!(values, vec!["deny", "fail"]);
    }
}
