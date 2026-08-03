//! `OpenApiRegistry`: register paths + components in any order.

use crate::path::{HttpMethod, OpenApiPath};
use crate::schema::SchemaRef;
use crate::spec::{Components, Info, License, OpenApiSpec, Server, OPENAPI_VERSION};

#[derive(Debug, Default)]
pub struct OpenApiRegistry {
    info: Option<Info>,
    servers: Vec<Server>,
    paths: indexmap::IndexMap<String, indexmap::IndexMap<String, OpenApiPath>>,
    components: Components,
}

impl OpenApiRegistry {
    #[must_use]
    pub fn builder() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn info(mut self, info: Info) -> Self {
        self.info = Some(info);
        self
    }

    #[must_use]
    pub fn server(mut self, url: impl Into<String>, description: Option<String>) -> Self {
        self.servers.push(Server {
            url: url.into(),
            description,
        });
        self
    }

    pub fn register_path(&mut self, path: &str, method: HttpMethod, op: OpenApiPath) -> &mut Self {
        self.paths
            .entry(path.to_string())
            .or_default()
            .insert(method.as_str().to_string(), op);
        self
    }

    pub fn register_schema(&mut self, name: impl Into<String>, schema: SchemaRef) -> &mut Self {
        let n = name.into();
        match schema {
            SchemaRef::Named { reference } => {
                self.components
                    .schemas
                    .insert(n, SchemaRef::Named { reference });
            }
            SchemaRef::Inline { schema: _ } => {
                self.components.schemas.insert(n, schema);
            }
        }
        self
    }

    #[must_use]
    pub fn build(self) -> OpenApiSpec {
        OpenApiSpec {
            openapi: OPENAPI_VERSION.to_string(),
            info: self.info.unwrap_or_else(|| Info {
                title: "Paperclip API".into(),
                version: "0.1.0".into(),
                description: Some("Paperclip Rust server".into()),
                contact: None,
                license: Some(License {
                    name: "MIT".into(),
                    url: None,
                }),
            }),
            servers: self.servers,
            paths: self.paths,
            components: self.components,
            tags: Vec::new(),
            external_docs: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::{Parameter, RequestBody, Response};
    use crate::{OpenApiPath, SchemaRef};

    #[test]
    fn builds_minimal_spec() {
        let mut reg = OpenApiRegistry::builder()
            .info(Info {
                title: "Test API".into(),
                version: "0.1.0".into(),
                description: None,
                contact: None,
                license: None,
            })
            .server("http://localhost:3100", Some("local".into()));
        let props = serde_json::json!({
            "agentId": {"type": "string", "format": "uuid"},
            "prompt": {"type": "string"},
        });
        let required = vec!["agentId".to_string()];
        reg.register_schema(
            "HeartbeatRequest",
            SchemaRef::object_with(&props, &required),
        );
        let op = OpenApiPath {
            summary: Some("Trigger agent heartbeat".into()),
            description: None,
            tags: vec!["agents".into()],
            operation_id: Some("trigger_heartbeat".into()),
            parameters: vec![Parameter::Path {
                name: "agent_id".into(),
                required: true,
                schema: SchemaRef::string(),
                description: None,
            }],
            request_body: Some(RequestBody {
                description: None,
                content: indexmap::IndexMap::new(),
                required: false,
            }),
            responses: indexmap::IndexMap::new(),
            security: Vec::new(),
        };
        reg.register_path("/api/agents/{agent_id}/heartbeat", HttpMethod::Post, op);

        let spec = reg.build();
        assert_eq!(spec.openapi, "3.1.0");
        assert!(spec.paths.contains_key("/api/agents/{agent_id}/heartbeat"));
        assert!(spec.components.schemas.contains_key("HeartbeatRequest"));
    }

    #[test]
    fn register_multiple_methods() {
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
            HttpMethod::Delete,
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
        assert!(spec.paths["/api/x"].contains_key("get"));
        assert!(spec.paths["/api/x"].contains_key("delete"));
    }

    #[test]
    fn schema_ref_refs_helper() {
        let r = SchemaRef::ref_to("Agent");
        match r {
            SchemaRef::Named { reference } => {
                assert_eq!(reference, "#/components/schemas/Agent");
            }
            SchemaRef::Inline { .. } => panic!("expected Named"),
        }
    }

    #[test]
    fn response_with_json_content() {
        let mut reg = OpenApiRegistry::builder();
        let mut content = indexmap::IndexMap::new();
        content.insert(
            "application/json".into(),
            crate::path::MediaType {
                schema: SchemaRef::ref_to("Agent"),
            },
        );
        let mut responses = indexmap::IndexMap::new();
        responses.insert(
            "200".into(),
            Response {
                description: "ok".into(),
                content: Some(content),
                headers: None,
            },
        );
        reg.register_path(
            "/api/agents",
            HttpMethod::Get,
            OpenApiPath {
                summary: Some("list agents".into()),
                description: None,
                tags: vec![],
                operation_id: None,
                parameters: vec![],
                request_body: None,
                responses,
                security: vec![],
            },
        );
        let spec = reg.build();
        assert!(spec.paths.contains_key("/api/agents"));
    }
}
