// SPDX-License-Identifier: MIT
//
// R690 parity tests for resumePluginEnvironmentLease +
// destroyPluginEnvironmentLease.

use pc_environment::PluginEnvironmentConfig;
use pc_environment::environment_lease::{
    destroy_plugin_environment_lease, resume_plugin_environment_lease,
    DestroyEnvironmentLeaseError, PluginEnvironmentLease, ResumeEnvironmentLeaseError,
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

// =================================================================
// resume_plugin_environment_lease
// =================================================================

#[test]
fn r690_resume_happy_path_returns_lease() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentResumeLease", |_params| {
        Ok(json!({
            "providerLeaseId": "lease-abc-123",
            "expiresAt": "2030-01-01T00:00:00Z",
            "metadata": {
                "region": "us-east-1",
                "instanceType": "t3.medium"
            }
        }))
    });

    let config = make_config("my-plugin", "gcp");
    let lease = resume_plugin_environment_lease(
        &reg,
        &wm,
        "company-1",
        "env-1",
        None,
        &config,
        "lease-abc-123",
        None,
    )
    .unwrap();

    assert_eq!(lease.provider_lease_id, Some("lease-abc-123".to_string()));
    assert_eq!(lease.expires_at, Some("2030-01-01T00:00:00Z".to_string()));
    let metadata = lease.metadata.unwrap();
    assert_eq!(metadata.get("region"), Some(&json!("us-east-1")));
}

#[test]
fn r690_resume_returns_null_provider_lease_id() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentResumeLease", |_params| {
        Ok(json!({
            "providerLeaseId": null,
            "expiresAt": null
        }))
    });

    let config = make_config("my-plugin", "gcp");
    let lease = resume_plugin_environment_lease(
        &reg, &wm, "c", "e", None, &config, "old-lease", None,
    )
    .unwrap();
    assert_eq!(lease.provider_lease_id, None);
    assert_eq!(lease.expires_at, None);
    assert!(lease.metadata.is_none());
}

#[test]
fn r690_resume_plugin_not_found() {
    let reg = InMemoryPluginRegistry::new();
    let wm = InMemoryPluginWorkerManager::new();
    let config = make_config("missing", "gcp");
    let err = resume_plugin_environment_lease(
        &reg, &wm, "c", "e", None, &config, "lease-1", None,
    )
    .unwrap_err();
    match err {
        ResumeEnvironmentLeaseError::Resolve(_) => {}
        _ => panic!("expected Resolve, got {:?}", err),
    }
}

#[test]
fn r690_resume_plugin_not_ready() {
    let reg = InMemoryPluginRegistry::new();
    let mut plugin = make_ready_plugin("plugin-1", "my-plugin", "gcp");
    plugin.status = PluginStatus::Registered;
    reg.add_plugin(plugin);
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    let config = make_config("my-plugin", "gcp");
    let err = resume_plugin_environment_lease(
        &reg, &wm, "c", "e", None, &config, "lease-1", None,
    )
    .unwrap_err();
    match err {
        ResumeEnvironmentLeaseError::Resolve(_) => {}
        _ => panic!("expected Resolve, got {:?}", err),
    }
}

#[test]
fn r690_resume_worker_not_running() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    let config = make_config("my-plugin", "gcp");
    let err = resume_plugin_environment_lease(
        &reg, &wm, "c", "e", None, &config, "lease-1", None,
    )
    .unwrap_err();
    match err {
        ResumeEnvironmentLeaseError::Resolve(_) => {}
        _ => panic!("expected Resolve, got {:?}", err),
    }
}

#[test]
fn r690_resume_worker_method_not_registered() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    let config = make_config("my-plugin", "gcp");
    let err = resume_plugin_environment_lease(
        &reg, &wm, "c", "e", None, &config, "lease-1", None,
    )
    .unwrap_err();
    match err {
        ResumeEnvironmentLeaseError::WorkerRpc(PluginRpcError::MethodNotRegistered { method, .. }) => {
            assert_eq!(method, "environmentResumeLease");
        }
        _ => panic!("expected WorkerRpc MethodNotRegistered, got {:?}", err),
    }
}

#[test]
fn r690_resume_worker_handler_error_propagates() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentResumeLease", |_params| {
        Err("lease already taken".to_string())
    });
    let config = make_config("my-plugin", "gcp");
    let err = resume_plugin_environment_lease(
        &reg, &wm, "c", "e", None, &config, "lease-1", None,
    )
    .unwrap_err();
    match err {
        ResumeEnvironmentLeaseError::WorkerRpc(PluginRpcError::HandlerError { message, .. }) => {
            assert_eq!(message, "lease already taken");
        }
        _ => panic!("expected WorkerRpc HandlerError, got {:?}", err),
    }
}

