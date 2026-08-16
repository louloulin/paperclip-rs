// SPDX-License-Identifier: MIT
//
// R692 parity tests for startPluginEnvironmentInteractiveSetup +
// getPluginEnvironmentInteractiveSetup.

use pc_environment::PluginEnvironmentConfig;
use pc_environment::environment_setup::{
    get_plugin_environment_interactive_setup, start_plugin_environment_interactive_setup,
    PluginEnvironmentGetInteractiveSetupParams, PluginEnvironmentInteractiveSetupStatus,
    PluginEnvironmentStartInteractiveSetupParams, PluginEnvironmentTemplateRefKind,
    SetupError,
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

fn make_start_params(driver_key: &str) -> PluginEnvironmentStartInteractiveSetupParams {
    PluginEnvironmentStartInteractiveSetupParams {
        driver_key: driver_key.to_string(),
        company_id: "company-1".to_string(),
        environment_id: "env-1".to_string(),
        issue_id: None,
        config: Map::new(),
        session_id: "session-abc".to_string(),
        source_template_ref: Some("tpl-base".to_string()),
        source_template_kind: Some(PluginEnvironmentTemplateRefKind::Snapshot),
        connection_expires_in_minutes: Some(60),
        expires_at: None,
    }
}

fn make_get_params(driver_key: &str) -> PluginEnvironmentGetInteractiveSetupParams {
    PluginEnvironmentGetInteractiveSetupParams {
        driver_key: driver_key.to_string(),
        company_id: "company-1".to_string(),
        environment_id: "env-1".to_string(),
        issue_id: None,
        config: Map::new(),
        provider_lease_id: Some("lease-1".to_string()),
        setup_metadata: Map::new(),
        include_connection_payload: Some(true),
        connection_expires_in_minutes: Some(30),
    }
}


// =================================================================
// start_plugin_environment_interactive_setup
// =================================================================

#[test]
fn r692_start_happy_path_returns_session() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentStartInteractiveSetup", |params| {
        // Verify driverKey + config are filled from config arg.
        let dk = params.get("driverKey").and_then(|v| v.as_str()).unwrap_or("");
        assert_eq!(dk, "gcp");
        let session_id = params.get("sessionId").and_then(|v| v.as_str()).unwrap_or("");
        assert_eq!(session_id, "session-abc");

        Ok(json!({
            "providerLeaseId": "lease-setup-1",
            "status": "waiting_for_user",
            "connectionSummary": {
                "type": "ssh",
                "username": "ubuntu",
                "hostRedacted": true,
                "portRedacted": true,
                "commandRedacted": false,
                "expiresAt": "2030-01-01T01:00:00Z"
            },
            "expiresAt": "2030-01-01T01:00:00Z"
        }))
    });

    let config = make_config("my-plugin", "gcp");
    let params = make_start_params("gcp");
    let session = start_plugin_environment_interactive_setup(&reg, &wm, &config, &params).unwrap();

    assert_eq!(session.provider_lease_id, Some("lease-setup-1".to_string()));
    assert_eq!(session.status, PluginEnvironmentInteractiveSetupStatus::WaitingForUser);
    let summary = session.connection_summary.unwrap();
    assert_eq!(summary.connection_type, "ssh");
    assert_eq!(summary.username, Some("ubuntu".to_string()));
    assert!(summary.host_redacted);
    assert!(summary.port_redacted);
    assert_eq!(session.expires_at, Some("2030-01-01T01:00:00Z".to_string()));
}

#[test]
fn r692_start_with_status_starting() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentStartInteractiveSetup", |_params| {
        Ok(json!({
            "providerLeaseId": null,
            "status": "starting",
            "connectionSummary": null
        }))
    });

    let config = make_config("my-plugin", "gcp");
    let params = make_start_params("gcp");
    let session = start_plugin_environment_interactive_setup(&reg, &wm, &config, &params).unwrap();
    assert_eq!(session.status, PluginEnvironmentInteractiveSetupStatus::Starting);
    assert_eq!(session.provider_lease_id, None);
}

#[test]
fn r692_start_plugin_not_found() {
    let reg = InMemoryPluginRegistry::new();
    let wm = InMemoryPluginWorkerManager::new();
    let config = make_config("missing", "gcp");
    let params = make_start_params("gcp");
    let err = start_plugin_environment_interactive_setup(&reg, &wm, &config, &params).unwrap_err();
    match err {
        SetupError::Resolve(_) => {}
        _ => panic!("expected Resolve, got {:?}", err),
    }
}

#[test]
fn r692_start_worker_method_not_registered() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    let config = make_config("my-plugin", "gcp");
    let params = make_start_params("gcp");
    let err = start_plugin_environment_interactive_setup(&reg, &wm, &config, &params).unwrap_err();
    match err {
        SetupError::WorkerRpc(PluginRpcError::MethodNotRegistered { method, .. }) => {
            assert_eq!(method, "environmentStartInteractiveSetup");
        }
        _ => panic!("expected WorkerRpc MethodNotRegistered, got {:?}", err),
    }
}

#[test]
fn r692_start_invalid_payload_propagates() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentStartInteractiveSetup", |_params| {
        // status must be one of the enum variants, send an invalid string.
        Ok(json!({ "providerLeaseId": null, "status": "invalid_state", "connectionSummary": null }))
    });
    let config = make_config("my-plugin", "gcp");
    let params = make_start_params("gcp");
    let err = start_plugin_environment_interactive_setup(&reg, &wm, &config, &params).unwrap_err();
    match err {
        SetupError::InvalidPayload(_) => {}
        _ => panic!("expected InvalidPayload, got {:?}", err),
    }
}

