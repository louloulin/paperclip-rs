// SPDX-License-Identifier: MIT
//
// R689 parity: probePluginEnvironmentDriver + listReadyPluginEnvironmentDrivers
// (full parity including the recovery flow that auto-starts stopped workers).

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::config::PluginEnvironmentConfig;
use crate::plugin_environment_driver_pure::{
    plugin_driver_provider_key, PluginEnvironmentDriverKey,
};
use crate::plugin_registry::{
    PluginDriverKind, PluginRegistry, PluginStatus, ReadyPluginEnvironmentDriver,
};
use crate::plugin_worker_manager::{PluginRpcDiagnostic, PluginRpcError, PluginRpcResult, PluginWorkerManager};
use crate::validate_environment_driver::resolve_plugin_environment_driver;

// ---------------------------------------------------------------------------
// Constants.
// ---------------------------------------------------------------------------

pub const DEFAULT_READY_PLUGIN_WORKER_RECOVERY_TIMEOUT_MS: u64 = 2_000;
pub const PROBE_TIMEOUT_MS: u64 = 120_000;
// ---------------------------------------------------------------------------
// Result types.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct EnvironmentProbeDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<PluginRpcDiagnostic>>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub metadata: Map<String, Value>,
    // ---- R764 parity: details for local / sandbox / ssh drivers. ----
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_workspace_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_lease_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct EnvironmentProbeResult {
    pub ok: bool,
    pub driver: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<EnvironmentProbeDetails>,
}
// ---------------------------------------------------------------------------
// Errors.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum ProbeEnvironmentDriverError {
    Resolve(crate::validate_environment_driver::ResolveEnvironmentDriverError),
    WorkerRpc(PluginRpcError),
}

