//! PluginEventBus 主实现（与 Node `createPluginEventBus()` + `forPlugin()` 1:1 对齐）。
//!
//! 设计：
//! - 单进程共享：调用方 clone `Arc<PluginEventBus>` 多处复用
//! - 订阅按 plugin_id 隔离（per-plugin 命名空间，互不可见）
//! - `emit` 并发投递到所有匹配订阅者，单 handler 错误被吞掉记录到 `errors`
//! - scoped bus 强制 plugin namespace 前缀（防仿冒）

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use serde_json::Value;

use super::filter::passes_filter;
use super::pattern::{matches_pattern, namespaced_event_type, validate_event_name};
use super::types::{
    AsyncHandler, EventFilter, PluginEvent, PluginEventBusDeliveryError, PluginEventBusEmitResult,
    ScopedBusError, Subscription,
};

// ============================================================================
// PluginEventBus
// ============================================================================

/// 进程级 plugin event 总线（与 Node `createPluginEventBus()` 返回值 1:1 对齐）。
#[derive(Default)]
pub struct PluginEventBus {
    /// plugin_id → 订阅列表（顺序保留：先订阅先投递）
    registry: Mutex<HashMap<String, Vec<Subscription>>>,
}

impl PluginEventBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// 投递一个事件到所有匹配订阅者；handler 错误被吞掉不中断其他投递。
    ///
    /// 与 Node `emit(event)` 1:1 对齐：每个 handler 在自己的 task 中调用，
    /// 错误收集到返回值的 `errors` 数组。
    pub async fn emit(&self, event: PluginEvent) -> PluginEventBusEmitResult {
        // Snapshot subs to avoid holding lock across await
        // Snapshot: 克隆每个匹配的 Subscription 到独立 Vec，释放 registry 锁。
        // 之后所有 handler 调用都基于 owned Arc<dyn AsyncHandler>，不再持有锁。
        let snapshot: Vec<(String, Vec<Arc<dyn AsyncHandler>>)> = {
            let registry = self.registry.lock().expect("registry poisoned");
            registry
                .iter()
                .flat_map(|(plugin_id, subs)| {
                    let matching: Vec<Arc<dyn AsyncHandler>> = subs
                        .iter()
                        .filter(|sub| matches_pattern(&event.event_type, &sub.event_pattern))
                        .filter(|sub| passes_filter(&event, sub.filter.as_ref()))
                        .map(|sub| sub.handler.clone())
                        .collect();
                    if matching.is_empty() {
                        None
                    } else {
                        Some((plugin_id.clone(), matching))
                    }
                })
                .collect()
        };

        let mut errors: Vec<PluginEventBusDeliveryError> = Vec::new();
        let mut futures: Vec<tokio::task::JoinHandle<()>> = Vec::new();

        for (plugin_id, handler_arc) in snapshot {
            for handler in handler_arc {
                let event_for_task = event.clone();
                let pid = plugin_id.clone();
                futures.push(tokio::spawn(async move {
                    handler.handle(&event_for_task).await;
                    let _ = pid; // currently unused but kept for future per-plugin error attribution
                }));
            }
        }

        for handle in futures {
            match handle.await {
                Ok(_) => {}
                Err(join_err) => {
                    if join_err.is_panic() {
                        errors.push(PluginEventBusDeliveryError {
                            plugin_id: "<unknown>".to_string(),
                            message: format!("handler panicked: {join_err}"),
                        });
                    }
                }
            }
        }

        PluginEventBusEmitResult { errors }
    }

    /// 清除某个 plugin 的所有订阅（worker 关闭 / 卸载时调用）。
    pub fn clear_plugin(&self, plugin_id: &str) {
        self.registry
            .lock()
            .expect("registry poisoned")
            .remove(plugin_id);
    }

    /// 返回某个 plugin 的订阅数量（用于测试 / 诊断；与 Node `subscriptionCount` 1:1 对齐）。
    pub fn subscription_count(&self, plugin_id: &str) -> usize {
        self.registry
            .lock()
            .expect("registry poisoned")
            .get(plugin_id)
            .map(|v| v.len())
            .unwrap_or(0)
    }

    /// 给定 plugin_id 返回 scoped handle（与 Node `forPlugin(pluginId)` 1:1 对齐）。
    pub fn for_plugin(&self, plugin_id: impl Into<String>) -> ScopedPluginEventBus<'_> {
        ScopedPluginEventBus {
            bus: self,
            plugin_id: plugin_id.into(),
        }
    }
}

