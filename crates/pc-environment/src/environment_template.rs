// SPDX-License-Identifier: MIT
//
// R693 parity: capturePluginEnvironmentTemplate +
// cancelPluginEnvironmentInteractiveSetup + deletePluginEnvironmentTemplate.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::config::PluginEnvironmentConfig;
use crate::environment_setup::PluginEnvironmentInteractiveSetupStatus;
use crate::environment_setup::PluginEnvironmentTemplateRefKind;
use crate::plugin_environment_driver_pure::resolve_plugin_execute_rpc_timeout_ms;
use crate::plugin_registry::PluginRegistry;
use crate::plugin_worker_manager::{PluginRpcError, PluginWorkerManager};
use crate::validate_environment_driver::{resolve_plugin_environment_driver, ResolveEnvironmentDriverError};


// ---------------------------------------------------------------------------
// capturePluginEnvironmentTemplate — params + result.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginEnvironmentCaptureTemplateParams {
    // base
    pub driver_key: String,
    pub company_id: String,
    pub environment_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<String>,
    #[serde(default)]
    pub config: Map<String, Value>,
    // capture-specific
    pub provider_lease_id: Option<String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub setup_metadata: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_template_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_template_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginEnvironmentCaptureTemplateResult {
    pub template_ref: String,
    pub template_kind: PluginEnvironmentTemplateRefKind,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub metadata: Map<String, Value>,
}


// ---------------------------------------------------------------------------
// cancelPluginEnvironmentInteractiveSetup — params + result.
// Status is a subset of PluginEnvironmentInteractiveSetupStatus: cancelled,
// timed_out, failed, missing.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginEnvironmentCancelInteractiveSetupParams {
    // base
    pub driver_key: String,
    pub company_id: String,
    pub environment_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<String>,
    #[serde(default)]
    pub config: Map<String, Value>,
    // cancel-specific
    pub provider_lease_id: Option<String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub setup_metadata: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginEnvironmentCancelInteractiveSetupResult {
    pub status: PluginEnvironmentInteractiveSetupStatus,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub metadata: Map<String, Value>,
}


// ---------------------------------------------------------------------------
// deletePluginEnvironmentTemplate — params + result.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginEnvironmentDeleteTemplateParams {
    // base
    pub driver_key: String,
    pub company_id: String,
    pub environment_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<String>,
    #[serde(default)]
    pub config: Map<String, Value>,
    // delete-specific
    pub template_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_kind: Option<PluginEnvironmentTemplateRefKind>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub metadata: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginEnvironmentDeleteTemplateResult {
    pub deleted: bool,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub metadata: Map<String, Value>,
}


// ---------------------------------------------------------------------------
// Errors.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum TemplateError {
    Resolve(ResolveEnvironmentDriverError),
    WorkerRpc(PluginRpcError),
    Serialization(String),
    InvalidPayload(String),
}

impl std::fmt::Display for TemplateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resolve(e) => write!(f, "{}", e),
            Self::WorkerRpc(e) => write!(f, "{}", e),
            Self::Serialization(msg) => write!(f, "failed to serialize params: {}", msg),
            Self::InvalidPayload(msg) => write!(f, "plugin worker returned invalid template payload: {}", msg),
        }
    }
}

impl std::error::Error for TemplateError {}

impl From<ResolveEnvironmentDriverError> for TemplateError {
    fn from(e: ResolveEnvironmentDriverError) -> Self {
        Self::Resolve(e)
    }
}

impl From<PluginRpcError> for TemplateError {
    fn from(e: PluginRpcError) -> Self {
        Self::WorkerRpc(e)
    }
}

impl From<serde_json::Error> for TemplateError {
    fn from(e: serde_json::Error) -> Self {
        Self::Serialization(e.to_string())
    }
}

// ---------------------------------------------------------------------------
// capturePluginEnvironmentTemplate — 1:1 with Node.
// ---------------------------------------------------------------------------

