// SPDX-License-Identifier: MIT
//
// R686 parity tests for PluginRegistry trait + resolvePluginSandboxProviderDriverByKey.

use pc_environment::plugin_registry::{
    list_ready_sandbox_provider_drivers, resolve_sandbox_provider_driver_key,
    InMemoryPluginRegistry, PluginDriverKind, PluginEnvironmentDriverDecl, PluginRegistry,
    PluginRow, PluginStatus, ReadyPluginEnvironmentDriver, ResolvedSandboxProviderDriver,
};
use pc_environment::plugin_worker_manager::{
    InMemoryPluginWorkerManager, PluginWorkerManager,
};
use serde_json::json;

fn make_plugin(id: &str, key: &str, status: PluginStatus) -> PluginRow {
    PluginRow {
        id: id.to_string(),
        plugin_key: key.to_string(),
        status,
        environment_drivers: vec![],
    }
}

fn add_sandbox_driver(plugin: &mut PluginRow, driver_key: &str) {
    plugin.environment_drivers.push(PluginEnvironmentDriverDecl {
        driver_key: driver_key.to_string(),
        kind: PluginDriverKind::SandboxProvider,
        display_name: Some(driver_key.to_string()),
        description: None,
        config_schema: None,
    ..Default::default()
    });
}

fn add_env_driver(plugin: &mut PluginRow, driver_key: &str) {
    plugin.environment_drivers.push(PluginEnvironmentDriverDecl {
        driver_key: driver_key.to_string(),
        kind: PluginDriverKind::Environment,
        display_name: None,
        description: None,
        config_schema: None,
    ..Default::default()
    });
}

fn ready_aws_plugin() -> PluginRow {
    let mut p = make_plugin("p-aws", "paperclip-aws", PluginStatus::Ready);
    add_sandbox_driver(&mut p, "aws");
    p
}

#[test]
fn r686_empty_registry_returns_none() {
    let reg = InMemoryPluginRegistry::new();
    assert!(resolve_sandbox_provider_driver_key(&reg, None, "aws", false).is_none());
}

#[test]
fn r686_plugin_without_drivers_returns_none() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_plugin("p", "k", PluginStatus::Ready));
    assert!(resolve_sandbox_provider_driver_key(&reg, None, "aws", false).is_none());
}

#[test]
fn r686_plugin_with_wrong_driver_key_returns_none() {
    let reg = InMemoryPluginRegistry::new();
    let mut p = make_plugin("p", "k", PluginStatus::Ready);
    add_sandbox_driver(&mut p, "gcp");
    reg.add_plugin(p);
    assert!(resolve_sandbox_provider_driver_key(&reg, None, "aws", false).is_none());
}

#[test]
fn r686_env_driver_kind_excluded_from_sandbox_search() {
    let reg = InMemoryPluginRegistry::new();
    let mut p = make_plugin("p", "k", PluginStatus::Ready);
    add_env_driver(&mut p, "aws"); // env kind, not sandbox
    reg.add_plugin(p);
    assert!(resolve_sandbox_provider_driver_key(&reg, None, "aws", false).is_none());
}

#[test]
fn r686_finds_sandbox_provider_by_key() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(ready_aws_plugin());
    let r = resolve_sandbox_provider_driver_key(&reg, None, "aws", false).unwrap();
    assert_eq!(r.plugin.plugin_key, "paperclip-aws");
    assert_eq!(r.driver.driver_key, "aws");
    assert_eq!(r.driver.kind, PluginDriverKind::SandboxProvider);
}

#[test]
fn r686_multiple_plugins_first_match_wins() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(ready_aws_plugin());
    let mut gcp = make_plugin("p-gcp", "paperclip-gcp", PluginStatus::Ready);
    add_sandbox_driver(&mut gcp, "gcp");
    reg.add_plugin(gcp);
    let r = resolve_sandbox_provider_driver_key(&reg, None, "gcp", false).unwrap();
    assert_eq!(r.plugin.id, "p-gcp");
}

#[test]
fn r686_require_running_skips_not_ready_plugin() {
    let reg = InMemoryPluginRegistry::new();
    let mut p = make_plugin("p-aws", "k", PluginStatus::Installed); // not Ready
    add_sandbox_driver(&mut p, "aws");
    reg.add_plugin(p);
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("p-aws");
    let r = resolve_sandbox_provider_driver_key(&reg, Some(&wm), "aws", true);
    assert!(r.is_none(), "not-ready plugin should be skipped when requireRunning");
}

#[test]
fn r686_require_running_skips_worker_not_running() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(ready_aws_plugin());
    let wm = InMemoryPluginWorkerManager::new();
    // worker NOT registered
    let r = resolve_sandbox_provider_driver_key(&reg, Some(&wm), "aws", true);
    assert!(r.is_none(), "worker not running should be skipped");
}