#[test]
fn r690_resume_invalid_payload_propagates() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentResumeLease", |_params| {
        Ok(json!({ "providerLeaseId": 12345 }))
    });
    let config = make_config("my-plugin", "gcp");
    let err = resume_plugin_environment_lease(
        &reg, &wm, "c", "e", None, &config, "lease-1", None,
    )
    .unwrap_err();
    match err {
        ResumeEnvironmentLeaseError::InvalidPayload(_) => {}
        _ => panic!("expected InvalidPayload, got {:?}", err),
    }
}


// =================================================================
// destroy_plugin_environment_lease
// =================================================================

#[test]
fn r690_destroy_happy_path_returns_ok() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentDestroyLease", |_params| {
        // Worker may return anything for destroy; we ignore it.
        Ok(Value::Null)
    });

    let config = make_config("my-plugin", "gcp");
    destroy_plugin_environment_lease(
        &reg, &wm, "c", "e", None, &config, Some("lease-1"), None,
    )
    .unwrap();
}

#[test]
fn r690_destroy_with_null_provider_lease_id() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentDestroyLease", |_params| {
        Ok(Value::Null)
    });

    let config = make_config("my-plugin", "gcp");
    destroy_plugin_environment_lease(
        &reg, &wm, "c", "e", None, &config, None, None,
    )
    .unwrap();
}

#[test]
fn r690_destroy_plugin_not_found() {
    let reg = InMemoryPluginRegistry::new();
    let wm = InMemoryPluginWorkerManager::new();
    let config = make_config("missing", "gcp");
    let err = destroy_plugin_environment_lease(
        &reg, &wm, "c", "e", None, &config, Some("lease-1"), None,
    )
    .unwrap_err();
    match err {
        DestroyEnvironmentLeaseError::Resolve(_) => {}
        _ => panic!("expected Resolve, got {:?}", err),
    }
}

#[test]
fn r690_destroy_worker_not_running() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    let config = make_config("my-plugin", "gcp");
    let err = destroy_plugin_environment_lease(
        &reg, &wm, "c", "e", None, &config, Some("lease-1"), None,
    )
    .unwrap_err();
    match err {
        DestroyEnvironmentLeaseError::Resolve(_) => {}
        _ => panic!("expected Resolve, got {:?}", err),
    }
}

#[test]
fn r690_destroy_worker_method_not_registered() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    let config = make_config("my-plugin", "gcp");
    let err = destroy_plugin_environment_lease(
        &reg, &wm, "c", "e", None, &config, Some("lease-1"), None,
    )
    .unwrap_err();
    match err {
        DestroyEnvironmentLeaseError::WorkerRpc(PluginRpcError::MethodNotRegistered { method, .. }) => {
            assert_eq!(method, "environmentDestroyLease");
        }
        _ => panic!("expected WorkerRpc MethodNotRegistered, got {:?}", err),
    }
}

#[test]
fn r690_destroy_worker_handler_error_propagates() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentDestroyLease", |_params| {
        Err("lease in use".to_string())
    });
    let config = make_config("my-plugin", "gcp");
    let err = destroy_plugin_environment_lease(
        &reg, &wm, "c", "e", None, &config, Some("lease-1"), None,
    )
    .unwrap_err();
    match err {
        DestroyEnvironmentLeaseError::WorkerRpc(PluginRpcError::HandlerError { message, .. }) => {
            assert_eq!(message, "lease in use");
        }
        _ => panic!("expected WorkerRpc HandlerError, got {:?}", err),
    }
}

#[test]
fn r690_destroy_with_issue_id_and_metadata() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_ready_plugin("plugin-1", "my-plugin", "gcp"));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_raw_handler("plugin-1", "environmentDestroyLease", |params| {
        // Verify issueId and leaseMetadata are passed through.
        let issue_id = params.get("issueId").and_then(|v| v.as_str()).unwrap_or("");
        assert_eq!(issue_id, "issue-99");
        let meta = params.get("leaseMetadata").and_then(|v| v.as_object()).unwrap();
        assert_eq!(meta.get("reason").and_then(|v| v.as_str()), Some("cleanup"));
        Ok(Value::Null)
    });

    let config = make_config("my-plugin", "gcp");
    let mut meta = Map::new();
    meta.insert("reason".to_string(), json!("cleanup"));
    destroy_plugin_environment_lease(
        &reg,
        &wm,
        "c",
        "e",
        Some("issue-99"),
        &config,
        Some("lease-1"),
        Some(&meta),
    )
    .unwrap();
}

#[test]
fn r690_plugin_environment_lease_default_matches_node() {
    let lease = PluginEnvironmentLease::default();
    assert_eq!(lease.provider_lease_id, None);
    assert_eq!(lease.metadata, None);
    assert_eq!(lease.expires_at, None);
}
