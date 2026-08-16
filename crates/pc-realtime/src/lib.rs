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
//! - `subscribe_from(last_event_id)` 在订阅时立即重放 `event_id > last_event_id` 的缓存事件
//! - 重连 resume 通过 `last_event_id` 参数传递；`live_events` 路由接收 `?resume=<id>` query

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use uuid::Uuid;

/// 默认 resume 缓存容量（保留最近 N 条事件用于重连重放）。
pub const DEFAULT_REPLAY_CAPACITY: usize = 1024;

/// 实时事件。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LiveEvent {
    /// 单调递增的事件 ID（用于重连 resume）。
    pub event_id: u64,
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
            event_id: 0, // 由 RealtimeHandle::publish 填充
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
    next_id: Arc<AtomicU64>,
    replay: Arc<Mutex<VecDeque<Arc<LiveEvent>>>>,
    replay_capacity: usize,
}

impl RealtimeHandle {
    /// 启动 bus（默认 capacity=1024, replay_capacity=1024）。
    pub fn start(capacity: usize) -> Self {
        Self::start_with_replay(capacity, DEFAULT_REPLAY_CAPACITY)
    }

    /// 启动 bus 并指定 resume 缓存容量。
    pub fn start_with_replay(capacity: usize, replay_capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self {
            tx,
            next_id: Arc::new(AtomicU64::new(1)),
            replay: Arc::new(Mutex::new(VecDeque::with_capacity(replay_capacity))),
            replay_capacity,
        }
    }

    /// 发布事件（自动分配 event_id），返回实际收到事件的订阅者数。
    pub fn publish(&self, mut ev: LiveEvent) -> usize {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        ev.event_id = id;
        let arc = Arc::new(ev);
        // 先入 replay buffer，再 broadcast
        if let Ok(mut buf) = self.replay.lock() {
            if buf.len() >= self.replay_capacity {
                buf.pop_front();
            }
            buf.push_back(arc.clone());
        }
        self.tx.send(arc).unwrap_or(0)
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

    /// 返回当前 next event_id（下一个被分配的值）。
    #[must_use]
    pub fn next_event_id(&self) -> u64 {
        self.next_id.load(Ordering::SeqCst)
    }

    /// 从给定 last_event_id 重放缓存中所有 event_id > last_event_id 的事件。
    /// 结果按 event_id 升序返回。如果 last_event_id >= 当前 next_id，返回空。
    pub fn replay_after(&self, last_event_id: u64) -> Vec<Arc<LiveEvent>> {
        let Ok(buf) = self.replay.lock() else {
            return Vec::new();
        };
        buf.iter()
            .filter(|e| e.event_id > last_event_id)
            .cloned()
            .collect()
    }

    /// 在订阅时立即重放 missed events，然后切换到 broadcast 订阅。
    /// 用于 WS 重连 resume。
    pub fn subscribe_with_resume(
        &self,
        last_event_id: u64,
    ) -> (Vec<Arc<LiveEvent>>, broadcast::Receiver<Arc<LiveEvent>>) {
        let replay = self.replay_after(last_event_id);
        let rx = self.subscribe();
        (replay, rx)
    }

    /// 返回当前 resume buffer 中的事件数。
    pub fn replay_len(&self) -> usize {
        self.replay.lock().map(|b| b.len()).unwrap_or(0)
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
        assert!(evt.event_id >= 1);
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

    #[test]
    fn event_ids_are_monotonic() {
        let h = RealtimeHandle::start(64);
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let id3 = Uuid::new_v4();
        h.publish(LiveEvent::new("a", "x", id1));
        h.publish(LiveEvent::new("b", "x", id2));
        h.publish(LiveEvent::new("c", "x", id3));
        let replay = h.replay_after(0);
        assert_eq!(replay.len(), 3);
        assert!(replay[0].event_id < replay[1].event_id);
        assert!(replay[1].event_id < replay[2].event_id);
    }

    #[test]
    fn replay_after_filters_by_last_event_id() {
        let h = RealtimeHandle::start(64);
        h.publish(LiveEvent::new("a", "x", Uuid::new_v4()));
        h.publish(LiveEvent::new("b", "x", Uuid::new_v4()));
        h.publish(LiveEvent::new("c", "x", Uuid::new_v4()));
        let all = h.replay_after(0);
        assert_eq!(all.len(), 3);
        let after_first = h.replay_after(all[0].event_id);
        assert_eq!(after_first.len(), 2);
        let after_last = h.replay_after(all[2].event_id);
        assert_eq!(after_last.len(), 0);
    }

    #[test]
    fn replay_buffer_caps_at_capacity() {
        let h = RealtimeHandle::start_with_replay(64, 4);
        for _ in 0..10 {
            h.publish(LiveEvent::new("x", "y", Uuid::new_v4()));
        }
        // 只保留最近 4 条
        assert_eq!(h.replay_len(), 4);
    }

    #[test]
    fn subscribe_with_resume_returns_replay_then_live() {
        let h = RealtimeHandle::start_with_replay(64, 16);
        h.publish(LiveEvent::new("e1", "x", Uuid::new_v4()));
        h.publish(LiveEvent::new("e2", "x", Uuid::new_v4()));
        // 第三个订阅者断开后回到 0 状态，再以 last_event_id=1 重连
        let (replay, _rx) = h.subscribe_with_resume(1);
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].event, "e2");
    }
}

