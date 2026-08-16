#![allow(clippy::needless_return)]
// SPDX-License-Identifier: MIT
//
// R687 parity: validatePluginSandboxProviderConfig — full async pipeline
// composition (Node's complete 7-step flow).
//
// Reference (Node):
//   paperclip/server/src/services/plugin-environment-driver.ts
//     validatePluginSandboxProviderConfig (lines ~199-258)
//
// R687 stitches together:
//   - resolve_sandbox_provider_driver_key (R686)
//   - validate_plugin_sandbox_provider_config_after_resolve (R685)
// into the Node function's exact 1:1 surface.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::plugin_environment_driver_validate::{
    validate_plugin_sandbox_provider_config_after_resolve, ResolvedDriver,
    ValidateConfigError, ValidatedDriverConfig,
};
use crate::plugin_registry::{
    resolve_sandbox_provider_driver_key, PluginRegistry, ResolvedSandboxProviderDriver,
};
use crate::plugin_worker_manager::PluginWorkerManager;

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

/// Mirrors the Node return value of validatePluginSandboxProviderConfig.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidatedSandboxProviderConfig {
    pub normalized_config: Value,
    pub plugin_id: String,
    pub plugin_key: String,
    pub driver_key: String,
}

// ---------------------------------------------------------------------------
// Error type — top-level pipeline errors (resolve + validate)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum ValidateSandboxProviderError {
    /// Provider not found in any plugin (or worker not running when requireRunning).
    NotFound {
        provider: String,
        reason: NotFoundReason,
    },
    /// validate-after-resolve pipeline failed.
    Validate(ValidateConfigError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotFoundReason {
    /// Provider key not registered by any plugin.
    NoSuchProvider,
    /// requireRunning=true but plugin is not in Ready status.
    PluginNotReady { plugin_id: String, plugin_key: String },
    /// requireRunning=true but worker is not running.
    WorkerNotRunning { plugin_id: String, plugin_key: String },
    /// requireRunning=true but no workerManager provided.
    NoWorkerManager,
}

impl std::fmt::Display for ValidateSandboxProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { provider, reason } => {
                let detail = match reason {
                    NotFoundReason::NoSuchProvider => "is not installed".to_string(),
                    NotFoundReason::PluginNotReady { .. } => {
                        "its plugin is not in ready state".to_string()
                    }
                    NotFoundReason::WorkerNotRunning { .. } => {
                        "its plugin worker is not running".to_string()
                    }
                    NotFoundReason::NoWorkerManager => {
                        "no worker manager provided".to_string()
                    }
                };
                write!(
                    f,
                    "Sandbox provider \"{}\" {} or its plugin worker is not running.",
                    provider, detail,
                )
            }
            Self::Validate(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for ValidateSandboxProviderError {}

impl From<ValidateConfigError> for ValidateSandboxProviderError {
    fn from(e: ValidateConfigError) -> Self {
        Self::Validate(e)
    }
}

// ---------------------------------------------------------------------------
// Top-level entry point
// ---------------------------------------------------------------------------

/// 1:1 with Node `validatePluginSandboxProviderConfig`.
///
/// Always calls resolve with `require_running = true` (Node hardcodes this).
pub fn validate_plugin_sandbox_provider_config(
    registry: &dyn PluginRegistry,
    worker_manager: &dyn PluginWorkerManager,
    provider: &str,
    config: &Value,
) -> Result<ValidatedSandboxProviderConfig, ValidateSandboxProviderError> {
    let resolved = match resolve_sandbox_provider_driver_key(
        registry,
        Some(worker_manager),
        provider,
        true,
    ) {
        Some(r) => r,
        None => {
            return Err(classify_not_found(registry, worker_manager, provider));
        }
    };

    let resolved_input = to_resolved_driver(&resolved);
    let validated = validate_plugin_sandbox_provider_config_after_resolve(
        &resolved_input,
        config,
        worker_manager,
    )?;

    Ok(ValidatedSandboxProviderConfig {
        normalized_config: validated.normalized_config,
        plugin_id: validated.plugin_id,
        plugin_key: validated.plugin_key,
        driver_key: validated.driver_key,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Converts the R686 resolved-driver payload into the R685 input shape.
fn to_resolved_driver(r: &ResolvedSandboxProviderDriver) -> ResolvedDriver {
    ResolvedDriver {
        plugin_id: r.plugin.id.clone(),
        plugin_key: r.plugin.plugin_key.clone(),
        driver_key: r.driver.driver_key.clone(),
        driver_schema: r.driver.config_schema.clone(),
    }
}

/// Classify why resolve failed, for a better error message.
fn classify_not_found(
    registry: &dyn PluginRegistry,
    worker_manager: &dyn PluginWorkerManager,
    provider: &str,
) -> ValidateSandboxProviderError {
    // Re-scan without requireRunning to determine the specific reason.
    for plugin in registry.list() {
        if let Some(driver) = plugin.environment_drivers.iter().find(|d| {
            d.driver_key == provider
                && d.kind == crate::plugin_registry::PluginDriverKind::SandboxProvider
        }) {
            let plugin_id = plugin.id.clone();
            let plugin_key = plugin.plugin_key.clone();
            let reason = if !worker_manager.is_running(&plugin_id) {
                NotFoundReason::WorkerNotRunning { plugin_id, plugin_key }
            } else {
                NotFoundReason::PluginNotReady { plugin_id, plugin_key }
            };
            return ValidateSandboxProviderError::NotFound {
                provider: provider.to_string(),
                reason,
            };
        }
    }
    ValidateSandboxProviderError::NotFound {
        provider: provider.to_string(),
        reason: NotFoundReason::NoSuchProvider,
    }
}