pub fn capture_plugin_environment_template(
    registry: &dyn PluginRegistry,
    worker_manager: &dyn PluginWorkerManager,
    config: &PluginEnvironmentConfig,
    params: &PluginEnvironmentCaptureTemplateParams,
) -> Result<PluginEnvironmentCaptureTemplateResult, TemplateError> {
    let resolved = resolve_plugin_environment_driver(registry, worker_manager, config)?;

    let mut wire_params = serde_json::to_value(params)?;
    if let Some(obj) = wire_params.as_object_mut() {
        obj.insert("driverKey".to_string(), Value::String(config.driver_key.clone()));
        obj.insert("config".to_string(), Value::Object(config.driver_config.clone()));
    }

    // capture uses params.timeoutMs (per-call) with config.driver_config fallback.
    let timeout_ms = resolve_plugin_execute_rpc_timeout_ms(
        params.timeout_ms.map(|x| x as f64),
        &Value::Object(config.driver_config.clone()),
    );

    let result = worker_manager.call_raw(
        &resolved.plugin.id,
        "environmentCaptureTemplate",
        wire_params,
        timeout_ms,
    )?;
    serde_json::from_value(result).map_err(|e| TemplateError::InvalidPayload(e.to_string()))
}

// ---------------------------------------------------------------------------
// cancelPluginEnvironmentInteractiveSetup — 1:1 with Node.
// ---------------------------------------------------------------------------

pub fn cancel_plugin_environment_interactive_setup(
    registry: &dyn PluginRegistry,
    worker_manager: &dyn PluginWorkerManager,
    config: &PluginEnvironmentConfig,
    params: &PluginEnvironmentCancelInteractiveSetupParams,
) -> Result<PluginEnvironmentCancelInteractiveSetupResult, TemplateError> {
    let resolved = resolve_plugin_environment_driver(registry, worker_manager, config)?;

    let mut wire_params = serde_json::to_value(params)?;
    if let Some(obj) = wire_params.as_object_mut() {
        obj.insert("driverKey".to_string(), Value::String(config.driver_key.clone()));
        obj.insert("config".to_string(), Value::Object(config.driver_config.clone()));
    }

    // cancel uses config.driver_config.timeoutMs fallback only (no params.timeoutMs).
    let timeout_ms = resolve_plugin_execute_rpc_timeout_ms(
        None,
        &Value::Object(config.driver_config.clone()),
    );

    let result = worker_manager.call_raw(
        &resolved.plugin.id,
        "environmentCancelInteractiveSetup",
        wire_params,
        timeout_ms,
    )?;
    serde_json::from_value(result).map_err(|e| TemplateError::InvalidPayload(e.to_string()))
}

// ---------------------------------------------------------------------------
// deletePluginEnvironmentTemplate — 1:1 with Node.
// ---------------------------------------------------------------------------

pub fn delete_plugin_environment_template(
    registry: &dyn PluginRegistry,
    worker_manager: &dyn PluginWorkerManager,
    config: &PluginEnvironmentConfig,
    params: &PluginEnvironmentDeleteTemplateParams,
) -> Result<PluginEnvironmentDeleteTemplateResult, TemplateError> {
    let resolved = resolve_plugin_environment_driver(registry, worker_manager, config)?;

    let mut wire_params = serde_json::to_value(params)?;
    if let Some(obj) = wire_params.as_object_mut() {
        obj.insert("driverKey".to_string(), Value::String(config.driver_key.clone()));
        obj.insert("config".to_string(), Value::Object(config.driver_config.clone()));
    }

    let timeout_ms = resolve_plugin_execute_rpc_timeout_ms(
        None,
        &Value::Object(config.driver_config.clone()),
    );

    let result = worker_manager.call_raw(
        &resolved.plugin.id,
        "environmentDeleteTemplate",
        wire_params,
        timeout_ms,
    )?;
    serde_json::from_value(result).map_err(|e| TemplateError::InvalidPayload(e.to_string()))
}
