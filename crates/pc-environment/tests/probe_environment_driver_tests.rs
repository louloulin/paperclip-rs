// SPDX-License-Identifier: MIT
//
// R689 parity tests for probePluginEnvironmentDriver +
// listReadyPluginEnvironmentDrivers (including the recovery flow).

use std::collections::HashSet;

use pc_environment::PluginEnvironmentConfig;
use pc_environment::plugin_registry::{
    InMemoryPluginRegistry, PluginDriverKind, PluginEnvironmentDriverDecl, PluginRow,
    PluginStatus,
};
use pc_environment::plugin_worker_manager::{
    InMemoryPluginWorkerManager, PluginRpcError, PluginRpcResult, PluginWorkerManager,
};
use pc_environment::probe_environment_driver::{
    list_ready_plugin_environment_drivers, probe_plugin_environment_driver,
    InMemoryRecovery, ProbeEnvironmentDriverError, ReadyPluginWorkerRecovery,
};
use serde_json::{json, Map, Value};

fn make_config(plugin_key: &str, driver_key: &str) -> PluginEnvironmentConfig {
    PluginEnvironmentConfig {
        plugin_key: plugin_key.to_string(),
        driver_key: driver_key.to_string(),
        driver_config: Map::new(),
    }
}

fn make_plugin_with_env(
    id: &str,
    plugin_key: &str,
    driver_key: &str,
    status: PluginStatus,
    kind: PluginDriverKind,
) -> PluginRow {
    PluginRow {
        id: id.to_string(),
        plugin_key: plugin_key.to_string(),
        status,
        environment_drivers: vec![PluginEnvironmentDriverDecl {
            driver_key: driver_key.to_string(),
            kind,
            display_name: Some(format!("{} Display", driver_key)),
            description: Some(format!("{} description", driver_key)),
            config_schema: Some(json!({"type": "object"})),
            supports_reusable_leases: Some(true),
            supports_interactive_setup: Some(false),
            interactive_setup_connection_types: Some(vec!["ssh".to_string()]),
            supports_template_capture: Some(true),
            template_ref_kind: Some("image".to_string()),
            template_config_binding: Some(json!({"key": "value"})),
            supports_template_delete: Some(false),
            ..Default::default()
        }],
    }
}

// =================================================================
// probe_plugin_environment_driver
// =================================================================

#[test]
fn r689_probe_happy_path_with_summary() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin_with_env(
        "plugin-1",
        "my-plugin",
        "gcp",
        PluginStatus::Ready,
        PluginDriverKind::Environment,
    ));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_handler("plugin-1", "environmentProbe", |_params| {
        Ok(PluginRpcResult {
            ok: true,
            summary: Some("Custom probe summary".to_string()),
            diagnostics: Some(vec![pc_environment::plugin_worker_manager::PluginRpcDiagnostic {
                severity: "info".to_string(),
                message: "All good".to_string(),
                code: Some("OK".to_string()),
            }]),
            metadata: {
                let mut m = Map::new();
                m.insert("region".to_string(), json!("us-east-1"));
                m
            },
            ..Default::default()
        })
    });

    let config = make_config("my-plugin", "gcp");
    let result = probe_plugin_environment_driver(
        &reg,
        &wm,
        "company-1",
        "env-1",
        &config,
    )
    .unwrap();

    assert!(result.ok);
    assert_eq!(result.driver, "plugin");
    assert_eq!(result.summary, "Custom probe summary");
    let details = result.details.unwrap();
    assert_eq!(details.plugin_key, Some("my-plugin".to_string()));
    assert_eq!(details.driver_key, Some("gcp".to_string()));
    assert_eq!(details.provider, None);
    let diagnostics = details.diagnostics.unwrap();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].severity, "info");
    assert_eq!(diagnostics[0].message, "All good");
    assert_eq!(diagnostics[0].code, Some("OK".to_string()));
    assert_eq!(details.metadata.get("region"), Some(&json!("us-east-1")));
}

