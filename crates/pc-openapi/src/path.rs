//! HTTP path / operation types.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Options,
    Head,
}

impl HttpMethod {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Post => "post",
            Self::Put => "put",
            Self::Patch => "patch",
            Self::Delete => "delete",
            Self::Options => "options",
            Self::Head => "head",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenApiPath {
    pub summary: Option<String>,
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tags: Vec<String>,
    pub operation_id: Option<String>,
    pub parameters: Vec<Parameter>,
    pub request_body: Option<RequestBody>,
    pub responses: indexmap::IndexMap<String, Response>,
    pub security: Vec<indexmap::IndexMap<String, Vec<String>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "in", rename_all = "camelCase")]
pub enum Parameter {
    Path {
        name: String,
        required: bool,
        schema: super::schema::SchemaRef,
        description: Option<String>,
    },
    Query {
        name: String,
        required: bool,
        schema: super::schema::SchemaRef,
        description: Option<String>,
    },
    Header {
        name: String,
        required: bool,
        schema: super::schema::SchemaRef,
    },
    Cookie {
        name: String,
        required: bool,
        schema: super::schema::SchemaRef,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestBody {
    pub description: Option<String>,
    pub content: indexmap::IndexMap<String, MediaType>,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaType {
    pub schema: super::schema::SchemaRef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    pub description: String,
    pub content: Option<indexmap::IndexMap<String, MediaType>>,
    pub headers: Option<indexmap::IndexMap<String, super::schema::SchemaRef>>,
}
