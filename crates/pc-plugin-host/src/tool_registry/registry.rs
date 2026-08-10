//! PluginToolRegistry —— host 端 tool 注册表 service。
//!
//! 高内聚：本模块是 Node `PluginToolRegistry` 接口的 1:1 Rust 复刻。
//! 所有公开 API（registerPlugin / unregisterPlugin / getTool / listTools / executeTool / ...）
//! 都在此文件。
//!
//! 低耦合：
//! - 持有 `Arc<dyn ToolStore + Send + Sync>` —— 不绑定具体存储
//! - 持有 `Arc<dyn ToolWorker>` —— 不绑定具体 worker pool
//! - types 与 error 在 sibling modules，可被复用

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tracing::{debug, info};
use uuid::Uuid;

use pc_plugin_protocol::{ExecuteToolParams, ToolResult};

use super::error::{ToolRegistryError, ToolRegistryResult};
use super::store::{parse_namespaced_name, ToolStore};
use super::types::{
    build_namespaced_name, RegisteredTool, ToolExecutionResult, ToolListFilter,
    TOOL_NAMESPACE_SEPARATOR,
};

// ============================================================================
// Manifest shape (最小子集)
// ============================================================================

/// Manifest 中的 tool declaration（与 Node `PluginToolDeclaration` 1:1 对齐）。
///
/// 不引入整个 `PaperclipPluginManifestV1` 类型以避免依赖膨胀。
#[derive(Debug, Clone)]
pub struct ManifestToolDeclaration {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub parameters_schema: Value,
}

/// Manifest 的最小子集（只需 `tools` 数组）。
#[derive(Debug, Clone, Default)]
pub struct PluginManifestTools {
    pub tools: Vec<ManifestToolDeclaration>,
}

impl PluginManifestTools {
    /// 从 `serde_json::Value` 解析 tools 数组。
    pub fn from_value(value: &Value) -> Self {
        let mut manifest = Self::default();
        if let Some(arr) = value.get("tools").and_then(|v| v.as_array()) {
            for item in arr {
                let name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if name.is_empty() {
                    continue;
                }
                let display_name = item
                    .get("displayName")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&name)
                    .to_string();
                let description = item
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let parameters_schema = item
                    .get("parametersSchema")
                    .cloned()
                    .unwrap_or_else(|| Value::Object(Default::default()));
                manifest.tools.push(ManifestToolDeclaration {
                    name,
                    display_name,
                    description,
                    parameters_schema,
                });
            }
        }
        manifest
    }
}

// ============================================================================
// ToolWorker trait
// ============================================================================

/// Worker 调用抽象 —— registry 不耦合具体 worker 实现。
///
/// `pc-plugin-host::WorkerPool` 实现此 trait。
#[async_trait]
pub trait ToolWorker: Send + Sync {
    /// worker 是否在运行（plugin worker 进程是否 alive）。
    async fn is_running(&self, plugin_db_id: &Uuid) -> bool;

    /// 调用 worker 的 RPC method，返回 worker 返回的 JSON 值。
    async fn call(
        &self,
        plugin_db_id: &Uuid,
        method: &str,
        params: Value,
    ) -> Result<Value, String>;
}

// ============================================================================
// PluginToolRegistry
// ============================================================================

/// Host 端 tool 注册表 service（与 Node `PluginToolRegistry` 1:1 对齐）。
///
/// 设计：
/// - cheap clone：内部 `Arc<dyn ToolStore>` + `Arc<dyn ToolWorker>` (Option)，可 clone 共享
/// - 默认用 [`InMemoryToolStore`]；可注入自定义 store 用于测试
pub struct PluginToolRegistry {
    store: Arc<dyn ToolStore>,
    worker: Option<Arc<dyn ToolWorker>>,
}

impl std::fmt::Debug for PluginToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginToolRegistry")
            .field("store_count", &self.store.count(None))
            .field("worker_configured", &self.worker.is_some())
            .finish()
    }
}

impl PluginToolRegistry {
    /// 用默认内存存储创建 registry（worker 稍后通过 [`Self::with_worker`] 注入）。
    pub fn new() -> Self {
        Self {
            store: Arc::new(super::store::InMemoryToolStore::new()),
            worker: None,
        }
    }

    /// 注入自定义 store（用于测试或替换为持久化后端）。
    pub fn with_store(mut self, store: Arc<dyn ToolStore>) -> Self {
        self.store = store;
        self
    }

    /// 注入 worker pool（执行 tool 调用时使用）。
    pub fn with_worker(mut self, worker: Arc<dyn ToolWorker>) -> Self {
        self.worker = Some(worker);
        self
    }

    // ========================================================================
    // Plugin lifecycle
    // ========================================================================

    /// 注册 plugin manifest 中的所有 tools。
    ///
    /// 与 Node 1:1：移除已存在的（幂等）→ 注册新 tools。
    ///
    /// `plugin_db_id` 必填 —— worker 用它做路由 key，缺失会抛 [`ToolRegistryError::MissingPluginDbId`].
    pub async fn register_plugin(
        &self,
        plugin_id: &str,
        manifest: &PluginManifestTools,
        plugin_db_id: Uuid,
    ) -> ToolRegistryResult<()> {
        let previous_count = self.store.remove_plugin(plugin_id);
        if previous_count > 0 {
            debug!(
                plugin_id,
                previous_count,
                "cleared previous tool registrations before re-registering"
            );
        }

        if manifest.tools.is_empty() {
            debug!(plugin_id, "plugin declares no tools");
            return Ok(());
        }

        let mut registered_names = Vec::with_capacity(manifest.tools.len());
        for decl in &manifest.tools {
            let namespaced = build_namespaced_name(plugin_id, &decl.name);
            let tool = RegisteredTool {
                plugin_id: plugin_id.to_string(),
                plugin_db_id,
                name: decl.name.clone(),
                namespaced_name: namespaced.clone(),
                display_name: decl.display_name.clone(),
                description: decl.description.clone(),
                parameters_schema: decl.parameters_schema.clone(),
            };
            self.store.put(tool);
            registered_names.push(namespaced);
        }

        info!(
            plugin_id,
            tool_count = manifest.tools.len(),
            tools = ?registered_names,
            "registered tools for plugin"
        );

        Ok(())
    }

