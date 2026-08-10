#![forbid(unsafe_code)]
//! `pc-plugin-stream-bus` —— 进程内 plugin SSE 事件 pub/sub 总线。
//!
//! 对应 Node `server/src/services/plugin-stream-bus.ts`（81 行）。
//!
//! 设计目标：1:1 复刻
//! - `subscribe(pluginId, channel, companyId, listener)` → 返回 unsubscribe 闭包
//! - `publish(pluginId, channel, companyId, event, eventType?)` → fan-out 到所有
//!   同 `(pluginId, channel, companyId)` 订阅者
//! - 事件类型枚举：`message` (default) / `open` / `close` / `error`
//! - key 格式：`pluginId:channel:companyId`

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// 流事件类型 —— 与 Node `StreamEventType` 1:1 对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamEventType {
    Message,
    Open,
    Close,
    Error,
}

impl StreamEventType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Open => "open",
            Self::Close => "close",
            Self::Error => "error",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "message" => Some(Self::Message),
            "open" => Some(Self::Open),
            "close" => Some(Self::Close),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// 流订阅者回调。
pub type StreamSubscriber = Arc<dyn Fn(serde_json::Value, StreamEventType) + Send + Sync + 'static>;

/// 内部存储：key → Vec<(subscriber_id, subscriber)>。
#[derive(Default)]
pub struct PluginStreamBus {
    subscribers: Mutex<HashMap<String, Vec<(u64, StreamSubscriber)>>>,
    next_id: Mutex<u64>,
}

impl PluginStreamBus {
    pub fn new() -> Self {
        Self::default()
    }

    fn key(plugin_id: &str, channel: &str, company_id: &str) -> String {
        format!("{plugin_id}:{channel}:{company_id}")
    }

    /// 订阅流事件。返回 unsubscribe 函数。
    pub fn subscribe(
        &self,
        plugin_id: &str,
        channel: &str,
        company_id: &str,
        listener: StreamSubscriber,
    ) -> impl FnOnce() + Send + '_ {
        let key = Self::key(plugin_id, channel, company_id);
        let id = {
            let mut next = self.next_id.lock().expect("next_id poisoned");
            let id = *next;
            *next += 1;
            id
        };
        let mut map = self.subscribers.lock().expect("subscribers poisoned");
        map.entry(key.clone()).or_default().push((id, listener));

        move || {
            let mut map = self.subscribers.lock().expect("subscribers poisoned");
            if let Some(vec) = map.get_mut(&key) {
                vec.retain(|(sid, _)| *sid != id);
                if vec.is_empty() {
                    map.remove(&key);
                }
            }
        }
    }

    /// 发布流事件。
    pub fn publish(
        &self,
        plugin_id: &str,
        channel: &str,
        company_id: &str,
        event: serde_json::Value,
        event_type: Option<StreamEventType>,
    ) {
        let key = Self::key(plugin_id, channel, company_id);
        let et = event_type.unwrap_or(StreamEventType::Message);
        let map = self.subscribers.lock().expect("subscribers poisoned");
        if let Some(subs) = map.get(&key) {
            for (_, listener) in subs {
                listener(event.clone(), et);
            }
        }
    }

    /// 当前订阅者总数（测试用）。
    pub fn subscriber_count(&self) -> usize {
        let map = self.subscribers.lock().unwrap();
        map.values().map(|v| v.len()).sum()
    }

    /// 当前 key 数量（测试用）。
    pub fn key_count(&self) -> usize {
        self.subscribers.lock().unwrap().len()
    }
}

