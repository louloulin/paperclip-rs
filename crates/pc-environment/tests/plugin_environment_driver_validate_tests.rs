// SPDX-License-Identifier: MIT
//
// R685 parity tests for validatePluginSandboxProviderConfig full async pipeline.

use pc_environment::plugin_environment_driver_validate::{
    validate_plugin_sandbox_provider_config_after_resolve, ResolvedDriver, ValidateConfigError,
};
use pc_environment::plugin_environment_driver_validate_config::SecretBindingNormalizeError;
use pc_environment::plugin_worker_manager::{
    InMemoryPluginWorkerManager, PluginRpcError, PluginRpcResult, PluginWorkerManager,
};
use serde_json::json;

fn binding(id: &str, version: Option<u64>) -> serde_json::Value {
    let mut v = json!({"type": "secret_ref", "secretId": id});
    if let Some(ver) = version {
        v.as_object_mut().unwrap().insert("version".to_string(), json!(ver));
    }
    v
}

fn make_worker() -> InMemoryPluginWorkerManager {
    InMemoryPluginWorkerManager::new()
}

fn register_aws_worker(wm: &InMemoryPluginWorkerManager) {
    wm.register_worker("plugin-aws");
    wm.register_handler("plugin-aws", "environmentValidateConfig", |params| {
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
}

fn resolved_aws(schema: Option<serde_json::Value>) -> ResolvedDriver {
    ResolvedDriver::new("plugin-aws", "paperclip-aws", "aws", schema)
}

#[test]
fn r685_happy_path_returns_validated() {
    let wm = make_worker();
    register_aws_worker(&wm);
    let resolved = resolved_aws(None);
    let config = json!({"region": "eu-west-1"});
    let out = validate_plugin_sandbox_provider_config_after_resolve(&resolved, &config, &wm).unwrap();
    assert_eq!(out.plugin_id, "plugin-aws");
    assert_eq!(out.plugin_key, "paperclip-aws");
    assert_eq!(out.driver_key, "aws");
    // Worker returned normalized_config with region=us-east-1
    assert_eq!(out.normalized_config["region"], "us-east-1");
}

#[test]
fn r685_secret_binding_normalized_before_worker_call() {
    let wm = make_worker();
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
    let schema = json!({"properties": {"apiKey": {"format": "secret-ref"}}});
    let resolved = resolved_aws(Some(schema));
    let config = json!({"apiKey": binding("01234567-89ab-cdef-0123-456789abcdef", None)});
    let out = validate_plugin_sandbox_provider_config_after_resolve(&resolved, &config, &wm).unwrap();
    assert_eq!(out.normalized_config["apiKey"], "01234567-89ab-cdef-0123-456789abcdef");
}

#[test]
fn r685_pinned_version_error_propagates() {
    let wm = make_worker();
    wm.register_worker("plugin-aws");
    let schema = json!({"properties": {"apiKey": {"format": "secret-ref"}}});
    let resolved = resolved_aws(Some(schema));
    let config = json!({"apiKey": binding("01234567-89ab-cdef-0123-456789abcdef", Some(3))});
    let err = validate_plugin_sandbox_provider_config_after_resolve(&resolved, &config, &wm).unwrap_err();
    match err {
        ValidateConfigError::SecretBinding(SecretBindingNormalizeError::PinnedVersion { path, version, provider }) => {
            assert_eq!(path, "apiKey");
            assert_eq!(version, "3");
            assert_eq!(provider, "aws");
        }
        _ => panic!("expected SecretBinding"),
    }
}

#[test]
fn r685_worker_rpc_not_running_propagates() {
    let wm = make_worker(); // no worker registered
    let resolved = resolved_aws(None);
    let config = json!({});
    let err = validate_plugin_sandbox_provider_config_after_resolve(&resolved, &config, &wm).unwrap_err();
    match err {
        ValidateConfigError::WorkerRpc(PluginRpcError::WorkerNotRunning { plugin_id }) => {
            assert_eq!(plugin_id, "plugin-aws");
        }
        _ => panic!("expected WorkerRpc"),
    }
}

#[test]
fn r685_worker_method_not_registered_propagates() {
    let wm = make_worker();
    wm.register_worker("plugin-aws");
    // no environmentValidateConfig handler registered
    let resolved = resolved_aws(None);
    let config = json!({});
    let err = validate_plugin_sandbox_provider_config_after_resolve(&resolved, &config, &wm).unwrap_err();
    assert!(matches!(err, ValidateConfigError::WorkerRpc(PluginRpcError::MethodNotRegistered { .. })));
}

#[test]
fn r685_worker_rejects_config_propagates_with_errors() {
    let wm = make_worker();
    wm.register_worker("plugin-aws");
    wm.register_handler("plugin-aws", "environmentValidateConfig", |_params| {
        Ok(PluginRpcResult {
            ok: false,
            errors: vec!["region is required".to_string(), "bucket is required".to_string()],
            warnings: vec![],
            normalized_config: None,
        ..Default::default()
        })
    });
    let resolved = resolved_aws(None);
    let config = json!({});
    let err = validate_plugin_sandbox_provider_config_after_resolve(&resolved, &config, &wm).unwrap_err();
    match err {
        ValidateConfigError::WorkerRejected { provider, first_error, errors, warnings } => {
            assert_eq!(provider, "aws");
            assert_eq!(first_error, "region is required");
            assert_eq!(errors.len(), 2);
            assert!(warnings.is_empty());
        }
        _ => panic!("expected WorkerRejected"),
    }
}

#[test]
fn r685_worker_rejected_no_errors_uses_default_message() {
    let wm = make_worker();
    wm.register_worker("plugin-aws");
    wm.register_handler("plugin-aws", "environmentValidateConfig", |_params| {
        Ok(PluginRpcResult {
            ok: false,
            errors: vec![],
            warnings: vec![],
            normalized_config: None,
        ..Default::default()
        })
    });
    let resolved = resolved_aws(None);
    let config = json!({});
    let err = validate_plugin_sandbox_provider_config_after_resolve(&resolved, &config, &wm).unwrap_err();
    match err {
        ValidateConfigError::WorkerRejected { first_error, .. } => {
            assert!(first_error.contains("aws"));
            assert!(first_error.contains("rejected"));
        }
        _ => panic!("expected WorkerRejected"),
    }
}

#[test]
fn r685_falls_back_to_local_normalized_config() {
    let wm = make_worker();
    wm.register_worker("plugin-aws");
    wm.register_handler("plugin-aws", "environmentValidateConfig", |params| {
        let config = params.get("config").cloned().unwrap_or(json!({}));
        Ok(PluginRpcResult {
            ok: true,
            errors: vec![],
            warnings: vec![],
            normalized_config: None, // worker did NOT return normalized
        ..Default::default()
        })
    });
    let schema = json!({"properties": {"apiKey": {"format": "secret-ref"}}});
    let resolved = resolved_aws(Some(schema));
    let config = json!({"apiKey": binding("01234567-89ab-cdef-0123-456789abcdef", None)});
    let out = validate_plugin_sandbox_provider_config_after_resolve(&resolved, &config, &wm).unwrap();
    // Falls back to locally-normalized config (binding -> bare secretId)
    assert_eq!(out.normalized_config["apiKey"], "01234567-89ab-cdef-0123-456789abcdef");
}

#[test]
fn r685_schema_must_be_object() {
    let wm = make_worker();
    register_aws_worker(&wm);
    let resolved = ResolvedDriver::new("plugin-aws", "k", "aws", Some(json!("not-an-object")));
    let config = json!({"apiKey": binding("01234567-89ab-cdef-0123-456789abcdef", None)});
    let out = validate_plugin_sandbox_provider_config_after_resolve(&resolved, &config, &wm).unwrap();
    // Schema not parsed -> config NOT normalized (binding stays as object)
    assert!(out.normalized_config["apiKey"].is_object());
}

#[test]
fn r685_empty_config_ok() {
    let wm = make_worker();
    register_aws_worker(&wm);
    let resolved = resolved_aws(None);
    let out = validate_plugin_sandbox_provider_config_after_resolve(&resolved, &json!({}), &wm).unwrap();
    assert_eq!(out.driver_key, "aws");
}

#[test]
fn r685_error_display_messages() {
    let e = ValidateConfigError::WorkerRejected {
        provider: "aws".to_string(),
        first_error: "bad".to_string(),
        errors: vec!["bad".to_string()],
        warnings: vec![],
    };
    let s = e.to_string();
    assert!(s.contains("aws"));
    assert!(s.contains("bad"));
}

#[test]
fn r685_resolved_driver_constructor() {
    let r = ResolvedDriver::new("id", "key", "driver", None);
    assert_eq!(r.plugin_id, "id");
    assert_eq!(r.plugin_key, "key");
    assert_eq!(r.driver_key, "driver");
    assert!(r.driver_schema.is_none());
}

#[test]
fn r685_nested_secret_binding_pipeline() {
    let wm = make_worker();
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
    let schema = json!({
        "properties": {
            "database": {
                "properties": {
                    "password": {"format": "secret-ref"}
                }
            }
        }
    });
    let resolved = resolved_aws(Some(schema));
    let config = json!({"database": {"password": binding("01234567-89ab-cdef-0123-456789abcdef", None)}});
    let out = validate_plugin_sandbox_provider_config_after_resolve(&resolved, &config, &wm).unwrap();
    assert_eq!(
        out.normalized_config["database"]["password"],
        "01234567-89ab-cdef-0123-456789abcdef"
    );
}
