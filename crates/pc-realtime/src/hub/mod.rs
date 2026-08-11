//! Live-events 进程内 pub/sub（原 `pc-live-events` 已下沉）。
//!
//! 对应 Node `server/src/services/live-events.ts`（54 行）。
//!
//! ## 设计目标
//!
//! - **进程内广播**：通过 `tokio::sync::broadcast` Channel 实现多 subscriber pub/sub。
//! - **公司作用域**：每个 `company_id` 一个独立 channel；`*` 全局 channel 用于跨公司事件。
//! - **零持久化**：本 crate 不持有任何持久化状态，所有事件 live 在内存中。
//! - **单调递增 event id**：与 Node `nextEventId += 1` 1:1 对齐。
//!
//! ## 公共 API
//!
//! - [`LiveEventHub::new`] —— 创建 hub
//! - [`publish_live_event`] / [`publish_global_live_event`] —— 顶层便捷函数
//! - [`LiveEventHub::subscribe_company`] / [`subscribe_global`] —— 订阅
//!
//! ## 设计原则：
//!
//! - **高内聚**：所有 pub/sub 逻辑集中在本 crate。
//! - **低耦合**：调用方通过 `LiveEventHub` 句柄订阅 / 发布。
//! - **可测**：channel-based 异步订阅易测试。

mod types;

pub use types::{LiveEvent, LiveEventPayload, LiveEventType, GLOBAL_COMPANY_ID};

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use tokio::sync::broadcast;

// ============================================================================
// Hub
// ============================================================================

/// Live events hub（与 Node `emitter` 全局单例 1:1 对齐，但作为可注入实例）。
///
/// 内部维护：
/// - 每个 `company_id` 一个 `broadcast::Sender<LiveEvent>`（capacity = 256）。
/// - 一个全局 `*` channel（capacity = 256）。
/// - 单调递增的 `next_event_id`（AtomicI64）。
pub struct LiveEventHub {
    inner: Arc<LiveEventHubInner>,
}

struct LiveEventHubInner {
    next_event_id: AtomicI64,
    /// `company_id -> Sender`
    company_senders: Mutex<HashMap<String, broadcast::Sender<LiveEvent>>>,
    /// 全局 `*` channel
    global_sender: broadcast::Sender<LiveEvent>,
}

impl LiveEventHub {
    /// 创建新 hub（global channel capacity = 256）。
    pub fn new() -> Self {
        let (global_sender, _) = broadcast::channel(256);
        Self {
            inner: Arc::new(LiveEventHubInner {
                next_event_id: AtomicI64::new(0),
                company_senders: Mutex::new(HashMap::new()),
                global_sender,
            }),
        }
    }

    /// 构造 event（与 Node `toLiveEvent` 1:1 对齐）。
    fn to_live_event(
        &self,
        company_id: &str,
        event_type: LiveEventType,
        payload: Option<LiveEventPayload>,
    ) -> LiveEvent {
        let id = self.inner.next_event_id.fetch_add(1, Ordering::SeqCst) + 1;
        LiveEvent {
            id,
            company_id: company_id.to_string(),
            event_type,
            created_at: chrono::Utc::now().to_rfc3339(),
            payload: payload.unwrap_or_default(),
        }
    }

    /// 获取或创建 company sender。
    fn get_or_create_company_sender(&self, company_id: &str) -> broadcast::Sender<LiveEvent> {
        let mut guard = self.inner.company_senders.lock();
        guard
            .entry(company_id.to_string())
            .or_insert_with(|| broadcast::channel(256).0)
            .clone()
    }

    /// 发布 company-scoped event（与 Node `publishLiveEvent` 1:1 对齐）。
    pub fn publish(
        &self,
        company_id: impl Into<String>,
        event_type: LiveEventType,
        payload: Option<LiveEventPayload>,
    ) -> LiveEvent {
        let company_id = company_id.into();
        let event = self.to_live_event(&company_id, event_type, payload);
        let sender = self.get_or_create_company_sender(&company_id);
        // 忽略错误（无订阅者时 send 返回 Err）
        let _ = sender.send(event.clone());
        event
    }