/// Round 252: Subscriber 抽象层（被 WS / SSE / 测试共用）。
pub mod subscriber;
pub use subscriber::{
    BroadcastSubscriber, FilteredSubscriber, ReplayThenLiveSubscriber, Subscriber,
};

/// Round 252: Realtime channel namespace + 客户端订阅过滤。
pub mod channels;
pub mod event_payload_pure;
pub use channels::{default_channels, matches_any, parse_channels, ChannelFilter};

/// Round 255: Rate limit + connection count limit（防滥用）。
pub mod rate_limit;
pub use rate_limit::{
    ConnectionGuard, ConnectionLimiter, IpRateLimiter, TokenBucket, DEFAULT_BUCKET_CAPACITY,
    DEFAULT_BUCKET_REFILL_PER_SECOND, DEFAULT_MAX_CONNECTIONS_PER_COMPANY,
};

/// R743: WebSocket 桥接层（原 `pc-ws` crate 已下沉）。
pub mod ws_bridge;

/// R743: Live-events 进程内 pub/sub hub（原 `pc-live-events` 已下沉）。
pub mod hooks;
pub mod hub;

/// WebSocket state（server 名 + realtime handle）。
#[derive(Clone)]
pub struct WsState {
    pub realtime: RealtimeHandle,
    pub server_name: String,
    /// R255: per-IP token bucket 限流器。
    pub ip_rate_limiter: Arc<crate::rate_limit::IpRateLimiter>,
    /// R255: per-company 并发连接数限制器。
    pub connection_limiter: Arc<crate::rate_limit::ConnectionLimiter>,
}

impl WsState {
    /// 构造默认配置的 WsState（含默认限流器）。
    pub fn new(realtime: RealtimeHandle, server_name: impl Into<String>) -> Self {
        Self {
            realtime,
            server_name: server_name.into(),
            ip_rate_limiter: Arc::new(crate::rate_limit::IpRateLimiter::default()),
            connection_limiter: Arc::new(crate::rate_limit::ConnectionLimiter::default()),
        }
    }

    /// 自定义限流器配置构造 WsState。
    pub fn with_limiters(
        realtime: RealtimeHandle,
        server_name: impl Into<String>,
        ip_rate_limiter: Arc<crate::rate_limit::IpRateLimiter>,
        connection_limiter: Arc<crate::rate_limit::ConnectionLimiter>,
    ) -> Self {
        Self {
            realtime,
            server_name: server_name.into(),
            ip_rate_limiter,
            connection_limiter,
        }
    }
}

/// Terminal WebSocket — R628 复刻 paperclip Node
/// `server/src/realtime/environment-custom-image-terminal-ws.ts` (766 LOC)。
///
/// 本轮范围：frame / path / traits（带单测）。
/// 后续轮次：handler（WS upgrade + auth + 桥接）+ ssh2 真实 connector。
pub mod terminal;