#[test]
fn r689_probe_falls_back_to_passed_summary_when_worker_returns_none() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin_with_env(
        "plugin-1",
        "my-plugin",
        "gcp",
        PluginStatus::Ready,
        PluginDriverKind::Environment,
    ));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_handler("plugin-1", "environmentProbe", |_params| {
        Ok(PluginRpcResult {
            ok: true,
            ..Default::default()
        })
    });

    let config = make_config("my-plugin", "gcp");
    let result = probe_plugin_environment_driver(&reg, &wm, "c", "e", &config).unwrap();
    assert!(result.ok);
    assert_eq!(
        result.summary,
        "Plugin environment driver \"my-plugin:gcp\" probe passed."
    );
}

#[test]
fn r689_probe_falls_back_to_failed_summary_on_ok_false() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin_with_env(
        "plugin-1",
        "my-plugin",
        "gcp",
        PluginStatus::Ready,
        PluginDriverKind::Environment,
    ));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_handler("plugin-1", "environmentProbe", |_params| {
        Ok(PluginRpcResult {
            ok: false,
            ..Default::default()
        })
    });

    let config = make_config("my-plugin", "gcp");
    let result = probe_plugin_environment_driver(&reg, &wm, "c", "e", &config).unwrap();
    assert!(!result.ok);
    assert_eq!(
        result.summary,
        "Plugin environment driver \"my-plugin:gcp\" probe failed."
    );
}

#[test]
fn r689_probe_plugin_not_found() {
    let reg = InMemoryPluginRegistry::new();
    let wm = InMemoryPluginWorkerManager::new();
    let config = make_config("missing", "gcp");
    let err = probe_plugin_environment_driver(&reg, &wm, "c", "e", &config).unwrap_err();
    match err {
        ProbeEnvironmentDriverError::Resolve(
            pc_environment::validate_environment_driver::ResolveEnvironmentDriverError::PluginNotFound {
                plugin_key,
            },
        ) => assert_eq!(plugin_key, "missing"),
        _ => panic!("expected PluginNotFound, got {:?}", err),
    }
}

#[test]
fn r689_probe_plugin_not_ready() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin_with_env(
        "plugin-1",
        "my-plugin",
        "gcp",
        PluginStatus::Registered,
        PluginDriverKind::Environment,
    ));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    let config = make_config("my-plugin", "gcp");
    let err = probe_plugin_environment_driver(&reg, &wm, "c", "e", &config).unwrap_err();
    match err {
        ProbeEnvironmentDriverError::Resolve(
            pc_environment::validate_environment_driver::ResolveEnvironmentDriverError::PluginNotReady {
                plugin_key,
                ..
            },
        ) => assert_eq!(plugin_key, "my-plugin"),
        _ => panic!("expected PluginNotReady, got {:?}", err),
    }
}

#[test]
fn r689_probe_driver_not_declared() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin_with_env(
        "plugin-1",
        "my-plugin",
        "gcp",
        PluginStatus::Ready,
        PluginDriverKind::Environment,
    ));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    let config = make_config("my-plugin", "aws");
    let err = probe_plugin_environment_driver(&reg, &wm, "c", "e", &config).unwrap_err();
    match err {
        ProbeEnvironmentDriverError::Resolve(
            pc_environment::validate_environment_driver::ResolveEnvironmentDriverError::DriverNotDeclared {
                plugin_key,
                driver_key,
            },
        ) => {
            assert_eq!(plugin_key, "my-plugin");
            assert_eq!(driver_key, "aws");
        }
        _ => panic!("expected DriverNotDeclared, got {:?}", err),
    }
}

#[test]
fn r689_probe_worker_not_running() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin_with_env(
        "plugin-1",
        "my-plugin",
        "gcp",
        PluginStatus::Ready,
        PluginDriverKind::Environment,
    ));
    let wm = InMemoryPluginWorkerManager::new();
    // Worker not registered
    let config = make_config("my-plugin", "gcp");
    let err = probe_plugin_environment_driver(&reg, &wm, "c", "e", &config).unwrap_err();
    match err {
        ProbeEnvironmentDriverError::Resolve(
            pc_environment::validate_environment_driver::ResolveEnvironmentDriverError::WorkerNotRunning {
                plugin_key,
            },
        ) => assert_eq!(plugin_key, "my-plugin"),
        _ => panic!("expected WorkerNotRunning, got {:?}", err),
    }
}

