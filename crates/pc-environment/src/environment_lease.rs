// SPDX-License-Identifier: MIT
//
// R690 parity: resumePluginEnvironmentLease + destroyPluginEnvironmentLease.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::config::PluginEnvironmentConfig;
use crate::plugin_registry::{PluginRegistry, ResolvedSandboxProviderDriver};
use crate::plugin_worker_manager::{PluginRpcError, PluginWorkerManager};
use crate::validate_environment_driver::{
    resolve_plugin_environment_driver, ResolveEnvironmentDriverError,
};

// ---------------------------------------------------------------------------
// Result type — mirrors Node PluginEnvironmentLease.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginEnvironmentLease {
    #[serde(default)]
    pub provider_lease_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

impl PluginEnvironmentLease {
    /// Construct from the raw worker response (Value). The worker's response
    /// uses camelCase wire keys; serde camelCase rename handles conversion.
    pub fn from_worker_payload(payload: Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(payload)
    }
}

// ---------------------------------------------------------------------------
// Errors.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum ResumeEnvironmentLeaseError {
    Resolve(ResolveEnvironmentDriverError),
    WorkerRpc(PluginRpcError),
    InvalidPayload(String),
}

impl std::fmt::Display for ResumeEnvironmentLeaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resolve(e) => write!(f, "{}", e),
            Self::WorkerRpc(e) => write!(f, "{}", e),
            Self::InvalidPayload(msg) => write!(f, "plugin worker returned invalid lease payload: {}", msg),
        }
    }
}

impl std::error::Error for ResumeEnvironmentLeaseError {}

impl From<ResolveEnvironmentDriverError> for ResumeEnvironmentLeaseError {
    fn from(e: ResolveEnvironmentDriverError) -> Self {
        Self::Resolve(e)
    }
}

impl From<PluginRpcError> for ResumeEnvironmentLeaseError {
    fn from(e: PluginRpcError) -> Self {
        Self::WorkerRpc(e)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DestroyEnvironmentLeaseError {
    Resolve(ResolveEnvironmentDriverError),
    WorkerRpc(PluginRpcError),
}

impl std::fmt::Display for DestroyEnvironmentLeaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resolve(e) => write!(f, "{}", e),
            Self::WorkerRpc(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for DestroyEnvironmentLeaseError {}

impl From<ResolveEnvironmentDriverError> for DestroyEnvironmentLeaseError {
    fn from(e: ResolveEnvironmentDriverError) -> Self {
        Self::Resolve(e)
    }
}

impl From<PluginRpcError> for DestroyEnvironmentLeaseError {
    fn from(e: PluginRpcError) -> Self {
        Self::WorkerRpc(e)
    }
}


// ---------------------------------------------------------------------------
// resumePluginEnvironmentLease — 1:1 with Node.
// ---------------------------------------------------------------------------

pub fn resume_plugin_environment_lease(
    registry: &dyn PluginRegistry,
    worker_manager: &dyn PluginWorkerManager,
    company_id: &str,
    environment_id: &str,
    issue_id: Option<&str>,
    config: &PluginEnvironmentConfig,
    provider_lease_id: &str,
    lease_metadata: Option<&Map<String, Value>>,
) -> Result<PluginEnvironmentLease, ResumeEnvironmentLeaseError> {
    let resolved: ResolvedSandboxProviderDriver =
        resolve_plugin_environment_driver(registry, worker_manager, config)?;

    let params = json!({
        "driverKey": config.driver_key,
        "companyId": company_id,
        "environmentId": environment_id,
        "issueId": issue_id,
        "config": config.driver_config,
        "providerLeaseId": provider_lease_id,
        "leaseMetadata": lease_metadata,
    });

    let payload: Value = worker_manager.call_raw(
        &resolved.plugin.id,
        "environmentResumeLease",
        params,
        None,
    )?;

    PluginEnvironmentLease::from_worker_payload(payload)
        .map_err(|e| ResumeEnvironmentLeaseError::InvalidPayload(e.to_string()))
}

// ---------------------------------------------------------------------------
// destroyPluginEnvironmentLease — 1:1 with Node.
// ---------------------------------------------------------------------------

pub fn destroy_plugin_environment_lease(
    registry: &dyn PluginRegistry,
    worker_manager: &dyn PluginWorkerManager,
    company_id: &str,
    environment_id: &str,
    issue_id: Option<&str>,
    config: &PluginEnvironmentConfig,
    provider_lease_id: Option<&str>,
    lease_metadata: Option<&Map<String, Value>>,
) -> Result<(), DestroyEnvironmentLeaseError> {
    let resolved: ResolvedSandboxProviderDriver =
        resolve_plugin_environment_driver(registry, worker_manager, config)?;

    let params = json!({
        "driverKey": config.driver_key,
        "companyId": company_id,
        "environmentId": environment_id,
        "issueId": issue_id,
        "config": config.driver_config,
        "providerLeaseId": provider_lease_id,
        "leaseMetadata": lease_metadata,
    });

    let _payload: Value = worker_manager.call_raw(
        &resolved.plugin.id,
        "environmentDestroyLease",
        params,
        None,
    )?;

    Ok(())
}
