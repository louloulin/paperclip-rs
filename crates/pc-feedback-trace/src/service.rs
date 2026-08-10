use async_trait::async_trait;
use pc_errors::{internal, Error as PcError, Result as PcResult};
pub use pc_repos::feedback_trace::FeedbackTraceRow;
use pc_repos::{feedback_trace::FeedbackTraceRepo, Db};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FeedbackTraceHookEvent {
    Deleted { trace_id: Uuid, issue_id: Uuid },
}
#[async_trait]
pub trait FeedbackTraceHook: Send + Sync {
    async fn on_feedback_trace_event(&self, _event: FeedbackTraceHookEvent) -> PcResult<()> {
        Ok(())
    }
}
pub struct NoopFeedbackTraceHook;
#[async_trait]
impl FeedbackTraceHook for NoopFeedbackTraceHook {}
#[derive(Default)]
pub struct RecordingFeedbackTraceHook {
    pub events: std::sync::Mutex<Vec<FeedbackTraceHookEvent>>,
}
impl RecordingFeedbackTraceHook {
    pub fn events_snapshot(&self) -> Vec<FeedbackTraceHookEvent> {
        self.events.lock().expect("mutex").clone()
    }
    pub fn clear(&self) {
        self.events.lock().expect("mutex").clear()
    }
}
#[derive(Debug, thiserror::Error)]
pub enum FeedbackTraceError {
    #[error("validation: {0}")]
    Validation(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Pc(#[from] PcError),
}
impl From<pc_repos::RepoError> for FeedbackTraceError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Pc(internal(e.to_string()))
    }
}
pub type TraceResult<T> = std::result::Result<T, FeedbackTraceError>;
#[async_trait]
impl FeedbackTraceHook for RecordingFeedbackTraceHook {
    async fn on_feedback_trace_event(&self, e: FeedbackTraceHookEvent) -> PcResult<()> {
        self.events.lock().expect("mutex").push(e);
        Ok(())
    }
}
#[derive(Clone)]
pub struct FeedbackTraceService {
    db: Db,
    hooks: Vec<Arc<dyn FeedbackTraceHook>>,
}
impl FeedbackTraceService {
    pub fn new(db: Db) -> Self {
        Self { db, hooks: vec![] }
    }
    pub fn with_hooks(db: Db, hooks: Vec<Arc<dyn FeedbackTraceHook>>) -> Self {
        Self { db, hooks }
    }
    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }
    fn repo(&self) -> FeedbackTraceRepo<'_> {
        FeedbackTraceRepo::new(&self.db)
    }
    async fn dispatch(&self, e: FeedbackTraceHookEvent) {
        for h in &self.hooks {
            if let Err(err) = h.on_feedback_trace_event(e.clone()).await {
                tracing::warn!(?err, "feedback trace hook failed")
            }
        }
    }
    pub async fn list_by_issue(
        &self,
        issue_id: Uuid,
        limit: i64,
    ) -> TraceResult<Vec<FeedbackTraceRow>> {
        if issue_id.is_nil() {
            return Err(FeedbackTraceError::Validation("issueId is required".into()));
        }
        Ok(self.repo().list_by_issue(issue_id, limit.max(0)).await?)
    }
    pub async fn list_for_company(
        &self,
        company_id: Uuid,
        limit: i64,
    ) -> TraceResult<Vec<FeedbackTraceRow>> {
        if company_id.is_nil() {
            return Err(FeedbackTraceError::Validation(
                "companyId is required".into(),
            ));
        }
        Ok(self
            .repo()
            .list_for_company(company_id, limit.max(0))
            .await?)
    }
    pub async fn get_full(
        &self,
        id: Uuid,
    ) -> TraceResult<Option<(Uuid, String, Option<serde_json::Value>, pc_core::Timestamp)>> {
        if id.is_nil() {
            return Err(FeedbackTraceError::Validation("traceId is required".into()));
        }
        Ok(self.repo().get_by_id_full(id).await?)
    }
    pub async fn get_bundle(
        &self,
        id: Uuid,
    ) -> TraceResult<Option<(Uuid, Option<serde_json::Value>)>> {
        if id.is_nil() {
            return Err(FeedbackTraceError::Validation("traceId is required".into()));
        }
        Ok(self.repo().get_bundle(id).await?)
    }
    pub async fn delete(&self, id: Uuid) -> TraceResult<bool> {
        if id.is_nil() {
            return Err(FeedbackTraceError::Validation("traceId is required".into()));
        }
        let Some((issue_id, _, _, _)) = self.repo().get_by_id_full(id).await? else {
            return Ok(false);
        };
        let deleted = self.repo().delete(id).await?;
        if deleted {
            self.dispatch(FeedbackTraceHookEvent::Deleted {
                trace_id: id,
                issue_id,
            })
            .await;
        }
        Ok(deleted)
    }
}
