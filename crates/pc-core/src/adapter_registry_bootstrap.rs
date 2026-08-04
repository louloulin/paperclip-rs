//! Declarative adapter-registry bootstrap.
//!
//! Ports the pure and IO-bound behavior from Node's
//! `server/src/services/adapter-registry-bootstrap.ts`. The module parses a strict
//! registry declaration and computes availability changes. Persisting the disabled
//! set and logging remain caller responsibilities.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub const PAPERCLIP_ADAPTERS: &str = "PAPERCLIP_ADAPTERS";
pub const PAPERCLIP_ADAPTERS_FILE: &str = "PAPERCLIP_ADAPTERS_FILE";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdapterRegistryEntry {
    pub adapter_type: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_keys: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_fqdns: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probe_command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_env: Option<HashMap<String, String>>,
}

const fn default_enabled() -> bool {
    true
}

#[derive(Debug, thiserror::Error)]
pub enum AdapterRegistryError {
    #[error("PAPERCLIP_ADAPTERS_FILE could not be read at \"{path}\": {source}")]
    FileRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("PAPERCLIP_ADAPTERS must be valid JSON: {0}")]
    InvalidJson(#[source] serde_json::Error),
    #[error("PAPERCLIP_ADAPTERS failed validation: {0}")]
    Validation(String),
    #[error(
        "PAPERCLIP_ADAPTERS declares adapter type(s) with no installed adapter: {}",
        .0.join(", ")
    )]
    MissingInstalledAdapters(Vec<String>),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdapterAvailabilityReconciliation {
    pub enabled: Vec<String>,
    pub disabled: Vec<String>,
}

/// Parse registry JSON with the same strict schema as the shared Zod validator.
pub fn parse_adapter_registry_json(
    raw_text: &str,
) -> Result<Vec<AdapterRegistryEntry>, AdapterRegistryError> {
    let entries: Vec<AdapterRegistryEntry> =
        serde_json::from_str(raw_text).map_err(classify_deserialization_error)?;

    let issues = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.adapter_type.is_empty())
        .map(|(index, _)| {
            format!("{index}.adapterType: String must contain at least 1 character(s)")
        })
        .collect::<Vec<_>>();

    if issues.is_empty() {
        Ok(entries)
    } else {
        Err(AdapterRegistryError::Validation(issues.join("; ")))
    }
}

fn classify_deserialization_error(error: serde_json::Error) -> AdapterRegistryError {
    match error.classify() {
        serde_json::error::Category::Syntax | serde_json::error::Category::Eof => {
            AdapterRegistryError::InvalidJson(error)
        }
        serde_json::error::Category::Data | serde_json::error::Category::Io => {
            AdapterRegistryError::Validation(error.to_string())
        }
    }
}

/// Parse `PAPERCLIP_ADAPTERS` or `PAPERCLIP_ADAPTERS_FILE`; inline JSON wins.
pub async fn parse_adapter_registry_env(
    env: &HashMap<String, String>,
) -> Result<Option<Vec<AdapterRegistryEntry>>, AdapterRegistryError> {
    let inline = trimmed_non_empty(env.get(PAPERCLIP_ADAPTERS));
    let file_path = trimmed_non_empty(env.get(PAPERCLIP_ADAPTERS_FILE));

    let raw_text = if let Some(inline) = inline {
        inline.to_owned()
    } else if let Some(file_path) = file_path {
        tokio::fs::read_to_string(Path::new(file_path))
            .await
            .map_err(|source| AdapterRegistryError::FileRead {
                path: PathBuf::from(file_path),
                source,
            })?
    } else {
        return Ok(None);
    };

    parse_adapter_registry_json(&raw_text).map(Some)
}

