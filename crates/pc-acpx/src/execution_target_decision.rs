//! High-cohesion execution-target decision layer shared by adapters.
//!
//! This module owns only pure policy: target source precedence, target kind,
//! effective cwd, timeout, managed-runtime strategy, bridge gates, and resume
//! identity. Staging, bridge startup, process spawning, and teardown remain in
//! their dedicated I/O modules.

use serde::{Deserialize, Serialize};

use crate::execution_target::{
    adapter_execution_target_from_remote_execution, adapter_execution_target_is_remote,
    adapter_execution_target_session_identity, adapter_execution_target_uses_managed_home,
    adapter_execution_target_uses_paperclip_bridge, describe_adapter_execution_target,
    effective_execution_cwd, parse_adapter_execution_target, resolve_adapter_execution_target_cwd,
    resolve_adapter_execution_target_timeout, AdapterExecutionTarget,
    AdapterExecutionTargetSessionIdentity, AdapterExecutionTargetTimeoutResolution,
    AdapterLocalExecutionTargetMetadata, AdapterRemoteExecutionTarget, AdapterWorkspaceRealization,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterExecutionTargetKind {
    Local,
    Ssh,
    Sandbox,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterManagedRuntimeStrategy {
    DirectLocal,
    SshManaged,
    SandboxManaged,
}

#[derive(Debug, Clone)]
pub struct ResolveAdapterExecutionTargetDecisionInput<'a> {
    pub execution_target: Option<&'a serde_json::Value>,
    pub legacy_remote_execution: Option<&'a serde_json::Value>,
    pub environment_id: Option<&'a str>,
    pub lease_id: Option<&'a str>,
    pub configured_cwd: Option<&'a str>,
    pub local_fallback_cwd: &'a str,
    pub configured_timeout_sec: Option<f64>,
    pub sandbox_runner_available: bool,
    pub agent_command_shell: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterExecutionTargetDecision {
    pub target: Option<AdapterExecutionTarget>,
    pub kind: AdapterExecutionTargetKind,
    pub runtime_strategy: AdapterManagedRuntimeStrategy,
    pub description: String,
    pub execution_cwd: String,
    pub workspace_realization: Option<AdapterWorkspaceRealization>,
    pub remote_execution_identity: Option<AdapterExecutionTargetSessionIdentity>,
    pub timeout: AdapterExecutionTargetTimeoutResolution,
    pub is_remote: bool,
    pub requires_managed_runtime_stage: bool,
    pub uses_managed_home: bool,
    pub uses_paperclip_bridge: bool,
    pub uses_remote_process_session: bool,
}

#[must_use]
pub fn resolve_adapter_execution_target_decision(
    input: &ResolveAdapterExecutionTargetDecisionInput<'_>,
) -> AdapterExecutionTargetDecision {
    let parsed_target = input
        .execution_target
        .and_then(parse_adapter_execution_target);
    let target = parsed_target.or_else(|| {
        input.legacy_remote_execution.and_then(|legacy| {
            adapter_execution_target_from_remote_execution(
                legacy,
                Some(AdapterLocalExecutionTargetMetadata {
                    environment_id: input.environment_id.map(str::to_owned),
                    lease_id: input.lease_id.map(str::to_owned),
                }),
            )
        })
    });

    let kind = match target.as_ref() {
        Some(AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Ssh(_))) => {
            AdapterExecutionTargetKind::Ssh
        }
        Some(AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Sandbox(_))) => {
            AdapterExecutionTargetKind::Sandbox
        }
        Some(AdapterExecutionTarget::Local(_)) | None => AdapterExecutionTargetKind::Local,
    };
    let runtime_strategy = match kind {
        AdapterExecutionTargetKind::Local => AdapterManagedRuntimeStrategy::DirectLocal,
        AdapterExecutionTargetKind::Ssh => AdapterManagedRuntimeStrategy::SshManaged,
        AdapterExecutionTargetKind::Sandbox => AdapterManagedRuntimeStrategy::SandboxManaged,
    };
    let workspace_realization = target
        .as_ref()
        .and_then(target_workspace_realization)
        .cloned();
    let base_cwd = resolve_adapter_execution_target_cwd(
        target.as_ref(),
        input.configured_cwd,
        input.local_fallback_cwd,
    );
    let execution_cwd =
        effective_execution_cwd(workspace_realization.as_ref(), target.as_ref(), &base_cwd);
    let timeout =
        resolve_adapter_execution_target_timeout(target.as_ref(), input.configured_timeout_sec);
    let is_remote = adapter_execution_target_is_remote(target.as_ref());
    let uses_remote_process_session = kind == AdapterExecutionTargetKind::Sandbox
        && input.sandbox_runner_available
        && input
            .agent_command_shell
            .map(str::trim)
            .is_some_and(|command| !command.is_empty());

    AdapterExecutionTargetDecision {
        description: describe_adapter_execution_target(target.as_ref()),
        runtime_strategy,
        kind,
        execution_cwd,
        workspace_realization,
        remote_execution_identity: adapter_execution_target_session_identity(target.as_ref()),
        timeout,
        is_remote,
        requires_managed_runtime_stage: is_remote,
        uses_managed_home: adapter_execution_target_uses_managed_home(target.as_ref()),
        uses_paperclip_bridge: adapter_execution_target_uses_paperclip_bridge(target.as_ref()),
        uses_remote_process_session,
        target,
    }
}

fn target_workspace_realization(
    target: &AdapterExecutionTarget,
) -> Option<&AdapterWorkspaceRealization> {
    match target {
        AdapterExecutionTarget::Local(local) => local.workspace_realization.as_ref(),
        AdapterExecutionTarget::Remote(remote) => remote.workspace_realization(),
    }
}
