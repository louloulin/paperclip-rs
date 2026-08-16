#![allow(clippy::needless_return)]
// SPDX-License-Identifier: MIT
//
// R683 parity: validatePluginSandboxProviderConfig /
// validatePluginEnvironmentDriverConfig — pure secret-binding normalize.

use serde::{Deserialize, Serialize};

use crate::json_schema_secret_refs::{
    collect_secret_ref_paths, parse_secret_ref_binding_object,
    read_config_value_at_path, write_config_value_at_path,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretBindingNormalizeError {
    PinnedVersion { path: String, version: String, provider: String },
}

impl std::fmt::Display for SecretBindingNormalizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PinnedVersion { path, version, provider } => write!(
                f,
                "Secret binding at {} pins version {}; sandbox provider secret references always resolve the latest version. (provider: {})",
                path, version, provider,
            ),
        }
    }
}

impl std::error::Error for SecretBindingNormalizeError {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SecretBindingNormalizeResult {
    pub normalized_config: serde_json::Value,
    pub rewritten_paths: Vec<String>,
    pub skipped_paths: Vec<String>,
}

pub fn normalize_config_secret_refs(
    config_schema: Option<&serde_json::Value>,
    config: &serde_json::Value,
    provider: &str,
) -> Result<SecretBindingNormalizeResult, SecretBindingNormalizeError> {
    let paths = collect_secret_ref_paths(config_schema);
    let mut current = config.clone();
    let mut rewritten: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();

    let mut sorted_paths: Vec<String> = paths.into_iter().collect();
    sorted_paths.sort();

    for path in sorted_paths {
        let value = read_config_value_at_path(&current, &path);
        let Some(value) = value else { continue; };
        let Some(binding) = parse_secret_ref_binding_object(value) else {
            skipped.push(path);
            continue;
        };
        match binding.version {
            crate::json_schema_secret_refs::SecretRefBindingVersion::Latest => {
                let id = serde_json::Value::String(binding.secret_id);
                current = write_config_value_at_path(&current, &path, Some(&id));
                rewritten.push(path);
            }
            crate::json_schema_secret_refs::SecretRefBindingVersion::Number(v) => {
                return Err(SecretBindingNormalizeError::PinnedVersion {
                    path: path.clone(),
                    version: v.to_string(),
                    provider: provider.to_string(),
                });
            }
        }
    }

    Ok(SecretBindingNormalizeResult {
        normalized_config: current,
        rewritten_paths: rewritten,
        skipped_paths: skipped,
    })
}

pub fn as_object_schema(value: Option<&serde_json::Value>) -> Option<&serde_json::Map<String, serde_json::Value>> {
    let v = value?;
    v.as_object()
}

pub fn schema_for_collect(schema: Option<&serde_json::Value>) -> Option<&serde_json::Value> {
    let s = schema?;
    if !s.is_object() { return None; }
    Some(s)
}
