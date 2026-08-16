// SPDX-License-Identifier: MIT
//
// R691 parity: realizePluginEnvironmentWorkspace +
// executePluginEnvironmentCommand.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;

use crate::config::PluginEnvironmentConfig;
use crate::environment_lease::PluginEnvironmentLease;
use crate::plugin_environment_driver_pure::resolve_plugin_execute_rpc_timeout_ms;
use crate::plugin_registry::PluginRegistry;
use crate::plugin_worker_manager::{PluginRpcError, PluginWorkerManager};
use crate::validate_environment_driver::ResolveEnvironmentDriverError;


// ---------------------------------------------------------------------------
// Param / Result types — mirror Node PluginEnvironment*Params / *Result.
// All structs use serde camelCase rename to match the wire format.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginEnvironmentWorkspaceSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub metadata: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginEnvironmentRealizeWorkspaceParams {
    pub driver_key: String,
    pub company_id: String,
    pub environment_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<String>,
    #[serde(default)]
    pub config: Map<String, Value>,
    pub lease: PluginEnvironmentLease,
    pub workspace: PluginEnvironmentWorkspaceSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginEnvironmentRealizeWorkspaceResult {
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub metadata: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginEnvironmentExecuteParams {
    pub driver_key: String,
    pub company_id: String,
    pub environment_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<String>,
    #[serde(default)]
    pub config: Map<String, Value>,
    pub lease: PluginEnvironmentLease,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginEnvironmentExecuteResult {
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    #[serde(default)]
    pub timed_out: bool,
}


// ---------------------------------------------------------------------------
// Errors.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum WorkspaceError {
    Resolve(ResolveEnvironmentDriverError),
    WorkerRpc(PluginRpcError),
    InvalidPayload(String),
    Serialization(String),
}

impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resolve(e) => write!(f, "{}", e),
            Self::WorkerRpc(e) => write!(f, "{}", e),
            Self::InvalidPayload(msg) => write!(f, "plugin worker returned invalid workspace payload: {}", msg),
            Self::Serialization(msg) => write!(f, "failed to serialize params: {}", msg),
        }
    }
}

impl std::error::Error for WorkspaceError {}

impl From<ResolveEnvironmentDriverError> for WorkspaceError {
    fn from(e: ResolveEnvironmentDriverError) -> Self {
        Self::Resolve(e)
    }
}

impl From<PluginRpcError> for WorkspaceError {
    fn from(e: PluginRpcError) -> Self {
        Self::WorkerRpc(e)
    }
}

impl From<serde_json::Error> for WorkspaceError {
    fn from(e: serde_json::Error) -> Self {
        Self::Serialization(e.to_string())
    }
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn resolve_plugin_id(
    registry: &dyn PluginRegistry,
    worker_manager: &dyn PluginWorkerManager,
    plugin_id: Option<&str>,
    config: &PluginEnvironmentConfig,
) -> Result<String, WorkspaceError> {
    match plugin_id {
        Some(id) => Ok(id.to_string()),
        None => {
            let resolved = crate::validate_environment_driver::resolve_plugin_environment_driver(
                registry,
                worker_manager,
                config,
            )?;
            Ok(resolved.plugin.id)
        }
    }
}


// ---------------------------------------------------------------------------
// realizePluginEnvironmentWorkspace — 1:1 with Node.
// ---------------------------------------------------------------------------

pub fn realize_plugin_environment_workspace(
    registry: &dyn PluginRegistry,
    worker_manager: &dyn PluginWorkerManager,
    plugin_id: Option<&str>,
    params: &PluginEnvironmentRealizeWorkspaceParams,
    config: &PluginEnvironmentConfig,
) -> Result<PluginEnvironmentRealizeWorkspaceResult, WorkspaceError> {
    let resolved_id = resolve_plugin_id(registry, worker_manager, plugin_id, config)?;
    let payload = serde_json::to_value(params)?;
    let result = worker_manager.call_raw(
        &resolved_id,
        "environmentRealizeWorkspace",
        payload,
        None,
    )?;
    serde_json::from_value(result).map_err(|e| WorkspaceError::InvalidPayload(e.to_string()))
}

// ---------------------------------------------------------------------------
// executePluginEnvironmentCommand — 1:1 with Node.
// ---------------------------------------------------------------------------

pub fn execute_plugin_environment_command(
    registry: &dyn PluginRegistry,
    worker_manager: &dyn PluginWorkerManager,
    plugin_id: Option<&str>,
    params: &PluginEnvironmentExecuteParams,
    config: &PluginEnvironmentConfig,
) -> Result<PluginEnvironmentExecuteResult, WorkspaceError> {
    let resolved_id = resolve_plugin_id(registry, worker_manager, plugin_id, config)?;

    // Resolve RPC timeout the same way Node does.
    let timeout_ms = resolve_plugin_execute_rpc_timeout_ms(
        params.timeout_ms.map(|x| x as f64),
        &Value::Object(config.driver_config.clone()),
    );

    let payload = serde_json::to_value(params)?;
    let result = worker_manager.call_raw(
        &resolved_id,
        "environmentExecute",
        payload,
        timeout_ms,
    )?;
    serde_json::from_value(result).map_err(|e| WorkspaceError::InvalidPayload(e.to_string()))
}
