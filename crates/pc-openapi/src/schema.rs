//! JSON Schema reference.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SchemaRef {
    /// `$ref` to a named schema (e.g. `#/components/schemas/Agent`).
    Named {
        #[serde(rename = "$ref")]
        reference: String,
    },
    /// Inline schema object — serialized as flattened key/value pairs into
    /// the surrounding object.
    Inline {
        #[serde(flatten)]
        schema: serde_json::Value,
    },
    /// Raw schema object — serialized verbatim. Use this when the schema
    /// contains top-level keys that conflict with serde flatten's behaviour
    /// (e.g. `type`, `required`).
    Raw(#[serde(serialize_with = "serialize_raw_value")] serde_json::Value),
}

fn serialize_raw_value<S>(value: &serde_json::Value, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::Serialize;
    value.serialize(serializer)
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
