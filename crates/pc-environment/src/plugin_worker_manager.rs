#![allow(clippy::needless_return)]
// SPDX-License-Identifier: MIT
//
// R684 parity: PluginWorkerManager trait + InMemoryPluginWorkerManager.
//
// Reference (Node):
//   paperclip/server/src/services/plugin-worker-manager.ts
//
// R684 abstracts the PluginWorkerManager surface that the async parity
// modules (plugin-environment-driver.ts, plugin-job-scheduler.ts) actually
// depend on, and ships an in-memory reference implementation suitable for
// testing. Real process management (subprocess spawn, IPC, heartbeat) lives
// outside the parity scope.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// Mirrors Node `WorkerStatus`: the high-level lifecycle of a managed worker.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStatus {
    Starting,
    Running,
    Stopping,
    Stopped,
    Crashed,
    Failed,
}

/// Outcome of an RPC call to a worker. Mirrors the parts of
/// `HostToWorkerMethods[M][1]` that are consumed by validatePlugin*Config.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PluginRpcResult {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Plugin-validated canonical form of the config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalized_config: Option<Value>,
    /// R689 parity: probe summary line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// R689 parity: probe diagnostics (severity / message / code).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<PluginRpcDiagnostic>>,
    /// R689 parity: probe metadata (free-form key/value map).
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub metadata: Map<String, Value>,
}

/// R689 parity: individual probe diagnostic record. Mirrors Node
/// `EnvironmentProbeDiagnostics`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PluginRpcDiagnostic {
    pub severity: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

/// Error returned by a worker RPC call. Mirrors the `throw unprocessable()`
/// path in validatePluginSandboxProviderConfig.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginRpcError {
    /// Worker is not registered or not in Running state.
    WorkerNotRunning { plugin_id: String },
    /// Worker is registered but does not handle the requested method.
    MethodNotRegistered { plugin_id: String, method: String },
    /// Handler itself raised an error.
    HandlerError { plugin_id: String, method: String, message: String },
    /// Call exceeded the supplied timeout.
    Timeout { plugin_id: String, method: String, timeout_ms: u64 },
}

impl std::fmt::Display for PluginRpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WorkerNotRunning { plugin_id } => {
                write!(f, "plugin worker not running: {}", plugin_id)
            }
            Self::MethodNotRegistered { plugin_id, method } => write!(
                f,
                "plugin worker {} does not handle method {}",
                plugin_id, method
            ),
            Self::HandlerError { plugin_id, method, message } => write!(
                f,
                "plugin worker {} handler for {} raised: {}",
                plugin_id, method, message
            ),
            Self::Timeout { plugin_id, method, timeout_ms } => write!(
                f,
                "plugin worker {} method {} timed out after {}ms",
                plugin_id, method, timeout_ms
            ),
        }
    }
}

impl std::error::Error for PluginRpcError {}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Mirrors the subset of Node `PluginWorkerManager` consumed by the async
/// parity modules. `isRunning` is checked before each call (Node `throw`
/// "worker is not running"). `call` invokes the registered handler and
/// returns its result.
pub trait PluginWorkerManager: Send + Sync {
    fn is_running(&self, plugin_id: &str) -> bool;
    /// Returns true when the worker entry is present in the manager at all,
    /// regardless of status. Mirrors Node `workerManager.getWorker(id)`
    /// (non-null means a worker object exists).
    fn worker_registered(&self, plugin_id: &str) -> bool;
    /// Mirror of Node `workerManager.call(...)` for handlers that return
    /// an arbitrary JSON payload (e.g. resumePluginEnvironmentLease,
    /// destroyPluginEnvironmentLease). The in-memory reference impl ignores
    /// `timeout_ms` (handlers are synchronous).
    fn call_raw(
        &self,
        plugin_id: &str,
        method: &str,
        params: Value,
        timeout_ms: Option<u64>,
    ) -> Result<Value, PluginRpcError>;
    /// Mirror of Node `workerManager.call(pluginId, method, params, timeoutMs)`.
    /// `timeout_ms` is accepted by the trait for parity; the in-memory
    /// reference impl ignores it (handlers are synchronous). Production
    /// implementations (e.g. an HTTP-backed manager) MUST honour the timeout.
    fn call(
        &self,
        plugin_id: &str,
        method: &str,
        params: Value,
        timeout_ms: Option<u64>,
    ) -> Result<PluginRpcResult, PluginRpcError>;
}

/// Convenience extension: a manager that records its registered workers.
pub trait PluginWorkerManagerInspect: PluginWorkerManager {
    fn worker_status(&self, plugin_id: &str) -> Option<WorkerStatus>;
    fn registered_methods(&self, plugin_id: &str) -> Vec<String>;
}

// ---------------------------------------------------------------------------
// In-memory reference implementation
// ---------------------------------------------------------------------------

type HandlerFn = Arc<dyn Fn(Value) -> Result<PluginRpcResult, String> + Send + Sync>;
type RawHandlerFn = Arc<dyn Fn(Value) -> Result<Value, String> + Send + Sync>;

struct WorkerEntry {
    status: WorkerStatus,
    handlers: HashMap<String, HandlerFn>,
    raw_handlers: HashMap<String, RawHandlerFn>,
}

impl std::fmt::Debug for WorkerEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerEntry")
            .field("status", &self.status)
            .field("handler_count", &self.handlers.len())
            .field("raw_handler_count", &self.raw_handlers.len())
            .finish()
    }
}

#[derive(Debug, Default)]
struct Inner {
    workers: HashMap<String, WorkerEntry>,
}

