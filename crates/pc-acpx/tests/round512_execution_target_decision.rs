use pc_acpx::execution_target::{
    resolve_adapter_execution_target_decision, AdapterExecutionTargetKind,
    AdapterExecutionTargetSessionIdentity, AdapterExecutionTargetTimeoutSource,
    AdapterManagedRuntimeStrategy, ResolveAdapterExecutionTargetDecisionInput,
};
use serde_json::json;

#[test]
fn local_decision_keeps_direct_execution_defaults() {
    let decision =
        resolve_adapter_execution_target_decision(&ResolveAdapterExecutionTargetDecisionInput {
            execution_target: None,
            legacy_remote_execution: None,
            environment_id: None,
            lease_id: None,
            configured_cwd: None,
            local_fallback_cwd: "/workspace/local",
            configured_timeout_sec: Some(0.0),
            sandbox_runner_available: false,
            agent_command_shell: None,
        });

    assert_eq!(decision.kind, AdapterExecutionTargetKind::Local);
    assert_eq!(
        decision.runtime_strategy,
        AdapterManagedRuntimeStrategy::DirectLocal
    );
    assert_eq!(decision.execution_cwd, "/workspace/local");
    assert_eq!(decision.timeout.timeout_sec, 0.0);
    assert_eq!(
        decision.timeout.source,
        AdapterExecutionTargetTimeoutSource::Unlimited
    );
    assert!(!decision.is_remote);
    assert!(!decision.requires_managed_runtime_stage);
    assert!(!decision.uses_managed_home);
    assert!(!decision.uses_paperclip_bridge);
    assert!(!decision.uses_remote_process_session);
    assert!(decision.remote_execution_identity.is_none());
}

#[test]
fn ssh_decision_uses_authoritative_in_place_root_and_bridge() {
    let target = json!({
        "kind": "remote",
        "transport": "ssh",
        "environmentId": "env-ssh",
        "leaseId": "lease-ssh",
        "remoteCwd": "/remote/original",
        "spec": {
            "host": "example.test",
            "port": 2222,
            "username": "paperclip",
            "remoteCwd": "/remote/original"
        },
        "workspaceRealization": {
            "mode": "in_place",
            "authoritativeRoot": "/remote/authoritative",
            "pathAliases": [],
            "outboundRestorePaths": []
        }
    });

    let decision =
        resolve_adapter_execution_target_decision(&ResolveAdapterExecutionTargetDecisionInput {
            execution_target: Some(&target),
            legacy_remote_execution: None,
            environment_id: None,
            lease_id: None,
            configured_cwd: Some("/configured/ignored-by-in-place"),
            local_fallback_cwd: "/workspace/local",
            configured_timeout_sec: None,
            sandbox_runner_available: false,
            agent_command_shell: Some("claude --print"),
        });

    assert_eq!(decision.kind, AdapterExecutionTargetKind::Ssh);
    assert_eq!(
        decision.runtime_strategy,
        AdapterManagedRuntimeStrategy::SshManaged
    );
    assert_eq!(decision.execution_cwd, "/remote/authoritative");
    assert!(decision.is_remote);
    assert!(decision.requires_managed_runtime_stage);
    assert!(!decision.uses_managed_home);
    assert!(decision.uses_paperclip_bridge);
    assert!(!decision.uses_remote_process_session);
    assert!(matches!(
        decision.remote_execution_identity,
        Some(AdapterExecutionTargetSessionIdentity::Ssh(_))
    ));
}

#[test]
fn sandbox_decision_applies_default_timeout_and_process_session_gate() {
    let target = json!({
        "kind": "remote",
        "transport": "sandbox",
        "providerKey": "daytona",
        "environmentId": "env-sandbox",
        "leaseId": "lease-sandbox",
        "remoteCwd": "/sandbox/workspace",
        "timeoutMs": 60000,
        "streamRunLogs": true
    });

    let decision =
        resolve_adapter_execution_target_decision(&ResolveAdapterExecutionTargetDecisionInput {
            execution_target: Some(&target),
            legacy_remote_execution: None,
            environment_id: None,
            lease_id: None,
            configured_cwd: None,
            local_fallback_cwd: "/workspace/local",
            configured_timeout_sec: Some(0.0),
            sandbox_runner_available: true,
            agent_command_shell: Some("codex acp"),
        });

    assert_eq!(decision.kind, AdapterExecutionTargetKind::Sandbox);
    assert_eq!(
        decision.runtime_strategy,
        AdapterManagedRuntimeStrategy::SandboxManaged
    );
    assert_eq!(decision.execution_cwd, "/sandbox/workspace");
    assert_eq!(decision.timeout.timeout_sec, 14_400.0);
    assert_eq!(
        decision.timeout.source,
        AdapterExecutionTargetTimeoutSource::SandboxDefault
    );
    assert!(decision.is_remote);
    assert!(decision.requires_managed_runtime_stage);
    assert!(decision.uses_managed_home);
    assert!(decision.uses_paperclip_bridge);
    assert!(decision.uses_remote_process_session);
    assert!(matches!(
        decision.remote_execution_identity,
        Some(AdapterExecutionTargetSessionIdentity::Sandbox(_))
    ));
}

#[test]
fn process_session_requires_runner_and_non_empty_agent_command() {
    let target = json!({
        "kind": "remote",
        "transport": "sandbox",
        "remoteCwd": "/sandbox/workspace"
    });
    let resolve = |sandbox_runner_available, agent_command_shell| {
        resolve_adapter_execution_target_decision(&ResolveAdapterExecutionTargetDecisionInput {
            execution_target: Some(&target),
            legacy_remote_execution: None,
            environment_id: None,
            lease_id: None,
            configured_cwd: None,
            local_fallback_cwd: "/workspace/local",
            configured_timeout_sec: None,
            sandbox_runner_available,
            agent_command_shell,
        })
    };

    assert!(!resolve(false, Some("codex acp")).uses_remote_process_session);
    assert!(!resolve(true, Some("   ")).uses_remote_process_session);
    assert!(!resolve(true, None).uses_remote_process_session);
}

#[test]
fn invalid_typed_target_falls_back_to_legacy_ssh() {
    let invalid_target = json!({ "kind": "remote", "transport": "unknown" });
    let legacy = json!({
        "host": "legacy.example.test",
        "port": 22,
        "username": "legacy",
        "remoteCwd": "/legacy/workspace"
    });

    let decision =
        resolve_adapter_execution_target_decision(&ResolveAdapterExecutionTargetDecisionInput {
            execution_target: Some(&invalid_target),
            legacy_remote_execution: Some(&legacy),
            environment_id: Some("env-legacy"),
            lease_id: Some("lease-legacy"),
            configured_cwd: None,
            local_fallback_cwd: "/workspace/local",
            configured_timeout_sec: Some(-1.0),
            sandbox_runner_available: false,
            agent_command_shell: None,
        });

    assert_eq!(decision.kind, AdapterExecutionTargetKind::Ssh);
    assert_eq!(decision.execution_cwd, "/legacy/workspace");
    assert_eq!(decision.timeout.timeout_sec, 0.0);
    assert_eq!(
        decision.timeout.source,
        AdapterExecutionTargetTimeoutSource::Configured
    );
    let target = decision
        .target
        .expect("legacy target should be materialized");
    assert_eq!(
        target
            .as_ssh()
            .and_then(|ssh| ssh.environment_id.as_deref()),
        Some("env-legacy")
    );
    assert_eq!(
        target.as_ssh().and_then(|ssh| ssh.lease_id.as_deref()),
        Some("lease-legacy")
    );
}
