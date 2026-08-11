#![forbid(unsafe_code)]
//! 当 plugin worker 停止 / 卸载时清理 host services（原 `pc-plugin-host-service-cleanup` 已下沉）。
//!
//! 对应 Node `server/src/services/plugin-host-service-cleanup.ts`（59 行）。
//!
//! 设计目标：1:1 复刻
//! - 监听 `plugin.worker_stopped` → 触发 dispose（保留 disposers）
//! - 监听 `plugin.unloaded` → 触发 dispose + 从 disposers 删除
//! - 提供 `handleWorkerEvent` —— 处理 `plugin.worker.crashed` 类型事件
//! - 提供 `disposeAll` —— 全部清理并清空 disposers
//! - 提供 `teardown` —— 解绑 lifecycle hooks

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Plugin worker 事件 —— 1:1 对应 Node `PluginWorkerRuntimeEvent`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginWorkerRuntimeEvent {
    Crashed { plugin_id: String },
    Restarted { plugin_id: String },
}

impl PluginWorkerRuntimeEvent {
    pub fn from_parts(event_type: &str, plugin_id: String) -> Option<Self> {
        match event_type {
            "plugin.worker.crashed" => Some(Self::Crashed { plugin_id }),
            "plugin.worker.restarted" => Some(Self::Restarted { plugin_id }),
            _ => None,
        }
    }
}

/// Lifecycle hook 抽象 —— 真实实现接入 pc-plugin-lifecycle；测试中用 mock。
pub trait LifecycleLike: Send + Sync + Any {
    fn on(&self, event: &str, handler: Arc<dyn Fn(&serde_json::Value) + Send + Sync>) -> u64;
    fn off(&self, event: &str, handler_id: u64);
    fn as_in_memory(&self) -> Option<&InMemoryLifecycle>;
}

/// 简化版 Lifecycle：用 HashMap 存 (event → handler_id → handler)。
pub struct InMemoryLifecycle {
    handlers: Mutex<HashMap<String, Vec<(u64, Arc<dyn Fn(&serde_json::Value) + Send + Sync>)>>>,
    next_id: Mutex<u64>,
}

impl InMemoryLifecycle {
    pub fn new() -> Self {
        Self {
            handlers: Mutex::new(HashMap::new()),
            next_id: Mutex::new(0),
        }
    }

    pub fn emit(&self, event: &str, payload: &serde_json::Value) {
        let map = self.handlers.lock().unwrap();
        if let Some(handlers) = map.get(event) {
            for (_, h) in handlers {
                h(payload);
            }
        }
    }

    pub fn handler_count(&self, event: &str) -> usize {
        self.handlers
            .lock()
            .unwrap()
            .get(event)
            .map(|v| v.len())
            .unwrap_or(0)
    }
}

impl Default for InMemoryLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

impl LifecycleLike for InMemoryLifecycle {
    fn on(&self, event: &str, handler: Arc<dyn Fn(&serde_json::Value) + Send + Sync>) -> u64 {
        let id = {
            let mut next = self.next_id.lock().unwrap();
            let id = *next;
            *next += 1;
            id
        };
        self.handlers
            .lock()
            .unwrap()
            .entry(event.to_string())
            .or_default()
            .push((id, handler));
        id
    }

    fn off(&self, event: &str, handler_id: u64) {
        let mut map = self.handlers.lock().unwrap();
        if let Some(handlers) = map.get_mut(event) {
            handlers.retain(|(id, _)| *id != handler_id);
        }
    }

    fn as_in_memory(&self) -> Option<&InMemoryLifecycle> {
        Some(self)
    }
}

/// Disposer 函数类型。
pub type Disposer = Arc<dyn Fn() + Send + Sync>;

/// Cleanup controller —— 1:1 对应 Node `PluginHostServiceCleanupController`。
pub struct PluginHostServiceCleanupController {
    lifecycle: Arc<dyn LifecycleLike>,
    disposers: Arc<Mutex<HashMap<String, Disposer>>>,
}

impl PluginHostServiceCleanupController {
    pub fn register_disposer(&self, plugin_id: &str, disposer: Disposer) {
        self.disposers
            .lock()
            .unwrap()
            .insert(plugin_id.to_string(), disposer);
    }

    pub fn handle_worker_event(&self, event: &PluginWorkerRuntimeEvent) {
        if let PluginWorkerRuntimeEvent::Crashed { plugin_id } = event {
            self.run_dispose(plugin_id, false);
        }
    }

