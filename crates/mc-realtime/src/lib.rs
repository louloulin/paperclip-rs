//! Multica realtime event bus：tokio broadcast + live-events WS 协议。
//!
//! 与 paperclip-rs `pc-realtime` 行为等价，但事件 envelope 与 multica `live-events.ts` 对齐：
//! `{ event_id, resource, resource_id, actor, at, data }`。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::{debug, warn};

use mc_core::Id;
use mc_core::timestamp::Timestamp;

pub mod envelope;
pub mod handle;

pub use envelope::{Event, EventEnvelope};
pub use handle::RealtimeHandle;

pub const DEFAULT_CHANNEL_CAPACITY: usize = 1024;

#[derive(Clone)]
pub struct WsState {
    pub handle: RealtimeHandle,
    pub service_label: &'static str,
}

impl WsState {
    pub fn new(handle: RealtimeHandle, label: &'static str) -> Self {
        Self { handle, service_label: label }
    }
}

/// 事件订阅者 token。
pub struct Subscription {
    pub rx: broadcast::Receiver<EventEnvelope>,
}

impl Subscription {
    pub async fn recv(&mut self) -> Option<EventEnvelope> {
        match self.rx.recv().await {
            Ok(env) => Some(env),
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                warn!(skipped, "ws subscription lagged; resetting");
                Some(EventEnvelope::lagged(skipped))
            }
            Err(broadcast::error::RecvError::Closed) => None,
        }
    }
}

/// Broadcast event bus (initial; in-memory).
#[derive(Clone)]
pub struct Bus {
    tx: broadcast::Sender<EventEnvelope>,
}

impl Bus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx }
    }

    pub fn publish(&self, envelope: EventEnvelope) {
        // 允许 send 失败（无订阅者），不 panic。
        let _ = self.tx.send(envelope);
    }

    pub fn subscribe(&self) -> Subscription {
        Subscription { rx: self.tx.subscribe() }
    }

    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

/// 全局事件总线（包装 Arc<Bus>）。
#[derive(Clone, Default)]
pub struct EventBus {
    inner: Arc<Bus>,
}

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self { inner: Arc::new(Bus::new(capacity)) }
    }

    pub fn publish<E: Into<EventEnvelope>>(&self, event: E) {
        let env = event.into();
        debug!(event_id = %env.event_id, resource = %env.resource, "publishing event");
        self.inner.publish(env);
    }

    pub fn subscribe(&self) -> Subscription {
        self.inner.subscribe()
    }

    pub fn receiver_count(&self) -> usize {
        self.inner.receiver_count()
    }
}

/// 便捷：构造一个 envelope。
pub fn envelope(
    resource: impl Into<String>,
    resource_id: impl Into<String>,
    actor: Option<Id>,
    data: serde_json::Value,
) -> EventEnvelope {
    EventEnvelope::new(resource, resource_id, actor, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscribe_receives_published_event() {
        let bus = EventBus::with_capacity(8);
        let mut sub = bus.subscribe();
        bus.publish(envelope("issue", "issue-1", None, serde_json::json!({"x":1})));
        let env = tokio::time::timeout(std::time::Duration::from_millis(100), sub.recv())
            .await
            .expect("event arrived")
            .expect("non-empty");
        assert_eq!(env.resource, "issue");
    }

    #[tokio::test]
    async fn receive_count_increases_with_subscribers() {
        let bus = EventBus::with_capacity(4);
        assert_eq!(bus.receiver_count(), 0);
        let _a = bus.subscribe();
        let _b = bus.subscribe();
        assert_eq!(bus.receiver_count(), 2);
    }
}