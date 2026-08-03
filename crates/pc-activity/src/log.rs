//! `ActivityLog`: handle that wraps any `ActivitySink`.
//!
//! Provides ergonomic `emit_*` helpers and async `query` API.
//! `InMemoryActivityLog` is the default sink for tests + dry-runs.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::debug;

use crate::kinds::ActivityKind;
use crate::sink::{ActivitySink, SharedActivitySink, SinkResult};
use crate::types::{ActivityActor, ActivityEvent, ActivityFilter, ActivityId};

#[derive(Clone)]
pub struct ActivityLog {
    sink: SharedActivitySink,
}

impl std::fmt::Debug for ActivityLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActivityLog")
            .field("sink", &"<dyn ActivitySink>")
            .finish()
    }
}

impl ActivityLog {
    #[must_use]
    pub fn new(sink: SharedActivitySink) -> Self {
        Self { sink }
    }

    pub async fn emit(&self, event: ActivityEvent) -> SinkResult<ActivityId> {
        let id = event.id;
        self.sink.append(&event).await?;
        debug!(kind = %event.kind.as_str(), "activity emitted");
        Ok(id)
    }

    pub async fn query(&self, filter: ActivityFilter) -> SinkResult<Vec<ActivityEvent>> {
        self.sink.query(&filter).await
    }

    pub async fn emit_quick(
        &self,
        kind: ActivityKind,
        actor: ActivityActor,
        subject_kind: impl Into<String>,
        subject_id: uuid::Uuid,
    ) -> SinkResult<ActivityId> {
        let ev = ActivityEvent::new(kind, actor, subject_kind, subject_id);
        self.emit(ev).await
    }
}

/// In-memory sink: stores events in a Vec<ActivityEvent>.
#[derive(Debug)]
pub struct InMemoryActivityLog {
    inner: Mutex<Vec<ActivityEvent>>,
}

impl InMemoryActivityLog {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().expect("activity sink poisoned").len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Read-only snapshot for assertions in tests.
    #[must_use]
    pub fn snapshot(&self) -> Vec<ActivityEvent> {
        self.inner.lock().expect("activity sink poisoned").clone()
    }
}

impl Default for InMemoryActivityLog {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl ActivitySink for InMemoryActivityLog {
    async fn append(&self, event: &ActivityEvent) -> SinkResult<()> {
        self.inner
            .lock()
            .expect("activity sink poisoned")
            .push(event.clone());
        Ok(())
    }

