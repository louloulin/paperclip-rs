//! 插件 manifest 解析 / 验证。
//!
//! 由原 `pc-plugin-manifest-validator` crate 合并而来。
//!
//! 对应 Node `server/src/services/plugin-manifest-validator.ts`（163 行）。
//!
//! 1:1 复刻：
//! - `ManifestParseResult` discriminated union（success / failure）
//! - `parse(input)` —— 永不抛
//! - `parseOrThrow(input)` —— 失败时返回 ManifestValidationException
//! - `getSupportedVersions()` —— 返回支持的 plugin API versions
//!
//! 实际 manifest schema 在 `@paperclipai/shared`；本模块通过 `ManifestSchema` trait
//! 让上层注入（生产环境接入 shared，测试中用 mock）。

use serde::{Deserialize, Serialize};

/// Plugin manifest API version 常量 —— 与 Node `PLUGIN_API_VERSION` 1:1 对齐。
pub const PLUGIN_API_VERSION: u32 = 1;

/// 支持的 manifest API version 集合 —— 与 Node `SUPPORTED_VERSIONS` 1:1 对齐。
pub const SUPPORTED_VERSIONS: &[u32] = &[PLUGIN_API_VERSION];

/// Manifest schema 抽象。
pub trait ManifestSchema: Send + Sync {
    type Manifest;

    fn validate(&self, input: &serde_json::Value) -> Result<Self::Manifest, Vec<ManifestError>>;
}

/// 单条 manifest 验证错误。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestError {
    pub path: Vec<serde_json::Value>,
    pub message: String,
}

/// 验证结果 —— 与 Node `ManifestParseSuccess` / `ManifestParseFailure` 1:1 对齐。
#[derive(Debug, Clone)]
pub enum ManifestParseResult<M> {
    Success {
        manifest: M,
    },
    Failure {
        errors: String,
        details: Vec<ManifestError>,
    },
}

impl<M> ManifestParseResult<M> {
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success { .. })
    }
    pub fn is_failure(&self) -> bool {
        !self.is_success()
    }
}

/// Manifest 验证异常（用于 `parseOrThrow`）。
#[derive(Debug, Clone)]
pub struct ManifestValidationException {
    pub message: String,
    pub details: Vec<ManifestError>,
}

