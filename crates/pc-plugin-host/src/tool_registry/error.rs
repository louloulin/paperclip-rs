//! Tool registry 错误类型。
//!
//! 高内聚：所有 tool-registry 相关错误集中在这。
//! 低耦合：仅依赖 thiserror + serde_json，零业务依赖。

use thiserror::Error;

/// Tool registry 错误（与 Node throw 1:1 对齐）。
#[derive(Debug, Error)]
pub enum ToolRegistryError {
    /// Tool 名格式错误（缺少命名空间分隔符）。
    #[error("invalid tool name \"{0}\". Expected format: \"<pluginId>{ns}<toolName>\"", ns = super::types::TOOL_NAMESPACE_SEPARATOR)]
    InvalidToolName(String),

    /// Tool 未注册（plugin 未安装或 worker 未运行）。
    #[error("tool \"{0}\" is not registered. The plugin may not be installed or its worker may not be running.")]
    ToolNotRegistered(String),

    /// 未配置 worker manager。
    #[error("cannot execute tool \"{0}\" — no worker manager configured. Tool execution requires a PluginWorkerManager.")]
    NoWorkerManager(String),

    /// Plugin worker 未运行。
    #[error("cannot execute tool \"{0}\" — worker for plugin \"{1}\" is not running.")]
    WorkerNotRunning(String, String),

    /// Worker 调用失败（plugin 进程 RPC 错误）。
    #[error("worker call failed for tool \"{0}\": {1}")]
    WorkerCallFailed(String, String),

    /// `pluginDbId` 缺失（Node `registerPlugin` 必填参数）。
    #[error("plugin-tool-registry.registerPlugin: pluginDbId is required (pluginId=\"{0}\"). Workers are keyed by DB UUID; omitting this guarantees worker-lookup failure.")]
    MissingPluginDbId(String),

    /// plugin worker pool 缺失（DI/调用方未注入）。
    #[error("plugin worker pool is required for executeTool; pass `Arc<WorkerPool>` via `plugin_tool_registry(worker_pool)`")]
    WorkerPoolMissing,
}

/// Result 简写别名。
pub type ToolRegistryResult<T> = std::result::Result<T, ToolRegistryError>;