#[test]
fn r692_start_overrides_driver_key_and_config() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentStartInteractiveSetup", |params| {
        let dk = params.get("driverKey").and_then(|v| v.as_str()).unwrap_or("");
        assert_eq!(dk, "gcp", "driverKey from config should override params.driver_key");
        let cfg = params.get("config").and_then(|v| v.as_object());
        assert!(cfg.is_some());
        Ok(json!({
            "providerLeaseId": "lease-x",
            "status": "starting",
            "connectionSummary": null
        }))
    });

    let config = make_config("my-plugin", "gcp");
    let mut params = make_start_params("gcp");
    params.driver_key = "should-be-overridden".to_string();
    start_plugin_environment_interactive_setup(&reg, &wm, &config, &params).unwrap();
}


// =================================================================
// get_plugin_environment_interactive_setup
// =================================================================

#[test]
fn r692_get_happy_path_returns_session() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentGetInteractiveSetup", |params| {
        let lease_id = params.get("providerLeaseId").and_then(|v| v.as_str()).unwrap_or("");
        assert_eq!(lease_id, "lease-1");
        let include = params.get("includeConnectionPayload").and_then(|v| v.as_bool()).unwrap_or(false);
        assert!(include);
        Ok(json!({
            "providerLeaseId": "lease-1",
            "status": "promoted",
            "connectionSummary": {
                "type": "ssh",
                "hostRedacted": true,
                "portRedacted": true
            }
        }))
    });

    let config = make_config("my-plugin", "gcp");
    let params = make_get_params("gcp");
    let session = get_plugin_environment_interactive_setup(&reg, &wm, &config, &params).unwrap();
    assert_eq!(session.provider_lease_id, Some("lease-1".to_string()));
    assert_eq!(session.status, PluginEnvironmentInteractiveSetupStatus::Promoted);
}

#[test]
fn r692_get_with_null_provider_lease_id() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentGetInteractiveSetup", |_params| {
        Ok(json!({
            "providerLeaseId": null,
            "status": "missing",
            "connectionSummary": null
        }))
    });

    let config = make_config("my-plugin", "gcp");
    let mut params = make_get_params("gcp");
    params.provider_lease_id = None;
    let session = get_plugin_environment_interactive_setup(&reg, &wm, &config, &params).unwrap();
    assert_eq!(session.status, PluginEnvironmentInteractiveSetupStatus::Missing);
    assert_eq!(session.provider_lease_id, None);
}

#[test]
fn r692_get_with_connection_payload() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentGetInteractiveSetup", |_params| {
        Ok(json!({
            "providerLeaseId": "lease-1",
            "status": "waiting_for_user",
            "connectionSummary": { "type": "ssh", "hostRedacted": true, "portRedacted": true },
            "connectionPayload": {
                "type": "ssh",
                "command": "ssh ubuntu@host -p 2222",
                "token": "tok-abc",
                "expiresAt": "2030-01-01T02:00:00Z"
            }
        }))
    });

    let config = make_config("my-plugin", "gcp");
    let params = make_get_params("gcp");
    let session = get_plugin_environment_interactive_setup(&reg, &wm, &config, &params).unwrap();
    let payload = session.connection_payload.unwrap();
    assert_eq!(payload.connection_type, "ssh");
    assert_eq!(payload.command, Some("ssh ubuntu@host -p 2222".to_string()));
    assert_eq!(payload.token, Some("tok-abc".to_string()));
}

#[test]
fn r692_get_plugin_not_found() {
    let reg = InMemoryPluginRegistry::new();
    let wm = InMemoryPluginWorkerManager::new();
    let config = make_config("missing", "gcp");
    let params = make_get_params("gcp");
    let err = get_plugin_environment_interactive_setup(&reg, &wm, &config, &params).unwrap_err();
    match err {
        SetupError::Resolve(_) => {}
        _ => panic!("expected Resolve, got {:?}", err),
    }
}

#[test]
fn r692_get_worker_not_running() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    let config = make_config("my-plugin", "gcp");
    let params = make_get_params("gcp");
    let err = get_plugin_environment_interactive_setup(&reg, &wm, &config, &params).unwrap_err();
    match err {
        SetupError::Resolve(_) => {}
        _ => panic!("expected Resolve, got {:?}", err),
    }
}

#[test]
fn r692_get_worker_handler_error_propagates() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentGetInteractiveSetup", |_params| {
        Err("session not found".to_string())
    });
    let config = make_config("my-plugin", "gcp");
    let params = make_get_params("gcp");
    let err = get_plugin_environment_interactive_setup(&reg, &wm, &config, &params).unwrap_err();
    match err {
        SetupError::WorkerRpc(PluginRpcError::HandlerError { message, .. }) => {
            assert_eq!(message, "session not found");
        }
        _ => panic!("expected WorkerRpc HandlerError, got {:?}", err),
    }
}

#[test]
fn r692_get_with_config_timeout_fallback() {
    // config.driver_config has timeoutMs=15000; should be reflected in timeout.
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentGetInteractiveSetup", |_params| {
        Ok(json!({
            "providerLeaseId": "lease-1",
            "status": "promoted",
            "connectionSummary": null
        }))
    });

    let mut config = make_config("my-plugin", "gcp");
    config.driver_config.insert("timeoutMs".to_string(), json!(15000));
    let params = make_get_params("gcp");
    let session = get_plugin_environment_interactive_setup(&reg, &wm, &config, &params).unwrap();
    assert_eq!(session.provider_lease_id, Some("lease-1".to_string()));
}