impl std::fmt::Display for ManifestValidationException {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ManifestValidationException {}

/// Validator 接口 —— 1:1 对应 Node `PluginManifestValidator`。
pub trait PluginManifestValidator<M>: Send + Sync {
    fn parse(&self, input: &serde_json::Value) -> ManifestParseResult<M>;
    fn parse_or_throw(&self, input: &serde_json::Value) -> Result<M, ManifestValidationException>;
    fn get_supported_versions(&self) -> &[u32];
}

/// 默认实现：基于 `ManifestSchema` trait + `SUPPORTED_VERSIONS`。
pub struct DefaultPluginManifestValidator<S: ManifestSchema> {
    schema: S,
}

impl<S: ManifestSchema> DefaultPluginManifestValidator<S> {
    pub fn new(schema: S) -> Self {
        Self { schema }
    }
}

impl<S: ManifestSchema> PluginManifestValidator<S::Manifest> for DefaultPluginManifestValidator<S> {
    fn parse(&self, input: &serde_json::Value) -> ManifestParseResult<S::Manifest> {
        match self.schema.validate(input) {
            Ok(manifest) => ManifestParseResult::Success { manifest },
            Err(details) => {
                let errors = details
                    .iter()
                    .map(|d| {
                        let path_str: Vec<String> = d
                            .path
                            .iter()
                            .map(|v| match v {
                                serde_json::Value::String(s) => s.clone(),
                                other => other.to_string(),
                            })
                            .collect();
                        if path_str.is_empty() {
                            d.message.clone()
                        } else {
                            format!("{}: {}", path_str.join("."), d.message)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                ManifestParseResult::Failure { errors, details }
            }
        }
    }

    fn parse_or_throw(
        &self,
        input: &serde_json::Value,
    ) -> Result<S::Manifest, ManifestValidationException> {
        match self.parse(input) {
            ManifestParseResult::Success { manifest } => Ok(manifest),
            ManifestParseResult::Failure { errors, details } => Err(ManifestValidationException {
                message: format!("Invalid plugin manifest: {}", errors),
                details,
            }),
        }
    }

    fn get_supported_versions(&self) -> &[u32] {
        SUPPORTED_VERSIONS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[derive(Default)]
    struct MockSchema {
        pub errors: Arc<std::sync::Mutex<Vec<ManifestError>>>,
    }

    impl ManifestSchema for MockSchema {
        type Manifest = serde_json::Value;

        fn validate(
            &self,
            _input: &serde_json::Value,
        ) -> Result<Self::Manifest, Vec<ManifestError>> {
            let errs = self.errors.lock().unwrap().clone();
            if errs.is_empty() {
                Ok(serde_json::json!({"name": "test-plugin", "apiVersion": 1}))
            } else {
                Err(errs)
            }
        }
    }

    #[test]
    fn r708_parse_success() {
        let v = DefaultPluginManifestValidator::new(MockSchema::default());
        let r = v.parse(&serde_json::json!({}));
        assert!(r.is_success());
    }

    #[test]
    fn r708_parse_failure() {
        let schema = MockSchema::default();
        *schema.errors.lock().unwrap() = vec![
            ManifestError {
                path: vec![serde_json::json!("name")],
                message: "required".to_string(),
            },
            ManifestError {
                path: vec![serde_json::json!("apiVersion")],
                message: "must be 1".to_string(),
            },
        ];
        let v = DefaultPluginManifestValidator::new(schema);
        let r = v.parse(&serde_json::json!({}));
        assert!(r.is_failure());
        match r {
            ManifestParseResult::Failure { errors, details } => {
                assert!(errors.contains("name: required"));
                assert!(errors.contains("apiVersion: must be 1"));
                assert_eq!(details.len(), 2);
            }
            _ => panic!("expected Failure"),
        }
    }

    #[test]
    fn r708_parse_failure_no_path() {
        let schema = MockSchema::default();
        *schema.errors.lock().unwrap() = vec![ManifestError {
            path: vec![],
            message: "general error".to_string(),
        }];
        let v = DefaultPluginManifestValidator::new(schema);
        let r = v.parse(&serde_json::json!({}));
        match r {
            ManifestParseResult::Failure { errors, .. } => {
                assert!(errors.contains("general error"));
                assert!(!errors.starts_with(": "));
            }
            _ => panic!("expected Failure"),
        }
    }

    #[test]
    fn r708_parse_or_throw_success() {
        let v = DefaultPluginManifestValidator::new(MockSchema::default());
        let r = v.parse_or_throw(&serde_json::json!({}));
        assert!(r.is_ok());
    }

    #[test]
    fn r708_parse_or_throw_failure() {
        let schema = MockSchema::default();
        *schema.errors.lock().unwrap() = vec![ManifestError {
            path: vec![serde_json::json!("x")],
            message: "bad".to_string(),
        }];
        let v = DefaultPluginManifestValidator::new(schema);
        let err = v.parse_or_throw(&serde_json::json!({})).unwrap_err();
        assert!(err.message.contains("Invalid plugin manifest"));
        assert!(err.message.contains("x: bad"));
        assert_eq!(err.details.len(), 1);
    }

    #[test]
    fn r708_supported_versions() {
        let v = DefaultPluginManifestValidator::new(MockSchema::default());
        assert_eq!(v.get_supported_versions(), &[1]);
    }

    #[test]
    fn r708_api_version_constant() {
        assert_eq!(PLUGIN_API_VERSION, 1);
        assert_eq!(SUPPORTED_VERSIONS, &[1]);
    }

    #[test]
    fn r708_path_mixed_types_joined() {
        let schema = MockSchema::default();
        *schema.errors.lock().unwrap() = vec![ManifestError {
            path: vec![serde_json::json!("items"), serde_json::json!(0)],
            message: "bad item".to_string(),
        }];
        let v = DefaultPluginManifestValidator::new(schema);
        let r = v.parse(&serde_json::json!({}));
        match r {
            ManifestParseResult::Failure { errors, .. } => {
                assert!(errors.contains("items.0: bad item"));
            }
            _ => panic!("expected Failure"),
        }
    }

    #[test]
    fn r708_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ManifestValidationException>();
    }
}
