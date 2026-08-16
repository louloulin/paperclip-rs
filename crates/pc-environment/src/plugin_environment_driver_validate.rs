#![allow(clippy::needless_return)]
// SPDX-License-Identifier: MIT
//
// R685 parity: validatePluginSandboxProviderConfig — full async pipeline
// composition (after the DB resolve step).
//
// Reference (Node):
//   paperclip/server/src/services/plugin-environment-driver.ts
//     validatePluginSandboxProviderConfig (lines ~199-258)
//
// This module stitches together the two pure pieces from R682 / R683 / R684:
//   - normalize_config_secret_refs (R683)
//   - PluginWorkerManager::call (R684)
//
// It does NOT handle the initial DB resolve step (resolvePluginSandboxProviderDriverByKey)
// because that requires a real DB; that work lives in R686+.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::json_schema_secret_refs::SecretRefBindingVersion;
use crate::plugin_environment_driver_validate_config::{
    normalize_config_secret_refs, SecretBindingNormalizeError, SecretBindingNormalizeResult,
};
use crate::plugin_worker_manager::{PluginRpcError, PluginRpcResult, PluginWorkerManager};

// ---------------------------------------------------------------------------
// Resolved driver (caller-supplied — typically produced by R686 DB query)
// ---------------------------------------------------------------------------

/// Mirrors the slice of `ResolvedSandboxProviderDriver` used by the validate
/// pipeline. `plugin` is the row stub; `driver` is the schema + metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedDriver {
    pub plugin_id: String,
    pub plugin_key: String,
    pub driver_key: String,
    pub driver_schema: Option<Value>,
}

/// Result of a successful validate run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatedDriverConfig {
    pub normalized_config: Value,
    pub plugin_id: String,
    pub plugin_key: String,
    pub driver_key: String,
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum ValidateConfigError {
    SecretBinding(SecretBindingNormalizeError),
    WorkerRpc(PluginRpcError),
    WorkerRejected {
        provider: String,
        first_error: String,
        errors: Vec<String>,
        warnings: Vec<String>,
    },
}

impl std::fmt::Display for ValidateConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SecretBinding(e) => write!(f, "{}", e),
            Self::WorkerRpc(e) => write!(f, "plugin worker rpc error: {}", e),
            Self::WorkerRejected { provider, first_error, .. } => write!(
                f,
                "Sandbox provider \"{}\" rejected its config. ({})",
                provider, first_error,
            ),
        }
    }
}

impl std::error::Error for ValidateConfigError {}

impl From<SecretBindingNormalizeError> for ValidateConfigError {
    fn from(e: SecretBindingNormalizeError) -> Self {
        Self::SecretBinding(e)
    }
}

impl From<PluginRpcError> for ValidateConfigError {
    fn from(e: PluginRpcError) -> Self {
        Self::WorkerRpc(e)
    }
}

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

/// Mirrors Node `validatePluginSandboxProviderConfig` after the resolve step.
/// Returns the normalized config + plugin metadata on success.
pub fn validate_plugin_sandbox_provider_config_after_resolve(
    resolved: &ResolvedDriver,
    config: &Value,
    worker_manager: &dyn PluginWorkerManager,
) -> Result<ValidatedDriverConfig, ValidateConfigError> {
    let provider = &resolved.driver_key;

    // 1. Schema guard (mirrors Node typeof + Array.isArray check)
    let schema = resolved
        .driver_schema
        .as_ref()
        .filter(|s| s.is_object())
        .cloned();

    // 2. Normalize secret bindings (R683)
    let SecretBindingNormalizeResult {
        normalized_config: mut config,
        rewritten_paths: _,
        skipped_paths: _,
    } = normalize_config_secret_refs(schema.as_ref(), config, provider)?;

    // 3. Worker RPC (R684)
    let params = serde_json::json!({
        "driverKey": provider,
        "config": config,
    });
    let result: PluginRpcResult = worker_manager.call(&resolved.plugin_id, "environmentValidateConfig", params, None)?;

    // 4. Validate worker result
    if !result.ok {
        let first_error = result
            .errors
            .first()
            .cloned()
            .unwrap_or_else(|| format!(
                "Sandbox provider \"{}\" rejected its config.",
                provider
            ));
        return Err(ValidateConfigError::WorkerRejected {
            provider: provider.clone(),
            first_error,
            errors: result.errors,
            warnings: result.warnings,
        });
    }

    // 5. Return result (prefer normalized_config from worker; fall back to local)
    let final_config = result.normalized_config.unwrap_or(config);
    Ok(ValidatedDriverConfig {
        normalized_config: final_config,
        plugin_id: resolved.plugin_id.clone(),
        plugin_key: resolved.plugin_key.clone(),
        driver_key: resolved.driver_key.clone(),
    })
}

// ---------------------------------------------------------------------------
// Helper: build ResolvedDriver from common test fixtures
// ---------------------------------------------------------------------------

impl ResolvedDriver {
    /// Convenience constructor for tests / mock data.
    pub fn new(
        plugin_id: impl Into<String>,
        plugin_key: impl Into<String>,
        driver_key: impl Into<String>,
        driver_schema: Option<Value>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            plugin_key: plugin_key.into(),
            driver_key: driver_key.into(),
            driver_schema,
        }
    }
}

// Force `SecretRefBindingVersion` import to remain used (referenced by R683).
#[allow(dead_code)]
fn _force_version_import(v: SecretRefBindingVersion) -> SecretRefBindingVersion {
    v
}
