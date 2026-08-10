//! `pc-plugin-host::tool_registry` —— 插件 tool 内存注册表。
//!
//! 高内聚：所有 "tool 名如何解析 / 如何分发到 plugin worker" 的逻辑都在这 4 个文件。
//! 低耦合：
//! - [`registry`] 只依赖 [`store`] trait，可以替换底层存储（内存 / sqlite / redis）
//! - 不直接 import `WorkerPool` —— 通过 [`registry::ToolWorker`] trait 解耦
//! - 类型（[`types`]）和错误（[`error`]）独立，跨模块复用
//!
//! ## 模块
//! - [`types`] —— `RegisteredTool` / `ToolListFilter` / `ToolExecutionResult` / 命名空间常量
//! - [`store`] —— `ToolStore` trait + `InMemoryToolStore` 实现 + `parse_namespaced_name` helper
//! - [`error`] —— `ToolRegistryError`
//! - [`registry`] —— `PluginToolRegistry` 公开 API + 工厂函数 + `ToolWorker` trait

pub mod error;
pub mod registry;
pub mod store;
pub mod types;

pub use error::ToolRegistryError;
pub use registry::{plugin_tool_registry, PluginToolRegistry, ToolWorker};
pub use store::{parse_namespaced_name, InMemoryToolStore, ToolStore};
pub use types::{
    RegisteredTool, ToolExecutionResult, ToolListFilter, TOOL_NAMESPACE_SEPARATOR,
};
