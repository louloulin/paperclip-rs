//! Plugin instance config JSON Schema validation (1:1 port of Node
//! `server/src/services/plugin-config-validator.ts`，54 行).
//!
//! 单一职责：用 manifest 声明的 JSON Schema 校验插件 instance configJson。
//!
//! 设计：
//! - 复用 Node 端 `Ajv + ajv-formats` 的语义
//! - 注册自定义 `secret-ref` 格式（恒真，作为 UI hint；UUID 校验在 secrets handler 中做）
//! - 默认 Draft 7（与 Node Ajv 默认对齐）
//! - 错误信息结构化：`{ field, message }`，`field` 为 instance path（根路径用 `/`）

use serde::{Deserialize, Serialize};
use serde_json::Value;

use jsonschema::Draft;

/// 单条验证错误（与 Node `ConfigValidationResult.errors[]` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigValidationError {
    /// JSON pointer path（如 `/foo/bar`）；根路径用 `/` 兜底
    pub field: String,
    /// Ajv 风格错误信息（"is required" / "must be string" 等）
    pub message: String,
}

/// 验证结果（与 Node `ConfigValidationResult` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigValidationResult {
    pub valid: bool,
    /// 校验失败时的结构化错误；通过时为 `None`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errors: Option<Vec<ConfigValidationError>>,
}

impl ConfigValidationResult {
    pub fn ok() -> Self {
        Self { valid: true, errors: None }
    }

    pub fn invalid(errors: Vec<ConfigValidationError>) -> Self {
        Self { valid: false, errors: Some(errors) }
    }
}

