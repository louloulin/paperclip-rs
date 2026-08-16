// SPDX-License-Identifier: MIT
//
// R688 parity tests for resolvePluginEnvironmentDriver +
// validatePluginEnvironmentDriverConfig.

use pc_environment::PluginEnvironmentConfig;
use pc_environment::plugin_registry::{
    InMemoryPluginRegistry, PluginDriverKind, PluginEnvironmentDriverDecl, PluginRegistry,
    PluginRow, PluginStatus,
};
use pc_environment::plugin_worker_manager::{
    InMemoryPluginWorkerManager, PluginRpcError, PluginRpcResult, PluginWorkerManager,
};
use pc_environment::validate_environment_driver::{
    resolve_plugin_environment_driver, validate_plugin_environment_driver_config,
    ResolveEnvironmentDriverError, ValidateEnvironmentDriverError,
};
use serde_json::json;
use serde_json::Map;

fn make_config(plugin_key: &str, driver_key: &str) -> PluginEnvironmentConfig {
    PluginEnvironmentConfig {
        plugin_key: plugin_key.to_string(),
        driver_key: driver_key.to_string(),
        driver_config: Map::new(),
    }
}

fn make_plugin_with_env(id: &str, plugin_key: &str, driver_key: &str, status: PluginStatus) -> PluginRow {
    PluginRow {
        id: id.to_string(),
        plugin_key: plugin_key.to_string(),
        status,
        environment_drivers: vec![PluginEnvironmentDriverDecl {
            driver_key: driver_key.to_string(),
            kind: PluginDriverKind::Environment,
            display_name: Some(driver_key.to_string()),
            description: None,
            config_schema: None,
        ..Default::default()
        }],
    }
}

#[test]
fn r688_resolve_plugin_not_found() {
    let reg = InMemoryPluginRegistry::new();
    let wm = InMemoryPluginWorkerManager::new();
    let err = resolve_plugin_environment_driver(&reg, &wm, &make_config("missing", "driver")).unwrap_err();
    match err {
        ResolveEnvironmentDriverError::PluginNotFound { plugin_key } => {
            assert_eq!(plugin_key, "missing");
        }
        _ => panic!("expected PluginNotFound"),
    }
}

#[test]
fn r688_resolve_plugin_not_ready() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin_with_env("p-1", "my-plugin", "driver", PluginStatus::Installed));
    let wm = InMemoryPluginWorkerManager::new();
    let err = resolve_plugin_environment_driver(&reg, &wm, &make_config("my-plugin", "driver")).unwrap_err();
    assert!(matches!(err, ResolveEnvironmentDriverError::PluginNotReady { .. }));
}

#[test]
fn r688_resolve_driver_not_declared() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin_with_env("p-1", "my-plugin", "aws", PluginStatus::Ready));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("p-1");
    let err = resolve_plugin_environment_driver(&reg, &wm, &make_config("my-plugin", "gcp")).unwrap_err();
    match err {
        ResolveEnvironmentDriverError::DriverNotDeclared { plugin_key, driver_key } => {
            assert_eq!(plugin_key, "my-plugin");
            assert_eq!(driver_key, "gcp");
        }
        _ => panic!("expected DriverNotDeclared"),
    }
}

#[test]
fn r688_resolve_worker_not_running() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin_with_env("p-1", "my-plugin", "driver", PluginStatus::Ready));
    let wm = InMemoryPluginWorkerManager::new();
    // worker NOT registered
    let err = resolve_plugin_environment_driver(&reg, &wm, &make_config("my-plugin", "driver")).unwrap_err();
    assert!(matches!(err, ResolveEnvironmentDriverError::WorkerNotRunning { .. }));
}

#[test]
fn r688_resolve_happy_path() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin_with_env("p-1", "my-plugin", "driver", PluginStatus::Ready));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("p-1");
    let resolved = resolve_plugin_environment_driver(&reg, &wm, &make_config("my-plugin", "driver")).unwrap();
    assert_eq!(resolved.plugin.id, "p-1");
    assert_eq!(resolved.driver.driver_key, "driver");
}

#[test]
fn r688_validate_happy_path_returns_normalized_config() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin_with_env("p-1", "my-plugin", "driver", PluginStatus::Ready));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("p-1");
    wm.register_handler("p-1", "environmentValidateConfig", |params| {
        let mut config = params.get("config").cloned().unwrap_or(json!({}));
        if let Some(obj) = config.as_object_mut() {
            obj.insert("region".to_string(), json!("us-east-1"));
        }
        Ok(PluginRpcResult {
            ok: true,
            errors: vec![],
            warnings: vec![],
            normalized_config: Some(config),
        ..Default::default()
        })
    });
    let config = make_config("my-plugin", "driver");
    let out = validate_plugin_environment_driver_config(&reg, &wm, &config).unwrap();
    assert_eq!(out.plugin_key, "my-plugin");
    assert_eq!(out.driver_key, "driver");
    assert_eq!(out.driver_config.get("region"), Some(&json!("us-east-1")));
}

