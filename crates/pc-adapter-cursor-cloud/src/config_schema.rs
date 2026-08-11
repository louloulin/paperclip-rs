//! Cursor Cloud adapter config schema (对齐 Node
//! `packages/adapters/cursor-cloud/src/ui/build-config.ts`)。
//!
//! 字段集合（按 Paperclip UI 表单顺序）：
//! - model                 — 模型选择（可选；缺省由 SDK 决定）
//! - repoUrl               — Git 仓库 URL（必填，user/workspace 兜底）
//! - repoStartingRef       — 起始 ref（默认分支自动）
//! - repoPullRequestUrl    — PR 链接（可选，提供了就 attach）
//! - runtimeEnvType        — cloud / pool / machine
//! - runtimeEnvName        — env 名称（pool/machine 需要）
//! - workOnCurrentBranch   — bool
//! - autoCreatePR          — bool
//! - skipReviewerRequest   — bool
//! - instructionsFilePath  — agent 指令文件路径（可选）
//! - promptTemplate        — user-visible prompt 模板
//! - bootstrapPromptTemplate — 首次启动注入（跳过已有 session）
//! - envBindings / envVars — env entries

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

use crate::constants::{
    ADAPTER_LABEL, ADAPTER_TYPE, DEFAULT_AUTO_CREATE_PR, DEFAULT_RUNTIME_ENV_TYPE,
    DEFAULT_SKIP_REVIEWER_REQUEST, DEFAULT_WORK_ON_CURRENT_BRANCH, RUNTIME_ENV_TYPES,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Select,
    Number,
    Boolean,
    Text,
    LongText,
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
    pub required: Option<bool>, // false → omitted
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConfigOption {
    pub value: String,
    pub label: String,
}

/// 构造 Cursor Cloud 配置 schema。
pub fn get_config_schema() -> Vec<ConfigField> {
    vec![
        ConfigField {
            key: "model".into(),
            label: "Model".into(),
            r#type: FieldType::Text,
            default: None,
            options: None,
            hint: Some(
                "Optional Cursor model id (e.g. \"gpt-4\", \"claude-3.5-sonnet\"). Leave blank to let the SDK choose.".to_owned(),
            ),
            required: Some(false),
        },
        ConfigField {
            key: "repoUrl".into(),
            label: "Repository URL".into(),
            r#type: FieldType::Text,
            default: None,
            options: None,
            hint: Some(
                "Git URL the Cloud agent will operate on. Falls back to context.paperclipWorkspace.repoUrl.".to_owned(),
            ),
            required: Some(true),
        },
        ConfigField {
            key: "repoStartingRef".into(),
            label: "Starting ref".into(),
            r#type: FieldType::Text,
            default: None,
            options: None,
            hint: Some("Branch / tag / SHA to start from (default branch auto when omitted).".to_owned()),
            required: Some(false),
        },
        ConfigField {
            key: "repoPullRequestUrl".into(),
            label: "Existing PR URL".into(),
            r#type: FieldType::Text,
            default: None,
            options: None,
            hint: Some("If you already have a PR, paste its URL here to attach the agent.".to_owned()),
            required: Some(false),
        },
        ConfigField {
            key: "runtimeEnvType".into(),
            label: "Runtime environment type".into(),
            r#type: FieldType::Select,
            default: Some(serde_json::json!(DEFAULT_RUNTIME_ENV_TYPE)),
            options: Some(
                RUNTIME_ENV_TYPES
                    .iter()
                    .map(|v| ConfigOption {
                        value: (*v).to_owned(),
                        label: match *v {
                            "cloud" => "Managed Cloud".to_owned(),
                            "pool" => "Dedicated Pool".to_owned(),
                            "machine" => "Self-hosted Machine".to_owned(),
                            _ => (*v).to_owned(),
                        },
                    })
                    .collect(),
            ),
            hint: Some("Where the agent runs. Pool / Machine need a name below.".to_owned()),
            required: Some(true),
        },
        ConfigField {
            key: "runtimeEnvName".into(),
            label: "Runtime environment name".into(),
            r#type: FieldType::Text,
            default: None,
            options: None,
            hint: Some("Only required when runtimeEnvType = pool or machine.".to_owned()),
            required: Some(false),
        },
        ConfigField {
            key: "workOnCurrentBranch".into(),
            label: "Work on current branch".into(),
            r#type: FieldType::Boolean,
            default: Some(serde_json::json!(DEFAULT_WORK_ON_CURRENT_BRANCH)),
            options: None,
            hint: Some("Let the agent push directly to the current branch instead of a new one.".to_owned()),
            required: Some(false),
        },
        ConfigField {
            key: "autoCreatePR".into(),
            label: "Auto-create PR".into(),
            r#type: FieldType::Boolean,
            default: Some(serde_json::json!(DEFAULT_AUTO_CREATE_PR)),
            options: None,
            hint: Some("When set, the agent opens a PR as soon as it pushes a branch.".to_owned()),
            required: Some(false),
        },
        ConfigField {
            key: "skipReviewerRequest".into(),
            label: "Skip reviewer request".into(),
            r#type: FieldType::Boolean,
            default: Some(serde_json::json!(DEFAULT_SKIP_REVIEWER_REQUEST)),
            options: None,
            hint: Some("Suppress the auto-generated reviewer request on agent-created PRs.".to_owned()),
            required: Some(false),
        },
        ConfigField {
            key: "instructionsFilePath".into(),
            label: "Agent instructions file".into(),
            r#type: FieldType::Text,
            default: None,
            options: None,
            hint: Some("Optional path to a file whose contents are prepended to every prompt.".to_owned()),
            required: Some(false),
        },
        ConfigField {
            key: "promptTemplate".into(),
            label: "Prompt template".into(),
            r#type: FieldType::LongText,
            default: None,
            options: None,
            hint: Some("Template variables: {{ agentId }}, {{ companyId }}, {{ runId }}, {{ agent.* }}.".to_owned()),
            required: Some(false),
        },
        ConfigField {
            key: "bootstrapPromptTemplate".into(),
            label: "Bootstrap prompt (first run)".into(),
            r#type: FieldType::LongText,
            default: None,
            options: None,
            hint: Some("Injected only when a fresh cloud agent session is created.".to_owned()),
            required: Some(false),
        },
        ConfigField {
            key: "envBindings".into(),
            label: "Environment variables (structured)".into(),
            r#type: FieldType::LongText,
            default: None,
            options: None,
            hint: Some(
                "JSON map: \"KEY\": { \"type\": \"plain\", \"value\": \"...\" } or secret_ref entries. \
                 PAPERCLIP_API_KEY / CURSOR_API_KEY are injected from bindings, never edited here."
                    .to_owned(),
            ),
            required: Some(false),
        },
        ConfigField {
            key: "envVars".into(),
            label: "Environment variables (legacy text)".into(),
            r#type: FieldType::LongText,
            default: None,
            options: None,
            hint: Some("KEY=VALUE lines, parsed only if key is not already in envBindings.".to_owned()),
            required: Some(false),
        },
    ]
}

/// Adapter metadata（Paperclip UI 用）。
pub fn describe_adapter() -> AdapterDescriptor {
    AdapterDescriptor {
        adapter_type: ADAPTER_TYPE.to_owned(),
        label: ADAPTER_LABEL.to_owned(),
        schema: get_config_schema(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AdapterDescriptor {
    #[serde(rename = "adapterType")]
    pub adapter_type: String,
    pub label: String,
    pub schema: Vec<ConfigField>,
}

/// Helper — 表单字段 key 集合（供 execute 决策使用）。
pub fn required_field_keys() -> &'static [&'static str] {
    &["repoUrl", "runtimeEnvType"]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_has_fourteen_fields() {
        let schema = get_config_schema();
        assert_eq!(schema.len(), 14);
    }

    #[test]
    fn schema_required_fields_marked() {
        let schema = get_config_schema();
        let mut required_keys = Vec::new();
        for field in &schema {
            if field.required == Some(true) {
                required_keys.push(field.key.clone());
            }
        }
        for required in ["repoUrl", "runtimeEnvType"] {
            assert!(
                required_keys.iter().any(|k| k == required),
                "missing required field: {required}"
            );
        }
    }

    #[test]
    fn runtime_env_type_options_align_with_constants() {
        let schema = get_config_schema();
        let env_field = schema.iter().find(|f| f.key == "runtimeEnvType").unwrap();
        let options = env_field.options.as_ref().expect("options");
        let values: Vec<&str> = options.iter().map(|o| o.value.as_str()).collect();
        assert_eq!(values, RUNTIME_ENV_TYPES);
    }

    #[test]
    fn boolean_defaults_align_with_constants() {
        let schema = get_config_schema();
        for (key, expected) in [
            ("workOnCurrentBranch", DEFAULT_WORK_ON_CURRENT_BRANCH),
            ("autoCreatePR", DEFAULT_AUTO_CREATE_PR),
            ("skipReviewerRequest", DEFAULT_SKIP_REVIEWER_REQUEST),
        ] {
            let f = schema.iter().find(|f| f.key == key).unwrap();
            assert_eq!(f.default, Some(serde_json::json!(expected)), "{key}");
            assert_eq!(f.r#type, FieldType::Boolean);
        }
    }

    #[test]
    fn runtime_env_type_default_is_cloud() {
        let schema = get_config_schema();
        let f = schema.iter().find(|f| f.key == "runtimeEnvType").unwrap();
        assert_eq!(f.default, Some(serde_json::json!(DEFAULT_RUNTIME_ENV_TYPE)));
    }

    #[test]
    fn describe_adapter_serializes_expected_keys() {
        let desc = describe_adapter();
        let json = serde_json::to_value(&desc).unwrap();
        assert_eq!(json["adapterType"], ADAPTER_TYPE);
        assert_eq!(json["label"], ADAPTER_LABEL);
        assert!(json["schema"].is_array());
        assert_eq!(json["schema"].as_array().unwrap().len(), 14);
    }

    #[test]
    fn schema_serializes_to_json_without_required_field_keys_when_falsy() {
        let schema = get_config_schema();
        let json = serde_json::to_value(&schema).unwrap();
        let arr = json.as_array().unwrap();
        // The first field `model` is not required → serialized as false (None skip → false serialized)
        let model_entry = arr.iter().find(|v| v["key"] == "model").unwrap();
        assert_eq!(model_entry["required"], serde_json::json!(false));
    }

    #[test]
    fn schema_serializes_required_field_with_true() {
        let schema = get_config_schema();
        let json = serde_json::to_value(&schema).unwrap();
        let arr = json.as_array().unwrap();
        let repo_entry = arr.iter().find(|v| v["key"] == "repoUrl").unwrap();
        assert_eq!(repo_entry["required"], serde_json::json!(true));
    }

    #[test]
    fn required_field_keys_helper_lists_mandatory_keys() {
        let keys = required_field_keys();
        assert!(keys.contains(&"repoUrl"));
        assert!(keys.contains(&"runtimeEnvType"));
    }
}