    pub fn dispose_all(&self) {
        let disposers = self.disposers.lock().unwrap();
        for dispose in disposers.values() {
            dispose();
        }
        drop(disposers);
        self.disposers.lock().unwrap().clear();
    }

    pub fn teardown(&self, worker_stopped_id: u64, plugin_unloaded_id: u64) {
        self.lifecycle.off("plugin.worker_stopped", worker_stopped_id);
        self.lifecycle.off("plugin.unloaded", plugin_unloaded_id);
    }

    fn run_dispose(&self, plugin_id: &str, remove: bool) {
        let mut map = self.disposers.lock().unwrap();
        let dispose = map.get(plugin_id).cloned();
        if let Some(dispose) = dispose {
            dispose();
            if remove {
                map.remove(plugin_id);
            }
        }
    }
}

/// 创建 cleanup controller 并绑定 lifecycle hooks。
///
/// 与 Node `createPluginHostServiceCleanup` 1:1 对齐。
/// 返回 `(controller, worker_stopped_id, plugin_unloaded_id)`。
pub fn create_plugin_host_service_cleanup(
    lifecycle: Arc<dyn LifecycleLike>,
    disposers: Arc<Mutex<HashMap<String, Disposer>>>,
) -> (PluginHostServiceCleanupController, u64, u64) {
    let controller = PluginHostServiceCleanupController {
        lifecycle: lifecycle.clone(),
        disposers: disposers.clone(),
    };

    // on("plugin.worker_stopped")
    let disposers_for_ws = disposers.clone();
    let handler_ws: Arc<dyn Fn(&serde_json::Value) + Send + Sync> = Arc::new(move |payload: &serde_json::Value| {
        let plugin_id = payload.get("pluginId").and_then(|v| v.as_str()).unwrap_or("");
        let mut map = disposers_for_ws.lock().unwrap();
        if let Some(dispose) = map.get(plugin_id).cloned() {
            dispose();
        }
    });
    let ws_id = lifecycle.on("plugin.worker_stopped", handler_ws);

    // on("plugin.unloaded")
    let disposers_for_pu = disposers.clone();
    let handler_pu: Arc<dyn Fn(&serde_json::Value) + Send + Sync> = Arc::new(move |payload: &serde_json::Value| {
        let plugin_id = payload.get("pluginId").and_then(|v| v.as_str()).unwrap_or("");
        let mut map = disposers_for_pu.lock().unwrap();
        let dispose = map.get(plugin_id).cloned();
        if let Some(dispose) = dispose {
            dispose();
            map.remove(plugin_id);
        }
    });
    let pu_id = lifecycle.on("plugin.unloaded", handler_pu);

    (controller, ws_id, pu_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn r707_worker_stopped_disposes_but_keeps() {
        let lifecycle = Arc::new(InMemoryLifecycle::new());
        let disposers: Arc<Mutex<HashMap<String, Disposer>>> = Arc::new(Mutex::new(HashMap::new()));

        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        disposers
            .lock()
            .unwrap()
            .insert("p1".to_string(), Arc::new(move || {
                c.fetch_add(1, Ordering::SeqCst);
            }));

        let (_ctrl, _ws_id, _pu_id) =
            create_plugin_host_service_cleanup(lifecycle.clone(), disposers.clone());

        lifecycle.emit("plugin.worker_stopped", &serde_json::json!({"pluginId": "p1"}));
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert!(disposers.lock().unwrap().contains_key("p1"));
    }

    #[test]
    fn r707_plugin_unloaded_disposes_and_removes() {
        let lifecycle = Arc::new(InMemoryLifecycle::new());
        let disposers: Arc<Mutex<HashMap<String, Disposer>>> = Arc::new(Mutex::new(HashMap::new()));

        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        disposers
            .lock()
            .unwrap()
            .insert("p1".to_string(), Arc::new(move || {
                c.fetch_add(1, Ordering::SeqCst);
            }));

        let (_ctrl, _ws_id, _pu_id) =
            create_plugin_host_service_cleanup(lifecycle.clone(), disposers.clone());

        lifecycle.emit("plugin.unloaded", &serde_json::json!({"pluginId": "p1"}));
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert!(!disposers.lock().unwrap().contains_key("p1"));
    }

    #[test]
    fn r707_handle_worker_crashed_triggers_dispose() {
        let lifecycle = Arc::new(InMemoryLifecycle::new());
        let disposers: Arc<Mutex<HashMap<String, Disposer>>> = Arc::new(Mutex::new(HashMap::new()));

        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        disposers
            .lock()
            .unwrap()
            .insert("p1".to_string(), Arc::new(move || {
                c.fetch_add(1, Ordering::SeqCst);
            }));

        let (ctrl, _ws_id, _pu_id) =
            create_plugin_host_service_cleanup(lifecycle.clone(), disposers.clone());

        ctrl.handle_worker_event(&PluginWorkerRuntimeEvent::Crashed {
            plugin_id: "p1".to_string(),
        });
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn r707_handle_worker_restarted_noop() {
        let lifecycle = Arc::new(InMemoryLifecycle::new());
        let disposers: Arc<Mutex<HashMap<String, Disposer>>> = Arc::new(Mutex::new(HashMap::new()));
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        disposers
            .lock()
            .unwrap()
            .insert("p1".to_string(), Arc::new(move || {
                c.fetch_add(1, Ordering::SeqCst);
            }));

        let (ctrl, _ws_id, _pu_id) =
            create_plugin_host_service_cleanup(lifecycle, disposers);

        ctrl.handle_worker_event(&PluginWorkerRuntimeEvent::Restarted {
            plugin_id: "p1".to_string(),
        });
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn r707_dispose_all_clears_all() {
        let lifecycle = Arc::new(InMemoryLifecycle::new());
        let disposers: Arc<Mutex<HashMap<String, Disposer>>> = Arc::new(Mutex::new(HashMap::new()));

        let counter = Arc::new(AtomicUsize::new(0));
        let c1 = counter.clone();
        let c2 = counter.clone();
        disposers
            .lock()
            .unwrap()
            .insert("p1".to_string(), Arc::new(move || {
                c1.fetch_add(1, Ordering::SeqCst);
            }));
        disposers
            .lock()
            .unwrap()
            .insert("p2".to_string(), Arc::new(move || {
                c2.fetch_add(1, Ordering::SeqCst);
            }));

        let (ctrl, _ws_id, _pu_id) =
            create_plugin_host_service_cleanup(lifecycle, disposers);

        ctrl.dispose_all();
        assert_eq!(counter.load(Ordering::SeqCst), 2);
        assert!(ctrl.disposers.lock().unwrap().is_empty());
    }

    #[test]
    fn r707_teardown_removes_handlers() {
        let lifecycle = Arc::new(InMemoryLifecycle::new());
        let disposers: Arc<Mutex<HashMap<String, Disposer>>> = Arc::new(Mutex::new(HashMap::new()));

        let (_ctrl, ws_id, pu_id) =
            create_plugin_host_service_cleanup(lifecycle.clone(), disposers);

        assert_eq!(lifecycle.handler_count("plugin.worker_stopped"), 1);
        assert_eq!(lifecycle.handler_count("plugin.unloaded"), 1);

        _ctrl.teardown(ws_id, pu_id);
        assert_eq!(lifecycle.handler_count("plugin.worker_stopped"), 0);
        assert_eq!(lifecycle.handler_count("plugin.unloaded"), 0);
    }

    #[test]
    fn r707_unknown_plugin_no_dispose() {
        let lifecycle = Arc::new(InMemoryLifecycle::new());
        let disposers: Arc<Mutex<HashMap<String, Disposer>>> = Arc::new(Mutex::new(HashMap::new()));
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        disposers
            .lock()
            .unwrap()
            .insert("p1".to_string(), Arc::new(move || {
                c.fetch_add(1, Ordering::SeqCst);
            }));

        let (_ctrl, _ws_id, _pu_id) =
            create_plugin_host_service_cleanup(lifecycle.clone(), disposers);

        lifecycle.emit("plugin.worker_stopped", &serde_json::json!({"pluginId": "p-unknown"}));
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn r707_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<InMemoryLifecycle>();
        assert_send_sync::<PluginHostServiceCleanupController>();
    }

    #[test]
    fn r707_event_from_parts() {
        let e = PluginWorkerRuntimeEvent::from_parts("plugin.worker.crashed", "p1".into()).unwrap();
        assert_eq!(
            e,
            PluginWorkerRuntimeEvent::Crashed {
                plugin_id: "p1".into()
            }
        );
        let e = PluginWorkerRuntimeEvent::from_parts("plugin.worker.restarted", "p2".into()).unwrap();
        assert_eq!(
            e,
            PluginWorkerRuntimeEvent::Restarted {
                plugin_id: "p2".into()
            }
        );
        assert!(PluginWorkerRuntimeEvent::from_parts("unknown", "p".into()).is_none());
    }
}
