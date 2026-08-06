//! Round 252: Subscriber trait —— 把 realtime 订阅抽象成可替换的接口。
//!
//! 背景：
//! - 现存 WS handler 直接消费 `broadcast::Receiver<Arc<LiveEvent>>`，
//!   后续 SSE handler / in-process consumer / 测试 mock 都需要统一的
//!   「下一个事件」抽象。
//! - 本 trait 不取代 `RealtimeHandle`，而是把「订阅源」这一层抽出来，
//!   让上层（WS / SSE / 测试）只关心「next_event() -> Option<Arc<LiveEvent>>」即可。
//!
//! 设计：
//! - `Subscriber::next_event() -> Option<Arc<LiveEvent>>`：异步等待下一个事件；
//!   返回 `None` 时表示订阅源已关闭。
//! - `Subscriber::try_next_event() -> Option<Arc<LiveEvent>>`：非阻塞探测。
//! - `BroadcastSubscriber`：包装 `broadcast::Receiver<Arc<LiveEvent>>`，
//!   把 `Lag(n)` 视为「跳过若干条」继续读，`Closed` 视为 `None`。
//! - `FilteredSubscriber<F>`：装饰器，按 predicate 过滤事件，循环 `next_event` 直到匹配。
//! - `ReplayThenLiveSubscriber`：先重放一批历史事件，再切换到 live。

use futures::future::BoxFuture;
use std::sync::Arc;
use tokio::sync::broadcast;

/// 通用订阅抽象（异步消费 `Arc<LiveEvent>` 流）。
pub trait Subscriber: Send + Sync + 'static {
    /// 异步等待下一个事件；返回 `None` 表示订阅源已关闭。
    fn next_event(&mut self) -> BoxFuture<'_, Option<Arc<crate::LiveEvent>>>;

    /// 非阻塞探测：返回 `None` 表示当前没有新事件（不代表订阅关闭）。
    fn try_next_event(&mut self) -> Option<Arc<crate::LiveEvent>>;
}

/// 包装 `broadcast::Receiver<Arc<LiveEvent>>` 的 Subscriber。
pub struct BroadcastSubscriber {
    rx: broadcast::Receiver<Arc<crate::LiveEvent>>,
}

impl BroadcastSubscriber {
    /// 从 broadcast receiver 构造。
    pub fn new(rx: broadcast::Receiver<Arc<crate::LiveEvent>>) -> Self {
        Self { rx }
    }
}

impl Subscriber for BroadcastSubscriber {
    fn next_event(&mut self) -> BoxFuture<'_, Option<Arc<crate::LiveEvent>>> {
        Box::pin(async move {
            loop {
                match self.rx.recv().await {
                    Ok(ev) => return Some(ev),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        })
    }

    fn try_next_event(&mut self) -> Option<Arc<crate::LiveEvent>> {
        match self.rx.try_recv() {
            Ok(ev) => Some(ev),
            Err(broadcast::error::TryRecvError::Empty) => None,
            Err(broadcast::error::TryRecvError::Lagged(_)) => {
                // 跳过落后事件后，再次尝试
                match self.rx.try_recv() {
                    Ok(ev) => Some(ev),
                    Err(_) => None,
                }
            }
            Err(broadcast::error::TryRecvError::Closed) => None,
        }
    }
}

/// 装饰器：按 predicate 过滤事件。
///
/// 内部循环 `inner.next_event()` 直到匹配 predicate；不匹配的事件被丢弃。
/// 闭包关闭后整个订阅关闭。
pub struct FilteredSubscriber<F>
where
    F: Fn(&crate::LiveEvent) -> bool + Send + Sync + 'static,
{
    inner: Box<dyn Subscriber>,
    predicate: Arc<F>,
    closed: bool,
}

impl<F> FilteredSubscriber<F>
where
    F: Fn(&crate::LiveEvent) -> bool + Send + Sync + 'static,
{
    pub fn new(inner: Box<dyn Subscriber>, predicate: F) -> Self {
        Self {
            inner,
            predicate: Arc::new(predicate),
            closed: false,
        }
    }
}

impl<F> Subscriber for FilteredSubscriber<F>
where
    F: Fn(&crate::LiveEvent) -> bool + Send + Sync + 'static,
{
    fn next_event(&mut self) -> BoxFuture<'_, Option<Arc<crate::LiveEvent>>> {
        Box::pin(async move {
            if self.closed {
                return None;
            }
            loop {
                match self.inner.next_event().await {
                    Some(ev) if (self.predicate)(ev.as_ref()) => return Some(ev),
                    Some(_) => continue,
                    None => {
                        self.closed = true;
                        return None;
                    }
                }
            }
        })
    }

    fn try_next_event(&mut self) -> Option<Arc<crate::LiveEvent>> {
        if self.closed {
            return None;
        }
        loop {
            match self.inner.try_next_event() {
                Some(ev) if (self.predicate)(ev.as_ref()) => return Some(ev),
                Some(_) => continue,
                None => return None,
            }
        }
    }
}