#[test]
fn r688_validate_falls_back_to_local_driver_config() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin_with_env("p-1", "my-plugin", "driver", PluginStatus::Ready));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("p-1");
    wm.register_handler("p-1", "environmentValidateConfig", |params| {
        let config = params.get("config").cloned().unwrap_or(json!({}));
        Ok(PluginRpcResult {
            ok: true,
            errors: vec![],
            warnings: vec![],
            normalized_config: None, // worker did NOT return normalized
        ..Default::default()
        })
    });
    let mut config = make_config("my-plugin", "driver");
    config.driver_config.insert("region".to_string(), json!("us-east-1"));
    let out = validate_plugin_environment_driver_config(&reg, &wm, &config).unwrap();
    assert_eq!(out.driver_config.get("region"), Some(&json!("us-east-1")));
}

#[test]
fn r688_validate_propagates_plugin_not_found() {
    let reg = InMemoryPluginRegistry::new();
    let wm = InMemoryPluginWorkerManager::new();
    let err = validate_plugin_environment_driver_config(&reg, &wm, &make_config("missing", "driver")).unwrap_err();
    assert!(matches!(err, ValidateEnvironmentDriverError::Resolve(ResolveEnvironmentDriverError::PluginNotFound { .. })));
}

#[test]
fn r688_validate_propagates_plugin_not_ready() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin_with_env("p-1", "my-plugin", "driver", PluginStatus::Installed));
    let wm = InMemoryPluginWorkerManager::new();
    let err = validate_plugin_environment_driver_config(&reg, &wm, &make_config("my-plugin", "driver")).unwrap_err();
    assert!(matches!(err, ValidateEnvironmentDriverError::Resolve(ResolveEnvironmentDriverError::PluginNotReady { .. })));
}

#[test]
fn r688_validate_propagates_driver_not_declared() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin_with_env("p-1", "my-plugin", "aws", PluginStatus::Ready));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("p-1");
    let err = validate_plugin_environment_driver_config(&reg, &wm, &make_config("my-plugin", "gcp")).unwrap_err();
    assert!(matches!(err, ValidateEnvironmentDriverError::Resolve(ResolveEnvironmentDriverError::DriverNotDeclared { .. })));
}

#[test]
fn r688_validate_propagates_worker_not_running() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin_with_env("p-1", "my-plugin", "driver", PluginStatus::Ready));
    let wm = InMemoryPluginWorkerManager::new();
    let err = validate_plugin_environment_driver_config(&reg, &wm, &make_config("my-plugin", "driver")).unwrap_err();
    assert!(matches!(err, ValidateEnvironmentDriverError::Resolve(ResolveEnvironmentDriverError::WorkerNotRunning { .. })));
}

#[test]
fn r688_validate_worker_method_not_registered() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin_with_env("p-1", "my-plugin", "driver", PluginStatus::Ready));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("p-1");
    // no handler registered
    let err = validate_plugin_environment_driver_config(&reg, &wm, &make_config("my-plugin", "driver")).unwrap_err();
    match err {
        ValidateEnvironmentDriverError::WorkerRpc(PluginRpcError::MethodNotRegistered { method, .. }) => {
            assert_eq!(method, "environmentValidateConfig");
        }
        _ => panic!("expected WorkerRpc(MethodNotRegistered)"),
    }
}

#[test]
fn r688_validate_worker_rejected_with_errors() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin_with_env("p-1", "my-plugin", "driver", PluginStatus::Ready));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("p-1");
    wm.register_handler("p-1", "environmentValidateConfig", |_params| {
        Ok(PluginRpcResult {
            ok: false,
            errors: vec!["region is required".to_string()],
            warnings: vec![],
            normalized_config: None,
        ..Default::default()
        })
    });
    let err = validate_plugin_environment_driver_config(&reg, &wm, &make_config("my-plugin", "driver")).unwrap_err();
    match err {
        ValidateEnvironmentDriverError::WorkerRejected { provider_key, first_error, errors, warnings } => {
            assert_eq!(provider_key, "my-plugin:driver");
            assert_eq!(first_error, "region is required");
            assert_eq!(errors.len(), 1);
            assert!(warnings.is_empty());
        }
        _ => panic!("expected WorkerRejected"),
    }
}

#[test]
fn r688_validate_worker_rejected_no_errors_default_message() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin_with_env("p-1", "my-plugin", "driver", PluginStatus::Ready));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("p-1");
    wm.register_handler("p-1", "environmentValidateConfig", |_params| {
        Ok(PluginRpcResult {
            ok: false,
            errors: vec![],
            warnings: vec![],
            normalized_config: None,
        ..Default::default()
        })
    });
    let err = validate_plugin_environment_driver_config(&reg, &wm, &make_config("my-plugin", "driver")).unwrap_err();
    match err {
        ValidateEnvironmentDriverError::WorkerRejected { provider_key, first_error, .. } => {
            assert_eq!(provider_key, "my-plugin:driver");
            assert!(first_error.contains("rejected"));
        }
        _ => panic!("expected WorkerRejected"),
    }
}

#[test]
fn r688_resolve_error_display() {
    let e = ResolveEnvironmentDriverError::DriverNotDeclared {
        plugin_key: "p".into(),
        driver_key: "d".into(),
    };
    let s = e.to_string();
    assert!(s.contains("p"));
    assert!(s.contains("d"));
}

#[test]
fn r688_validate_error_display_from_resolve() {
    let inner = ResolveEnvironmentDriverError::WorkerNotRunning {
        plugin_key: "p".into(),
    };
    let outer = ValidateEnvironmentDriverError::from(inner);
    let s = outer.to_string();
    assert!(s.contains("no running worker"));
}
