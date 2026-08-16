// SPDX-License-Identifier: MIT
//
// R687 parity tests for the top-level validatePluginSandboxProviderConfig pipeline.

use pc_environment::plugin_environment_driver_validate::ValidateConfigError;
use pc_environment::plugin_registry::{
    InMemoryPluginRegistry, PluginDriverKind, PluginEnvironmentDriverDecl, PluginRegistry,
    PluginRow, PluginStatus,
};
use pc_environment::plugin_worker_manager::{
    InMemoryPluginWorkerManager, PluginRpcResult, PluginWorkerManager,
};
use pc_environment::validate_sandbox_provider::{
    validate_plugin_sandbox_provider_config, NotFoundReason, ValidateSandboxProviderError,
};
use serde_json::json;

fn binding(id: &str, version: Option<u64>) -> serde_json::Value {
    let mut v = json!({"type": "secret_ref", "secretId": id});
    if let Some(ver) = version {
        v.as_object_mut().unwrap().insert("version".to_string(), json!(ver));
    }
    v
}

fn make_aws_plugin(status: PluginStatus) -> PluginRow {
    PluginRow {
        id: "plugin-aws".into(),
        plugin_key: "paperclip-aws".into(),
        status,
        environment_drivers: vec![PluginEnvironmentDriverDecl {
            driver_key: "aws".into(),
            kind: PluginDriverKind::SandboxProvider,
            display_name: Some("AWS".into()),
            description: None,
            config_schema: None,
        ..Default::default()
        }],
    }
}

#[test]
fn r687_happy_path_returns_normalized() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_aws_plugin(PluginStatus::Ready));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-aws");
    wm.register_handler("plugin-aws", "environmentValidateConfig", |params| {
        let config = params.get("config").cloned().unwrap_or(json!({}));
        Ok(PluginRpcResult {
            ok: true,
            errors: vec![],
            warnings: vec![],
            normalized_config: Some(config),
        ..Default::default()
                })
    });

    let out = validate_plugin_sandbox_provider_config(
        &reg, &wm, "aws", &json!({"region": "us-east-1"}),
    ).unwrap();
    assert_eq!(out.driver_key, "aws");
    assert_eq!(out.plugin_id, "plugin-aws");
    assert_eq!(out.plugin_key, "paperclip-aws");
    assert_eq!(out.normalized_config["region"], "us-east-1");
}

#[test]
fn r687_not_found_no_such_provider() {
    let reg = InMemoryPluginRegistry::new();
    let wm = InMemoryPluginWorkerManager::new();
    let err = validate_plugin_sandbox_provider_config(
        &reg, &wm, "missing-provider", &json!({}),
    ).unwrap_err();
    match err {
        ValidateSandboxProviderError::NotFound { provider, reason } => {
            assert_eq!(provider, "missing-provider");
            assert_eq!(reason, NotFoundReason::NoSuchProvider);
        }
        _ => panic!("expected NotFound"),
    }
}

#[test]
fn r687_not_found_plugin_not_ready() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_aws_plugin(PluginStatus::Installed)); // not Ready
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-aws");
    let err = validate_plugin_sandbox_provider_config(
        &reg, &wm, "aws", &json!({}),
    ).unwrap_err();
    match err {
        ValidateSandboxProviderError::NotFound { provider, reason } => {
            assert_eq!(provider, "aws");
            match reason {
                NotFoundReason::PluginNotReady { plugin_id, plugin_key } => {
                    assert_eq!(plugin_id, "plugin-aws");
                    assert_eq!(plugin_key, "paperclip-aws");
                }
                _ => panic!("expected PluginNotReady, got {:?}", reason),
            }
        }
        _ => panic!("expected NotFound"),
    }
}

#[test]
fn r687_not_found_worker_not_running() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_aws_plugin(PluginStatus::Ready));
    let wm = InMemoryPluginWorkerManager::new();
    // worker NOT registered
    let err = validate_plugin_sandbox_provider_config(
        &reg, &wm, "aws", &json!({}),
    ).unwrap_err();
    match err {
        ValidateSandboxProviderError::NotFound { reason, .. } => {
            match reason {
                NotFoundReason::WorkerNotRunning { plugin_id, .. } => {
                    assert_eq!(plugin_id, "plugin-aws");
                }
                _ => panic!("expected WorkerNotRunning"),
            }
        }
        _ => panic!("expected NotFound"),
    }
}

#[test]
fn r687_secret_binding_normalized_end_to_end() {
    let reg = InMemoryPluginRegistry::new();
    let mut p = make_aws_plugin(PluginStatus::Ready);
    p.environment_drivers[0].config_schema = Some(json!({
        "properties": {"apiKey": {"format": "secret-ref"}}
    }));
    reg.add_plugin(p);
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-aws");
    wm.register_handler("plugin-aws", "environmentValidateConfig", |params| {
        let config = params.get("config").cloned().unwrap_or(json!({}));
        Ok(PluginRpcResult {
            ok: true,
            errors: vec![],
            warnings: vec![],
            normalized_config: Some(config),
        ..Default::default()
                })
    });

    let config = json!({"apiKey": binding("01234567-89ab-cdef-0123-456789abcdef", None)});
    let out = validate_plugin_sandbox_provider_config(&reg, &wm, "aws", &config).unwrap();
    assert_eq!(out.normalized_config["apiKey"], "01234567-89ab-cdef-0123-456789abcdef");
}

