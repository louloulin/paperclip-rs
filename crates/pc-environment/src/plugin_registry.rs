#![allow(clippy::needless_return)]
// SPDX-License-Identifier: MIT
//
// R686 parity: PluginRegistry trait + resolvePluginSandboxProviderDriverByKey.
//
// Reference (Node):
//   paperclip/server/src/services/plugin-environment-driver.ts
//     resolvePluginSandboxProviderDriverByKey (lines ~111-132)
//
// R686 abstracts the plugin-registry surface that resolveSandboxProviderDriverByKey
// depends on. It also ships the core lookup logic (driver-key + kind filter +
// ready check + worker-running check) as a pure function over the trait.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::plugin_worker_manager::{PluginWorkerManager, PluginWorkerManagerInspect};

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

/// Mirrors Node plugin `status`. The Node type is a string union; only
/// "ready" is checked by R686.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PluginStatus {
    #[default]
    Installed,
    Registered,
    Ready,
    Failed,
    Disabled,
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PluginDriverKind {
    #[default]
    SandboxProvider,
    Environment,
}

/// Mirrors the slice of `PluginEnvironmentDriverDeclaration` consumed by
/// resolve / listReady / validate. Full declaration lives in @paperclipai/shared.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PluginEnvironmentDriverDecl {
    pub driver_key: String,
    pub kind: PluginDriverKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<Value>,
    // R689 parity: extended driver declaration fields consumed by
    // listReadyPluginEnvironmentDrivers (Node PluginEnvironmentDriverDeclaration).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_reusable_leases: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_interactive_setup: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interactive_setup_connection_types: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_template_capture: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_ref_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_config_binding: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_template_delete: Option<bool>,
}

/// Mirrors the slice of the `plugins` row used by the resolve helpers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PluginRow {
    pub id: String,
    pub plugin_key: String,
    pub status: PluginStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub environment_drivers: Vec<PluginEnvironmentDriverDecl>,
}

// ---------------------------------------------------------------------------
// PluginRegistry trait
// ---------------------------------------------------------------------------

pub trait PluginRegistry: Send + Sync {
    fn list(&self) -> Vec<PluginRow>;
}

// ---------------------------------------------------------------------------
// In-memory implementation
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct InMemoryPluginRegistryInner {
    plugins: Vec<PluginRow>,
}

#[derive(Debug, Clone, Default)]
pub struct InMemoryPluginRegistry {
    inner: Arc<Mutex<InMemoryPluginRegistryInner>>,
}

impl InMemoryPluginRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the entire plugin list (Node `pluginRegistry.list()` returns
    /// the persisted state, so any mutation goes through a single snapshot).
    pub fn set_plugins(&self, plugins: Vec<PluginRow>) {
        let mut inner = self.inner.lock().unwrap();
        inner.plugins = plugins;
    }

    pub fn add_plugin(&self, plugin: PluginRow) {
        let mut inner = self.inner.lock().unwrap();
        inner.plugins.push(plugin);
    }

    pub fn plugin_count(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        inner.plugins.len()
    }
}

impl PluginRegistry for InMemoryPluginRegistry {
    fn list(&self) -> Vec<PluginRow> {
        let inner = self.inner.lock().unwrap();
        inner.plugins.clone()
    }
}

// ---------------------------------------------------------------------------
// Resolve: pure logic over the registry + worker manager
// ---------------------------------------------------------------------------

/// Result of a successful resolve. The two fields together form the
/// `resolved` payload consumed by validatePluginSandboxProviderConfig
/// (R685).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedSandboxProviderDriver {
    pub plugin: PluginRow,
    pub driver: PluginEnvironmentDriverDecl,
}

/// 1:1 parity with Node `resolvePluginSandboxProviderDriverByKey`.
///
/// Algorithm:
///   1. List plugins from the registry.
///   2. For each plugin, find a driver with matching `driverKey` and
///      `kind === sandbox_provider`.
///   3. If `require_running` is set, additionally check
///      `plugin.status === ready` and `worker_manager.is_running(plugin.id)`.
///   4. Return the first match, or `None`.
pub fn resolve_sandbox_provider_driver_key(
    registry: &dyn PluginRegistry,
    worker_manager: Option<&dyn PluginWorkerManager>,
    driver_key: &str,
    require_running: bool,
) -> Option<ResolvedSandboxProviderDriver> {
    for plugin in registry.list() {
        let driver = plugin.environment_drivers.iter().find(|d| {
            d.driver_key == driver_key && d.kind == PluginDriverKind::SandboxProvider
        });
        let driver = match driver {
            Some(d) => d.clone(),
            None => continue,
        };
        if require_running {
            if plugin.status != PluginStatus::Ready {
                continue;
            }
            let wm = match worker_manager {
                Some(w) => w,
                None => return None, // Node returns null when worker_manager missing + requireRunning
            };
            if !wm.is_running(&plugin.id) {
                continue;
            }
        }
        return Some(ResolvedSandboxProviderDriver {
            plugin,
            driver,
        });
    }
    None
}

// ---------------------------------------------------------------------------
// Companion helper: listReadyPluginEnvironmentDrivers (subset)
// ---------------------------------------------------------------------------

/// Result row mirroring Node `ReadyPluginEnvironmentDriver`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadyPluginEnvironmentDriver {
    pub plugin_id: String,
    pub plugin_key: String,
    pub driver_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<Value>,
    // R689 parity: extended fields mirroring Node `ReadyPluginEnvironmentDriver`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_reusable_leases: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_interactive_setup: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interactive_setup_connection_types: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_template_capture: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_ref_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_config_binding: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_template_delete: Option<bool>,
}

/// Pure subset of `listReadyPluginEnvironmentDrivers`: returns the
/// sandbox-provider drivers exposed by ready plugins whose worker is running.
pub fn list_ready_sandbox_provider_drivers(
    registry: &dyn PluginRegistry,
    worker_manager: &dyn PluginWorkerManager,
) -> Vec<ReadyPluginEnvironmentDriver> {
    let mut rows = Vec::new();
    for plugin in registry.list() {
        if plugin.status != PluginStatus::Ready {
            continue;
        }
        if !worker_manager.is_running(&plugin.id) {
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
                interactive_setup_connection_types: driver.interactive_setup_connection_types.clone(),
                supports_template_capture: driver.supports_template_capture,
                template_ref_kind: driver.template_ref_kind.clone(),
                template_config_binding: driver.template_config_binding.clone(),
                supports_template_delete: driver.supports_template_delete,
            });
        }
    }
    rows
}

// Force trait imports.
#[allow(dead_code)]
fn _force_inspect_import(w: &dyn PluginWorkerManagerInspect) -> bool {
    w.is_running("placeholder")
}
