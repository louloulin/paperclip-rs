#![allow(clippy::needless_return)]
// SPDX-License-Identifier: MIT
//
// R688 parity: resolvePluginEnvironmentDriver +
// validatePluginEnvironmentDriverConfig (full async pipeline).
//
// Reference (Node):
//   paperclip/server/src/services/plugin-environment-driver.ts
//     resolvePluginEnvironmentDriver (lines ~76-95)
//     validatePluginEnvironmentDriverConfig (lines ~263-287)

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::PluginEnvironmentConfig;
use crate::plugin_environment_driver_pure::{plugin_driver_provider_key, PluginEnvironmentDriverKey};
use crate::plugin_registry::{
    PluginDriverKind, PluginEnvironmentDriverDecl, PluginRegistry, PluginRow, PluginStatus,
    ResolvedSandboxProviderDriver,
};
use crate::plugin_worker_manager::{PluginRpcError, PluginRpcResult, PluginWorkerManager};

// ---------------------------------------------------------------------------
// Resolve errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum ResolveEnvironmentDriverError {
    PluginNotFound { plugin_key: String },
    PluginNotReady { plugin_key: String },
    DriverNotDeclared { plugin_key: String, driver_key: String },
    WorkerNotRunning { plugin_key: String },
}

impl std::fmt::Display for ResolveEnvironmentDriverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PluginNotFound { plugin_key } => write!(
                f,
                "Plugin environment driver \"{}\" is not ready.",
                plugin_key
            ),
            Self::PluginNotReady { plugin_key } => write!(
                f,
                "Plugin environment driver \"{}\" is not ready.",
                plugin_key
            ),
            Self::DriverNotDeclared { plugin_key, driver_key } => write!(
                f,
                "Plugin \"{}\" does not declare environment driver \"{}\".",
                plugin_key, driver_key
            ),
            Self::WorkerNotRunning { plugin_key } => write!(
                f,
                "Plugin environment driver \"{}\" has no running worker.",
                plugin_key
            ),
        }
    }
}

impl std::error::Error for ResolveEnvironmentDriverError {}

// ---------------------------------------------------------------------------
// Validate errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum ValidateEnvironmentDriverError {
    Resolve(ResolveEnvironmentDriverError),
    WorkerRpc(PluginRpcError),
    WorkerRejected {
        provider_key: String,
        first_error: String,
        errors: Vec<String>,
        warnings: Vec<String>,
    },
}

impl std::fmt::Display for ValidateEnvironmentDriverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resolve(e) => write!(f, "{}", e),
            Self::WorkerRpc(e) => write!(f, "plugin worker rpc error: {}", e),
            Self::WorkerRejected { provider_key, first_error, .. } => write!(
                f,
                "Plugin environment driver \"{}\" rejected its config. ({})",
                provider_key, first_error,
            ),
        }
    }
}

impl std::error::Error for ValidateEnvironmentDriverError {}

impl From<ResolveEnvironmentDriverError> for ValidateEnvironmentDriverError {
    fn from(e: ResolveEnvironmentDriverError) -> Self {
        Self::Resolve(e)
    }
}

impl From<PluginRpcError> for ValidateEnvironmentDriverError {
    fn from(e: PluginRpcError) -> Self {
        Self::WorkerRpc(e)
    }
}

// ---------------------------------------------------------------------------
// resolvePluginEnvironmentDriver
// ---------------------------------------------------------------------------

/// 1:1 with Node resolvePluginEnvironmentDriver.
pub fn resolve_plugin_environment_driver(
    registry: &dyn PluginRegistry,
    worker_manager: &dyn PluginWorkerManager,
    config: &PluginEnvironmentConfig,
) -> Result<ResolvedSandboxProviderDriver, ResolveEnvironmentDriverError> {
    let plugin = find_plugin_by_key(registry, &config.plugin_key)
        .ok_or_else(|| ResolveEnvironmentDriverError::PluginNotFound {
            plugin_key: config.plugin_key.clone(),
        })?;

    if plugin.status != PluginStatus::Ready {
        return Err(ResolveEnvironmentDriverError::PluginNotReady {
            plugin_key: config.plugin_key.clone(),
        });
    }

    let driver = plugin
        .environment_drivers
        .iter()
        .find(|d| d.driver_key == config.driver_key)
        .cloned()
        .ok_or_else(|| ResolveEnvironmentDriverError::DriverNotDeclared {
            plugin_key: config.plugin_key.clone(),
            driver_key: config.driver_key.clone(),
        })?;

    if !worker_manager.is_running(&plugin.id) {
        return Err(ResolveEnvironmentDriverError::WorkerNotRunning {
            plugin_key: config.plugin_key.clone(),
        });
    }

    Ok(ResolvedSandboxProviderDriver { plugin, driver })
}

// ---------------------------------------------------------------------------
// validatePluginEnvironmentDriverConfig (top-level)
// ---------------------------------------------------------------------------

/// 1:1 with Node validatePluginEnvironmentDriverConfig.
pub fn validate_plugin_environment_driver_config(
    registry: &dyn PluginRegistry,
    worker_manager: &dyn PluginWorkerManager,
    config: &PluginEnvironmentConfig,
) -> Result<PluginEnvironmentConfig, ValidateEnvironmentDriverError> {
    let resolved = resolve_plugin_environment_driver(registry, worker_manager, config)?;
    let provider_key = plugin_driver_provider_key(&PluginEnvironmentDriverKey {
        plugin_key: config.plugin_key.clone(),
        driver_key: config.driver_key.clone(),
    });

    // Worker RPC (no secret-binding normalize for environment drivers)
    let params = serde_json::json!({
        "driverKey": config.driver_key,
        "config": config.driver_config,
    });
    let result: PluginRpcResult = worker_manager.call(
        &resolved.plugin.id,
        "environmentValidateConfig",
        params,
        None,
    )?;

    if !result.ok {
        let first_error = result
            .errors
            .first()
            .cloned()
            .unwrap_or_else(|| format!(
                "Plugin environment driver \"{}\" rejected its config.",
                provider_key
            ));
        return Err(ValidateEnvironmentDriverError::WorkerRejected {
            provider_key: provider_key.clone(),
            first_error,
            errors: result.errors,
            warnings: result.warnings,
        });
    }

    // Merge: keep all input fields, replace driverConfig with normalized
    let driver_config_value: Value = result
        .normalized_config
        .clone()
        .unwrap_or_else(|| Value::Object(config.driver_config.clone().into_iter().collect()));

    let mut merged = config.clone();
    merged.driver_config = match driver_config_value {
        Value::Object(map) => map,
        _ => config.driver_config.clone(),
    };
    Ok(merged)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn find_plugin_by_key(registry: &dyn PluginRegistry, plugin_key: &str) -> Option<PluginRow> {
    registry.list().into_iter().find(|p| p.plugin_key == plugin_key)
}

#[allow(dead_code)]
fn _force_driver_kind_import(k: PluginDriverKind) -> PluginDriverKind {
    k
}

#[allow(dead_code)]
fn _force_driver_decl_import(d: &PluginEnvironmentDriverDecl) -> &str {
    &d.driver_key
}
