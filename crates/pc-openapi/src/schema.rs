//! JSON Schema reference.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SchemaRef {
    Named {
        #[serde(rename = "$ref")]
        reference: String,
    },
    Inline {
        #[serde(flatten)]
        schema: serde_json::Value,
    },
}

impl SchemaRef {
    #[must_use]
    pub fn ref_to(name: &str) -> Self {
        Self::Named {
            reference: format!("#/components/schemas/{name}"),
        }
    }

    #[must_use]
    pub fn bool() -> Self {
        Self::Inline {
            schema: serde_json::json!({"type": "boolean"}),
        }
    }

    #[must_use]
    pub fn string() -> Self {
        Self::Inline {
            schema: serde_json::json!({"type": "string"}),
        }
    }

    #[must_use]
    pub fn integer() -> Self {
        Self::Inline {
            schema: serde_json::json!({"type": "integer", "format": "int64"}),
        }
    }

    #[must_use]
    pub fn object_with(properties: &serde_json::Value, required: &[String]) -> Self {
        Self::Inline {
            schema: serde_json::json!({
                "type": "object",
                "properties": properties.clone(),
                "required": required.to_vec(),
            }),
        }
    }
}