#[test]
fn r686_require_running_no_worker_manager_returns_none() {
    // Node: requireRunning=true without workerManager returns null on candidate match.
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(ready_aws_plugin());
    let r = resolve_sandbox_provider_driver_key(&reg, None, "aws", true);
    assert!(r.is_none());
}

#[test]
fn r686_require_running_all_checks_pass() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(ready_aws_plugin());
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("p-aws");
    let r = resolve_sandbox_provider_driver_key(&reg, Some(&wm), "aws", true).unwrap();
    assert_eq!(r.plugin.plugin_key, "paperclip-aws");
}

#[test]
fn r686_set_plugins_replaces_list() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(ready_aws_plugin());
    assert_eq!(reg.plugin_count(), 1);
    reg.set_plugins(vec![]);
    assert_eq!(reg.plugin_count(), 0);
}

#[test]
fn r686_plugin_status_serde() {
    let s = serde_json::to_string(&PluginStatus::Ready).unwrap();
    assert_eq!(s, "\"ready\"");
    let back: PluginStatus = serde_json::from_str(&s).unwrap();
    assert_eq!(back, PluginStatus::Ready);
}

#[test]
fn r686_driver_kind_serde() {
    let s = serde_json::to_string(&PluginDriverKind::SandboxProvider).unwrap();
    assert_eq!(s, "\"sandbox_provider\"");
    let back: PluginDriverKind = serde_json::from_str(&s).unwrap();
    assert_eq!(back, PluginDriverKind::SandboxProvider);
}

#[test]
fn r686_plugin_row_default() {
    let p = PluginRow::default();
    assert_eq!(p.id, "");
    assert_eq!(p.plugin_key, "");
    assert_eq!(p.status, PluginStatus::Installed);
    assert!(p.environment_drivers.is_empty());
}

#[test]
fn r686_driver_decl_default() {
    let d = PluginEnvironmentDriverDecl::default();
    assert_eq!(d.driver_key, "");
    assert_eq!(d.kind, PluginDriverKind::SandboxProvider);
    assert!(d.display_name.is_none());
}

#[test]
fn r686_list_ready_sandbox_provider_drivers_filters_correctly() {
    let reg = InMemoryPluginRegistry::new();
    // ready + running aws
    reg.add_plugin(ready_aws_plugin());
    // ready + running gcp
    let mut gcp = make_plugin("p-gcp", "paperclip-gcp", PluginStatus::Ready);
    add_sandbox_driver(&mut gcp, "gcp");
    reg.add_plugin(gcp);
    // ready but worker NOT running (excluded)
    let mut other = make_plugin("p-other", "k", PluginStatus::Ready);
    add_sandbox_driver(&mut other, "other");
    reg.add_plugin(other);
    // not ready (excluded)
    let mut pending = make_plugin("p-pending", "k", PluginStatus::Registered);
    add_sandbox_driver(&mut pending, "pending");
    reg.add_plugin(pending);
    // env driver (excluded by kind)
    let mut env_plugin = make_plugin("p-env", "k", PluginStatus::Ready);
    add_env_driver(&mut env_plugin, "env-driver");
    reg.add_plugin(env_plugin);

    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("p-aws");
    wm.register_worker("p-gcp");
    wm.register_worker("p-env");

    let drivers = list_ready_sandbox_provider_drivers(&reg, &wm);
    let keys: Vec<String> = drivers.iter().map(|d| d.driver_key.clone()).collect();
    assert_eq!(keys, vec!["aws".to_string(), "gcp".to_string()]);
}

#[test]
fn r686_list_ready_empty_registry() {
    let reg = InMemoryPluginRegistry::new();
    let wm = InMemoryPluginWorkerManager::new();
    assert!(list_ready_sandbox_provider_drivers(&reg, &wm).is_empty());
}

#[test]
fn r686_resolved_struct_serde() {
    let mut p = ready_aws_plugin();
    add_sandbox_driver(&mut p, "aws");
    let r = ResolvedSandboxProviderDriver {
        plugin: p,
        driver: PluginEnvironmentDriverDecl {
            driver_key: "aws".to_string(),
            kind: PluginDriverKind::SandboxProvider,
            display_name: Some("AWS".to_string()),
            description: None,
            config_schema: Some(json!({"type": "object"})),
        ..Default::default()
        },
    };
    let s = serde_json::to_string(&r).unwrap();
    let back: ResolvedSandboxProviderDriver = serde_json::from_str(&s).unwrap();
    assert_eq!(back, r);
}

#[test]
fn r686_ready_plugin_environment_driver_default() {
    let d = ReadyPluginEnvironmentDriver {
        plugin_id: "p".into(),
        plugin_key: "k".into(),
        driver_key: "aws".into(),
        display_name: None,
        description: None,
        config_schema: None,
    ..Default::default()
    };
    let s = serde_json::to_string(&d).unwrap();
    assert!(!s.contains("display_name"));
    assert!(!s.contains("config_schema"));
}
