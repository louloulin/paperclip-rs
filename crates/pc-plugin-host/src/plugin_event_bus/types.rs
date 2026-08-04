//! Plugin event bus 域类型（对齐 Node `packages/plugins/sdk/src/types.ts` 的
//! `PluginEvent` + `EventFilter` 形状）。
//!
//! 单一职责：定义 bus 用的事件 / 过滤器 / 错误 / 句柄类型，零业务逻辑。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use thiserror::Error;

// ============================================================================
// Core domain types
// ============================================================================

/// Actor 来源类型（与 Node `PluginEvent.actorType` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActorType {
    User,
    Agent,
    System,
    Plugin,
}

/// 域事件信封（与 Node `PluginEvent<TPayload>` 1:1 对齐）。
///
/// 字段：
/// - `event_id`：UUID，唯一事件 id
/// - `event_type`：事件类型（核心事件或 `plugin.<id>.<name>` 命名空间）
/// - `occurred_at`：发生时间（ISO 8601）
/// - `actor_id` / `actor_type`：触发者（可选）
/// - `entity_id` / `entity_type`：主实体（可选）
/// - `company_id`：所属公司
/// - `payload`：事件负载（任意 JSON）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginEvent {
    pub event_id: String,
    pub event_type: String,
    pub occurred_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor_type: Option<ActorType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_type: Option<String>,
    pub company_id: String,
    pub payload: Value,
}

/// 服务端过滤器（与 Node `EventFilter` 1:1 对齐）。
///
/// 所有字段可选；未指定则不约束对应维度。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EventFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}

// ============================================================================
// Subscription & delivery
// ============================================================================

/// Bus 内部订阅记录：pattern + filter + 异步 handler。
///
/// 与 Node `Subscription` 1:1 对齐；handler 是 `async`（实际是 tokio future）。
///
/// 使用 `Arc<dyn AsyncHandler>` 以便跨锁 / 跨 await 共享。
#[derive(Clone)]
pub struct Subscription {
    pub event_pattern: String,
    pub filter: Option<EventFilter>,
    pub handler: Arc<dyn AsyncHandler>,
}

/// 异步 handler trait（与 Node `(event: PluginEvent) => Promise<void>` 1:1 对齐）。
///
/// 实现需 Send + Sync 以便跨订阅者并发投递；通常通过 `Arc<F>` 共享。
pub trait AsyncHandler: Send + Sync {
    fn handle<'a>(
        &'a self,
        event: &'a PluginEvent,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>;
}

/// 便捷 fn 适配器：把 `async fn` 或 `Fn(PluginEvent) -> impl Future` 转成 `Subscription`。
impl<F, Fut> AsyncHandler for F
where
    F: Fn(PluginEvent) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    fn handle<'a>(
        &'a self,
        event: &'a PluginEvent,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        let e = event.clone();
        Box::pin(async move { self(e).await })
    }
}

/// `emit` 返回的错误聚合（与 Node `PluginEventBusEmitResult` 1:1 对齐）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginEventBusEmitResult {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<PluginEventBusDeliveryError>,
}

/// 单条投递错误（pluginId + error 序列化）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginEventBusDeliveryError {
    pub plugin_id: String,
    pub message: String,
}

// ============================================================================
// Errors
// ============================================================================

/// Scoped bus 调用错误（与 Node `forPlugin().emit()` throw 语义 1:1 对齐）。
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ScopedBusError {
    #[error("plugin \"{plugin_id}\" must provide a non-empty event name")]
    EmptyEventName { plugin_id: String },
    #[error("plugin \"{plugin_id}\" must provide a non-empty companyId")]
    EmptyCompanyId { plugin_id: String },
    #[error(
        "plugin \"{plugin_id}\" must not include the \"plugin.\" prefix when emitting events. \
         Emit the bare event name (e.g. \"sync-done\") and the bus will namespace it automatically."
    )]
    ForbiddenPrefix { plugin_id: String },
    #[error("handler function is required when a filter is provided")]
    MissingHandlerWithFilter,
}