    async fn query(&self, filter: &ActivityFilter) -> SinkResult<Vec<ActivityEvent>> {
        let all = self.inner.lock().expect("activity sink poisoned");
        let mut out: Vec<ActivityEvent> = all
            .iter()
            .filter(|e| {
                if let Some(c) = filter.company_id {
                    if e.company_id != Some(c) {
                        return false;
                    }
                }
                if let Some(k) = filter.kind {
                    if e.kind != k {
                        return false;
                    }
                }
                if let Some(actor_id) = filter.actor_id {
                    let matches = matches!(&e.actor,
                        ActivityActor::User { id, .. } if *id == actor_id
                    ) || matches!(&e.actor,
                        ActivityActor::Agent { id, .. } if *id == actor_id
                    ) || matches!(&e.actor,
                        ActivityActor::Plugin { plugin_id, .. } if *plugin_id == actor_id
                    );
                    if !matches {
                        return false;
                    }
                }
                if let Some(ref sk) = filter.subject_kind {
                    if &e.subject_kind != sk {
                        return false;
                    }
                }
                if let Some(since) = filter.since {
                    if e.occurred_at < since {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect();
        if let Some(limit) = filter.limit {
            out.truncate(limit);
        }
        Ok(out)
    }
}

/// Async variant that batches writes through an mpsc channel.
#[derive(Debug)]
pub struct ChannelActivityLog {
    tx: mpsc::UnboundedSender<ActivityEvent>,
}

impl ChannelActivityLog {
    #[must_use]
    pub fn new(buffer: usize) -> (Self, mpsc::UnboundedReceiver<ActivityEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let _ = buffer; // buffer reserved for future bounded use
        (Self { tx }, rx)
    }
}

#[async_trait]
impl ActivitySink for ChannelActivityLog {
    async fn append(&self, event: &ActivityEvent) -> SinkResult<()> {
        self.tx
            .send(event.clone())
            .map_err(|e| crate::sink::ActivitySinkError::Unavailable(e.to_string()))?;
        Ok(())
    }
    async fn query(&self, _filter: &ActivityFilter) -> SinkResult<Vec<ActivityEvent>> {
        Ok(Vec::new())
    }
}

/// Factory: build a default `ActivityLog` backed by an in-memory sink.
#[must_use]
pub fn in_memory_log() -> (ActivityLog, Arc<InMemoryActivityLog>) {
    let sink = Arc::new(InMemoryActivityLog::new());
    let log = ActivityLog::new(SharedActivitySink::new(sink.clone()));
    (log, sink)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[tokio::test]
    async fn emit_and_snapshot() {
        let (log, sink) = in_memory_log();
        let _ = log
            .emit_quick(
                ActivityKind::IssueCreated,
                ActivityActor::User {
                    id: Uuid::new_v4(),
                    name: "u".into(),
                },
                "issue",
                Uuid::new_v4(),
            )
            .await
            .unwrap();
        assert_eq!(sink.len(), 1);
        assert!(!sink.is_empty());
    }

    #[tokio::test]
    async fn query_filters_by_kind() {
        let (log, sink) = in_memory_log();
        for _ in 0..3 {
            let _ = log
                .emit_quick(
                    ActivityKind::IssueCreated,
                    ActivityActor::System {
                        component: "test".into(),
                    },
                    "issue",
                    Uuid::new_v4(),
                )
                .await
                .unwrap();
        }
        let _ = log
            .emit_quick(
                ActivityKind::AgentHeartbeat,
                ActivityActor::System {
                    component: "test".into(),
                },
                "agent",
                Uuid::new_v4(),
            )
            .await
            .unwrap();
        assert_eq!(sink.len(), 4);

        let filtered = log
            .query(ActivityFilter {
                kind: Some(ActivityKind::IssueCreated),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(filtered.len(), 3);
    }

    #[tokio::test]
    async fn query_filters_by_company() {
        let (log, _) = in_memory_log();
        let c1 = Uuid::new_v4();
        let c2 = Uuid::new_v4();
        for _ in 0..2 {
            let ev = ActivityEvent::new(
                ActivityKind::DecisionProposed,
                ActivityActor::System {
                    component: "x".into(),
                },
                "decision",
                Uuid::new_v4(),
            )
            .with_company(c1);
            let _ = log.emit(ev).await.unwrap();
        }
        let ev = ActivityEvent::new(
            ActivityKind::DecisionProposed,
            ActivityActor::System {
                component: "x".into(),
            },
            "decision",
            Uuid::new_v4(),
        )
        .with_company(c2);
        let _ = log.emit(ev).await.unwrap();

        let c1_results = log
            .query(ActivityFilter {
                company_id: Some(c1),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(c1_results.len(), 2);
    }

    #[tokio::test]
    async fn channel_sink_routes_events() {
        let (ch_log, mut rx) = ChannelActivityLog::new(64);
        let log = ActivityLog::new(SharedActivitySink::new(Arc::new(ch_log)));
        log.emit_quick(
            ActivityKind::AgentStarted,
            ActivityActor::Agent {
                id: Uuid::new_v4(),
                name: "a".into(),
            },
            "agent",
            Uuid::new_v4(),
        )
        .await
        .unwrap();
        let ev = rx.recv().await.unwrap();
        assert_eq!(ev.kind, ActivityKind::AgentStarted);
    }

    #[tokio::test]
    async fn shared_sink_delegates() {
        let sink = Arc::new(InMemoryActivityLog::new());
        let s1 = SharedActivitySink::new(sink.clone());
        let s2 = s1.clone();
        let ev = ActivityEvent::new(
            ActivityKind::RoutineRan,
            ActivityActor::System {
                component: "t".into(),
            },
            "r",
            Uuid::new_v4(),
        );
        s1.append(&ev).await.unwrap();
        s2.append(&ev).await.unwrap();
        assert_eq!(sink.len(), 2);
    }
}
