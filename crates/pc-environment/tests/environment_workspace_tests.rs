// SPDX-License-Identifier: MIT
//
// R691 parity tests for realizePluginEnvironmentWorkspace +
// executePluginEnvironmentCommand.

use std::collections::HashMap;

use pc_environment::PluginEnvironmentConfig;
use pc_environment::environment_lease::PluginEnvironmentLease;
use pc_environment::environment_workspace::{
    execute_plugin_environment_command, realize_plugin_environment_workspace,
    PluginEnvironmentExecuteParams, PluginEnvironmentRealizeWorkspaceParams,
    PluginEnvironmentRealizeWorkspaceResult, PluginEnvironmentWorkspaceSpec,
    WorkspaceError,
};
use pc_environment::plugin_registry::{
    InMemoryPluginRegistry, PluginDriverKind, PluginEnvironmentDriverDecl, PluginRow,
    PluginStatus,
};
use pc_environment::plugin_worker_manager::{
    InMemoryPluginWorkerManager, PluginRpcError, PluginWorkerManager,
};
use serde_json::{json, Map, Value};

fn make_config(plugin_key: &str, driver_key: &str) -> PluginEnvironmentConfig {
    PluginEnvironmentConfig {
        plugin_key: plugin_key.to_string(),
        driver_key: driver_key.to_string(),
        driver_config: Map::new(),
    }
}

fn make_ready_plugin(id: &str, plugin_key: &str, driver_key: &str) -> PluginRow {
    PluginRow {
        id: id.to_string(),
        plugin_key: plugin_key.to_string(),
        status: PluginStatus::Ready,
        environment_drivers: vec![PluginEnvironmentDriverDecl {
            driver_key: driver_key.to_string(),
            kind: PluginDriverKind::Environment,
            ..Default::default()
        }],
    }
}

fn make_lease() -> PluginEnvironmentLease {
    PluginEnvironmentLease {
        provider_lease_id: Some("lease-1".to_string()),
        metadata: None,
        expires_at: None,
    }
}

fn make_realize_params(driver_key: &str) -> PluginEnvironmentRealizeWorkspaceParams {
    PluginEnvironmentRealizeWorkspaceParams {
        driver_key: driver_key.to_string(),
        company_id: "company-1".to_string(),
        environment_id: "env-1".to_string(),
        issue_id: None,
        config: Map::new(),
        lease: make_lease(),
        workspace: PluginEnvironmentWorkspaceSpec {
            local_path: Some("/workspace".to_string()),
            remote_path: None,
            mode: Some("snapshot".to_string()),
            metadata: Map::new(),
        },
    }
}

fn make_execute_params(driver_key: &str) -> PluginEnvironmentExecuteParams {
    PluginEnvironmentExecuteParams {
        driver_key: driver_key.to_string(),
        company_id: "company-1".to_string(),
        environment_id: "env-1".to_string(),
        issue_id: None,
        config: Map::new(),
        lease: make_lease(),
        command: "echo".to_string(),
        args: Some(vec!["hello".to_string()]),
        cwd: Some("/workspace".to_string()),
        env: HashMap::new(),
        stdin: None,
        timeout_ms: Some(30_000),
    }
}


// =================================================================
// realize_plugin_environment_workspace
// =================================================================

#[test]
fn r691_realize_happy_path_returns_cwd() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentRealizeWorkspace", |_params| {
        Ok(json!({
            "cwd": "/workspace/snapshot-abc",
            "metadata": { "fingerprint": "abc-123" }
        }))
    });

    let config = make_config("my-plugin", "gcp");
    let params = make_realize_params("gcp");
    let result: PluginEnvironmentRealizeWorkspaceResult =
        realize_plugin_environment_workspace(&reg, &wm, None, &params, &config).unwrap();

    assert_eq!(result.cwd, "/workspace/snapshot-abc");
    assert_eq!(
        result.metadata.get("fingerprint"),
        Some(&json!("abc-123"))
    );
}

#[test]
fn r691_realize_uses_explicit_plugin_id_when_provided() {
    let reg = InMemoryPluginRegistry::new();
    // Note: registry is empty — but we provide plugin_id directly so resolve
    // is skipped.
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-explicit");
    wm.register_raw_handler("plugin-explicit", "environmentRealizeWorkspace", |params| {
        let driver_key = params.get("driverKey").and_then(|v| v.as_str()).unwrap_or("");
        assert_eq!(driver_key, "gcp");
        Ok(json!({ "cwd": "/explicit-workspace", "metadata": {} }))
    });

    let config = make_config("not-in-registry", "gcp");
    let params = make_realize_params("gcp");
    let result = realize_plugin_environment_workspace(
        &reg,
        &wm,
        Some("plugin-explicit"),
        &params,
        &config,
    )
    .unwrap();
    assert_eq!(result.cwd, "/explicit-workspace");
}

#[test]
fn r691_realize_plugin_not_found() {
    let reg = InMemoryPluginRegistry::new();
    let wm = InMemoryPluginWorkerManager::new();
    let config = make_config("missing", "gcp");
    let params = make_realize_params("gcp");
    let err = realize_plugin_environment_workspace(&reg, &wm, None, &params, &config).unwrap_err();
    match err {
        WorkspaceError::Resolve(_) => {}
        _ => panic!("expected Resolve, got {:?}", err),
    }
}

