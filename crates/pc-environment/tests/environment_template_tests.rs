// SPDX-License-Identifier: MIT
//
// R693 parity tests for capturePluginEnvironmentTemplate +
// cancelPluginEnvironmentInteractiveSetup + deletePluginEnvironmentTemplate.

use pc_environment::PluginEnvironmentConfig;
use pc_environment::environment_setup::{
    PluginEnvironmentInteractiveSetupStatus, PluginEnvironmentTemplateRefKind,
};
use pc_environment::environment_template::{
    cancel_plugin_environment_interactive_setup, capture_plugin_environment_template,
    delete_plugin_environment_template, PluginEnvironmentCancelInteractiveSetupParams,
    PluginEnvironmentCaptureTemplateParams, PluginEnvironmentDeleteTemplateParams, TemplateError,
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

fn make_capture_params(driver_key: &str) -> PluginEnvironmentCaptureTemplateParams {
    PluginEnvironmentCaptureTemplateParams {
        driver_key: driver_key.to_string(),
        company_id: "company-1".to_string(),
        environment_id: "env-1".to_string(),
        issue_id: None,
        config: Map::new(),
        provider_lease_id: Some("lease-1".to_string()),
        setup_metadata: Map::new(),
        source_template_ref: Some("tpl-base".to_string()),
        previous_template_ref: None,
        template_label: Some("snapshot-2026".to_string()),
        timeout_ms: Some(60_000),
    }
}

fn make_cancel_params(driver_key: &str) -> PluginEnvironmentCancelInteractiveSetupParams {
    PluginEnvironmentCancelInteractiveSetupParams {
        driver_key: driver_key.to_string(),
        company_id: "company-1".to_string(),
        environment_id: "env-1".to_string(),
        issue_id: None,
        config: Map::new(),
        provider_lease_id: Some("lease-1".to_string()),
        setup_metadata: Map::new(),
        reason: Some("user cancelled".to_string()),
    }
}

fn make_delete_params(driver_key: &str) -> PluginEnvironmentDeleteTemplateParams {
    PluginEnvironmentDeleteTemplateParams {
        driver_key: driver_key.to_string(),
        company_id: "company-1".to_string(),
        environment_id: "env-1".to_string(),
        issue_id: None,
        config: Map::new(),
        template_ref: "tpl-snapshot-1".to_string(),
        template_kind: Some(PluginEnvironmentTemplateRefKind::Snapshot),
        metadata: Map::new(),
        reason: Some("cleanup".to_string()),
    }
}


// =================================================================
// capture_plugin_environment_template
// =================================================================

#[test]
fn r693_capture_happy_path_returns_template_ref() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentCaptureTemplate", |params| {
        let dk = params.get("driverKey").and_then(|v| v.as_str()).unwrap_or("");
        assert_eq!(dk, "gcp");
        let label = params.get("templateLabel").and_then(|v| v.as_str()).unwrap_or("");
        assert_eq!(label, "snapshot-2026");
        Ok(json!({
            "templateRef": "tpl-snapshot-abc-123",
            "templateKind": "snapshot",
            "metadata": { "fingerprint": "abc-123" }
        }))
    });

    let config = make_config("my-plugin", "gcp");
    let params = make_capture_params("gcp");
    let result = capture_plugin_environment_template(&reg, &wm, &config, &params).unwrap();
    assert_eq!(result.template_ref, "tpl-snapshot-abc-123");
    assert_eq!(result.template_kind, PluginEnvironmentTemplateRefKind::Snapshot);
    assert_eq!(
        result.metadata.get("fingerprint"),
        Some(&json!("abc-123"))
    );
}

#[test]
fn r693_capture_image_template_kind() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentCaptureTemplate", |_params| {
        Ok(json!({
            "templateRef": "image-xyz",
            "templateKind": "image",
            "metadata": {}
        }))
    });

    let config = make_config("my-plugin", "gcp");
    let params = make_capture_params("gcp");
    let result = capture_plugin_environment_template(&reg, &wm, &config, &params).unwrap();
    assert_eq!(result.template_kind, PluginEnvironmentTemplateRefKind::Image);
}