impl std::fmt::Display for ProbeEnvironmentDriverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resolve(e) => write!(f, "{}", e),
            Self::WorkerRpc(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for ProbeEnvironmentDriverError {}

impl From<crate::validate_environment_driver::ResolveEnvironmentDriverError>
    for ProbeEnvironmentDriverError
{
    fn from(e: crate::validate_environment_driver::ResolveEnvironmentDriverError) -> Self {
        Self::Resolve(e)
    }
}

impl From<PluginRpcError> for ProbeEnvironmentDriverError {
    fn from(e: PluginRpcError) -> Self {
        Self::WorkerRpc(e)
    }
}

// ---------------------------------------------------------------------------
// Recovery trait.
// ---------------------------------------------------------------------------

pub trait ReadyPluginWorkerRecovery: Send + Sync {
    fn plugin_keys(&self) -> Vec<String>;
    fn start_worker(&self, plugin_id: &str, plugin_key: &str) -> bool;
    fn timeout_ms(&self) -> Option<u64> {
        None
    }
}

pub type BoxedReadyPluginWorkerRecovery = Arc<dyn ReadyPluginWorkerRecovery>;

/// Re-export of the worker diagnostic record (mirrors Node
/// EnvironmentProbeDiagnostics).
pub type ProbeDiagnostic = PluginRpcDiagnostic;
// ---------------------------------------------------------------------------
// probePluginEnvironmentDriver - 1:1 with Node.
// ---------------------------------------------------------------------------

pub fn probe_plugin_environment_driver(
    registry: &dyn PluginRegistry,
    worker_manager: &dyn PluginWorkerManager,
    company_id: &str,
    environment_id: &str,
    config: &PluginEnvironmentConfig,
) -> Result<EnvironmentProbeResult, ProbeEnvironmentDriverError> {
    let resolved = resolve_plugin_environment_driver(registry, worker_manager, config)?;
    let provider_key = plugin_driver_provider_key(&PluginEnvironmentDriverKey {
        plugin_key: config.plugin_key.clone(),
        driver_key: config.driver_key.clone(),
    });

    let params = json!({
        "driverKey": config.driver_key,
        "companyId": company_id,
        "environmentId": environment_id,
        "config": config.driver_config,
    });

    let result: PluginRpcResult = worker_manager.call(
        &resolved.plugin.id,
        "environmentProbe",
        params,
        Some(PROBE_TIMEOUT_MS),
    )?;

    let passed_msg = format!(
        "Plugin environment driver \"{}\" probe passed.",
        provider_key
    );
    let failed_msg = format!(
        "Plugin environment driver \"{}\" probe failed.",
        provider_key
    );
    let summary = result
        .summary
        .unwrap_or_else(|| if result.ok { passed_msg } else { failed_msg });

    Ok(EnvironmentProbeResult {
        ok: result.ok,
        driver: "plugin".to_string(),
        summary,
        details: Some(EnvironmentProbeDetails {
            plugin_key: Some(config.plugin_key.clone()),
            driver_key: Some(config.driver_key.clone()),
            provider: None,
            diagnostics: Some(result.diagnostics.unwrap_or_default()),
            metadata: result.metadata,
            ..Default::default()
        }),
    })
}
// ---------------------------------------------------------------------------
// listReadyPluginEnvironmentDrivers - 1:1 with Node.
// ---------------------------------------------------------------------------

pub fn list_ready_plugin_environment_drivers(
    registry: &dyn PluginRegistry,
    worker_manager: Option<&dyn PluginWorkerManager>,
    recover_missing_worker: Option<&dyn ReadyPluginWorkerRecovery>,
) -> Vec<ReadyPluginEnvironmentDriver> {
    let wm = match worker_manager {
        Some(w) => w,
        None => return Vec::new(),
    };

    let plugins = registry.list();
    let recoverable_keys: HashSet<String> = recover_missing_worker
        .map(|r| r.plugin_keys().into_iter().collect())
        .unwrap_or_default();

    let ready_plugins: Vec<_> = plugins
        .into_iter()
        .filter(|p| p.status == PluginStatus::Ready)
        .collect();

    for plugin in &ready_plugins {
        let has_sandbox = plugin
            .environment_drivers
            .iter()
            .any(|d| d.kind == PluginDriverKind::SandboxProvider);
        let can_recover = has_sandbox
            && !wm.is_running(&plugin.id)
            && recoverable_keys.contains(&plugin.plugin_key)
            && !wm.worker_registered(&plugin.id);
        if !can_recover || recover_missing_worker.is_none() {
            continue;
        }
        let rec = recover_missing_worker.unwrap();
        let _ = rec.timeout_ms();
        rec.start_worker(&plugin.id, &plugin.plugin_key);
    }

    let mut rows: Vec<ReadyPluginEnvironmentDriver> = Vec::new();
    for plugin in ready_plugins {
        if !wm.is_running(&plugin.id) {
            continue;
        }
        for driver in &plugin.environment_drivers {
            if driver.kind != PluginDriverKind::SandboxProvider {
                continue;
            }
            rows.push(ReadyPluginEnvironmentDriver {
                plugin_id: plugin.id.clone(),
                plugin_key: plugin.plugin_key.clone(),
                driver_key: driver.driver_key.clone(),
                display_name: driver.display_name.clone(),
                description: driver.description.clone(),
                config_schema: driver.config_schema.clone(),
                supports_reusable_leases: driver.supports_reusable_leases,
                supports_interactive_setup: driver.supports_interactive_setup,
                interactive_setup_connection_types: driver
                    .interactive_setup_connection_types
                    .clone(),
                supports_template_capture: driver.supports_template_capture,
                template_ref_kind: driver.template_ref_kind.clone(),
                template_config_binding: driver.template_config_binding.clone(),
                supports_template_delete: driver.supports_template_delete,
            });
        }
    }
    rows
}

// ---------------------------------------------------------------------------
// In-memory recovery helper for tests.
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct InMemoryRecovery {
    pub recoverable: HashSet<String>,
    pub started: Mutex<Vec<(String, String)>>,
    pub timeout_ms: Option<u64>,
    pub start_outcome: bool,
}

impl ReadyPluginWorkerRecovery for InMemoryRecovery {
    fn plugin_keys(&self) -> Vec<String> {
        self.recoverable.iter().cloned().collect()
    }
    fn start_worker(&self, plugin_id: &str, plugin_key: &str) -> bool {
        self.started
            .lock()
            .unwrap()
            .push((plugin_id.to_string(), plugin_key.to_string()));
        self.start_outcome
    }
    fn timeout_ms(&self) -> Option<u64> {
        self.timeout_ms
            .or(Some(DEFAULT_READY_PLUGIN_WORKER_RECOVERY_TIMEOUT_MS))
    }
}

#[allow(dead_code)]
fn _force_duration_import() -> Duration {
    Duration::from_millis(PROBE_TIMEOUT_MS)
}
