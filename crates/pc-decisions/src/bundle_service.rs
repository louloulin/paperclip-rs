use async_trait::async_trait;
use pc_errors::{internal, Error as PcError, Result as PcResult};
use pc_repos::{
    decision_bundle::{
        DecisionBundleDetail, DecisionBundleFilter, DecisionBundleRepo, DecisionBundleRow,
        NewDecisionBundle,
    },
    Db,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DecisionBundleHookEvent {
    Created {
        company_id: Uuid,
        bundle_id: Uuid,
        title: String,
    },
    Deleted {
        bundle_id: Uuid,
    },
}

#[async_trait]
pub trait DecisionBundleHook: Send + Sync {
    async fn on_decision_bundle_event(&self, _event: DecisionBundleHookEvent) -> PcResult<()> {
        Ok(())
    }
}

pub struct NoopDecisionBundleHook;
#[async_trait]
impl DecisionBundleHook for NoopDecisionBundleHook {}

#[derive(Default)]
pub struct RecordingDecisionBundleHook {
    pub events: std::sync::Mutex<Vec<DecisionBundleHookEvent>>,
}
impl RecordingDecisionBundleHook {
    pub fn events_snapshot(&self) -> Vec<DecisionBundleHookEvent> {
        self.events.lock().expect("mutex").clone()
    }
    pub fn clear(&self) {
        self.events.lock().expect("mutex").clear()
    }
    pub fn len(&self) -> usize {
        self.events.lock().expect("mutex").len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
#[async_trait]
impl DecisionBundleHook for RecordingDecisionBundleHook {
    async fn on_decision_bundle_event(&self, e: DecisionBundleHookEvent) -> PcResult<()> {
        self.events.lock().expect("mutex").push(e);
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DecisionBundleError {
    #[error("validation: {0}")]
    Validation(String),
    #[error("not found: {0}")]
    NotFound(Uuid),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Pc(#[from] PcError),
}
impl From<pc_repos::RepoError> for DecisionBundleError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Pc(internal(e.to_string()))
    }
}
pub type DecisionBundleResult<T> = std::result::Result<T, DecisionBundleError>;

fn require_non_nil(id: Uuid, field: &str) -> DecisionBundleResult<()> {
    if id.is_nil() {
        Err(DecisionBundleError::Validation(format!(
            "{field} is required"
        )))
    } else {
        Ok(())
    }
}

#[derive(Clone)]
pub struct DecisionBundleService {
    db: Db,
    hooks: Vec<Arc<dyn DecisionBundleHook>>,
}

impl DecisionBundleService {
    pub fn new(db: Db) -> Self {
        Self { db, hooks: vec![] }
    }
    pub fn with_hooks(db: Db, hooks: Vec<Arc<dyn DecisionBundleHook>>) -> Self {
        Self { db, hooks }
    }
    pub fn add_hook(mut self, h: Arc<dyn DecisionBundleHook>) -> Self {
        self.hooks.push(h);
        self
    }
    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }
    fn repo(&self) -> DecisionBundleRepo<'_> {
        DecisionBundleRepo::new(&self.db)
    }
    async fn dispatch(&self, e: DecisionBundleHookEvent) {
        for h in &self.hooks {
            if let Err(err) = h.on_decision_bundle_event(e.clone()).await {
                tracing::warn!(?err, "decision bundle hook failed");
            }
        }
    }

    pub async fn list_by_company(
        &self,
        company_id: Uuid,
        filter: DecisionBundleFilter,
    ) -> DecisionBundleResult<Vec<DecisionBundleRow>> {
        require_non_nil(company_id, "companyId")?;
        Ok(self.repo().list_by_company(company_id, &filter).await?)
    }
    pub async fn get(&self, id: Uuid) -> DecisionBundleResult<Option<DecisionBundleRow>> {
        require_non_nil(id, "bundleId")?;
        Ok(self.repo().get(id).await?)
    }
    pub async fn get_with_decisions(
        &self,
        id: Uuid,
    ) -> DecisionBundleResult<Option<DecisionBundleDetail>> {
        require_non_nil(id, "bundleId")?;
        Ok(self.repo().get_with_decisions(id).await?)
    }
    pub async fn create(
        &self,
        company_id: Uuid,
        input: NewDecisionBundle,
    ) -> DecisionBundleResult<DecisionBundleRow> {
        require_non_nil(company_id, "companyId")?;
        require_non_nil(input.origin_agent_id, "originAgentId")?;
        require_non_nil(input.origin_issue_id, "originIssueId")?;
        require_non_nil(input.origin_run_id, "originRunId")?;
        if input.title.trim().is_empty() {
            return Err(DecisionBundleError::Validation(
                "title must not be empty".into(),
            ));
        }
        let row = self.repo().create(company_id, input).await
            .map_err(|e| DecisionBundleError::Pc(internal(e.to_string())))?;
        self.dispatch(DecisionBundleHookEvent::Created {
            company_id,
            bundle_id: row.id,
            title: row.title.clone(),
        })
        .await;
        Ok(row)
    }
    pub async fn delete(&self, id: Uuid) -> DecisionBundleResult<bool> {
        require_non_nil(id, "bundleId")?;
        let ok = self.repo().delete(id).await?;
        if ok {
            self.dispatch(DecisionBundleHookEvent::Deleted { bundle_id: id })
                .await;
        }
        Ok(ok)
    }
}
