use async_trait::async_trait;
use pc_errors::{internal, Error as PcError, Result as PcResult};
use pc_repos::{
    label::{LabelPatch, LabelRepo, LabelRow, NewLabel},
    Db,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum LabelHookEvent {
    Created {
        label_id: Uuid,
        company_id: Uuid,
        name: String,
    },
    Updated {
        label_id: Uuid,
        company_id: Uuid,
    },
    Deleted {
        label_id: Uuid,
        company_id: Uuid,
    },
}
#[async_trait]
pub trait LabelHook: Send + Sync {
    async fn on_label_event(&self, _event: LabelHookEvent) -> PcResult<()> {
        Ok(())
    }
}
pub struct NoopLabelHook;
#[async_trait]
impl LabelHook for NoopLabelHook {}
#[derive(Default)]
pub struct RecordingLabelHook {
    pub events: std::sync::Mutex<Vec<LabelHookEvent>>,
}
impl RecordingLabelHook {
    pub fn events_snapshot(&self) -> Vec<LabelHookEvent> {
        self.events.lock().expect("mutex").clone()
    }
    pub fn clear(&self) {
        self.events.lock().expect("mutex").clear()
    }
}
#[async_trait]
impl LabelHook for RecordingLabelHook {
    async fn on_label_event(&self, e: LabelHookEvent) -> PcResult<()> {
        self.events.lock().expect("mutex").push(e);
        Ok(())
    }
}
#[derive(Debug, thiserror::Error)]
pub enum LabelError {
    #[error("validation: {0}")]
    Validation(String),
    #[error("label name already exists in company")]
    Conflict,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Pc(#[from] PcError),
}
impl From<pc_repos::RepoError> for LabelError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Pc(internal(e.to_string()))
    }
}
pub type LabelResult<T> = std::result::Result<T, LabelError>;
#[derive(Clone)]
pub struct LabelService {
    db: Db,
    hooks: Vec<Arc<dyn LabelHook>>,
}
impl LabelService {
    pub fn new(db: Db) -> Self {
        Self { db, hooks: vec![] }
    }
    pub fn with_hooks(db: Db, hooks: Vec<Arc<dyn LabelHook>>) -> Self {
        Self { db, hooks }
    }
    pub fn add_hook(mut self, h: Arc<dyn LabelHook>) -> Self {
        self.hooks.push(h);
        self
    }
    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }
    fn repo(&self) -> LabelRepo<'_> {
        LabelRepo::new(&self.db)
    }
    async fn dispatch(&self, e: LabelHookEvent) {
        for h in &self.hooks {
            if let Err(err) = h.on_label_event(e.clone()).await {
                tracing::warn!(?err, "label hook failed")
            }
        }
    }
    pub async fn list_by_company(&self, company_id: Uuid) -> LabelResult<Vec<LabelRow>> {
        require(company_id, "companyId")?;
        Ok(self.repo().list_by_company(company_id).await?)
    }
    pub async fn get(&self, id: Uuid) -> LabelResult<Option<LabelRow>> {
        require(id, "labelId")?;
        Ok(self.repo().get_by_id(id).await?)
    }
    pub async fn create(&self, input: NewLabel) -> LabelResult<LabelRow> {
        require(input.company_id, "companyId")?;
        let name = input.name.trim().to_owned();
        if name.is_empty() {
            return Err(LabelError::Validation("name must not be empty".into()));
        }
        if self
            .repo()
            .find_by_name(input.company_id, &name)
            .await?
            .is_some()
        {
            return Err(LabelError::Conflict);
        }
        let row = self
            .repo()
            .create(&NewLabel {
                company_id: input.company_id,
                name,
                color: input.color,
            })
            .await?;
        self.dispatch(LabelHookEvent::Created {
            label_id: row.id,
            company_id: row.company_id,
            name: row.name.clone(),
        })
        .await;
        Ok(row)
    }
    pub async fn patch(&self, id: Uuid, patch: LabelPatch) -> LabelResult<Option<LabelRow>> {
        require(id, "labelId")?;
        if let Some(name) = patch.name.as_deref() {
            if name.trim().is_empty() {
                return Err(LabelError::Validation("name must not be empty".into()));
            }
        }
        let Some(row) = self.repo().patch(id, &patch).await? else {
            return Ok(None);
        };
        self.dispatch(LabelHookEvent::Updated {
            label_id: row.id,
            company_id: row.company_id,
        })
        .await;
        Ok(Some(row))
    }
    pub async fn delete(&self, id: Uuid) -> LabelResult<bool> {
        require(id, "labelId")?;
        let Some(row) = self.repo().get_by_id(id).await? else {
            return Ok(false);
        };
        let deleted = self.repo().delete(id).await?;
        if deleted {
            self.dispatch(LabelHookEvent::Deleted {
                label_id: id,
                company_id: row.company_id,
            })
            .await;
        }
        Ok(deleted)
    }
    pub async fn count(&self, company_id: Uuid) -> LabelResult<i64> {
        require(company_id, "companyId")?;
        Ok(self.repo().count_by_company(company_id).await?)
    }
    pub async fn validate_ids(&self, company_id: Uuid, ids: &[Uuid]) -> LabelResult<()> {
        require(company_id, "companyId")?;
        let found = self.repo().filter_to_company(company_id, ids).await?;
        if found.len()
            != ids
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>()
                .len()
        {
            return Err(LabelError::Validation(
                "one or more labels do not belong to company".into(),
            ));
        }
        Ok(())
    }
}
fn require(id: Uuid, field: &str) -> LabelResult<()> {
    if id.is_nil() {
        Err(LabelError::Validation(format!("{field} is required")))
    } else {
        Ok(())
    }
}
