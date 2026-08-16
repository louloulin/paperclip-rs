// SPDX-License-Identifier: MIT
//
// R684 parity tests for PluginWorkerManager trait + InMemoryPluginWorkerManager.

use pc_environment::plugin_worker_manager::{
    InMemoryPluginWorkerManager, PluginRpcError, PluginRpcResult, PluginWorkerManager,
    PluginWorkerManagerInspect, WorkerStatus,
};
use serde_json::json;

#[test]
fn r684_new_manager_no_workers() {
    let m = InMemoryPluginWorkerManager::new();
    assert!(m.registered_workers().is_empty());
    assert!(!m.is_running("plugin-1"));
}

#[test]
fn r684_register_worker_marks_running() {
    let m = InMemoryPluginWorkerManager::new();
    m.register_worker("plugin-1");
    assert!(m.is_running("plugin-1"));
    assert_eq!(
        m.worker_status("plugin-1"),
        Some(WorkerStatus::Running)
    );
    assert_eq!(m.registered_workers(), vec!["plugin-1".to_string()]);
}

#[test]
fn r684_call_unknown_worker_returns_not_running() {
    let m = InMemoryPluginWorkerManager::new();
    let err = m.call("missing", "anyMethod", json!({}), None).unwrap_err();
    match err {
        PluginRpcError::WorkerNotRunning { plugin_id } => {
            assert_eq!(plugin_id, "missing");
        }
        _ => panic!("expected WorkerNotRunning"),
    }
}

#[test]
fn r684_call_stopped_worker_returns_not_running() {
    let m = InMemoryPluginWorkerManager::new();
    m.register_worker("plugin-1");
    m.stop_worker("plugin-1");
    assert!(!m.is_running("plugin-1"));
    let err = m.call("plugin-1", "anyMethod", json!({}), None).unwrap_err();
    assert!(matches!(err, PluginRpcError::WorkerNotRunning { .. }));
}

#[test]
fn r684_call_unregistered_method_returns_method_error() {
    let m = InMemoryPluginWorkerManager::new();
    m.register_worker("plugin-1");
    let err = m.call("plugin-1", "noSuchMethod", json!({}), None).unwrap_err();
    match err {
        PluginRpcError::MethodNotRegistered { plugin_id, method } => {
            assert_eq!(plugin_id, "plugin-1");
            assert_eq!(method, "noSuchMethod");
        }
        _ => panic!("expected MethodNotRegistered"),
    }
}

#[test]
fn r684_call_handler_returns_ok_result() {
    let m = InMemoryPluginWorkerManager::new();
    m.register_worker("plugin-1");
    m.register_handler("plugin-1", "environmentValidateConfig", |_params| {
        Ok(PluginRpcResult {
            ok: true,
            errors: vec![],
            warnings: vec![],
            normalized_config: Some(json!({"region": "us-east-1"})),
        ..Default::default()
                })
    });
    let r = m.call("plugin-1", "environmentValidateConfig", json!({}), None).unwrap();
    assert!(r.ok);
    assert!(r.errors.is_empty());
    assert_eq!(
        r.normalized_config,
        Some(json!({"region": "us-east-1"}))
    );
}

#[test]
fn r684_call_handler_returns_error_propagates() {
    let m = InMemoryPluginWorkerManager::new();
    m.register_worker("plugin-1");
    m.register_handler("plugin-1", "validateX", |_params| {
        Err("rejected by plugin".to_string())
    });
    let err = m.call("plugin-1", "validateX", json!({}), None).unwrap_err();
    match err {
        PluginRpcError::HandlerError { plugin_id, method, message } => {
            assert_eq!(plugin_id, "plugin-1");
            assert_eq!(method, "validateX");
            assert_eq!(message, "rejected by plugin");
        }
        _ => panic!("expected HandlerError"),
    }
}

#[test]
fn r684_call_handler_receives_params() {
    let m = InMemoryPluginWorkerManager::new();
    m.register_worker("plugin-1");
    m.register_handler("plugin-1", "echo", |params| {
        Ok(PluginRpcResult {
            ok: true,
            errors: vec![],
            warnings: vec![],
            normalized_config: Some(params),
        ..Default::default()
                })
    });
    let params = json!({"a": 1, "b": [true, false]});
    let r = m.call("plugin-1", "echo", params.clone(), None).unwrap();
    assert_eq!(r.normalized_config, Some(params));
}

