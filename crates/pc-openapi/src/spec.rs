//! Top-level OpenAPI 3.1 document types.

use serde::{Deserialize, Serialize};

use crate::path::OpenApiPath;
use crate::schema::SchemaRef;

pub const OPENAPI_VERSION: &str = "3.1.0";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenApiSpec {
    pub openapi: String,
    pub info: Info,
    pub servers: Vec<Server>,
    pub paths: indexmap::IndexMap<String, indexmap::IndexMap<String, OpenApiPath>>,
    pub components: Components,
    pub tags: Vec<Tag>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_docs: Option<ExternalDocs>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Info {
    pub title: String,
    pub version: String,
    pub description: Option<String>,
    pub contact: Option<Contact>,
    pub license: Option<License>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contact {
    pub name: Option<String>,
    pub url: Option<String>,
    pub email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct License {
    pub name: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Server {
    pub url: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Components {
    #[serde(skip_serializing_if = "indexmap::IndexMap::is_empty", default)]
    pub schemas: indexmap::IndexMap<String, SchemaRef>,
    #[serde(skip_serializing_if = "indexmap::IndexMap::is_empty", default)]
    pub security_schemes: indexmap::IndexMap<String, SecurityScheme>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalDocs {
    pub url: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SecurityScheme {
    ApiKey {
        name: String,
        location: String,
    },
    Http {
        scheme: String,
        bearer_format: Option<String>,
    },
    OAuth2 {
        flows: serde_json::Value,
    },
    OpenIdConnect {
        open_id_connect_url: String,
    },
}
