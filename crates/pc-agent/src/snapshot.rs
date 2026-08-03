use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

pub const REDACTED_VALUE: &str = "***REDACTED***";

const CONFIG_FIELDS: [(&str, fn(&AgentConfigSnapshot) -> Value); 12] = [
    ("name", |value| Value::String(value.name.clone())),
    ("role", |value| Value::String(value.role.clone())),
    ("title", |value| {
        serde_json::to_value(&value.title).expect("serialize title")
    }),
    ("icon", |value| {
        serde_json::to_value(&value.icon).expect("serialize icon")
    }),
    ("reportsTo", |value| {
        serde_json::to_value(value.reports_to).expect("serialize reports_to")
    }),
    ("capabilities", |value| {
        serde_json::to_value(&value.capabilities).expect("serialize capabilities")
    }),
    ("adapterType", |value| {
        Value::String(value.adapter_type.clone())
    }),
    ("adapterConfig", |value| value.adapter_config.clone()),
    ("runtimeConfig", |value| value.runtime_config.clone()),
    ("defaultEnvironmentId", |value| {
        serde_json::to_value(value.default_environment_id).expect("serialize environment")
    }),
    ("budgetMonthlyCents", |value| {
        Value::from(value.budget_monthly_cents)
    }),
    ("metadata", |value| {
        serde_json::to_value(&value.metadata).expect("serialize metadata")
    }),
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConfigSnapshot {
    pub name: String,
    pub role: String,
    pub title: Option<String>,
    pub icon: Option<String>,
    pub reports_to: Option<Uuid>,
    pub capabilities: Option<String>,
    pub adapter_type: String,
    pub adapter_config: Value,
    pub runtime_config: Value,
    pub default_environment_id: Option<Uuid>,
    pub budget_monthly_cents: i32,
    pub metadata: Option<Value>,
}

impl AgentConfigSnapshot {
    pub fn changed_keys(&self, after: &Self) -> Vec<&'static str> {
        CONFIG_FIELDS
            .iter()
            .filter_map(|(key, value)| (value(self) != value(after)).then_some(*key))
            .collect()
    }

    #[must_use]
    pub fn sanitized(mut self) -> Self {
        self.adapter_config = sanitize_snapshot_value(&self.adapter_config);
        self.runtime_config = sanitize_snapshot_value(&self.runtime_config);
        self.metadata = self.metadata.as_ref().map(sanitize_snapshot_value);
        self
    }
}

impl From<&pc_repos::agent::AgentRow> for AgentConfigSnapshot {
    fn from(row: &pc_repos::agent::AgentRow) -> Self {
        Self {
            name: row.name.clone(),
            role: row.role.clone(),
            title: row.title.clone(),
            icon: row.icon.clone(),
            reports_to: row.reports_to,
            capabilities: row.capabilities.clone(),
            adapter_type: row.adapter_type.clone(),
            adapter_config: row.adapter_config.clone(),
            runtime_config: row.runtime_config.clone(),
            default_environment_id: row.default_environment_id,
            budget_monthly_cents: row.budget_monthly_cents,
            metadata: row.metadata.clone(),
        }
    }
}

#[must_use]
pub fn contains_redacted_marker(value: &Value) -> bool {
    match value {
        Value::String(value) => value == REDACTED_VALUE,
        Value::Array(values) => values.iter().any(contains_redacted_marker),
        Value::Object(values) => values.values().any(contains_redacted_marker),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

#[must_use]
pub fn sanitize_snapshot_value(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(sanitize_snapshot_value).collect()),
        Value::Object(values) if is_secret_reference(values) => value.clone(),
        Value::Object(values) if is_plain_binding(values) => {
            let mut sanitized = values.clone();
            sanitized.insert("value".into(), Value::String(REDACTED_VALUE.into()));
            Value::Object(sanitized)
        }
        Value::Object(values) => Value::Object(sanitize_object(values)),
        Value::String(value) if looks_like_jwt(value) => Value::String(REDACTED_VALUE.into()),
        _ => value.clone(),
    }
}

fn sanitize_object(values: &Map<String, Value>) -> Map<String, Value> {
    values
        .iter()
        .map(|(key, value)| {
            let sanitized = if is_secret_key(key) {
                if value.as_object().is_some_and(is_secret_reference) {
                    value.clone()
                } else if value.as_object().is_some_and(is_plain_binding) {
                    sanitize_snapshot_value(value)
                } else {
                    Value::String(REDACTED_VALUE.into())
                }
            } else {
                sanitize_snapshot_value(value)
            };
            (key.clone(), sanitized)
        })
        .collect()
}

fn is_secret_reference(values: &Map<String, Value>) -> bool {
    matches!(
        values.get("type").and_then(Value::as_str),
        Some("secret_ref" | "user_secret_ref")
    )
}

fn is_plain_binding(values: &Map<String, Value>) -> bool {
    values.get("type").and_then(Value::as_str) == Some("plain") && values.contains_key("value")
}

fn is_secret_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    [
        "apikey",
        "accesstoken",
        "auth",
        "token",
        "authorization",
        "bearer",
        "secret",
        "passwd",
        "password",
        "credential",
        "jwt",
        "privatekey",
        "cookie",
        "connectionstring",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

fn looks_like_jwt(value: &str) -> bool {
    let segments: Vec<&str> = value.split('.').collect();
    (segments.len() == 3 || segments.len() == 4)
        && segments.iter().all(|segment| {
            !segment.is_empty()
                && segment.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
                })
        })
}
