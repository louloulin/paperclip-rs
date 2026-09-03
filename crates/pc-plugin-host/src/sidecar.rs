//! Plugin sidecar runtime launcher (R877).
//!
//! ## 为什么需要 sidecar
//!
//! Node paperclip 用 `node:vm` 在进程内加载 JS 插件。Rust 没有内建
//! JS 运行时，且 `pc-plugin-host` 通过子进程 + JSON-RPC 隔离。
//!
//! 为了让现有 `@paperclipai/plugin-*` JS 插件可被 Rust host 直接加载，
//! 我们采用 **sidecar proxy** 模式：
//!
//! ```text
//! ┌─────────────────────┐
//! │ Rust host           │
//! │  (pc-plugin-host)   │ JSON-RPC over stdio (协议不变)
//! │                     │ ──────────────────────────┐
//! └─────────────────────┘                            ▼
//!                                          ┌─────────────────┐
//!                                          │ Node sidecar    │  ← 一个小 Node
//!                                          │ (200 LOC)       │     进程，内部用
//!                                          │                 │     `node:vm` 加载
//!                                          │  eval(plugin.js)│     真实 JS 插件
//!                                          │  ↕ JSON-RPC     │
//!                                          │  ┌────────────┐ │
//!                                          │  │ JS plugin  │ │
//!                                          │  └────────────┘ │
//!                                          └─────────────────┘
//! ```
//!
//! ## manifest 路由
//!
//! Plugin manifest 新增可选字段 `runtime: "node" | "python" | "builtin"`：
//! - `builtin`：纯 Rust 插件（当前 model）
//! - `node`：spawn Node sidecar proxy
//! - `python`：spawn Python sidecar proxy（未来扩展）
//!
//! 默认 `runtime: "builtin"`，向后兼容。

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::process::Command;
use uuid::Uuid;

/// Plugin runtime kind (declared by manifest).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PluginRuntimeKind {
    /// Pure Rust plugin (default, current model).
    #[default]
    Builtin,
    /// Node.js plugin — loaded via Node sidecar proxy that wraps `node:vm`.
    Node,
    /// Python plugin — reserved for future expansion.
    Python,
}

impl PluginRuntimeKind {
    pub fn from_manifest_field(value: Option<&str>) -> Self {
        match value {
            Some("node") | Some("nodejs") | Some("js") => Self::Node,
            Some("python") | Some("py") => Self::Python,
            Some("builtin") | Some("rust") | None => Self::Builtin,
            Some(other) => {
                tracing::warn!(
                    runtime = other,
                    "unknown plugin runtime kind, defaulting to builtin"
                );
                Self::Builtin
            }
        }
    }

    pub fn requires_sidecar(self) -> bool {
        matches!(self, Self::Node | Self::Python)
    }
}

/// Configuration for sidecar launcher.
#[derive(Debug, Clone)]
pub struct SidecarConfig {
    /// Path to `node` binary. If `None`, launcher tries `node` from PATH.
    pub node_binary: Option<PathBuf>,
    /// Path to the Node sidecar proxy script. Defaults to the bundled
    /// `bin/paperclip-plugin-sidecar.mjs` shipped with `pc-plugin-host`.
    pub sidecar_script: Option<PathBuf>,
    /// Wall-clock timeout for plugin initialize handshake (ms).
    pub initialize_timeout_ms: u64,
    /// Extra environment variables to pass to the sidecar.
    pub env: Vec<(String, String)>,
}

impl Default for SidecarConfig {
    fn default() -> Self {
        Self {
            node_binary: None,
            sidecar_script: None,
            initialize_timeout_ms: 10_000,
            env: Vec::new(),
        }
    }
}

/// Errors raised by sidecar launcher.
#[derive(Debug, Error)]
pub enum SidecarError {
    #[error("node binary not found in PATH and `node_binary` not configured")]
    NodeBinaryNotFound,
    #[error("sidecar script not found at {0}")]
    SidecarScriptNotFound(PathBuf),
    #[error("failed to spawn sidecar process: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("sidecar plugin initialization timed out after {0}ms")]
    InitializeTimeout(u64),
    #[error("sidecar plugin initialization failed: {0}")]
    InitializeFailed(String),
}

/// Trait for spawning a sidecar process that wraps a non-Rust plugin.
#[async_trait::async_trait]
pub trait SidecarLauncher: Send + Sync {
    /// Returns true if this launcher can handle the given runtime kind.
    fn supports(&self, kind: PluginRuntimeKind) -> bool;

    /// Spawn a sidecar process for the given plugin.
    ///
    /// Returns the spawned `tokio::process::Child`. The host then attaches
    /// stdin/stdout to the JSON-RPC stream.
    async fn spawn(
        &self,
        plugin_id: Uuid,
        manifest_path: &Path,
        config: &SidecarConfig,
    ) -> Result<tokio::process::Child, SidecarError>;
}

