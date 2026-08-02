//! pc-realtime：实时事件总线。
//!
//! 内部基于 `tokio::sync::broadcast`，单进程全局唯一。
//! `pc-core::actor_runtime::kameo_api` 仍可被上层用来做 actor 化扩展
//! （例如每个 WS 连接作为一个被监管 actor），但本 bus 自身不强依赖 kameo。
//!
//! 设计取舍：
//! - `broadcast::channel` 是高频 fan-out 的标准做法，避免 kameo 0.22 API 变更
//! - `RealtimeHandle` 是 `Clone` + `Send + Sync`，可直接放进 `AppState`
//! - `subscribe()` 返回 `broadcast::Receiver`，可直接喂给 WS / SSE handler

use std::sync::Arc;
use tokio::sync::broadcast;
use uuid::Uuid;

/// 实时事件。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LiveEvent {
    pub event: String,
    pub resource: String,
    pub resource_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    pub at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl LiveEvent {
    pub fn new(event: impl Into<String>, resource: impl Into<String>, resource_id: Uuid) -> Self {
        Self {
            event: event.into(),
            resource: resource.into(),
            resource_id,
            company_id: None,
            actor: None,
            at: chrono::Utc::now(),
            data: None,
        }
    }
    #[must_use]
    pub fn with_company(mut self, cid: Uuid) -> Self {
        self.company_id = Some(cid);
        self
    }
    #[must_use]
    pub fn with_actor(mut self, a: impl Into<String>) -> Self {
        self.actor = Some(a.into());
        self
    }
    #[must_use]
    pub fn with_data(mut self, d: serde_json::Value) -> Self {
        self.data = Some(d);
        self
    }
}

/// 事件总线：clone-便宜，多 handler 共享。
#[derive(Clone)]
pub struct RealtimeHandle {
    tx: broadcast::Sender<Arc<LiveEvent>>,
}

impl RealtimeHandle {
    /// 启动 bus（默认容量 1024）。通常在 main 里调用一次。
    pub fn start(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx }
    }

    /// 发布事件，返回实际收到事件的订阅者数。
    pub fn publish(&self, ev: LiveEvent) -> usize {
        self.tx.send(Arc::new(ev)).unwrap_or(0)
    }

    /// 订阅，返回 `broadcast::Receiver`。
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<LiveEvent>> {
        self.tx.subscribe()
    }

    /// 当前订阅者数。
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pub_sub_roundtrip() {
        let h = RealtimeHandle::start(16);
        let mut rx = h.subscribe();
        let id = Uuid::new_v4();
        let n = h.publish(LiveEvent::new("test.ping", "ping", id));
        assert!(n >= 1, "expected >= 1 subscriber, got {n}");
        let evt = rx.recv().await.unwrap();
        assert_eq!(evt.event, "test.ping");
        assert_eq!(evt.resource_id, id);
    }

    #[tokio::test]
    async fn multiple_subscribers() {
        let h = RealtimeHandle::start(16);
        let mut a = h.subscribe();
        let mut b = h.subscribe();
        assert_eq!(h.subscriber_count(), 2);
        let id = Uuid::new_v4();
        assert_eq!(h.publish(LiveEvent::new("multi", "x", id)), 2);
        assert_eq!(a.recv().await.unwrap().resource_id, id);
        assert_eq!(b.recv().await.unwrap().resource_id, id);
    }

    #[tokio::test]
    async fn no_subscriber_no_panic() {
        let h = RealtimeHandle::start(4);
        let n = h.publish(LiveEvent::new("lonely", "x", Uuid::new_v4()));
        assert_eq!(n, 0);
    }
}

/// WebSocket state（server 名 + realtime handle）。
#[derive(Clone)]
pub struct WsState {
    pub realtime: RealtimeHandle,
    pub server_name: String,
}