#[test]
fn r693_capture_plugin_not_found() {
    let reg = InMemoryPluginRegistry::new();
    let wm = InMemoryPluginWorkerManager::new();
    let config = make_config("missing", "gcp");
    let params = make_capture_params("gcp");
    let err = capture_plugin_environment_template(&reg, &wm, &config, &params).unwrap_err();
    match err {
        TemplateError::Resolve(_) => {}
        _ => panic!("expected Resolve, got {:?}", err),
    }
}

#[test]
fn r693_capture_worker_method_not_registered() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    let config = make_config("my-plugin", "gcp");
    let params = make_capture_params("gcp");
    let err = capture_plugin_environment_template(&reg, &wm, &config, &params).unwrap_err();
    match err {
        TemplateError::WorkerRpc(PluginRpcError::MethodNotRegistered { method, .. }) => {
            assert_eq!(method, "environmentCaptureTemplate");
        }
        _ => panic!("expected WorkerRpc MethodNotRegistered, got {:?}", err),
    }
}

#[test]
fn r693_capture_worker_handler_error_propagates() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentCaptureTemplate", |_params| {
        Err("capture failed".to_string())
    });
    let config = make_config("my-plugin", "gcp");
    let params = make_capture_params("gcp");
    let err = capture_plugin_environment_template(&reg, &wm, &config, &params).unwrap_err();
    match err {
        TemplateError::WorkerRpc(PluginRpcError::HandlerError { message, .. }) => {
            assert_eq!(message, "capture failed");
        }
        _ => panic!("expected WorkerRpc HandlerError, got {:?}", err),
    }
}

#[test]
fn r693_capture_invalid_template_kind_propagates() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentCaptureTemplate", |_params| {
        Ok(json!({
            "templateRef": "tpl-x",
            "templateKind": "invalid_kind"
        }))
    });
    let config = make_config("my-plugin", "gcp");
    let params = make_capture_params("gcp");
    let err = capture_plugin_environment_template(&reg, &wm, &config, &params).unwrap_err();
    match err {
        TemplateError::InvalidPayload(_) => {}
        _ => panic!("expected InvalidPayload, got {:?}", err),
    }
}


// =================================================================
// cancel_plugin_environment_interactive_setup
// =================================================================

#[test]
fn r693_cancel_happy_path_returns_cancelled_status() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentCancelInteractiveSetup", |params| {
        let reason = params.get("reason").and_then(|v| v.as_str()).unwrap_or("");
        assert_eq!(reason, "user cancelled");
        Ok(json!({
            "status": "cancelled",
            "metadata": { "cancelledBy": "user" }
        }))
    });

    let config = make_config("my-plugin", "gcp");
    let params = make_cancel_params("gcp");
    let result = cancel_plugin_environment_interactive_setup(&reg, &wm, &config, &params).unwrap();
    assert_eq!(result.status, PluginEnvironmentInteractiveSetupStatus::Cancelled);
    assert_eq!(
        result.metadata.get("cancelledBy"),
        Some(&json!("user"))
    );
}

#[test]
fn r693_cancel_with_timed_out_status() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentCancelInteractiveSetup", |_params| {
        Ok(json!({
            "status": "timed_out",
            "metadata": {}
        }))
    });

    let config = make_config("my-plugin", "gcp");
    let params = make_cancel_params("gcp");
    let result = cancel_plugin_environment_interactive_setup(&reg, &wm, &config, &params).unwrap();
    assert_eq!(result.status, PluginEnvironmentInteractiveSetupStatus::TimedOut);
}

#[test]
fn r693_cancel_plugin_not_found() {
    let reg = InMemoryPluginRegistry::new();
    let wm = InMemoryPluginWorkerManager::new();
    let config = make_config("missing", "gcp");
    let params = make_cancel_params("gcp");
    let err = cancel_plugin_environment_interactive_setup(&reg, &wm, &config, &params).unwrap_err();
    match err {
        TemplateError::Resolve(_) => {}
        _ => panic!("expected Resolve, got {:?}", err),
    }
}

