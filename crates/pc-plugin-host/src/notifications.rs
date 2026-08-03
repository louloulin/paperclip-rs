//! Worker → host 通知通道。
//!
//! 协议里 worker 可以主动通过 `workerToHost.emitEvent` / `streams.emit` 等
//! 通知 host。host 内部通过 `NotificationBus` 将这些通知广播给 HTTP 路由和
//! SSE stream bridge。结构与原 `plugin-stream-bus.ts` 对齐：按
//! `(plugin_id, channel, company_id)` 三元组订阅。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct Notification {
    pub plugin_id: Uuid,
    pub method: String,
    pub params: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct StreamBridgeEvent {
    pub event: serde_json::Value,
    pub event_type: &'static str,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct SubscriptionKey {
    pub plugin_id: Uuid,
    pub channel: String,
    pub company_id: Uuid,
}

type StreamSender = broadcast::Sender<StreamBridgeEvent>;
type NotificationSender = broadcast::Sender<Notification>;

#[derive(Default)]
struct BusState {
    streams: HashMap<SubscriptionKey, StreamSender>,
    notifications: HashMap<(Uuid, String), NotificationSender>,
}

#[derive(Clone, Default)]
pub struct NotificationBus {
    inner: Arc<RwLock<BusState>>,
}

impl NotificationBus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn subscribe_stream(
        &self,
        key: SubscriptionKey,
    ) -> (SubscriptionGuard, broadcast::Receiver<StreamBridgeEvent>) {
        let mut state = self.inner.write().expect("bus lock poisoned");
        let sender = state
            .streams
            .entry(key.clone())
            .or_insert_with(|| broadcast::channel(64).0);
        let rx = sender.subscribe();
        let guard = SubscriptionGuard {
            key,
            bus: self.clone(),
        };
        (guard, rx)
    }

    pub fn publish_stream(
        &self,
        key: &SubscriptionKey,
        event: serde_json::Value,
        event_type: &'static str,
    ) {
        let state = self.inner.read().expect("bus lock poisoned");
        if let Some(sender) = state.streams.get(key) {
            let _ = sender.send(StreamBridgeEvent { event, event_type });
        }
    }

    pub fn subscribe_plugin(
        &self,
        plugin_id: Uuid,
        method: &str,
    ) -> (PluginSubscriptionGuard, broadcast::Receiver<Notification>) {
        let key = (plugin_id, method.to_string());
        let mut state = self.inner.write().expect("bus lock poisoned");
        let sender = state
            .notifications
            .entry(key.clone())
            .or_insert_with(|| broadcast::channel(64).0);
        let rx = sender.subscribe();
        let guard = PluginSubscriptionGuard {
            key,
            bus: self.clone(),
        };
        (guard, rx)
    }

    pub fn publish_notification(&self, notification: Notification) {
        let key = (notification.plugin_id, notification.method.clone());
        let state = self.inner.read().expect("bus lock poisoned");
        if let Some(sender) = state.notifications.get(&key) {
            let _ = sender.send(notification);
        }
    }
}

pub struct SubscriptionGuard {
    key: SubscriptionKey,
    bus: NotificationBus,
}

impl Drop for SubscriptionGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.bus.inner.write() {
            if let Some(sender) = state.streams.get(&self.key) {
                if sender.receiver_count() == 0 {
                    state.streams.remove(&self.key);
                }
            }
        }
    }
}

pub struct PluginSubscriptionGuard {
    key: (Uuid, String),
    bus: NotificationBus,
}

impl Drop for PluginSubscriptionGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.bus.inner.write() {
            if let Some(sender) = state.notifications.get(&self.key) {
                if sender.receiver_count() == 0 {
                    state.notifications.remove(&self.key);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscribe_and_publish_stream() {
        let bus = NotificationBus::new();
        let key = SubscriptionKey {
            plugin_id: Uuid::new_v4(),
            channel: "ui".into(),
            company_id: Uuid::new_v4(),
        };
        let (_guard, mut rx) = bus.subscribe_stream(key.clone());
        bus.publish_stream(&key, serde_json::json!({"n": 1}), "message");
        let evt = rx.recv().await.unwrap();
        assert_eq!(evt.event_type, "message");
        assert_eq!(evt.event["n"], 1);
    }

    #[tokio::test]
    async fn drop_guard_clears_subscription() {
        let bus = NotificationBus::new();
        let key = SubscriptionKey {
            plugin_id: Uuid::new_v4(),
            channel: "ui".into(),
            company_id: Uuid::new_v4(),
        };
        {
            let (_guard, _rx) = bus.subscribe_stream(key.clone());
        }
        let state = bus.inner.read().unwrap();
        assert!(state.streams.is_empty());
    }
}
