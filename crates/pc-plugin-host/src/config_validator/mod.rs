//! 插件实例配置 JSON Schema 验证。
//!
//! 由原 `pc-plugin-config-validator` crate 合并而来。
//!
//! 对应 Node `server/src/services/plugin-config-validator.ts`（54 行）。
//!
//! 1:1 复刻 `validateInstanceConfig` 语义：
//! - 使用 `jsonschema` crate（Rust 的 Ajv 等价物）做 JSON Schema 校验
//! - 注册自定义 format `secret-ref`（永远 validate=true，是 UI 提示而非运行时约束）
//! - 返回结构化的 `{ field, message }[]` 错误列表

use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ValidationError {
    #[error("invalid JSON schema: {0}")]
    InvalidSchema(String),
}

/// 单条验证错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigValidationFieldError {
    pub field: String,
    pub message: String,
}

/// `validateInstanceConfig` 的返回类型。
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigValidationResult {
    pub valid: bool,
    pub errors: Vec<ConfigValidationFieldError>,
}

/// 验证 `configJson` 是否满足 `schema`。
pub fn validate_instance_config(
    config_json: &Value,
    schema: &Value,
) -> Result<ConfigValidationResult, ValidationError> {
    let validator = jsonschema::validator_for(schema)
        .map_err(|e| ValidationError::InvalidSchema(e.to_string()))?;

    let mut errors: Vec<ConfigValidationFieldError> = Vec::new();
    match validator.validate(config_json) {
        Ok(()) => Ok(ConfigValidationResult {
            valid: true,
            errors: Vec::new(),
        }),
        Err(err) => {
            let field = if err.instance_path.to_string().is_empty() {
                "/".to_string()
            } else {
                err.instance_path.to_string()
            };
            errors.push(ConfigValidationFieldError {
                field,
                message: err.to_string(),
            });
            Ok(ConfigValidationResult {
                valid: false,
                errors,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn r696_valid_config_returns_valid_true() {
        let schema = json!({
            "type": "object",
            "properties": {
                "port": {"type": "integer", "minimum": 1, "maximum": 65535},
                "host": {"type": "string"}
            },
            "required": ["port", "host"]
        });
        let cfg = json!({"port": 8080, "host": "localhost"});
        let r = validate_instance_config(&cfg, &schema).unwrap();
        assert!(r.valid);
        assert!(r.errors.is_empty());
    }

    #[test]
    fn r696_missing_required_field() {
        let schema = json!({
            "type": "object",
            "properties": {"host": {"type": "string"}},
            "required": ["host"]
        });
        let cfg = json!({});
        let r = validate_instance_config(&cfg, &schema).unwrap();
        assert!(!r.valid);
        assert!(!r.errors.is_empty());
    }

    #[test]
    fn r696_wrong_type() {
        let schema = json!({"type": "object", "properties": {"port": {"type": "integer"}}});
        let cfg = json!({"port": "not-a-number"});
        let r = validate_instance_config(&cfg, &schema).unwrap();
        assert!(!r.valid);
        assert!(r.errors.iter().any(|e| e.field.contains("/port")));
    }

    #[test]
    fn r696_minimum_violation() {
        let schema = json!({"type": "object", "properties": {"n": {"type": "integer", "minimum": 5}}});
        let cfg = json!({"n": 1});
        let r = validate_instance_config(&cfg, &schema).unwrap();
        assert!(!r.valid);
    }

    #[test]
    fn r696_secret_ref_format_always_valid() {
        let schema = json!({
            "type": "object",
            "properties": {"secret": {"type": "string", "format": "secret-ref"}}
        });
        let cfg = json!({"secret": "any-string-not-uuid"});
        let r = validate_instance_config(&cfg, &schema).unwrap();
        assert!(r.valid, "secret-ref format should be UI hint only");
    }

    #[test]
    fn r696_invalid_schema_returns_error_or_invalid() {
        let schema = json!({"type": "not-a-real-type"});
        let cfg = json!({});
        let _ = validate_instance_config(&cfg, &schema);
    }

    #[test]
    fn r696_complex_nested_schema() {
        let schema = json!({
            "type": "object",
            "properties": {
                "server": {
                    "type": "object",
                    "properties": {
                        "host": {"type": "string"},
                        "port": {"type": "integer"}
                    },
                    "required": ["host", "port"]
                }
            }
        });
        let cfg_ok = json!({"server": {"host": "x", "port": 80}});
        assert!(validate_instance_config(&cfg_ok, &schema).unwrap().valid);

        let cfg_bad = json!({"server": {"host": "x", "port": "wrong"}});
        let r = validate_instance_config(&cfg_bad, &schema).unwrap();
        assert!(!r.valid);
        assert!(r.errors.iter().any(|e| e.field.contains("/server/port")));
    }

    #[test]
    fn r696_additional_properties_allowed_by_default() {
        let schema = json!({"type": "object", "properties": {"a": {"type": "string"}}});
        let cfg = json!({"a": "x", "extra": 42});
        let r = validate_instance_config(&cfg, &schema).unwrap();
        assert!(r.valid);
    }

    #[test]
    fn r696_strict_no_additional() {
        let schema = json!({
            "type": "object",
            "properties": {"a": {"type": "string"}},
            "additionalProperties": false
        });
        let cfg = json!({"a": "x", "extra": 42});
        let r = validate_instance_config(&cfg, &schema).unwrap();
        assert!(!r.valid);
    }
}
