//! `RealtimeHandle`：`apps/mc-server` 持有的全局 event bus handle。

use tokio::sync::broadcast;

use crate::EventBus;

#[derive(Clone)]
pub struct RealtimeHandle {
    bus: EventBus,
}

impl RealtimeHandle {
    pub fn start(capacity: usize) -> Self {
        Self {
            bus: EventBus::with_capacity(capacity),
        }
    }

    pub fn publish(&self, envelope: crate::envelope::EventEnvelope) {
        self.bus.publish(envelope);
    }

    pub fn subscribe(&self) -> crate::Subscription {
        self.bus.subscribe()
    }

    pub fn receiver_count(&self) -> usize {
        self.bus.receiver_count()
    }

    pub fn raw(&self) -> &EventBus {
        &self.bus
    }

    /// Convert to `broadcast::Receiver` (for WS handler).
    pub fn subscribe_raw(&self) -> broadcast::Receiver<crate::envelope::EventEnvelope> {
        self.bus.subscribe().rx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_starts_with_zero_subscribers() {
        let h = RealtimeHandle::start(16);
        assert_eq!(h.receiver_count(), 0);
    }
}
