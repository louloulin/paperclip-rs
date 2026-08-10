//! Tool registry 数据类型。
//!
//! 高内聚：所有"tool 是什么 / 怎么命名"的形状集中在这。
//! 低耦合：纯数据 + Clone + serde，零业务依赖。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// ============================================================================
// Constants
// ============================================================================

/// Plugin ID 与 tool 名之间的命名空间分隔符（与 Node `TOOL_NAMESPACE_SEPARATOR` 1:1 对齐）。
///
/// 例子：`"acme.linear:search-issues"`。
pub const TOOL_NAMESPACE_SEPARATOR: char = ':';

// ============================================================================
// RegisteredTool
// ============================================================================

/// 已注册 tool 记录（与 Node `RegisteredTool` 1:1 对齐）。
///
/// 设计：包含 manifest 声明 + 路由元数据，使 host 能 O(1) 解析名字。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredTool {
    /// Plugin 的稳定 ID（如 `"acme.linear"`），用于命名空间。
    pub plugin_id: String,
    /// Plugin 的 DB UUID，用于 worker 路由 / 可用性检查。
    pub plugin_db_id: Uuid,
    /// Tool 的原始名（不带 namespace 前缀）。
    pub name: String,
    /// 完整命名空间 ID：`"<plugin_id>:<tool_name>"`。
    pub namespaced_name: String,
    /// 展示名。
    pub display_name: String,
    /// 给 agent 看的描述。
    pub description: String,
    /// tool 参数的 JSON Schema。
    pub parameters_schema: Value,
}

// ============================================================================
// ToolListFilter
// ============================================================================

/// 列出 tool 时的过滤条件（与 Node `ToolListFilter` 1:1 对齐）。
#[derive(Debug, Clone, Default)]
pub struct ToolListFilter {
    /// 只列出此 plugin 的 tools。
    pub plugin_id: Option<String>,
}

// ============================================================================
// ToolExecutionResult
// ============================================================================

/// `executeTool` 返回值（与 Node `ToolExecutionResult` 1:1 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExecutionResult {
    /// 处理此次调用的 plugin。
    pub plugin_id: String,
    /// 被调用的 tool 原始名。
    pub tool_name: String,
    /// Plugin handler 返回的结果。
    pub result: pc_plugin_protocol::ToolResult,
}

// ============================================================================
// Helpers
// ============================================================================

/// 构造命名空间 tool 名（与 Node `buildName` 1:1 对齐）。
pub fn build_namespaced_name(plugin_id: &str, tool_name: &str) -> String {
    format!("{plugin_id}{}{tool_name}", TOOL_NAMESPACE_SEPARATOR)
}