#[test]
fn r691_realize_worker_not_running() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    let config = make_config("my-plugin", "gcp");
    let params = make_realize_params("gcp");
    let err = realize_plugin_environment_workspace(&reg, &wm, None, &params, &config).unwrap_err();
    match err {
        WorkspaceError::Resolve(_) => {}
        _ => panic!("expected Resolve, got {:?}", err),
    }
}

#[test]
fn r691_realize_worker_method_not_registered() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    let config = make_config("my-plugin", "gcp");
    let params = make_realize_params("gcp");
    let err = realize_plugin_environment_workspace(&reg, &wm, None, &params, &config).unwrap_err();
    match err {
        WorkspaceError::WorkerRpc(PluginRpcError::MethodNotRegistered { method, .. }) => {
            assert_eq!(method, "environmentRealizeWorkspace");
        }
        _ => panic!("expected WorkerRpc MethodNotRegistered, got {:?}", err),
    }
}

#[test]
fn r691_realize_invalid_payload_propagates() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentRealizeWorkspace", |_params| {
        // cwd must be a string, send null.
        Ok(json!({ "cwd": null }))
    });
    let config = make_config("my-plugin", "gcp");
    let params = make_realize_params("gcp");
    let err = realize_plugin_environment_workspace(&reg, &wm, None, &params, &config).unwrap_err();
    match err {
        WorkspaceError::InvalidPayload(_) => {}
        _ => panic!("expected InvalidPayload, got {:?}", err),
    }
}


// =================================================================
// execute_plugin_environment_command
// =================================================================

#[test]
fn r691_execute_happy_path_returns_result() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentExecute", |params| {
        let command = params.get("command").and_then(|v| v.as_str()).unwrap_or("");
        assert_eq!(command, "echo");
        Ok(json!({
            "exitCode": 0,
            "signal": null,
            "timedOut": false
        }))
    });

    let config = make_config("my-plugin", "gcp");
    let params = make_execute_params("gcp");
    let result = execute_plugin_environment_command(&reg, &wm, None, &params, &config).unwrap();
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.signal, None);
    assert!(!result.timed_out);
}

#[test]
fn r691_execute_uses_explicit_plugin_id() {
    let reg = InMemoryPluginRegistry::new();
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-explicit");
    wm.register_raw_handler("plugin-explicit", "environmentExecute", |_params| {
        Ok(json!({ "exitCode": 0, "signal": null, "timedOut": false }))
    });

    let config = make_config("not-in-registry", "gcp");
    let params = make_execute_params("gcp");
    let result = execute_plugin_environment_command(
        &reg,
        &wm,
        Some("plugin-explicit"),
        &params,
        &config,
    )
    .unwrap();
    assert_eq!(result.exit_code, Some(0));
}

#[test]
fn r691_execute_timed_out_result() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentExecute", |_params| {
        Ok(json!({
            "exitCode": null,
            "signal": "SIGTERM",
            "timedOut": true
        }))
    });

    let config = make_config("my-plugin", "gcp");
    let params = make_execute_params("gcp");
    let result = execute_plugin_environment_command(&reg, &wm, None, &params, &config).unwrap();
    assert_eq!(result.exit_code, None);
    assert_eq!(result.signal, Some("SIGTERM".to_string()));
    assert!(result.timed_out);
}

#[test]
fn r691_execute_plugin_not_found() {
    let reg = InMemoryPluginRegistry::new();
    let wm = InMemoryPluginWorkerManager::new();
    let config = make_config("missing", "gcp");
    let params = make_execute_params("gcp");
    let err = execute_plugin_environment_command(&reg, &wm, None, &params, &config).unwrap_err();
    match err {
        WorkspaceError::Resolve(_) => {}
        _ => panic!("expected Resolve, got {:?}", err),
    }
}

#[test]
fn r691_execute_worker_handler_error_propagates() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentExecute", |_params| {
        Err("execution refused".to_string())
    });
    let config = make_config("my-plugin", "gcp");
    let params = make_execute_params("gcp");
    let err = execute_plugin_environment_command(&reg, &wm, None, &params, &config).unwrap_err();
    match err {
        WorkspaceError::WorkerRpc(PluginRpcError::HandlerError { message, .. }) => {
            assert_eq!(message, "execution refused");
        }
        _ => panic!("expected WorkerRpc HandlerError, got {:?}", err),
    }
}

#[test]
fn r691_execute_with_config_timeout_fallback() {
    // config.driver_config has timeoutMs=15000, params.timeout_ms is None.
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentExecute", |params| {
        // Verify params.timeoutMs was forwarded correctly (None in this test).
        let to = params.get("timeoutMs");
        assert!(to.is_none() || to == Some(&Value::Null));
        Ok(json!({ "exitCode": 0, "signal": null, "timedOut": false }))
    });

    let mut config = make_config("my-plugin", "gcp");
    config.driver_config.insert("timeoutMs".to_string(), json!(15000));
    let mut params = make_execute_params("gcp");
    params.timeout_ms = None;
    let result = execute_plugin_environment_command(&reg, &wm, None, &params, &config).unwrap();
    assert_eq!(result.exit_code, Some(0));
}
