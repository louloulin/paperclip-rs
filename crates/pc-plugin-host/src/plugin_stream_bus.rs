//! Plugin stream bus (1:1 port of Node `server/src/services/plugin-stream-bus.ts`，81 行).
//!
//! 单一职责：内存 pub/sub bus，路由 plugin SSE stream 事件到匹配的订阅者。
//!
//! - 订阅键：`(pluginId, channel, companyId)` 三元组
//! - 订阅者收到 `(event, eventType)` 调用（同步回调）
//! - `publish` 触发所有匹配订阅者；无人订阅 → no-op
//!
//! 设计：
//! - 同步语义与 Node 一致（listener 同步调用）
//! - 内部 `HashMap<String, HashSet<SubscriptionId>>` + `Vec<Subscription>` 解耦 listener 与 key
//! - `unsubscribe` closure 在 `Subscription` 上直接 remove，无需遍历 HashMap

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

// ============================================================================
// Types
// ============================================================================

/// SSE event 类型（与 Node `StreamEventType` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "message" => Some(Self::Message),
            "open" => Some(Self::Open),
            "close" => Some(Self::Close),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

impl Default for StreamEventType {
    fn default() -> Self {
        Self::Message
    }
}

/// 订阅者回调签名（与 Node `StreamSubscriber` 1:1 对齐）。
pub type StreamSubscriber = Box<dyn Fn(serde_json::Value, StreamEventType) + Send + Sync>;

/// 内部订阅记录：listener + 唯一 id。
struct Subscription {
    id: u64,
    listener: StreamSubscriber,
}

/// Plugin stream bus 抽象（与 Node `PluginStreamBus` interface 1:1 对齐）。
pub trait PluginStreamBus: Send + Sync {
    fn subscribe<'a>(
        &'a self,
        plugin_id: &str,
        channel: &str,
        company_id: &str,
        listener: StreamSubscriber,
    ) -> Box<dyn FnOnce() + Send + Sync + 'a>;

    fn publish(
        &self,
        plugin_id: &str,
        channel: &str,
        company_id: &str,
        event: serde_json::Value,
        event_type: Option<StreamEventType>,
    );
}

// ============================================================================
// In-memory implementation
// ============================================================================

/// 内存版 bus（与 Node `createPluginStreamBus()` 1:1 对齐）。
pub struct InMemoryPluginStreamBus {
    /// 订阅索引：key → subscription ids
    subscribers: Mutex<HashMap<String, HashSet<u64>>>,
    /// subscription 池：id → Subscription
    subscriptions: Mutex<HashMap<u64, Subscription>>,
    /// 下一个 subscription id（单调递增）
    next_id: Mutex<u64>,
}

impl InMemoryPluginStreamBus {
    pub fn new() -> Self {
        Self {
            subscribers: Mutex::new(HashMap::new()),
            subscriptions: Mutex::new(HashMap::new()),
            next_id: Mutex::new(0),
        }
    }
}

impl Default for InMemoryPluginStreamBus {
    fn default() -> Self {
        Self::new()
    }
}

/// 构造订阅 key（与 Node `streamKey` 1:1 对齐）。
fn stream_key(plugin_id: &str, channel: &str, company_id: &str) -> String {
    format!("{plugin_id}:{channel}:{company_id}")
}

