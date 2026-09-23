//! Multica `OpenAPI` 3.1 spec generator。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const OPENAPI_VERSION: &str = "3.1.0";

/// 公开 Action API（`/v1/*`）的片段声明（M6-1 落地 9 个 Operation；M6-7 的 route 表对齐它）。
pub mod v1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenApiSpec {
    pub openapi: String,
    pub info: OpenApiInfo,
    pub servers: Vec<OpenApiServer>,
    pub paths: Value,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub components: Vec<OpenApiComponent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenApiInfo {
    pub title: String,
    pub description: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenApiServer {
    pub url: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenApiComponent {
    pub name: String,
    pub schema: Value,
}

impl OpenApiSpec {
    pub fn minimal(
        title: impl Into<String>,
        version: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            openapi: OPENAPI_VERSION.into(),
            info: OpenApiInfo {
                title: title.into(),
                description: "Multica-rs API".into(),
                version: version.into(),
            },
            servers: vec![OpenApiServer {
                url: base_url.into(),
                description: None,
            }],
            paths: json!({}),
            components: vec![],
        }
    }

    pub fn add_path(
        &mut self,
        path: impl Into<String>,
        method: impl Into<String>,
        operation: Value,
    ) {
        let path = path.into();
        let method = method.into().to_ascii_lowercase();
        let path_entry = self
            .paths
            .as_object_mut()
            .unwrap()
            .entry(path)
            .or_insert_with(|| json!({}));
        path_entry
            .as_object_mut()
            .unwrap()
            .insert(method, operation);
    }

    pub fn add_schema(&mut self, name: impl Into<String>, schema: Value) {
        self.components.push(OpenApiComponent {
            name: name.into(),
            schema,
        });
    }

    pub fn render(&self) -> Value {
        let mut components = serde_json::Map::new();
        for comp in &self.components {
            components.insert(comp.name.clone(), comp.schema.clone());
        }
        let mut out = serde_json::to_value(self).unwrap();
        out["components"] = json!({ "schemas": Value::Object(components) });
        out
    }
}

/// axum 路由：暴露 `/openapi.json`。
#[allow(clippy::unused_async)] // 保留 axum handler 形状；本切片尚未挂载。
pub async fn openapi_json_handler() -> axum::Json<Value> {
    let spec = OpenApiSpec::minimal(
        "Multica-rs API",
        env!("CARGO_PKG_VERSION"),
        "http://127.0.0.1:3500",
    );
    axum::Json(spec.render())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_spec_is_valid() {
        let mut spec = OpenApiSpec::minimal("T", "0.1.0", "http://x");
        spec.add_path(
            "/api/health",
            "get",
            json!({
                "summary": "Health check",
                "responses": {"200": {"description": "OK"}}
            }),
        );
        spec.add_schema(
            "ErrorResponse",
            json!({"type": "object", "properties": {"code": {"type": "string"}}}),
        );
        let rendered = spec.render();
        assert_eq!(rendered["openapi"], OPENAPI_VERSION);
        assert!(rendered["paths"]["/api/health"]["get"].is_object());
        assert!(rendered["components"]["schemas"]["ErrorResponse"].is_object());
    }
}