#[test]
fn r689_probe_worker_rpc_error_propagates() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin_with_env(
        "plugin-1",
        "my-plugin",
        "gcp",
        PluginStatus::Ready,
        PluginDriverKind::Environment,
    ));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_handler("plugin-1", "environmentProbe", |_params| {
        Err("worker exploded".to_string())
    });
    let config = make_config("my-plugin", "gcp");
    let err = probe_plugin_environment_driver(&reg, &wm, "c", "e", &config).unwrap_err();
    match err {
        ProbeEnvironmentDriverError::WorkerRpc(PluginRpcError::HandlerError { .. }) => {}
        _ => panic!("expected WorkerRpc HandlerError, got {:?}", err),
    }
}

// =================================================================
// list_ready_plugin_environment_drivers
// =================================================================

#[test]
fn r689_list_ready_returns_empty_when_no_worker_manager() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin_with_env(
        "plugin-1",
        "my-plugin",
        "gcp",
        PluginStatus::Ready,
        PluginDriverKind::SandboxProvider,
    ));
    let rows = list_ready_plugin_environment_drivers(&reg, None, None);
    assert!(rows.is_empty());
}

#[test]
fn r689_list_ready_filters_non_ready_plugins() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin_with_env(
        "plugin-1",
        "p1",
        "gcp",
        PluginStatus::Registered,
        PluginDriverKind::SandboxProvider,
    ));
    reg.add_plugin(make_plugin_with_env(
        "plugin-2",
        "p2",
        "aws",
        PluginStatus::Registered,
        PluginDriverKind::SandboxProvider,
    ));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_worker("plugin-2");
    let rows = list_ready_plugin_environment_drivers(&reg, Some(&wm), None);
    assert!(rows.is_empty(), "non-ready plugins should be filtered out");
}

#[test]
fn r689_list_ready_filters_non_sandbox_drivers() {
    let reg = InMemoryPluginRegistry::new();
    // A plugin declaring only Environment kind drivers should NOT appear
    // in the sandbox-provider listing (Node parity).
    reg.add_plugin(make_plugin_with_env(
        "plugin-1",
        "my-plugin",
        "gcp",
        PluginStatus::Ready,
        PluginDriverKind::Environment,
    ));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    let rows = list_ready_plugin_environment_drivers(&reg, Some(&wm), None);
    assert!(rows.is_empty(), "Environment drivers filtered out");
}

#[test]
fn r689_list_ready_happy_path_returns_rows_with_extended_fields() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin_with_env(
        "plugin-1",
        "my-plugin",
        "gcp",
        PluginStatus::Ready,
        PluginDriverKind::SandboxProvider,
    ));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    let rows = list_ready_plugin_environment_drivers(&reg, Some(&wm), None);
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.plugin_id, "plugin-1");
    assert_eq!(row.plugin_key, "my-plugin");
    assert_eq!(row.driver_key, "gcp");
    assert_eq!(row.display_name, Some("gcp Display".to_string()));
    assert_eq!(row.description, Some("gcp description".to_string()));
    assert_eq!(row.supports_reusable_leases, Some(true));
    assert_eq!(row.supports_interactive_setup, Some(false));
    assert_eq!(
        row.interactive_setup_connection_types,
        Some(vec!["ssh".to_string()])
    );
    assert_eq!(row.supports_template_capture, Some(true));
    assert_eq!(row.template_ref_kind, Some("image".to_string()));
    assert_eq!(row.supports_template_delete, Some(false));
}

#[test]
fn r689_list_ready_skips_plugins_whose_worker_is_not_running() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin_with_env(
        "plugin-1",
        "p1",
        "gcp",
        PluginStatus::Ready,
        PluginDriverKind::SandboxProvider,
    ));
    let wm = InMemoryPluginWorkerManager::new();
    // Worker is stopped but registered.
    wm.register_worker("plugin-1");
    wm.stop_worker("plugin-1");
    let rows = list_ready_plugin_environment_drivers(&reg, Some(&wm), None);
    assert!(
        rows.is_empty(),
        "stopped workers must not appear in listReady"
    );
}