/// 工厂函数。
pub fn create_plugin_stream_bus() -> PluginStreamBus {
    PluginStreamBus::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn r705_subscribe_returns_unsubscribe_fn() {
        let bus = create_plugin_stream_bus();
        let counter = Arc::new(AtomicUsize::new(0));
        let c2 = counter.clone();
        let unsub = bus.subscribe("p1", "ch", "co1", Arc::new(move |_, _| {
            c2.fetch_add(1, Ordering::SeqCst);
        }));
        bus.publish("p1", "ch", "co1", serde_json::json!({"k": "v"}), None);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        unsub();
        bus.publish("p1", "ch", "co1", serde_json::json!({"k": "v2"}), None);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn r705_unsubscribe_cleans_up_empty_key() {
        let bus = create_plugin_stream_bus();
        assert_eq!(bus.key_count(), 0);
        let unsub = bus.subscribe("p1", "ch", "co1", Arc::new(|_, _| {}));
        assert_eq!(bus.key_count(), 1);
        unsub();
        assert_eq!(bus.key_count(), 0);
    }

    #[test]
    fn r705_multiple_subscribers_fan_out() {
        let bus = create_plugin_stream_bus();
        let c1 = Arc::new(AtomicUsize::new(0));
        let c2 = Arc::new(AtomicUsize::new(0));
        let c1c = c1.clone();
        let c2c = c2.clone();
        let _u1 = bus.subscribe("p", "ch", "co", Arc::new(move |_, _| {
            c1c.fetch_add(1, Ordering::SeqCst);
        }));
        let _u2 = bus.subscribe("p", "ch", "co", Arc::new(move |_, _| {
            c2c.fetch_add(1, Ordering::SeqCst);
        }));
        bus.publish("p", "ch", "co", serde_json::json!({}), None);
        assert_eq!(c1.load(Ordering::SeqCst), 1);
        assert_eq!(c2.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn r705_different_keys_independent() {
        let bus = create_plugin_stream_bus();
        let c1 = Arc::new(AtomicUsize::new(0));
        let c2 = Arc::new(AtomicUsize::new(0));
        let c1c = c1.clone();
        let c2c = c2.clone();
        let _u1 = bus.subscribe("p", "ch1", "co", Arc::new(move |_, _| {
            c1c.fetch_add(1, Ordering::SeqCst);
        }));
        let _u2 = bus.subscribe("p", "ch2", "co", Arc::new(move |_, _| {
            c2c.fetch_add(1, Ordering::SeqCst);
        }));
        bus.publish("p", "ch1", "co", serde_json::json!({}), None);
        assert_eq!(c1.load(Ordering::SeqCst), 1);
        assert_eq!(c2.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn r705_publish_to_empty_key_noop() {
        let bus = create_plugin_stream_bus();
        bus.publish("p", "ch", "co", serde_json::json!({}), None);
    }

    #[test]
    fn r705_event_type_default_is_message() {
        let bus = create_plugin_stream_bus();
        let received: Arc<Mutex<Option<StreamEventType>>> = Arc::new(Mutex::new(None));
        let r2 = received.clone();
        let _u = bus.subscribe("p", "ch", "co", Arc::new(move |_, et| {
            *r2.lock().unwrap() = Some(et);
        }));
        bus.publish("p", "ch", "co", serde_json::json!({}), None);
        assert_eq!(*received.lock().unwrap(), Some(StreamEventType::Message));
    }

    #[test]
    fn r705_event_type_explicit() {
        let bus = create_plugin_stream_bus();
        for et in [
            StreamEventType::Message,
            StreamEventType::Open,
            StreamEventType::Close,
            StreamEventType::Error,
        ] {
            let received: Arc<Mutex<Option<StreamEventType>>> = Arc::new(Mutex::new(None));
            let r2 = received.clone();
            let _u = bus.subscribe("p", "ch", "co", Arc::new(move |_, x| {
                *r2.lock().unwrap() = Some(x);
            }));
            bus.publish("p", "ch", "co", serde_json::json!({}), Some(et));
            assert_eq!(*received.lock().unwrap(), Some(et));
        }
    }

    #[test]
    fn r705_event_type_string_round_trip() {
        for et in [
            StreamEventType::Message,
            StreamEventType::Open,
            StreamEventType::Close,
            StreamEventType::Error,
        ] {
            assert_eq!(StreamEventType::from_str(et.as_str()), Some(et));
        }
        assert_eq!(StreamEventType::from_str("unknown"), None);
    }

    #[test]
    fn r705_subscribers_count_correct() {
        let bus = create_plugin_stream_bus();
        let _u1 = bus.subscribe("p", "a", "c", Arc::new(|_, _| {}));
        let _u2 = bus.subscribe("p", "a", "c", Arc::new(|_, _| {}));
        let _u3 = bus.subscribe("p", "b", "c", Arc::new(|_, _| {}));
        assert_eq!(bus.subscriber_count(), 3);
        assert_eq!(bus.key_count(), 2);
    }

    #[test]
    fn r705_one_removed_other_remains() {
        let bus = create_plugin_stream_bus();
        let c1 = Arc::new(AtomicUsize::new(0));
        let c2 = Arc::new(AtomicUsize::new(0));
        let c1c = c1.clone();
        let c2c = c2.clone();
        let u1 = bus.subscribe("p", "ch", "co", Arc::new(move |_, _| {
            c1c.fetch_add(1, Ordering::SeqCst);
        }));
        let _u2 = bus.subscribe("p", "ch", "co", Arc::new(move |_, _| {
            c2c.fetch_add(1, Ordering::SeqCst);
        }));
        u1();
        bus.publish("p", "ch", "co", serde_json::json!({}), None);
        assert_eq!(c1.load(Ordering::SeqCst), 0);
        assert_eq!(c2.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn r705_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PluginStreamBus>();
    }
}