/// 用 JSON Schema 校验插件 instance config（与 Node `validateInstanceConfig` 1:1 对齐）。
///
/// - `config_json`：待校验的配置值（任意 JSON）
/// - `schema`：manifest 中的 `instanceConfigSchema` JSON Schema
/// - 返回 `ConfigValidationResult`：通过 → `{ valid: true }`；失败 → `{ valid: false, errors: [...] }`
///
/// 行为细节：
/// - 编译失败（schema 本身非法）→ 返回带错误信息的结果，不抛异常（与 Node Ajv `compile` 失败语义对齐）
/// - 自定义格式 `secret-ref` 恒真（UI hint，不做 UUID 校验）
/// - 默认 Draft 7
pub fn validate_instance_config(config_json: &Value, schema: &Value) -> ConfigValidationResult {
    // 编译 schema
    let compiled = match jsonschema::options()
        .with_draft(Draft::Draft7)
        .with_format("secret-ref", |_| true)
        .build(schema)
    {
        Ok(s) => s,
        Err(err) => {
            return ConfigValidationResult::invalid(vec![ConfigValidationError {
                field: "/".to_string(),
                message: format!("invalid JSON Schema: {err}"),
            }]);
        }
    };

    // 校验
    let validation = compiled.validate(config_json);
    if validation.is_ok() {
        return ConfigValidationResult::ok();
    }

    let errors: Vec<ConfigValidationError> = compiled
        .iter_errors(config_json)
        .map(|err| {
            let field = err.instance_path.to_string();
            let field = if field.is_empty() { "/".to_string() } else { field };
            let message = err.to_string();
            // jsonschema 0.30 ValidationError Display 是完整 sentence；
            // Node 端 `err.message` 短一些；这里保留完整 message 以便上层识别
            ConfigValidationError { field, message }
        })
        .collect();

    if errors.is_empty() {
        // validate 返回 Err 但 iter_errors 为空（边界）：fallback
        ConfigValidationResult::invalid(vec![ConfigValidationError {
            field: "/".to_string(),
            message: "validation failed".to_string(),
        }])
    } else {
        ConfigValidationResult::invalid(errors)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn valid_config_returns_ok() {
        let schema = json!({
            "type": "object",
            "properties": {
                "apiKey": { "type": "string" },
                "endpoint": { "type": "string" },
            },
            "required": ["apiKey"],
            "additionalProperties": false,
        });
        let config = json!({
            "apiKey": "sk-xxx",
            "endpoint": "https://example.com",
        });
        let r = validate_instance_config(&config, &schema);
        assert!(r.valid);
        assert!(r.errors.is_none());
    }

    #[test]
    fn missing_required_field_returns_error() {
        let schema = json!({
            "type": "object",
            "properties": { "apiKey": { "type": "string" } },
            "required": ["apiKey"],
        });
        let config = json!({});
        let r = validate_instance_config(&config, &schema);
        assert!(!r.valid);
        let errs = r.errors.expect("errors");
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].field, "/");
        assert!(errs[0].message.contains("required"));
    }

    #[test]
    fn wrong_type_returns_error() {
        let schema = json!({
            "type": "object",
            "properties": { "count": { "type": "integer" } },
            "required": ["count"],
        });
        let config = json!({ "count": "not-a-number" });
        let r = validate_instance_config(&config, &schema);
        assert!(!r.valid);
        let errs = r.errors.expect("errors");
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].field, "/count");
    }

    #[test]
    fn nested_field_path_is_reported() {
        let schema = json!({
            "type": "object",
            "properties": {
                "database": {
                    "type": "object",
                    "properties": {
                        "port": { "type": "integer" }
                    },
                    "required": ["port"],
                }
            },
            "required": ["database"],
        });
        let config = json!({ "database": { "port": "nope" } });
        let r = validate_instance_config(&config, &schema);
        assert!(!r.valid);
        let errs = r.errors.expect("errors");
        assert_eq!(errs[0].field, "/database/port");
    }

    #[test]
    fn additional_properties_violation_is_reported() {
        let schema = json!({
            "type": "object",
            "properties": { "a": { "type": "string" } },
            "additionalProperties": false,
        });
        let config = json!({ "a": "x", "b": "y" });
        let r = validate_instance_config(&config, &schema);
        assert!(!r.valid);
        let errs = r.errors.expect("errors");
        assert!(!errs.is_empty());
    }

    #[test]
    fn secret_ref_format_is_permissive() {
        // secret-ref 永远返回 true（不校验是否为合法 UUID）
        let schema = json!({
            "type": "object",
            "properties": {
                "secretId": { "type": "string", "format": "secret-ref" }
            },
            "required": ["secretId"],
        });
        let config = json!({ "secretId": "definitely-not-a-uuid" });
        let r = validate_instance_config(&config, &schema);
        assert!(r.valid, "secret-ref should be permissive: {:?}", r.errors);
    }

    #[test]
    fn empty_schema_accepts_anything() {
        let schema = json!({});
        let config = json!({ "anything": [1, 2, 3], "goes": true });
        let r = validate_instance_config(&config, &schema);
        assert!(r.valid);
    }

    #[test]
    fn array_validation_reports_index() {
        let schema = json!({
            "type": "array",
            "items": { "type": "integer" },
        });
        let config = json!([1, "two", 3]);
        let r = validate_instance_config(&config, &schema);
        assert!(!r.valid);
        let errs = r.errors.expect("errors");
        assert_eq!(errs[0].field, "/1");
    }

    #[test]
    fn malformed_schema_returns_invalid_result() {
        // type 写成非法值，Draft 7 应拒绝编译
        let schema = json!({ "type": "not-a-valid-type" });
        let config = json!({});
        let r = validate_instance_config(&config, &schema);
        assert!(!r.valid);
        let errs = r.errors.expect("errors");
        assert!(errs[0].message.contains("invalid JSON Schema"));
    }

    #[test]
    fn multiple_errors_are_collected() {
        let schema = json!({
            "type": "object",
            "properties": {
                "a": { "type": "integer" },
                "b": { "type": "integer" },
            },
            "required": ["a", "b"],
        });
        let config = json!({ "a": "x", "b": "y" });
        let r = validate_instance_config(&config, &schema);
        assert!(!r.valid);
        let errs = r.errors.expect("errors");
        assert!(errs.len() >= 2, "expected ≥2 errors, got {errs:?}");
    }

    #[test]
    fn enum_violation_returns_error() {
        let schema = json!({
            "type": "string",
            "enum": ["red", "green", "blue"],
        });
        let config = json!("yellow");
        let r = validate_instance_config(&config, &schema);
        assert!(!r.valid);
        let errs = r.errors.expect("errors");
        assert!(!errs.is_empty());
    }

    #[test]
    fn ok_helper_returns_valid_no_errors() {
        let r = ConfigValidationResult::ok();
        assert!(r.valid);
        assert!(r.errors.is_none());
    }

    #[test]
    fn invalid_helper_returns_valid_false_with_errors() {
        let r = ConfigValidationResult::invalid(vec![ConfigValidationError {
            field: "/x".into(),
            message: "bad".into(),
        }]);
        assert!(!r.valid);
        assert_eq!(r.errors.expect("errors").len(), 1);
    }

    #[test]
    fn serialization_round_trip() {
        let r = ConfigValidationResult::invalid(vec![ConfigValidationError {
            field: "/x".into(),
            message: "bad".into(),
        }]);
        let s = serde_json::to_string(&r).unwrap();
        let parsed: ConfigValidationResult = serde_json::from_str(&s).unwrap();
        assert_eq!(r, parsed);

        let ok = ConfigValidationResult::ok();
        let s2 = serde_json::to_string(&ok).unwrap();
        assert!(!s2.contains("errors"), "ok result must skip errors: {s2}");
    }
}