/// In-memory `PluginWorkerManager` for tests and parity-only consumers.
#[derive(Debug, Clone, Default)]
pub struct InMemoryPluginWorkerManager {
    inner: Arc<Mutex<Inner>>,
}

impl InMemoryPluginWorkerManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a worker for a plugin and mark it as Running.
    pub fn register_worker(&self, plugin_id: impl Into<String>) {
        let mut inner = self.inner.lock().unwrap();
        inner.workers.insert(
            plugin_id.into(),
            WorkerEntry {
                status: WorkerStatus::Running,
                handlers: HashMap::new(),
                raw_handlers: HashMap::new(),
            },
        );
    }

    /// Register a method handler for an already-registered worker.
    pub fn register_handler<F>(&self, plugin_id: &str, method: impl Into<String>, handler: F)
    where
        F: Fn(Value) -> Result<PluginRpcResult, String> + Send + Sync + 'static,
    {
        let mut inner = self.inner.lock().unwrap();
        let entry = inner.workers.get_mut(plugin_id).expect("worker not registered");
        entry.handlers.insert(method.into(), Arc::new(handler));
    }

    /// Register a raw handler (returns arbitrary JSON Value) for a worker method.
    /// Used by resumePluginEnvironmentLease / destroyPluginEnvironmentLease.
    pub fn register_raw_handler<F>(&self, plugin_id: &str, method: impl Into<String>, handler: F)
    where
        F: Fn(Value) -> Result<Value, String> + Send + Sync + 'static,
    {
        let mut inner = self.inner.lock().unwrap();
        let entry = inner.workers.get_mut(plugin_id).expect("worker not registered");
        entry.raw_handlers.insert(method.into(), Arc::new(handler));
    }

    /// Mark a worker as stopped (or remove it entirely with `remove_worker`).
    pub fn stop_worker(&self, plugin_id: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(entry) = inner.workers.get_mut(plugin_id) {
            entry.status = WorkerStatus::Stopped;
        }
    }

    /// Remove a worker entirely.
    pub fn remove_worker(&self, plugin_id: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner.workers.remove(plugin_id);
    }

    pub fn registered_workers(&self) -> Vec<String> {
        let inner = self.inner.lock().unwrap();
        inner.workers.keys().cloned().collect()
    }
}

impl PluginWorkerManager for InMemoryPluginWorkerManager {
    fn is_running(&self, plugin_id: &str) -> bool {
        let inner = self.inner.lock().unwrap();
        inner
            .workers
            .get(plugin_id)
            .map(|e| e.status == WorkerStatus::Running)
            .unwrap_or(false)
    }

    fn worker_registered(&self, plugin_id: &str) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.workers.contains_key(plugin_id)
    }

    fn call_raw(
        &self,
        plugin_id: &str,
        method: &str,
        _params: Value,
        _timeout_ms: Option<u64>,
    ) -> Result<Value, PluginRpcError> {
        let handler = {
            let inner = self.inner.lock().unwrap();
            let entry = inner.workers.get(plugin_id).ok_or_else(|| {
                PluginRpcError::WorkerNotRunning {
                    plugin_id: plugin_id.to_string(),
                }
            })?;
            if entry.status != WorkerStatus::Running {
                return Err(PluginRpcError::WorkerNotRunning {
                    plugin_id: plugin_id.to_string(),
                });
            }
            entry.raw_handlers.get(method).cloned().ok_or_else(|| {
                PluginRpcError::MethodNotRegistered {
                    plugin_id: plugin_id.to_string(),
                    method: method.to_string(),
                }
            })?
        };
        handler(_params).map_err(|msg| PluginRpcError::HandlerError {
            plugin_id: plugin_id.to_string(),
            method: method.to_string(),
            message: msg,
        })
    }

    fn call(
        &self,
        plugin_id: &str,
        method: &str,
        params: Value,
        _timeout_ms: Option<u64>,
    ) -> Result<PluginRpcResult, PluginRpcError> {
        // Clone what we need out of the mutex before invoking the handler
        // (the handler may take arbitrarily long and must not hold the lock).
        let handler = {
            let inner = self.inner.lock().unwrap();
            let entry = inner
                .workers
                .get(plugin_id)
                .ok_or_else(|| PluginRpcError::WorkerNotRunning {
                    plugin_id: plugin_id.to_string(),
                })?;
            if entry.status != WorkerStatus::Running {
                return Err(PluginRpcError::WorkerNotRunning {
                    plugin_id: plugin_id.to_string(),
                });
            }
            entry.handlers.get(method).cloned().ok_or_else(|| {
                PluginRpcError::MethodNotRegistered {
                    plugin_id: plugin_id.to_string(),
                    method: method.to_string(),
                }
            })?
        };

        (handler)(params).map_err(|msg| PluginRpcError::HandlerError {
            plugin_id: plugin_id.to_string(),
            method: method.to_string(),
            message: msg,
        })
    }
}

impl PluginWorkerManagerInspect for InMemoryPluginWorkerManager {
    fn worker_status(&self, plugin_id: &str) -> Option<WorkerStatus> {
        let inner = self.inner.lock().unwrap();
        inner.workers.get(plugin_id).map(|e| e.status)
    }

    fn registered_methods(&self, plugin_id: &str) -> Vec<String> {
        let inner = self.inner.lock().unwrap();
        inner
            .workers
            .get(plugin_id)
            .map(|e| {
                let mut v: Vec<String> = e.handlers.keys().cloned().collect();
                v.sort();
                v
            })
            .unwrap_or_default()
    }
}