#[test]
fn r689_list_ready_recovery_triggers_start_worker_for_unregistered_worker() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin_with_env(
        "plugin-1",
        "p1",
        "gcp",
        PluginStatus::Ready,
        PluginDriverKind::SandboxProvider,
    ));
    let wm = InMemoryPluginWorkerManager::new();
    // Worker is NOT registered (can recover).

    let recovery = InMemoryRecovery {
        recoverable: {
            let mut s = HashSet::new();
            s.insert("p1".to_string());
            s
        },
        start_outcome: true,
        ..Default::default()
    };

    // Pre-condition: no row would appear without recovery (worker not running).
    assert!(list_ready_plugin_environment_drivers(&reg, Some(&wm), None).is_empty());

    // With recovery set, the start_worker is invoked but no row appears because
    // worker_registered still returns false (the in-memory impl is pure data).
    let rows = list_ready_plugin_environment_drivers(&reg, Some(&wm), Some(&recovery));
    let started = recovery.started.lock().unwrap();
    assert_eq!(started.len(), 1, "start_worker should be invoked once");
    assert_eq!(started[0].0, "plugin-1");
    assert_eq!(started[0].1, "p1");
    assert!(
        rows.is_empty(),
        "no rows because worker still not registered after recovery call"
    );
}

#[test]
fn r689_list_ready_recovery_only_for_recoverable_plugin_keys() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin_with_env(
        "plugin-1",
        "p1",
        "gcp",
        PluginStatus::Ready,
        PluginDriverKind::SandboxProvider,
    ));
    reg.add_plugin(make_plugin_with_env(
        "plugin-2",
        "p2",
        "aws",
        PluginStatus::Ready,
        PluginDriverKind::SandboxProvider,
    ));
    let wm = InMemoryPluginWorkerManager::new();

    let recovery = InMemoryRecovery {
        recoverable: {
            let mut s = HashSet::new();
            s.insert("p1".to_string()); // p2 NOT in recoverable set
            s
        },
        start_outcome: true,
        ..Default::default()
    };

    let _ = list_ready_plugin_environment_drivers(&reg, Some(&wm), Some(&recovery));
    let started = recovery.started.lock().unwrap();
    assert_eq!(started.len(), 1, "only p1 should be recovered");
    assert_eq!(started[0].0, "plugin-1");
}

#[test]
fn r689_list_ready_recovery_only_for_plugins_with_sandbox_provider_driver() {
    let reg = InMemoryPluginRegistry::new();
    // Plugin declares only Environment kind driver -> NOT recoverable.
    reg.add_plugin(make_plugin_with_env(
        "plugin-1",
        "p1",
        "gcp",
        PluginStatus::Ready,
        PluginDriverKind::Environment,
    ));
    let wm = InMemoryPluginWorkerManager::new();

    let recovery = InMemoryRecovery {
        recoverable: {
            let mut s = HashSet::new();
            s.insert("p1".to_string());
            s
        },
        start_outcome: true,
        ..Default::default()
    };

    let _ = list_ready_plugin_environment_drivers(&reg, Some(&wm), Some(&recovery));
    let started = recovery.started.lock().unwrap();
    assert_eq!(
        started.len(),
        0,
        "plugins without sandbox_provider drivers must not be recovered"
    );
}

#[test]
fn r689_list_ready_no_recovery_when_worker_is_running() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin_with_env(
        "plugin-1",
        "p1",
        "gcp",
        PluginStatus::Ready,
        PluginDriverKind::SandboxProvider,
    ));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    // Worker is already running: no recovery should fire.

    let recovery = InMemoryRecovery {
        recoverable: {
            let mut s = HashSet::new();
            s.insert("p1".to_string());
            s
        },
        start_outcome: true,
        ..Default::default()
    };

    let rows = list_ready_plugin_environment_drivers(&reg, Some(&wm), Some(&recovery));
    let started = recovery.started.lock().unwrap();
    assert_eq!(started.len(), 0, "running workers must not be recovered");
    assert_eq!(rows.len(), 1);
}

#[test]
fn r689_list_ready_returns_rows_for_multiple_plugins() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin_with_env(
        "plugin-1",
        "p1",
        "gcp",
        PluginStatus::Ready,
        PluginDriverKind::SandboxProvider,
    ));
    reg.add_plugin(make_plugin_with_env(
        "plugin-2",
        "p2",
        "aws",
        PluginStatus::Ready,
        PluginDriverKind::SandboxProvider,
    ));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-1");
    wm.register_worker("plugin-2");
    let rows = list_ready_plugin_environment_drivers(&reg, Some(&wm), None);
    assert_eq!(rows.len(), 2);
}
