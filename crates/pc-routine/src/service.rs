use async_trait::async_trait;
use pc_errors::{internal, Error as PcError, Result as PcResult};
use pc_repos::{routine::{RoutineRepo, RoutineRow}, Db};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone, Default)]
pub struct RoutinePatch {
    pub title: Option<String>,
    pub description: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum RoutineHookEvent {
    Created {
        company_id: Uuid,
        routine_id: Uuid,
        title: String,
    },
    Patched {
        routine_id: Uuid,
    },
    Triggered {
        routine_id: Uuid,
    },
    Deleted {
        routine_id: Uuid,
    },
}

#[async_trait]
pub trait RoutineHook: Send + Sync {
    async fn on_routine_event(&self, _event: RoutineHookEvent) -> PcResult<()> {
        Ok(())
    }
}

pub struct NoopRoutineHook;
#[async_trait]
impl RoutineHook for NoopRoutineHook {}

#[derive(Default)]
pub struct RecordingRoutineHook {
    pub events: std::sync::Mutex<Vec<RoutineHookEvent>>,
}
impl RecordingRoutineHook {
    pub fn events_snapshot(&self) -> Vec<RoutineHookEvent> {
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
impl RoutineHook for RecordingRoutineHook {
    async fn on_routine_event(&self, e: RoutineHookEvent) -> PcResult<()> {
        self.events.lock().expect("mutex").push(e);
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RoutineError {
    #[error("validation: {0}")]
    Validation(String),
    #[error("routine not found: {0}")]
    NotFound(Uuid),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Pc(#[from] PcError),
}
impl From<pc_repos::RepoError> for RoutineError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Pc(internal(e.to_string()))
    }
}
pub type RoutineResult<T> = std::result::Result<T, RoutineError>;

fn require_non_nil(id: Uuid, field: &str) -> RoutineResult<()> {
    if id.is_nil() {
        Err(RoutineError::Validation(format!("{field} is required")))
    } else {
        Ok(())
    }
}

#[derive(Clone)]
pub struct RoutineService {
    db: Db,
    hooks: Vec<Arc<dyn RoutineHook>>,
}

impl RoutineService {
    pub fn new(db: Db) -> Self {
        Self { db, hooks: vec![] }
    }
    pub fn with_hooks(db: Db, hooks: Vec<Arc<dyn RoutineHook>>) -> Self {
        Self { db, hooks }
    }
    pub fn add_hook(mut self, h: Arc<dyn RoutineHook>) -> Self {
        self.hooks.push(h);
        self
    }
    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }
    fn repo(&self) -> RoutineRepo<'_> {
        RoutineRepo::new(&self.db)
    }
    async fn dispatch(&self, e: RoutineHookEvent) {
        for h in &self.hooks {
            if let Err(err) = h.on_routine_event(e.clone()).await {
                tracing::warn!(?err, "routine hook failed");
            }
        }
    }

    // ---- reads ----
    pub async fn list_for_company(&self, company_id: Uuid) -> RoutineResult<Vec<RoutineRow>> {
        require_non_nil(company_id, "companyId")?;
        Ok(self.repo().list_by_company(company_id).await?)
    }
    pub async fn get(&self, id: Uuid) -> RoutineResult<Option<RoutineRow>> {
        require_non_nil(id, "routineId")?;
        Ok(self.repo().get(id).await?)
    }
    pub async fn require(&self, id: Uuid) -> RoutineResult<RoutineRow> {
        self.get(id).await?.ok_or(RoutineError::NotFound(id))
    }

    // ---- writes ----
    pub async fn create(
        &self,
        company_id: Uuid,
        title: &str,
        description: Option<&str>,
        assignee_agent_id: Option<Uuid>,
    ) -> RoutineResult<RoutineRow> {
        require_non_nil(company_id, "companyId")?;
        if title.trim().is_empty() {
            return Err(RoutineError::Validation("title must not be empty".into()));
        }
        if let Some(aid) = assignee_agent_id {
            require_non_nil(aid, "assigneeAgentId")?;
        }
        let row = self
            .repo()
            .create(company_id, title, description, assignee_agent_id)
            .await?;
        self.dispatch(RoutineHookEvent::Created {
            company_id,
            routine_id: row.id,
            title: row.title.clone(),
        })
        .await;
        Ok(row)
    }
    pub async fn patch(&self, id: Uuid, patch: RoutinePatch) -> RoutineResult<Option<RoutineRow>> {
        require_non_nil(id, "routineId")?;
        if let Some(t) = patch.title.as_deref() {
            if t.trim().is_empty() {
                return Err(RoutineError::Validation("title must not be empty".into()));
            }
        }
        if let Some(s) = patch.status.as_deref() {
            if !matches!(s, "active" | "paused" | "archived" | "draft") {
                return Err(RoutineError::Validation(format!("unsupported status {s}")));
            }
        }
        let row = self
            .repo()
            .update(
                id,
                patch.title.as_deref(),
                patch.description.as_deref(),
                patch.status.as_deref(),
            )
            .await?;
        if let Some(r) = &row {
            self.dispatch(RoutineHookEvent::Patched { routine_id: r.id })
                .await;
        }
        Ok(row)
    }
    pub async fn trigger(&self, id: Uuid) -> RoutineResult<Option<RoutineRow>> {
        require_non_nil(id, "routineId")?;
        let row = self.repo().trigger(id).await?;
        if let Some(r) = &row {
            self.dispatch(RoutineHookEvent::Triggered { routine_id: r.id })
                .await;
        }
        Ok(row)
    }
    pub async fn delete(&self, id: Uuid) -> RoutineResult<bool> {
        require_non_nil(id, "routineId")?;
        let ok = self.repo().delete(id).await?;
        if ok {
            self.dispatch(RoutineHookEvent::Deleted { routine_id: id })
                .await;
        }
        Ok(ok)
    }
}