/// Default launcher for Node.js plugins.
pub struct NodeSidecarLauncher {
    config: Arc<SidecarConfig>,
}

impl NodeSidecarLauncher {
    pub fn new(config: Arc<SidecarConfig>) -> Self {
        Self { config }
    }
}

#[async_trait::async_trait]
impl SidecarLauncher for NodeSidecarLauncher {
    fn supports(&self, kind: PluginRuntimeKind) -> bool {
        kind == PluginRuntimeKind::Node
    }

    async fn spawn(
        &self,
        plugin_id: Uuid,
        manifest_path: &Path,
        config: &SidecarConfig,
    ) -> Result<tokio::process::Child, SidecarError> {
        let node = config
            .node_binary
            .as_deref()
            .unwrap_or(Path::new("node"));

        let sidecar = config
            .sidecar_script
            .as_deref()
            .ok_or_else(|| {
                SidecarError::SidecarScriptNotFound(PathBuf::from(
                    "bundled:bin/paperclip-plugin-sidecar.mjs (not packaged)",
                ))
            })?;

        if !sidecar.exists() {
            return Err(SidecarError::SidecarScriptNotFound(sidecar.to_path_buf()));
        }

        let mut cmd = Command::new(node);
        cmd.arg(sidecar)
            .arg("--plugin-id")
            .arg(plugin_id.to_string())
            .arg("--manifest")
            .arg(manifest_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        for (k, v) in &config.env {
            cmd.env(k, v);
        }

        cmd.spawn().map_err(SidecarError::Spawn)
    }
}

/// Registry of sidecar launchers (one per runtime kind).
#[derive(Default)]
pub struct SidecarLauncherRegistry {
    launchers: Vec<Arc<dyn SidecarLauncher>>,
}

impl SidecarLauncherRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a launcher. The first launcher that returns `supports(kind) == true` wins.
    pub fn register(mut self, launcher: Arc<dyn SidecarLauncher>) -> Self {
        self.launchers.push(launcher);
        self
    }

    /// Register the default Node.js launcher.
    pub fn with_default_node(self, config: Arc<SidecarConfig>) -> Self {
        self.register(Arc::new(NodeSidecarLauncher::new(config)))
    }

    /// Pick the launcher for the given runtime kind.
    pub fn pick(&self, kind: PluginRuntimeKind) -> Option<Arc<dyn SidecarLauncher>> {
        self.launchers
            .iter()
            .find(|l| l.supports(kind))
            .cloned()
    }

    /// Number of registered launchers.
    pub fn len(&self) -> usize {
        self.launchers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.launchers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_kind_defaults_to_builtin() {
        assert_eq!(PluginRuntimeKind::default(), PluginRuntimeKind::Builtin);
    }

    #[test]
    fn runtime_kind_parses_manifest_field() {
        assert_eq!(
            PluginRuntimeKind::from_manifest_field(None),
            PluginRuntimeKind::Builtin
        );
        assert_eq!(
            PluginRuntimeKind::from_manifest_field(Some("node")),
            PluginRuntimeKind::Node
        );
        assert_eq!(
            PluginRuntimeKind::from_manifest_field(Some("nodejs")),
            PluginRuntimeKind::Node
        );
        assert_eq!(
            PluginRuntimeKind::from_manifest_field(Some("python")),
            PluginRuntimeKind::Python
        );
        assert_eq!(
            PluginRuntimeKind::from_manifest_field(Some("builtin")),
            PluginRuntimeKind::Builtin
        );
        // Unknown defaults to builtin with warning
        assert_eq!(
            PluginRuntimeKind::from_manifest_field(Some("wasm")),
            PluginRuntimeKind::Builtin
        );
    }

    #[test]
    fn only_sidecar_kinds_require_sidecar() {
        assert!(!PluginRuntimeKind::Builtin.requires_sidecar());
        assert!(PluginRuntimeKind::Node.requires_sidecar());
        assert!(PluginRuntimeKind::Python.requires_sidecar());
    }

    #[test]
    fn registry_picks_first_matching_launcher() {
        let registry = SidecarLauncherRegistry::new()
            .with_default_node(Arc::new(SidecarConfig::default()));
        assert!(!registry.is_empty());
        assert!(registry.pick(PluginRuntimeKind::Node).is_some());
        assert!(registry.pick(PluginRuntimeKind::Python).is_none());
        assert!(registry.pick(PluginRuntimeKind::Builtin).is_none());
    }

    #[test]
    fn default_sidecar_config_has_reasonable_timeouts() {
        let cfg = SidecarConfig::default();
        assert_eq!(cfg.initialize_timeout_ms, 10_000);
        assert!(cfg.node_binary.is_none());
        assert!(cfg.sidecar_script.is_none());
    }
}