    /// 发布 global event（与 Node `publishGlobalLiveEvent` 1:1 对齐）。
    pub fn publish_global(
        &self,
        event_type: LiveEventType,
        payload: Option<LiveEventPayload>,
    ) -> LiveEvent {
        let event = self.to_live_event(GLOBAL_COMPANY_ID, event_type, payload);
        let _ = self.inner.global_sender.send(event.clone());
        event
    }

    /// 订阅 company events（与 Node `subscribeCompanyLiveEvents` 1:1 对齐）。
    pub fn subscribe_company(&self, company_id: &str) -> broadcast::Receiver<LiveEvent> {
        self.get_or_create_company_sender(company_id).subscribe()
    }

    /// 订阅 global events（与 Node `subscribeGlobalLiveEvents` 1:1 对齐）。
    pub fn subscribe_global(&self) -> broadcast::Receiver<LiveEvent> {
        self.inner.global_sender.subscribe()
    }

    /// 当前活跃的 event 数（自 hub 创建起）。
    pub fn next_event_id(&self) -> i64 {
        self.inner.next_event_id.load(Ordering::SeqCst)
    }
}

impl Default for LiveEventHub {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// 顶层便捷函数（与 Node 直接调用风格 1:1 对齐）
// ============================================================================

static GLOBAL_HUB: parking_lot::Mutex<Option<Arc<LiveEventHub>>> = parking_lot::Mutex::new(None);

/// 获取全局 hub 单例（lazy init）。
///
/// Node 端 `emitter` 是模块级单例；Rust 端通过 lazy 静态变量模拟。
pub fn global_hub() -> Arc<LiveEventHub> {
    let mut guard = GLOBAL_HUB.lock();
    if guard.is_none() {
        *guard = Some(Arc::new(LiveEventHub::new()));
    }
    guard.as_ref().unwrap().clone()
}

/// 发布 company event 到全局 hub（与 Node `publishLiveEvent` 1:1 对齐）。
pub fn publish_live_event(
    company_id: impl Into<String>,
    event_type: LiveEventType,
    payload: Option<LiveEventPayload>,
) -> LiveEvent {
    global_hub().publish(company_id, event_type, payload)
}

/// 发布 global event 到全局 hub（与 Node `publishGlobalLiveEvent` 1:1 对齐）。
pub fn publish_global_live_event(
    event_type: LiveEventType,
    payload: Option<LiveEventPayload>,
) -> LiveEvent {
    global_hub().publish_global(event_type, payload)
}

/// 订阅 company events（与 Node `subscribeCompanyLiveEvents` 1:1 对齐）。
pub fn subscribe_company_live_events(
    company_id: impl Into<String>,
) -> broadcast::Receiver<LiveEvent> {
    global_hub().subscribe_company(&company_id.into())
}

/// 订阅 global events（与 Node `subscribeGlobalLiveEvents` 1:1 对齐）。
pub fn subscribe_global_live_events() -> broadcast::Receiver<LiveEvent> {
    global_hub().subscribe_global()
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn r677_publish_assigns_monotonic_ids() {
        let hub = LiveEventHub::new();
        let e1 = hub.publish("c1", LiveEventType("issue.created".into()), None);
        let e2 = hub.publish("c1", LiveEventType("issue.updated".into()), None);
        let e3 = hub.publish_global(LiveEventType("global.event".into()), None);
        assert_eq!(e1.id, 1);
        assert_eq!(e2.id, 2);
        assert_eq!(e3.id, 3);
        assert_eq!(e3.company_id, GLOBAL_COMPANY_ID);
    }

    #[tokio::test]
    async fn r677_publish_default_payload_is_empty_object() {
        let hub = LiveEventHub::new();
        let event = hub.publish("c1", LiveEventType("x".into()), None);
        assert_eq!(event.payload, LiveEventPayload::default());
        assert_eq!(event.payload, serde_json::Map::new());
    }

    #[tokio::test]
    async fn r677_publish_sets_created_at_iso() {
        let hub = LiveEventHub::new();
        let event = hub.publish("c1", LiveEventType("x".into()), None);
        // RFC3339 格式
        assert!(event.created_at.contains('T'));
        assert!(event.created_at.ends_with('Z') || event.created_at.contains('+'));
    }

    #[tokio::test]
    async fn r677_subscribe_company_receives_only_matching_events() {
        let hub = LiveEventHub::new();
        let mut rx_a = hub.subscribe_company("A");
        let mut rx_b = hub.subscribe_company("B");

        hub.publish("A", LiveEventType("x".into()), Some(json!({"v": 1}).as_object().unwrap().clone()));
        hub.publish("B", LiveEventType("y".into()), Some(json!({"v": 2}).as_object().unwrap().clone()));

        let event_a = rx_a.recv().await.unwrap();
        assert_eq!(event_a.company_id, "A");
        assert_eq!(event_a.payload["v"], json!(1));

        let event_b = rx_b.recv().await.unwrap();
        assert_eq!(event_b.company_id, "B");
        assert_eq!(event_b.payload["v"], json!(2));
    }

    #[tokio::test]
    async fn r677_global_subscribers_do_not_receive_company_events() {
        let hub = LiveEventHub::new();
        let mut rx_global = hub.subscribe_global();

        hub.publish("A", LiveEventType("x".into()), None);

        // 给 global publisher 一点时间
        let res =
            tokio::time::timeout(std::time::Duration::from_millis(50), rx_global.recv()).await;
        assert!(
            res.is_err() || matches!(res, Ok(Err(_))),
            "global subscriber should NOT receive company event"
        );
    }

    #[tokio::test]
    async fn r677_global_publisher_routes_to_global_subscribers() {
        let hub = LiveEventHub::new();
        let mut rx_global = hub.subscribe_global();

        hub.publish_global(LiveEventType("system.announcement".into()), None);

        let event = tokio::time::timeout(std::time::Duration::from_millis(100), rx_global.recv())
            .await
            .expect("timeout")
            .expect("recv");
        assert_eq!(event.company_id, GLOBAL_COMPANY_ID);
        assert_eq!(event.event_type.0, "system.announcement");
    }

    #[tokio::test]
    async fn r677_multiple_subscribers_each_receive_event() {
        let hub = LiveEventHub::new();
        let mut rx1 = hub.subscribe_company("X");
        let mut rx2 = hub.subscribe_company("X");

        hub.publish("X", LiveEventType("evt".into()), None);

        let e1 = rx1.recv().await.unwrap();
        let e2 = rx2.recv().await.unwrap();
        assert_eq!(e1.id, e2.id);
        assert_eq!(e1.event_type.0, "evt");
    }

    #[tokio::test]
    async fn r677_unsubscribe_via_drop() {
        // broadcast::Receiver 没有显式 unsubscribe，drop 即可。
        let hub = LiveEventHub::new();
        let rx = hub.subscribe_company("X");
        drop(rx);
        // 不会 panic
        hub.publish("X", LiveEventType("after".into()), None);
    }

    #[tokio::test]
    async fn r677_global_hub_singleton_persists() {
        // 验证 global_hub() 每次返回同一实例
        let h1 = global_hub();
        let h2 = global_hub();
        assert!(Arc::ptr_eq(&h1, &h2));
    }

    #[tokio::test]
    async fn r677_top_level_publish_uses_global_hub() {
        let mut rx = subscribe_company_live_events("co-top");
        let event = publish_live_event("co-top", LiveEventType("top".into()), Some(json!({"k": "v"}).as_object().unwrap().clone()));
        let received = rx.recv().await.unwrap();
        assert_eq!(received.id, event.id);
        assert_eq!(received.payload["k"], "v");
    }
}