#[test]
fn r687_pinned_version_error_propagates_as_validate_error() {
    let reg = InMemoryPluginRegistry::new();
    let mut p = make_aws_plugin(PluginStatus::Ready);
    p.environment_drivers[0].config_schema = Some(json!({
        "properties": {"apiKey": {"format": "secret-ref"}}
    }));
    reg.add_plugin(p);
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-aws");
    wm.register_handler("plugin-aws", "environmentValidateConfig", |params| {
        let config = params.get("config").cloned().unwrap_or(json!({}));
        Ok(PluginRpcResult {
            ok: true,
            errors: vec![],
            warnings: vec![],
            normalized_config: Some(config),
        ..Default::default()
                })
    });

    let config = json!({"apiKey": binding("01234567-89ab-cdef-0123-456789abcdef", Some(3))});
    let err = validate_plugin_sandbox_provider_config(&reg, &wm, "aws", &config).unwrap_err();
    match err {
        ValidateSandboxProviderError::Validate(ValidateConfigError::SecretBinding(_)) => {
            // expected
        }
        _ => panic!("expected Validate(SecretBinding)"),
    }
}

#[test]
fn r687_worker_rejected_propagates() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_aws_plugin(PluginStatus::Ready));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-aws");
    wm.register_handler("plugin-aws", "environmentValidateConfig", |_params| {
        Ok(PluginRpcResult {
            ok: false,
            errors: vec!["bad config".to_string()],
            warnings: vec![],
            normalized_config: None,
        ..Default::default()
                })
    });

    let err = validate_plugin_sandbox_provider_config(&reg, &wm, "aws", &json!({})).unwrap_err();
    match err {
        ValidateSandboxProviderError::Validate(ValidateConfigError::WorkerRejected { errors, .. }) => {
            assert_eq!(errors, vec!["bad config".to_string()]);
        }
        _ => panic!("expected Validate(WorkerRejected)"),
    }
}

#[test]
fn r687_worker_method_not_registered_propagates() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_aws_plugin(PluginStatus::Ready));
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-aws"); // no handler
    let err = validate_plugin_sandbox_provider_config(&reg, &wm, "aws", &json!({})).unwrap_err();
    match err {
        ValidateSandboxProviderError::Validate(ValidateConfigError::WorkerRpc(_)) => {
        }
        _ => panic!("expected Validate(WorkerRpc)"),
    }
}

#[test]
fn r687_multiple_plugins_first_match_wins() {
    let reg = InMemoryPluginRegistry::new();
    reg.add_plugin(make_aws_plugin(PluginStatus::Ready));
    let mut gcp = PluginRow {
        id: "plugin-gcp".into(),
        plugin_key: "paperclip-gcp".into(),
        status: PluginStatus::Ready,
        environment_drivers: vec![PluginEnvironmentDriverDecl {
            driver_key: "gcp".into(),
            kind: PluginDriverKind::SandboxProvider,
            display_name: Some("GCP".into()),
            description: None,
            config_schema: None,
        ..Default::default()
        }],
    };
    reg.add_plugin(gcp);
    let wm = InMemoryPluginWorkerManager::new();
    wm.register_worker("plugin-aws");
    wm.register_worker("plugin-gcp");
    wm.register_handler("plugin-gcp", "environmentValidateConfig", |params| {
        let config = params.get("config").cloned().unwrap_or(json!({}));
        Ok(PluginRpcResult {
            ok: true,
            errors: vec![],
            warnings: vec![],
            normalized_config: Some(config),
        ..Default::default()
                })
    });
    let out = validate_plugin_sandbox_provider_config(&reg, &wm, "gcp", &json!({})).unwrap();
    assert_eq!(out.plugin_id, "plugin-gcp");
}

#[test]
fn r687_error_display_not_found() {
    let err = ValidateSandboxProviderError::NotFound {
        provider: "aws".into(),
        reason: NotFoundReason::NoSuchProvider,
    };
    let s = err.to_string();
    assert!(s.contains("aws"));
    assert!(s.contains("not installed"));
}

#[test]
fn r687_error_display_validate_propagates() {
    let inner = ValidateConfigError::WorkerRejected {
        provider: "aws".into(),
        first_error: "bad".into(),
        errors: vec!["bad".into()],
        warnings: vec![],
    };
    let outer = ValidateSandboxProviderError::from(inner);
    assert!(matches!(outer, ValidateSandboxProviderError::Validate(_)));
}

#[test]
fn r687_not_found_reason_variants() {
    assert_eq!(NotFoundReason::NoSuchProvider, NotFoundReason::NoSuchProvider);
    let a = NotFoundReason::PluginNotReady {
        plugin_id: "p".into(),
        plugin_key: "k".into(),
    };
    let b = NotFoundReason::PluginNotReady {
        plugin_id: "p".into(),
        plugin_key: "k".into(),
    };
    assert_eq!(a, b);
}

#[test]
fn r687_empty_registry_not_found() {
    let reg = InMemoryPluginRegistry::new();
    let wm = InMemoryPluginWorkerManager::new();
    let err = validate_plugin_sandbox_provider_config(&reg, &wm, "anything", &json!({})).unwrap_err();
    assert!(matches!(err, ValidateSandboxProviderError::NotFound { .. }));
}