// ============================================================================
// ScopedPluginEventBus
// ============================================================================

/// Plugin 视角的 bus handle（与 Node `ScopedPluginEventBus` 1:1 对齐）。
///
/// - `subscribe(pattern, handler)`：订阅核心域事件或 plugin 命名空间事件
/// - `subscribe(pattern, filter, handler)`：带服务端过滤
/// - `emit(name, company_id, payload)`：自动命名空间化为 `plugin.<id>.<name>`
/// - `clear()`：清除该 plugin 的所有订阅
pub struct ScopedPluginEventBus<'a> {
    bus: &'a PluginEventBus,
    plugin_id: String,
}

impl<'a> ScopedPluginEventBus<'a> {
    /// 订阅事件 pattern；可选 `EventFilter` 服务端过滤。
    ///
    /// 与 Node `subscribe(pattern, handler)` / `subscribe(pattern, filter, handler)` 1:1 对齐。
    pub fn subscribe<H>(
        &self,
        event_pattern: impl Into<String>,
        fn_or_filter: FilterOrHandler<H>,
        maybe_fn: Option<H>,
    ) -> Result<(), ScopedBusError>
    where
        H: AsyncHandler + 'static,
    {
        let (filter, handler): (Option<EventFilter>, Arc<dyn AsyncHandler>) = match fn_or_filter {
            FilterOrHandler::Handler(h) => (None, Arc::new(h)),
            FilterOrHandler::Filter(f) => {
                let h = maybe_fn.ok_or(ScopedBusError::MissingHandlerWithFilter)?;
                (Some(f), Arc::new(h))
            }
        };

        let mut registry = self.bus.registry.lock().expect("registry poisoned");
        registry
            .entry(self.plugin_id.clone())
            .or_insert_with(Vec::new)
            .push(Subscription {
                event_pattern: event_pattern.into(),
                filter,
                handler,
            });
        Ok(())
    }

    /// 投递一个 plugin 命名空间事件。
    ///
    /// 与 Node `forPlugin().emit(name, companyId, payload)` 1:1 对齐：
    /// - 校验 name 非空、不以 `plugin.` 前缀
    /// - 校验 companyId 非空
    /// - 自动 namespace 为 `plugin.<id>.<name>`
    pub async fn emit(
        &self,
        name: &str,
        company_id: &str,
        payload: Value,
    ) -> Result<PluginEventBusEmitResult, ScopedBusError> {
        validate_event_name(&self.plugin_id, name)?;
        if company_id.trim().is_empty() {
            return Err(ScopedBusError::EmptyCompanyId {
                plugin_id: self.plugin_id.clone(),
            });
        }

        let event = PluginEvent {
            event_id: uuid::Uuid::new_v4().to_string(),
            event_type: namespaced_event_type(&self.plugin_id, name),
            occurred_at: Utc::now(),
            actor_id: Some(self.plugin_id.clone()),
            actor_type: Some(super::types::ActorType::Plugin),
            entity_id: None,
            entity_type: None,
            company_id: company_id.to_string(),
            payload,
        };

        Ok(self.bus.emit(event).await)
    }

    /// 清除该 plugin 的所有订阅。
    pub fn clear(&self) {
        self.bus.clear_plugin(&self.plugin_id);
    }
}

// ============================================================================
// FilterOrHandler enum (subscribe overload)
// ============================================================================

/// `subscribe` 的参数多态：`Handler` 或 `Filter + Handler`。
///
/// 与 Node `subscribe(pattern, fnOrFilter, maybeFn?)` 重载 1:1 对齐。
pub enum FilterOrHandler<H> {
    Handler(H),
    Filter(EventFilter),
}

// ============================================================================
// Convenience: namespace constant re-export
// ============================================================================