#[test]
fn r684_multiple_workers_independent() {
    let m = InMemoryPluginWorkerManager::new();
    m.register_worker("a");
    m.register_worker("b");
    m.register_handler("a", "x", |_| Ok(PluginRpcResult { ok: true, ..Default::default() }));
    m.register_handler("b", "y", |_| Ok(PluginRpcResult { ok: true, ..Default::default() }));
    assert!(m.is_running("a"));
    assert!(m.is_running("b"));
    assert!(m.call("a", "x", json!({}), None).is_ok());
    // b's x is not registered
    let err = m.call("b", "x", json!({}), None).unwrap_err();
    assert!(matches!(err, PluginRpcError::MethodNotRegistered { .. }));
}

#[test]
fn r684_registered_methods_sorted() {
    let m = InMemoryPluginWorkerManager::new();
    m.register_worker("plugin-1");
    m.register_handler("plugin-1", "zeta", |_| Ok(PluginRpcResult::default()));
    m.register_handler("plugin-1", "alpha", |_| Ok(PluginRpcResult::default()));
    m.register_handler("plugin-1", "mu", |_| Ok(PluginRpcResult::default()));
    assert_eq!(
        m.registered_methods("plugin-1"),
        vec!["alpha".to_string(), "mu".to_string(), "zeta".to_string()]
    );
}

#[test]
fn r684_remove_worker_clears_all() {
    let m = InMemoryPluginWorkerManager::new();
    m.register_worker("plugin-1");
    m.register_handler("plugin-1", "x", |_| Ok(PluginRpcResult::default()));
    m.remove_worker("plugin-1");
    assert!(!m.is_running("plugin-1"));
    assert_eq!(m.worker_status("plugin-1"), None);
}

#[test]
fn r684_stop_then_remove_yields_none_status() {
    let m = InMemoryPluginWorkerManager::new();
    m.register_worker("plugin-1");
    m.stop_worker("plugin-1");
    assert_eq!(m.worker_status("plugin-1"), Some(WorkerStatus::Stopped));
    m.remove_worker("plugin-1");
    assert_eq!(m.worker_status("plugin-1"), None);
}

#[test]
fn r684_call_after_remove_returns_not_running() {
    let m = InMemoryPluginWorkerManager::new();
    m.register_worker("plugin-1");
    m.register_handler("plugin-1", "x", |_| Ok(PluginRpcResult::default()));
    m.remove_worker("plugin-1");
    let err = m.call("plugin-1", "x", json!({}), None).unwrap_err();
    assert!(matches!(err, PluginRpcError::WorkerNotRunning { .. }));
}

#[test]
fn r684_register_handler_to_unregistered_worker_panics() {
    let m = InMemoryPluginWorkerManager::new();
    let result = std::panic::catch_unwind(|| {
        m.register_handler("missing", "x", |_| Ok(PluginRpcResult::default()));
    });
    assert!(result.is_err());
}

#[test]
fn r684_re_register_worker_resets_handlers() {
    let m = InMemoryPluginWorkerManager::new();
    m.register_worker("plugin-1");
    m.register_handler("plugin-1", "x", |_| Ok(PluginRpcResult::default()));
    m.register_worker("plugin-1"); // re-register
    assert_eq!(m.registered_methods("plugin-1").len(), 0);
}

#[test]
fn r684_error_display_messages() {
    let e1 = PluginRpcError::WorkerNotRunning { plugin_id: "p1".into() };
    assert!(e1.to_string().contains("p1"));
    let e2 = PluginRpcError::MethodNotRegistered {
        plugin_id: "p1".into(),
        method: "foo".into(),
    };
    assert!(e2.to_string().contains("foo"));
    let e3 = PluginRpcError::HandlerError {
        plugin_id: "p1".into(),
        method: "foo".into(),
        message: "bad".into(),
    };
    assert!(e3.to_string().contains("bad"));
}

#[test]
fn r684_concurrent_call_does_not_deadlock() {
    use std::thread;
    let m = InMemoryPluginWorkerManager::new();
    m.register_worker("plugin-1");
    m.register_handler("plugin-1", "slow", |_| {
        std::thread::sleep(std::time::Duration::from_millis(10));
        Ok(PluginRpcResult { ok: true, ..Default::default() })
    });
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let m = m.clone();
            thread::spawn(move || {
                m.call("plugin-1", "slow", json!({"i": i}), None).is_ok()
            })
        })
        .collect();
    for h in handles {
        assert!(h.join().unwrap());
    }
}

#[test]
fn r684_plugin_rpc_result_default() {
    let r = PluginRpcResult::default();
    assert!(!r.ok);
    assert!(r.errors.is_empty());
    assert!(r.warnings.is_empty());
    assert!(r.normalized_config.is_none());
}

#[test]
fn r684_worker_status_serde() {
    let s = serde_json::to_string(&WorkerStatus::Running).unwrap();
    assert_eq!(s, "\"running\"");
    let back: WorkerStatus = serde_json::from_str(&s).unwrap();
    assert_eq!(back, WorkerStatus::Running);
}