fn trimmed_non_empty(value: Option<&String>) -> Option<&str> {
    value
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// Compute the same enabled/disabled partition as Node's availability reconciliation.
pub fn reconcile_adapter_availability<I, S>(
    registry: Option<&[AdapterRegistryEntry]>,
    installed_adapter_types: I,
) -> Result<AdapterAvailabilityReconciliation, AdapterRegistryError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let Some(registry) = registry else {
        return Ok(AdapterAvailabilityReconciliation::default());
    };

    let installed = installed_adapter_types
        .into_iter()
        .map(Into::into)
        .collect::<Vec<_>>();
    let installed_set = installed.iter().map(String::as_str).collect::<HashSet<_>>();
    let declared = registry
        .iter()
        .map(|entry| (entry.adapter_type.as_str(), entry))
        .collect::<HashMap<_, _>>();
    let missing = declared
        .keys()
        .filter(|adapter_type| !installed_set.contains(**adapter_type))
        .map(|adapter_type| (*adapter_type).to_owned())
        .collect::<Vec<_>>();

    if !missing.is_empty() {
        return Err(AdapterRegistryError::MissingInstalledAdapters(missing));
    }

    let mut result = AdapterAvailabilityReconciliation::default();
    for adapter_type in installed {
        let should_enable = declared
            .get(adapter_type.as_str())
            .is_some_and(|entry| entry.enabled);
        if should_enable {
            result.enabled.push(adapter_type);
        } else {
            result.disabled.push(adapter_type);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(adapter_type: &str, enabled: bool) -> AdapterRegistryEntry {
        AdapterRegistryEntry {
            adapter_type: adapter_type.to_owned(),
            enabled,
            runtime_image: None,
            env_keys: None,
            allow_fqdns: None,
            probe_command: None,
            default_env: None,
        }
    }

    #[test]
    fn parses_empty_registry() {
        assert_eq!(parse_adapter_registry_json("[]").unwrap(), Vec::new());
    }

    #[test]
    fn defaults_enabled_to_true() {
        let parsed = parse_adapter_registry_json(r#"[{"adapterType":"claude"}]"#).unwrap();
        assert!(parsed[0].enabled);
    }

    #[test]
    fn parses_all_optional_fields() {
        let parsed = parse_adapter_registry_json(
            r#"[{"adapterType":"codex","enabled":false,"runtimeImage":"paperclip/codex","envKeys":["TOKEN"],"allowFqdns":["api.example.com"],"probeCommand":["codex","--version"],"defaultEnv":{"MODE":"safe"}}]"#,
        )
        .unwrap();
        assert!(!parsed[0].enabled);
        assert_eq!(parsed[0].runtime_image.as_deref(), Some("paperclip/codex"));
        assert_eq!(parsed[0].default_env.as_ref().unwrap()["MODE"], "safe");
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(matches!(
            parse_adapter_registry_json("["),
            Err(AdapterRegistryError::InvalidJson(_))
        ));
    }

    #[test]
    fn rejects_non_array_root() {
        assert!(matches!(
            parse_adapter_registry_json("{}"),
            Err(AdapterRegistryError::Validation(_))
        ));
    }

    #[test]
    fn rejects_unknown_fields() {
        assert!(matches!(
            parse_adapter_registry_json(r#"[{"adapterType":"codex","extra":true}]"#),
            Err(AdapterRegistryError::Validation(_))
        ));
    }

    #[test]
    fn rejects_empty_adapter_type() {
        let error = parse_adapter_registry_json(r#"[{"adapterType":""}]"#).unwrap_err();
        assert!(error.to_string().contains("0.adapterType"));
    }

    #[test]
    fn rejects_missing_adapter_type() {
        assert!(matches!(
            parse_adapter_registry_json("[{}]"),
            Err(AdapterRegistryError::Validation(_))
        ));
    }

    #[test]
    fn rejects_non_boolean_enabled() {
        assert!(matches!(
            parse_adapter_registry_json(r#"[{"adapterType":"codex","enabled":"yes"}]"#),
            Err(AdapterRegistryError::Validation(_))
        ));
    }

    #[test]
    fn rejects_non_string_default_env_value() {
        assert!(matches!(
            parse_adapter_registry_json(r#"[{"adapterType":"codex","defaultEnv":{"PORT":3000}}]"#),
            Err(AdapterRegistryError::Validation(_))
        ));
    }

    #[tokio::test]
    async fn unconfigured_env_returns_none() {
        assert_eq!(
            parse_adapter_registry_env(&HashMap::new()).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn blank_env_values_return_none() {
        let env = HashMap::from([
            (PAPERCLIP_ADAPTERS.to_owned(), "  ".to_owned()),
            (PAPERCLIP_ADAPTERS_FILE.to_owned(), "\n".to_owned()),
        ]);
        assert_eq!(parse_adapter_registry_env(&env).await.unwrap(), None);
    }

    #[tokio::test]
    async fn parses_trimmed_inline_json() {
        let env = HashMap::from([(
            PAPERCLIP_ADAPTERS.to_owned(),
            "  [{\"adapterType\":\"codex\"}]  ".to_owned(),
        )]);
        assert_eq!(
            parse_adapter_registry_env(&env).await.unwrap().unwrap()[0].adapter_type,
            "codex"
        );
    }

    #[tokio::test]
    async fn inline_json_takes_precedence_over_file() {
        let env = HashMap::from([
            (PAPERCLIP_ADAPTERS.to_owned(), "[]".to_owned()),
            (
                PAPERCLIP_ADAPTERS_FILE.to_owned(),
                "/missing/file".to_owned(),
            ),
        ]);
        assert_eq!(
            parse_adapter_registry_env(&env).await.unwrap(),
            Some(Vec::new())
        );
    }

    #[tokio::test]
    async fn parses_file_json() {
        let path =
            std::env::temp_dir().join(format!("paperclip-adapters-{}.json", uuid::Uuid::new_v4()));
        tokio::fs::write(&path, r#"[{"adapterType":"claude"}]"#)
            .await
            .unwrap();
        let env = HashMap::from([(
            PAPERCLIP_ADAPTERS_FILE.to_owned(),
            format!(" {} ", path.display()),
        )]);
        let parsed = parse_adapter_registry_env(&env).await.unwrap().unwrap();
        tokio::fs::remove_file(path).await.unwrap();
        assert_eq!(parsed[0].adapter_type, "claude");
    }

    #[tokio::test]
    async fn missing_file_fails_loudly() {
        let env = HashMap::from([(
            PAPERCLIP_ADAPTERS_FILE.to_owned(),
            "/definitely/missing/adapters.json".to_owned(),
        )]);
        assert!(matches!(
            parse_adapter_registry_env(&env).await,
            Err(AdapterRegistryError::FileRead { .. })
        ));
    }

    #[test]
    fn absent_registry_is_noop() {
        assert_eq!(
            reconcile_adapter_availability(None, ["codex"]).unwrap(),
            AdapterAvailabilityReconciliation::default()
        );
    }

    #[test]
    fn enables_only_declared_enabled_adapters() {
        let registry = [entry("codex", true), entry("claude", false)];
        let result =
            reconcile_adapter_availability(Some(&registry), ["codex", "claude", "gemini"]).unwrap();
        assert_eq!(result.enabled, ["codex"]);
        assert_eq!(result.disabled, ["claude", "gemini"]);
    }

    #[test]
    fn undeclared_installed_adapters_are_disabled() {
        let result = reconcile_adapter_availability(Some(&[]), ["codex", "claude"]).unwrap();
        assert!(result.enabled.is_empty());
        assert_eq!(result.disabled, ["codex", "claude"]);
    }

    #[test]
    fn missing_installed_adapter_fails_loudly() {
        let registry = [entry("unknown", true)];
        let error = reconcile_adapter_availability(Some(&registry), ["codex"]).unwrap_err();
        assert_eq!(
            error.to_string(),
            "PAPERCLIP_ADAPTERS declares adapter type(s) with no installed adapter: unknown"
        );
    }

    #[test]
    fn duplicate_declarations_use_last_entry_like_javascript_map() {
        let registry = [entry("codex", false), entry("codex", true)];
        let result = reconcile_adapter_availability(Some(&registry), ["codex"]).unwrap();
        assert_eq!(result.enabled, ["codex"]);
    }

    #[test]
    fn serialization_uses_camel_case_and_omits_none() {
        let value = serde_json::to_value(entry("codex", true)).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"adapterType": "codex", "enabled": true})
        );
    }
}
