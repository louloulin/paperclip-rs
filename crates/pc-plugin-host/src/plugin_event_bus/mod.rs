//! Plugin event bus（1:1 port of Node `server/src/services/plugin-event-bus.ts`，412 行）。
//!
//! 单一职责：内存 pub/sub bus，把核心域事件路由到 plugin 订阅者，支持：
//! - pattern matching（精确 + 尾随通配 `plugin.foo.*`）
//! - 服务端 EventFilter（projectId / companyId / agentId 多字段 AND）
//! - plugin 命名空间隔离（订阅按 plugin_id 分桶）
//! - 命名空间守卫（plugin 不能 emit 带 `plugin.` 前缀的事件名）
//! - handler 错误隔离（单 handler 异常不影响其他投递）
//!
//! ## 模块结构（mod/ 拆分，遵循 docs/08-RUST-MODULAR-ARCHITECTURE.md）
//!
//! ```text
//! plugin_event_bus/
//! ├── mod.rs       # facade, pub use 重导出
//! ├── types.rs     # PluginEvent / EventFilter / Subscription / 错误类型
//! ├── pattern.rs   # matches_pattern + 命名空间守卫
//! ├── filter.rs    # passes_filter + 字段解析
//! ├── bus.rs       # PluginEventBus + ScopedPluginEventBus 主实现
//! └── tests.rs     # 模块内单测
//! ```

mod bus;
mod filter;
mod pattern;
mod types;

#[cfg(test)]
mod tests;

// Public facade: 重导出稳定 API
pub use bus::{FilterOrHandler, PluginEventBus, ScopedPluginEventBus};
pub use pattern::{
    matches_pattern, namespaced_event_type, validate_event_name, PLUGIN_EVENT_PREFIX,
};
pub use types::{
    ActorType, AsyncHandler, EventFilter, PluginEvent, PluginEventBusDeliveryError,
    PluginEventBusEmitResult, ScopedBusError, Subscription,
};