#[test]
fn r693_cancel_worker_handler_error_propagates() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentCancelInteractiveSetup", |_params| {
        Err("cancel refused".to_string())
    });
    let config = make_config("my-plugin", "gcp");
    let params = make_cancel_params("gcp");
    let err = cancel_plugin_environment_interactive_setup(&reg, &wm, &config, &params).unwrap_err();
    match err {
        TemplateError::WorkerRpc(PluginRpcError::HandlerError { message, .. }) => {
            assert_eq!(message, "cancel refused");
        }
        _ => panic!("expected WorkerRpc HandlerError, got {:?}", err),
    }
}

// =================================================================
// delete_plugin_environment_template
// =================================================================

#[test]
fn r693_delete_happy_path_returns_deleted_true() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentDeleteTemplate", |params| {
        let tpl = params.get("templateRef").and_then(|v| v.as_str()).unwrap_or("");
        assert_eq!(tpl, "tpl-snapshot-1");
        Ok(json!({
            "deleted": true,
            "metadata": { "deletedBy": "cleanup" }
        }))
    });

    let config = make_config("my-plugin", "gcp");
    let params = make_delete_params("gcp");
    let result = delete_plugin_environment_template(&reg, &wm, &config, &params).unwrap();
    assert!(result.deleted);
    assert_eq!(
        result.metadata.get("deletedBy"),
        Some(&json!("cleanup"))
    );
}

#[test]
fn r693_delete_returns_deleted_false_when_not_found() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentDeleteTemplate", |_params| {
        Ok(json!({ "deleted": false, "metadata": {} }))
    });

    let config = make_config("my-plugin", "gcp");
    let params = make_delete_params("gcp");
    let result = delete_plugin_environment_template(&reg, &wm, &config, &params).unwrap();
    assert!(!result.deleted);
}

#[test]
fn r693_delete_plugin_not_found() {
    let reg = InMemoryPluginRegistry::new();
    let wm = InMemoryPluginWorkerManager::new();
    let config = make_config("missing", "gcp");
    let params = make_delete_params("gcp");
    let err = delete_plugin_environment_template(&reg, &wm, &config, &params).unwrap_err();
    match err {
        TemplateError::Resolve(_) => {}
        _ => panic!("expected Resolve, got {:?}", err),
    }
}

#[test]
fn r693_delete_worker_method_not_registered() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    let config = make_config("my-plugin", "gcp");
    let params = make_delete_params("gcp");
    let err = delete_plugin_environment_template(&reg, &wm, &config, &params).unwrap_err();
    match err {
        TemplateError::WorkerRpc(PluginRpcError::MethodNotRegistered { method, .. }) => {
            assert_eq!(method, "environmentDeleteTemplate");
        }
        _ => panic!("expected WorkerRpc MethodNotRegistered, got {:?}", err),
    }
}

#[test]
fn r693_delete_worker_not_running() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    let config = make_config("my-plugin", "gcp");
    let params = make_delete_params("gcp");
    let err = delete_plugin_environment_template(&reg, &wm, &config, &params).unwrap_err();
    match err {
        TemplateError::Resolve(_) => {}
        _ => panic!("expected Resolve, got {:?}", err),
    }
}

#[test]
fn r693_delete_invalid_payload_propagates() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentDeleteTemplate", |_params| {
        // deleted must be a bool; send a string.
        Ok(json!({ "deleted": "yes" }))
    });
    let config = make_config("my-plugin", "gcp");
    let params = make_delete_params("gcp");
    let err = delete_plugin_environment_template(&reg, &wm, &config, &params).unwrap_err();
    match err {
        TemplateError::InvalidPayload(_) => {}
        _ => panic!("expected InvalidPayload, got {:?}", err),
    }
}
