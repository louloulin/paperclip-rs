//! JSON / YAML serialization helpers for [`OpenApiSpec`].
//!
//! Adding these as a small module (instead of methods on the spec) keeps
//! the spec struct pure data, which in turn keeps the builder pattern
//! honest. The helpers are intentionally infallible for JSON — any
//! serialization failure would be a programmer bug, not a runtime one.

use crate::spec::OpenApiSpec;

/// Errors that can arise when serializing an [`OpenApiSpec`].
///
/// JSON serialization is infallible in practice (we control the
/// types), but we wrap it in a Result for API symmetry with the YAML
/// helper which can fail on non-ASCII key collisions.
#[derive(Debug, thiserror::Error)]
pub enum OpenApiSerializeError {
    #[error("openapi spec has no paths; call `register_path` at least once")]
    EmptySpec,
    #[error("json serialization failed: {0}")]
    Json(#[from] serde_json::Error),
}

impl OpenApiSpec {
    /// Number of registered paths (each path is a URL like
    /// `/api/companies/:id`). 1:1 with the upstream `countPaths` check
    /// in `routes/openapi.ts`.
    #[must_use]
    pub fn path_count(&self) -> usize {
        self.paths.len()
    }

    /// Number of HTTP operations (method × path). One path with GET +
    /// POST counts as two operations.
    #[must_use]
    pub fn operation_count(&self) -> usize {
        self.paths.values().map(|m| m.len()).sum()
    }

    /// Number of named component schemas.
    #[must_use]
    pub fn schema_count(&self) -> usize {
        self.components.schemas.len()
    }

    /// Serialize the spec as pretty JSON. Returns a string suitable for
    /// serving at `GET /openapi.json`.
    #[must_use]
    pub fn to_json_string(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// Serialize the spec as a compact JSON value.
    #[must_use]
    pub fn to_json_value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::json!({}))
    }

    /// Serialize the spec as minimal YAML. We hand-roll the emitter
    /// instead of pulling in a `serde_yaml` dependency — the format
    /// is small enough and we only need to round-trip strings, numbers,
    /// booleans, and nested objects/arrays.
    pub fn to_yaml_string(&self) -> Result<String, OpenApiSerializeError> {
        let v = self.to_json_value();
        Ok(emit_yaml(&v, 0))
    }
}

fn indent(depth: usize) -> String {
    "  ".repeat(depth)
}

fn emit_scalar(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => format!("{}", serde_json::to_string(s).unwrap_or_default()),
        // Other shapes handled by callers.
        _ => String::new(),
    }
}

fn emit_yaml(v: &serde_json::Value, depth: usize) -> String {
    match v {
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            emit_scalar(v)
        }
        serde_json::Value::String(s) => emit_scalar(&serde_json::Value::String(s.clone())),
        serde_json::Value::Array(items) => {
            if items.is_empty() {
                return "[]".to_string();
            }
            let mut out = String::new();
            for item in items {
                out.push_str(&indent(depth));
                out.push_str("- ");
                let rendered = emit_yaml(item, depth + 1);
                // Inline the scalar but break for object/array.
                match item {
                    serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
                        out.push('\n');
                        out.push_str(&rendered);
                        out.push('\n');
                    }
                    _ => {
                        out.push_str(&rendered);
                        out.push('\n');
                    }
                }
            }
            out
        }
        serde_json::Value::Object(map) => {
            if map.is_empty() {
                return "{}".to_string();
            }
            let mut out = String::new();
            for (k, val) in map {
                out.push_str(&indent(depth));
                out.push_str(&format!("{}: ", k));
                match val {
                    serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
                        out.push('\n');
                        out.push_str(&emit_yaml(val, depth + 1));
                    }
                    _ => {
                        out.push_str(&emit_scalar(val));
                    }
                }
                out.push('\n');
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::OpenApiRegistry;
    use crate::path::{HttpMethod, OpenApiPath};
    use crate::schema::SchemaRef;

    fn empty_spec() -> OpenApiSpec {
        OpenApiRegistry::builder()
            .info(crate::spec::Info {
                title: "T".into(),
                version: "0.1.0".into(),
                description: None,
                contact: None,
                license: None,
            })
            .build()
    }

    #[test]
    fn r501_path_count_zero_on_empty() {
        assert_eq!(empty_spec().path_count(), 0);
    }

    #[test]
    fn r501_path_count_one_per_url() {
        let mut reg = OpenApiRegistry::builder();
        reg.register_path(
            "/api/a",
            HttpMethod::Get,
            OpenApiPath {
                summary: None,
                description: None,
                tags: vec![],
                operation_id: None,
                parameters: vec![],
                request_body: None,
                responses: indexmap::IndexMap::new(),
                security: vec![],
            },
        );
        reg.register_path(
            "/api/b",
            HttpMethod::Post,
            OpenApiPath {
                summary: None,
                description: None,
                tags: vec![],
                operation_id: None,
                parameters: vec![],
                request_body: None,
                responses: indexmap::IndexMap::new(),
                security: vec![],
            },
        );
        let spec = reg.build();
        assert_eq!(spec.path_count(), 2);
        assert_eq!(spec.operation_count(), 2);
    }

    #[test]
    fn r501_operation_count_handles_multi_method() {
        let mut reg = OpenApiRegistry::builder();
        reg.register_path(
            "/api/x",
            HttpMethod::Get,
            OpenApiPath {
                summary: None,
                description: None,
                tags: vec![],
                operation_id: None,
                parameters: vec![],
                request_body: None,
                responses: indexmap::IndexMap::new(),
                security: vec![],
            },
        );
        reg.register_path(
            "/api/x",
            HttpMethod::Post,
            OpenApiPath {
                summary: None,
                description: None,
                tags: vec![],
                operation_id: None,
                parameters: vec![],
                request_body: None,
                responses: indexmap::IndexMap::new(),
                security: vec![],
            },
        );
        let spec = reg.build();
        assert_eq!(spec.path_count(), 1);
        assert_eq!(spec.operation_count(), 2);
    }

    #[test]
    fn r501_schema_count_zero_on_empty() {
        assert_eq!(empty_spec().schema_count(), 0);
    }

    #[test]
    fn r501_schema_count_after_register() {
        let mut reg = OpenApiRegistry::builder();
        reg.register_schema("Agent", SchemaRef::object_with(&serde_json::json!({}), &[]));
        reg.register_schema("Issue", SchemaRef::object_with(&serde_json::json!({}), &[]));
        assert_eq!(reg.build().schema_count(), 2);
    }

    #[test]
    fn r501_to_json_string_contains_openapi_version() {
        let spec = empty_spec();
        let s = spec.to_json_string();
        assert!(s.contains("\"openapi\": \"3.1.0\""));
        assert!(s.contains("\"title\": \"T\""));
    }

    #[test]
    fn r501_to_json_value_is_object() {
        let spec = empty_spec();
        let v = spec.to_json_value();
        assert!(v.is_object());
        assert_eq!(v["openapi"], "3.1.0");
    }

    #[test]
    fn r501_to_yaml_string_contains_top_level_keys() {
        let spec = empty_spec();
        let y = spec.to_yaml_string().unwrap();
        // YAML emitter uses `openapi:` at top level.
        assert!(y.contains("openapi:"));
        assert!(y.contains("info:"));
        assert!(y.contains("paths:"));
        assert!(y.contains("components:"));
    }
}
