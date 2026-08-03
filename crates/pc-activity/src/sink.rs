//! Activity sink trait + shared wrapper.

use async_trait::async_trait;
use thiserror::Error;

use crate::types::{ActivityEvent, ActivityFilter};

#[derive(Debug, Error)]
pub enum ActivitySinkError {
    #[error("activity sink: io {0}")]
    Io(String),
    #[error("activity sink: serialization {0}")]
    Serde(#[from] serde_json::Error),
    #[error("activity sink: backend unavailable {0}")]
    Unavailable(String),
}

pub type SinkResult<T> = Result<T, ActivitySinkError>;

#[async_trait]
pub trait ActivitySink: Send + Sync + std::fmt::Debug {
    async fn append(&self, event: &ActivityEvent) -> SinkResult<()>;
    async fn query(&self, filter: &ActivityFilter) -> SinkResult<Vec<ActivityEvent>>;
}

#[derive(Clone, Debug)]
pub struct SharedActivitySink(pub std::sync::Arc<dyn ActivitySink>);

impl SharedActivitySink {
    pub fn new(sink: std::sync::Arc<dyn ActivitySink>) -> Self {
        Self(sink)
    }
}

#[async_trait]
impl ActivitySink for SharedActivitySink {
    async fn append(&self, event: &ActivityEvent) -> SinkResult<()> {
        self.0.append(event).await
    }
    async fn query(&self, filter: &ActivityFilter) -> SinkResult<Vec<ActivityEvent>> {
        self.0.query(filter).await
    }
}