impl PluginStreamBus for InMemoryPluginStreamBus {
    fn subscribe<'a>(
        &'a self,
        plugin_id: &str,
        channel: &str,
        company_id: &str,
        listener: StreamSubscriber,
    ) -> Box<dyn FnOnce() + Send + Sync + 'a> {
        let key = stream_key(plugin_id, channel, company_id);

        // 分配新 id
        let id = {
            let mut next = self.next_id.lock().expect("next_id poisoned");
            *next += 1;
            *next
        };

        // 插入 subscription
        self.subscriptions
            .lock()
            .expect("subscriptions poisoned")
            .insert(id, Subscription { id, listener });

        // 加入 key 索引
        self.subscribers
            .lock()
            .expect("subscribers poisoned")
            .entry(key.clone())
            .or_insert_with(HashSet::new)
            .insert(id);

        // 返回 unsubscribe closure
        Box::new(move || {
            // 从 subscriptions 移除
            self.subscriptions
                .lock()
                .expect("subscriptions poisoned")
                .remove(&id);
            // 从 subscribers 索引移除
            let mut subs = self.subscribers.lock().expect("subscribers poisoned");
            if let Some(set) = subs.get_mut(&key) {
                set.remove(&id);
                if set.is_empty() {
                    subs.remove(&key);
                }
            }
        })
    }

    fn publish(
        &self,
        plugin_id: &str,
        channel: &str,
        company_id: &str,
        event: serde_json::Value,
        event_type: Option<StreamEventType>,
    ) {
        let key = stream_key(plugin_id, channel, company_id);
        let event_type = event_type.unwrap_or_default();

        // 取出当前 key 的订阅者 id 列表（释放锁后再调用 listener，避免持锁）
        let ids: Vec<u64> = {
            let subs = self.subscribers.lock().expect("subscribers poisoned");
            subs.get(&key)
                .map(|set| set.iter().copied().collect())
                .unwrap_or_default()
        };

        // 取出 listener 并调用
        let subscriptions = self.subscriptions.lock().expect("subscriptions poisoned");
        for id in ids {
            if let Some(sub) = subscriptions.get(&id) {
                (sub.listener)(event.clone(), event_type);
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

// ============================================================================
// Factory
// ============================================================================

/// 构造默认 `InMemoryPluginStreamBus` 的工厂函数。
///
/// 旧 `pc-plugin-stream-bus` crate 的 `create_plugin_stream_bus()` API 等价物。
pub fn create_plugin_stream_bus() -> InMemoryPluginStreamBus {
    InMemoryPluginStreamBus::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // ---- StreamEventType ----

    #[test]
    fn event_type_as_str_matches_node() {
        assert_eq!(StreamEventType::Message.as_str(), "message");
        assert_eq!(StreamEventType::Open.as_str(), "open");
        assert_eq!(StreamEventType::Close.as_str(), "close");
        assert_eq!(StreamEventType::Error.as_str(), "error");
    }

    #[test]
    fn event_type_parse_round_trip() {
        for t in [
            StreamEventType::Message,
            StreamEventType::Open,
            StreamEventType::Close,
            StreamEventType::Error,
        ] {
            assert_eq!(StreamEventType::parse(t.as_str()), Some(t));
        }
        assert_eq!(StreamEventType::parse("unknown"), None);
    }

    #[test]
    fn event_type_default_is_message() {
        assert_eq!(StreamEventType::default(), StreamEventType::Message);
    }

    // ---- stream_key ----

    #[test]
    fn stream_key_concatenates_with_colons() {
        assert_eq!(stream_key("p1", "c1", "co1"), "p1:c1:co1");
    }

    // ---- publish ----

    #[test]
    fn publish_to_empty_bus_is_noop() {
        let bus = InMemoryPluginStreamBus::new();
        bus.publish("p1", "c1", "co1", json!({}), None);
        // 无订阅者，不应 panic
    }

    #[test]
    fn publish_calls_subscribed_listener() {
        let bus = InMemoryPluginStreamBus::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();

        let _unsub = bus.subscribe(
            "p1",
            "c1",
            "co1",
            Box::new(move |_event, _event_type| {
                counter_clone.fetch_add(1, Ordering::SeqCst);
            }),
        );

        bus.publish("p1", "c1", "co1", json!({"x": 1}), None);
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        bus.publish("p1", "c1", "co1", json!({"x": 2}), None);
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn publish_to_different_key_does_not_invoke_listener() {
        let bus = InMemoryPluginStreamBus::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();

        let _unsub = bus.subscribe(
            "p1",
            "c1",
            "co1",
            Box::new(move |_, _| {
                counter_clone.fetch_add(1, Ordering::SeqCst);
            }),
        );

        // 不同 plugin_id
        bus.publish("p2", "c1", "co1", json!({}), None);
        // 不同 channel
        bus.publish("p1", "c2", "co1", json!({}), None);
        // 不同 company_id
        bus.publish("p1", "c1", "co2", json!({}), None);

        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn publish_default_event_type_is_message() {
        let bus = InMemoryPluginStreamBus::new();
        let received_type = Arc::new(Mutex::new(None));
        let received_type_clone = received_type.clone();

        let _unsub = bus.subscribe(
            "p1",
            "c1",
            "co1",
            Box::new(move |_event, event_type| {
                *received_type_clone.lock().unwrap() = Some(event_type);
            }),
        );

        bus.publish("p1", "c1", "co1", json!({}), None);
        assert_eq!(
            *received_type.lock().unwrap(),
            Some(StreamEventType::Message)
        );
    }

    #[test]
    fn publish_explicit_event_type_is_used() {
        let bus = InMemoryPluginStreamBus::new();
        let received_type = Arc::new(Mutex::new(None));
        let received_type_clone = received_type.clone();

        let _unsub = bus.subscribe(
            "p1",
            "c1",
            "co1",
            Box::new(move |_event, event_type| {
                *received_type_clone.lock().unwrap() = Some(event_type);
            }),
        );

        bus.publish("p1", "c1", "co1", json!({}), Some(StreamEventType::Error));
        assert_eq!(*received_type.lock().unwrap(), Some(StreamEventType::Error));
    }

    #[test]
    fn publish_event_payload_passed_through() {
        let bus = InMemoryPluginStreamBus::new();
        let received = Arc::new(Mutex::new(None));
        let received_clone = received.clone();

        let _unsub = bus.subscribe(
            "p1",
            "c1",
            "co1",
            Box::new(move |event, _event_type| {
                *received_clone.lock().unwrap() = Some(event);
            }),
        );

        bus.publish("p1", "c1", "co1", json!({"msg": "hello", "code": 42}), None);

        let payload = received.lock().unwrap().clone().unwrap();
        assert_eq!(payload["msg"], "hello");
        assert_eq!(payload["code"], 42);
    }

    // ---- 多订阅者 ----

    #[test]
    fn multiple_subscribers_all_receive_event() {
        let bus = InMemoryPluginStreamBus::new();
        let counter_a = Arc::new(AtomicUsize::new(0));
        let counter_b = Arc::new(AtomicUsize::new(0));

        let ca = counter_a.clone();
        let cb = counter_b.clone();

        let _u1 = bus.subscribe(
            "p1",
            "c1",
            "co1",
            Box::new(move |_, _| {
                ca.fetch_add(1, Ordering::SeqCst);
            }),
        );
        let _u2 = bus.subscribe(
            "p1",
            "c1",
            "co1",
            Box::new(move |_, _| {
                cb.fetch_add(1, Ordering::SeqCst);
            }),
        );

        bus.publish("p1", "c1", "co1", json!({}), None);
        assert_eq!(counter_a.load(Ordering::SeqCst), 1);
        assert_eq!(counter_b.load(Ordering::SeqCst), 1);
    }

    // ---- unsubscribe ----

    #[test]
    fn unsubscribe_stops_callbacks() {
        let bus = InMemoryPluginStreamBus::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();

        let unsub = bus.subscribe(
            "p1",
            "c1",
            "co1",
            Box::new(move |_, _| {
                counter_clone.fetch_add(1, Ordering::SeqCst);
            }),
        );

        bus.publish("p1", "c1", "co1", json!({}), None);
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        unsub();

        bus.publish("p1", "c1", "co1", json!({}), None);
        assert_eq!(counter.load(Ordering::SeqCst), 1); // 不再增加
    }

    #[test]
    fn unsubscribe_removes_empty_key() {
        let bus = InMemoryPluginStreamBus::new();
        let unsub = bus.subscribe("p1", "c1", "co1", Box::new(|_, _| {}));

        unsub();

        // key 已清空，publish 应该是 noop
        let key_count = bus.subscribers.lock().unwrap().contains_key("p1:c1:co1");
        assert!(!key_count);
    }

    #[test]
    fn unsubscribe_keeps_key_when_other_subscribers_remain() {
        let bus = InMemoryPluginStreamBus::new();
        let counter_a = Arc::new(AtomicUsize::new(0));
        let counter_b = Arc::new(AtomicUsize::new(0));

        let ca = counter_a.clone();
        let cb = counter_b.clone();

        let unsub_a = bus.subscribe(
            "p1",
            "c1",
            "co1",
            Box::new(move |_, _| {
                ca.fetch_add(1, Ordering::SeqCst);
            }),
        );
        let _unsub_b = bus.subscribe(
            "p1",
            "c1",
            "co1",
            Box::new(move |_, _| {
                cb.fetch_add(1, Ordering::SeqCst);
            }),
        );

        unsub_a();

        bus.publish("p1", "c1", "co1", json!({}), None);
        assert_eq!(counter_a.load(Ordering::SeqCst), 0);
        assert_eq!(counter_b.load(Ordering::SeqCst), 1);

        // key 仍存在（因为还有 b）
        let key_count = bus.subscribers.lock().unwrap().contains_key("p1:c1:co1");
        assert!(key_count);
    }

    // ---- Default ----

    #[test]
    fn new_creates_empty_bus() {
        let bus = InMemoryPluginStreamBus::new();
        assert!(bus.subscribers.lock().unwrap().is_empty());
        assert!(bus.subscriptions.lock().unwrap().is_empty());
        assert_eq!(*bus.next_id.lock().unwrap(), 0);
    }
}