    /// 移除 plugin 的所有 tools（worker 停止 / 卸载时调用）。
    pub fn unregister_plugin(&self, plugin_id: &str) -> usize {
        let removed = self.store.remove_plugin(plugin_id);
        if removed > 0 {
            info!(plugin_id, removed_count = removed, "unregistered tools for plugin");
        }
        removed
    }

    // ========================================================================
    // Discovery
    // ========================================================================

    /// 通过命名空间名查 tool。
    pub fn get_tool(&self, namespaced_name: &str) -> Option<RegisteredTool> {
        self.store.get(namespaced_name)
    }

    /// 通过 `(plugin_id, tool_name)` 查 tool。
    pub fn get_tool_by_plugin(&self, plugin_id: &str, tool_name: &str) -> Option<RegisteredTool> {
        self.store.get_by_plugin(plugin_id, tool_name)
    }

    /// 列出所有 tools（可选 plugin_id 过滤）。
    pub fn list_tools(&self, filter: Option<ToolListFilter>) -> Vec<RegisteredTool> {
        self.store.list(filter.as_ref().unwrap_or(&ToolListFilter::default()))
    }

    /// 解析命名空间名为 `(plugin_id, tool_name)`。
    pub fn parse_namespaced_name(
        &self,
        namespaced_name: &str,
    ) -> Option<super::store::ParsedToolName> {
        parse_namespaced_name(namespaced_name)
    }

    /// 构造命名空间名。
    pub fn build_namespaced_name(&self, plugin_id: &str, tool_name: &str) -> String {
        build_namespaced_name(plugin_id, tool_name)
    }

    /// tool 总数（可选 plugin_id 过滤）。
    pub fn tool_count(&self, plugin_id: Option<&str>) -> usize {
        self.store.count(plugin_id)
    }

    // ========================================================================
    // Execution
    // ========================================================================

    /// 执行一个 tool：解析名字 → 查注册表 → 检查 worker → RPC 调用。
    ///
    /// 与 Node `executeTool` 1:1 对齐。
    pub async fn execute_tool(
        &self,
        namespaced_name: &str,
        parameters: Value,
        run_context: Value,
    ) -> ToolRegistryResult<ToolExecutionResult> {
        // 1. 解析名字
        let parsed = parse_namespaced_name(namespaced_name).ok_or_else(|| {
            ToolRegistryError::InvalidToolName(format!(
                "\"{}\". Expected format: \"<pluginId>{}{}<toolName>\"",
                namespaced_name, TOOL_NAMESPACE_SEPARATOR, TOOL_NAMESPACE_SEPARATOR
            ))
        })?;

        // 2. 查注册表
        let tool = self.store.get(namespaced_name).ok_or_else(|| {
            ToolRegistryError::ToolNotRegistered(namespaced_name.to_string())
        })?;

        // 3. 检查 worker manager
        let worker = self
            .worker
            .as_ref()
            .ok_or_else(|| ToolRegistryError::NoWorkerManager(namespaced_name.to_string()))?;

        // 4. 检查 worker alive（用 pluginDbId）
        let plugin_db_id = tool.plugin_db_id;
        if !worker.is_running(&plugin_db_id).await {
            return Err(ToolRegistryError::WorkerNotRunning(
                namespaced_name.to_string(),
                parsed.plugin_id.clone(),
            ));
        }

        // 5. 构造 RPC params
        let rpc_params = ExecuteToolParams {
            tool_name: parsed.tool_name.clone(),
            parameters,
            run_context,
        };
        let rpc_params_value = serde_json::to_value(&rpc_params).map_err(|e| {
            ToolRegistryError::WorkerCallFailed(
                namespaced_name.to_string(),
                format!("serialize params: {e}"),
            )
        })?;

        // 6. 调用 worker
        debug!(
            plugin_id = %parsed.plugin_id,
            tool_name = %parsed.tool_name,
            namespaced_name,
            "executing tool via plugin worker"
        );
        let result_value = worker
            .call(&plugin_db_id, "executeTool", rpc_params_value)
            .await
            .map_err(|e| {
                ToolRegistryError::WorkerCallFailed(namespaced_name.to_string(), e)
            })?;

        // 7. 反序列化结果
        let result: ToolResult = serde_json::from_value(result_value).map_err(|e| {
            ToolRegistryError::WorkerCallFailed(
                namespaced_name.to_string(),
                format!("deserialize result: {e}"),
            )
        })?;

        debug!(
            plugin_id = %parsed.plugin_id,
            tool_name = %parsed.tool_name,
            namespaced_name,
            "tool execution completed"
        );

        Ok(ToolExecutionResult {
            plugin_id: parsed.plugin_id,
            tool_name: parsed.tool_name,
            result,
        })
    }
}

impl Default for PluginToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// 工厂函数（与 Node `pluginToolRegistry` 1:1 对齐）。
pub fn plugin_tool_registry() -> PluginToolRegistry {
    PluginToolRegistry::new()
}
