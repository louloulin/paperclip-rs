// SPDX-License-Identifier: MIT
//
// R692 parity: startPluginEnvironmentInteractiveSetup +
// getPluginEnvironmentInteractiveSetup.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::config::PluginEnvironmentConfig;
use crate::plugin_environment_driver_pure::resolve_plugin_execute_rpc_timeout_ms;
use crate::plugin_registry::PluginRegistry;
use crate::plugin_worker_manager::{PluginRpcError, PluginWorkerManager};
use crate::validate_environment_driver::{resolve_plugin_environment_driver, ResolveEnvironmentDriverError};


// ---------------------------------------------------------------------------
// Enums (mirror Node union types).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PluginEnvironmentInteractiveSetupStatus {
    #[default]
    Starting,
    WaitingForUser,
    Capturing,
    Promoted,
    Cancelled,
    TimedOut,
    Failed,
    Missing,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PluginEnvironmentTemplateRefKind {
    Snapshot,
    Image,
    ProviderTemplate,
    #[default]
    Unknown,
}


// ---------------------------------------------------------------------------
// Connection types.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginEnvironmentInteractiveSetupConnectionSummary {
    #[serde(rename = "type")]
    pub connection_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    pub host_redacted: bool,
    pub port_redacted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_redacted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub metadata: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginEnvironmentInteractiveSetupConnectionPayload {
    #[serde(rename = "type")]
    pub connection_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub metadata: Map<String, Value>,
}

// ---------------------------------------------------------------------------
// Session — returned by both start and get.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginEnvironmentInteractiveSetupSession {
    pub provider_lease_id: Option<String>,
    pub status: PluginEnvironmentInteractiveSetupStatus,
    pub connection_summary: Option<PluginEnvironmentInteractiveSetupConnectionSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_payload: Option<PluginEnvironmentInteractiveSetupConnectionPayload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub metadata: Map<String, Value>,
}


// ---------------------------------------------------------------------------
// Params (mirror Node Start/Get InteractiveSetup Params).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginEnvironmentStartInteractiveSetupParams {
    // base
    pub driver_key: String,
    pub company_id: String,
    pub environment_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<String>,
    #[serde(default)]
    pub config: Map<String, Value>,
    // start-specific
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_template_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_template_kind: Option<PluginEnvironmentTemplateRefKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_expires_in_minutes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginEnvironmentGetInteractiveSetupParams {
    // base
    pub driver_key: String,
    pub company_id: String,
    pub environment_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<String>,
    #[serde(default)]
    pub config: Map<String, Value>,
    // get-specific
    pub provider_lease_id: Option<String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub setup_metadata: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_connection_payload: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_expires_in_minutes: Option<u32>,
}


// ---------------------------------------------------------------------------
// Errors.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum SetupError {
    Resolve(ResolveEnvironmentDriverError),
    WorkerRpc(PluginRpcError),
    Serialization(String),
    InvalidPayload(String),
}

impl std::fmt::Display for SetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resolve(e) => write!(f, "{}", e),
            Self::WorkerRpc(e) => write!(f, "{}", e),
            Self::Serialization(msg) => write!(f, "failed to serialize params: {}", msg),
            Self::InvalidPayload(msg) => write!(f, "plugin worker returned invalid setup session: {}", msg),
        }
    }
}

impl std::error::Error for SetupError {}

impl From<ResolveEnvironmentDriverError> for SetupError {
    fn from(e: ResolveEnvironmentDriverError) -> Self {
        Self::Resolve(e)
    }
}

impl From<PluginRpcError> for SetupError {
    fn from(e: PluginRpcError) -> Self {
        Self::WorkerRpc(e)
    }
}

impl From<serde_json::Error> for SetupError {
    fn from(e: serde_json::Error) -> Self {
        Self::Serialization(e.to_string())
    }
}

// ---------------------------------------------------------------------------
// startPluginEnvironmentInteractiveSetup — 1:1 with Node.
// ---------------------------------------------------------------------------

pub fn start_plugin_environment_interactive_setup(
    registry: &dyn PluginRegistry,
    worker_manager: &dyn PluginWorkerManager,
    config: &PluginEnvironmentConfig,
    params: &PluginEnvironmentStartInteractiveSetupParams,
) -> Result<PluginEnvironmentInteractiveSetupSession, SetupError> {
    let resolved = resolve_plugin_environment_driver(registry, worker_manager, config)?;

    // Build the wire params: take caller's params, override driverKey + config
    // from the config arg (matches Node behavior exactly).
    let mut wire_params = serde_json::to_value(params)?;
    if let Some(obj) = wire_params.as_object_mut() {
        obj.insert("driverKey".to_string(), Value::String(config.driver_key.clone()));
        obj.insert("config".to_string(), Value::Object(config.driver_config.clone()));
    }

    // Resolve RPC timeout using config.driver_config (no requestedTimeout).
    let timeout_ms = resolve_plugin_execute_rpc_timeout_ms(
        None,
        &Value::Object(config.driver_config.clone()),
    );

    let result = worker_manager.call_raw(
        &resolved.plugin.id,
        "environmentStartInteractiveSetup",
        wire_params,
        timeout_ms,
    )?;
    serde_json::from_value(result).map_err(|e| SetupError::InvalidPayload(e.to_string()))
}

// ---------------------------------------------------------------------------
// getPluginEnvironmentInteractiveSetup — 1:1 with Node.
// ---------------------------------------------------------------------------

pub fn get_plugin_environment_interactive_setup(
    registry: &dyn PluginRegistry,
    worker_manager: &dyn PluginWorkerManager,
    config: &PluginEnvironmentConfig,
    params: &PluginEnvironmentGetInteractiveSetupParams,
) -> Result<PluginEnvironmentInteractiveSetupSession, SetupError> {
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
        "environmentGetInteractiveSetup",
        wire_params,
        timeout_ms,
    )?;
    serde_json::from_value(result).map_err(|e| SetupError::InvalidPayload(e.to_string()))
}