/// 先重放一批历史事件，再切换到 live 订阅。
///
/// 用于 SSE 重连 resume：客户端传入 `last_event_id`，先发 `replay` 中
/// `event_id > last_event_id` 的事件，再切换到 `live` 订阅。
pub struct ReplayThenLiveSubscriber {
    replay: std::vec::IntoIter<Arc<crate::LiveEvent>>,
    live: Option<Box<dyn Subscriber>>,
}

impl ReplayThenLiveSubscriber {
    /// 构造：传入 replay 缓冲与 live subscriber。
    pub fn new(replay: Vec<Arc<crate::LiveEvent>>, live: Box<dyn Subscriber>) -> Self {
        Self {
            replay: replay.into_iter(),
            live: Some(live),
        }
    }
}

impl Subscriber for ReplayThenLiveSubscriber {
    fn next_event(&mut self) -> BoxFuture<'_, Option<Arc<crate::LiveEvent>>> {
        Box::pin(async move {
            if let Some(ev) = self.replay.next() {
                return Some(ev);
            }
            match self.live.as_mut() {
                Some(live) => live.next_event().await,
                None => None,
            }
        })
    }

    fn try_next_event(&mut self) -> Option<Arc<crate::LiveEvent>> {
        if let Some(ev) = self.replay.next() {
            return Some(ev);
        }
        match self.live.as_mut() {
            Some(live) => live.try_next_event(),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LiveEvent, RealtimeHandle};
    use uuid::Uuid;

    #[tokio::test]
    async fn broadcast_subscriber_roundtrip() {
        let h = RealtimeHandle::start(16);
        let mut sub = BroadcastSubscriber::new(h.subscribe());
        let id = Uuid::new_v4();
        h.publish(LiveEvent::new("test.foo", "x", id));
        let evt = sub.next_event().await.expect("must receive");
        assert_eq!(evt.event, "test.foo");
        assert_eq!(evt.resource_id, id);
    }

    #[tokio::test]
    async fn filtered_subscriber_drops_non_matching() {
        let h = RealtimeHandle::start(16);
        let mut sub =
            FilteredSubscriber::new(Box::new(BroadcastSubscriber::new(h.subscribe())), |e| {
                e.event.starts_with("issue.")
            });
        h.publish(LiveEvent::new("heartbeat.tick", "x", Uuid::new_v4()));
        h.publish(LiveEvent::new("issue.created", "issue", Uuid::new_v4()));
        let evt = sub.next_event().await.expect("filtered event");
        assert_eq!(evt.event, "issue.created");
    }

    #[tokio::test]
    async fn filtered_subscriber_returns_none_when_inner_closed() {
        let h = RealtimeHandle::start(4);
        let rx = h.subscribe();
        let mut sub = FilteredSubscriber::new(Box::new(BroadcastSubscriber::new(rx)), |_| true);
        drop(h);
        assert!(sub.next_event().await.is_none());
    }

    #[tokio::test]
    async fn replay_then_live_drains_replay_first() {
        let h = RealtimeHandle::start(16);
        // 先发 2 条
        h.publish(LiveEvent::new("a", "x", Uuid::new_v4()));
        h.publish(LiveEvent::new("b", "x", Uuid::new_v4()));
        // 模拟客户端以 last_event_id=0 重连：拉取 replay 中全部 2 条
        let replay = h.replay_after(0);
        let mut sub = ReplayThenLiveSubscriber::new(
            replay,
            Box::new(BroadcastSubscriber::new(h.subscribe())),
        );
        let e1 = sub.next_event().await.unwrap();
        let e2 = sub.next_event().await.unwrap();
        assert_eq!(e1.event, "a");
        assert_eq!(e2.event, "b");
        // live 还没发新事件 → try_next_event 返回 None
        assert!(sub.try_next_event().is_none());
    }

    #[tokio::test]
    async fn replay_then_live_serves_published_event() {
        let h = RealtimeHandle::start(16);
        h.publish(LiveEvent::new("a", "x", Uuid::new_v4()));
        let replay = h.replay_after(0);
        let mut sub = ReplayThenLiveSubscriber::new(
            replay,
            Box::new(BroadcastSubscriber::new(h.subscribe())),
        );
        // 先消费 replay
        assert_eq!(sub.next_event().await.unwrap().event, "a");
        // live 阶段还没事件
        assert!(sub.try_next_event().is_none());
        // publish 一条 live 事件，应该被消费到
        h.publish(LiveEvent::new("live1", "x", Uuid::new_v4()));
        // 等一点点时间让 broadcast 传递
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let live = sub.next_event().await.expect("must consume live event");
        assert_eq!(live.event, "live1");
    }

    #[tokio::test]
    async fn try_next_event_returns_empty_when_nothing() {
        let h = RealtimeHandle::start(16);
        let mut sub = BroadcastSubscriber::new(h.subscribe());
        assert!(sub.try_next_event().is_none());
    }
}
