use async_trait::async_trait;
use pc_errors::{internal, Error as PcError, Result as PcResult};
use pc_repos::{
    tool::{NewToolApplication, PatchToolApplication, ToolApplicationRow, ToolRepo},
    Db,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ToolHookEvent {
    Created {
        company_id: Uuid,
        application_id: Uuid,
        name: String,
        kind: String,
    },
    Patched {
        company_id: Uuid,
        application_id: Uuid,
    },
    StatusChanged {
        company_id: Uuid,
        application_id: Uuid,
        status: String,
    },
    Deleted {
        company_id: Uuid,
        application_id: Uuid,
    },
}

#[async_trait]
pub trait ToolHook: Send + Sync {
    async fn on_tool_event(&self, _event: ToolHookEvent) -> PcResult<()> {
        Ok(())
    }
}

pub struct NoopToolHook;
#[async_trait]
impl ToolHook for NoopToolHook {}

#[derive(Default)]
pub struct RecordingToolHook {
    pub events: std::sync::Mutex<Vec<ToolHookEvent>>,
}
impl RecordingToolHook {
    pub fn events_snapshot(&self) -> Vec<ToolHookEvent> {
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
impl ToolHook for RecordingToolHook {
    async fn on_tool_event(&self, e: ToolHookEvent) -> PcResult<()> {
        self.events.lock().expect("mutex").push(e);
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("validation: {0}")]
    Validation(String),
    #[error("tool application not found: {0}")]
    NotFound(Uuid),
    #[error("tool application with the same name already exists")]
    Conflict,
    #[error("transient: {0}")]
    Transient(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Pc(#[from] PcError),
}
impl From<pc_repos::RepoError> for ToolError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Pc(internal(e.to_string()))
    }
}
pub type ToolResult<T> = std::result::Result<T, ToolError>;

fn require_non_nil(id: Uuid, field: &str) -> ToolResult<()> {
    if id.is_nil() {
        Err(ToolError::Validation(format!("{field} is required")))
    } else {
        Ok(())
    }
}

/// Service-layer patch payload. Maps to the repo `PatchToolApplication`.
#[derive(Debug, Clone, Default)]
pub struct ToolApplicationPatch {
    pub name: Option<String>,
    pub description: Option<String>,
    pub metadata_merge: Option<serde_json::Map<String, Value>>,
    pub status: Option<String>,
}

#[derive(Clone)]
pub struct ToolService {
    db: Db,
    hooks: Vec<Arc<dyn ToolHook>>,
}

impl ToolService {
    pub fn new(db: Db) -> Self {
        Self { db, hooks: vec![] }
    }
    pub fn with_hooks(db: Db, hooks: Vec<Arc<dyn ToolHook>>) -> Self {
        Self { db, hooks }
    }
    pub fn add_hook(mut self, h: Arc<dyn ToolHook>) -> Self {
        self.hooks.push(h);
        self
    }
    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }
    fn repo(&self) -> ToolRepo<'_> {
        ToolRepo::new(&self.db)
    }
    async fn dispatch(&self, e: ToolHookEvent) {
        for h in &self.hooks {
            if let Err(err) = h.on_tool_event(e.clone()).await {
                tracing::warn!(?err, "tool hook failed");
            }
        }
    }

    // ---- reads ----
    pub async fn list_for_company(&self, company_id: Uuid) -> ToolResult<Vec<ToolApplicationRow>> {
        require_non_nil(company_id, "companyId")?;
        Ok(self.repo().list_by_company(company_id).await?)
    }
    pub async fn list_active(&self, company_id: Uuid) -> ToolResult<Vec<ToolApplicationRow>> {
        require_non_nil(company_id, "companyId")?;
        Ok(self.repo().list_active_applications(company_id).await?)
    }
    pub async fn get(&self, company_id: Uuid, id: Uuid) -> ToolResult<Option<ToolApplicationRow>> {
        require_non_nil(company_id, "companyId")?;
        require_non_nil(id, "applicationId")?;
        Ok(self.repo().get(company_id, id).await?)
    }
    pub async fn get_by_name(
        &self,
        company_id: Uuid,
        name: &str,
    ) -> ToolResult<Option<ToolApplicationRow>> {
        require_non_nil(company_id, "companyId")?;
        if name.trim().is_empty() {
            return Err(ToolError::Validation("name is required".into()));
        }
        Ok(self.repo().get_by_name(company_id, name).await?)
    }

    // ---- writes ----
    pub async fn create(
        &self,
        company_id: Uuid,
        name: &str,
        kind: &str,
        description: Option<&str>,
        metadata: Value,
    ) -> ToolResult<ToolApplicationRow> {
        require_non_nil(company_id, "companyId")?;
        if name.trim().is_empty() {
            return Err(ToolError::Validation("name must not be empty".into()));
        }
        if kind.trim().is_empty() {
            return Err(ToolError::Validation("kind must not be empty".into()));
        }
        if !metadata.is_object() {
            return Err(ToolError::Validation("metadata must be an object".into()));
        }
        if self.repo().get_by_name(company_id, name).await?.is_some() {
            return Err(ToolError::Conflict);
        }
        let new_app = NewToolApplication {
            company_id,
            name: name.to_string(),
            kind: kind.to_string(),
            description: description.map(|s| s.to_string()),
            metadata,
        };
        let row = self.repo().create_application(&new_app).await?;
        self.dispatch(ToolHookEvent::Created {
            company_id,
            application_id: row.id,
            name: row.name.clone(),
            kind: row.kind.clone(),
        })
        .await;
        Ok(row)
    }
    pub async fn patch(
        &self,
        company_id: Uuid,
        id: Uuid,
        patch: ToolApplicationPatch,
    ) -> ToolResult<bool> {
        require_non_nil(company_id, "companyId")?;
        require_non_nil(id, "applicationId")?;
        if let Some(name) = patch.name.as_deref() {
            if name.trim().is_empty() {
                return Err(ToolError::Validation("name must not be empty".into()));
            }
        }
        // metadata_merge entries are validated when used
        let patch_obj = PatchToolApplication {
            name: patch.name,
            status: patch.status,
            description: patch.description,
            config: None,
            metadata_merge: patch.metadata_merge.unwrap_or_default(),
        };
        let changed = self
            .repo()
            .patch_application(company_id, id, &patch_obj)
            .await?;
        if changed {
            self.dispatch(ToolHookEvent::Patched {
                company_id,
                application_id: id,
            })
            .await;
        }
        Ok(changed)
    }
    pub async fn set_status(&self, company_id: Uuid, id: Uuid, status: &str) -> ToolResult<bool> {
        require_non_nil(company_id, "companyId")?;
        require_non_nil(id, "applicationId")?;
        if status.trim().is_empty() {
            return Err(ToolError::Validation("status must not be empty".into()));
        }
        let ok = self
            .repo()
            .set_application_status(company_id, id, status)
            .await?;
        if ok {
            self.dispatch(ToolHookEvent::StatusChanged {
                company_id,
                application_id: id,
                status: status.to_string(),
            })
            .await;
        }
        Ok(ok)
    }
    pub async fn delete(&self, company_id: Uuid, id: Uuid) -> ToolResult<bool> {
        require_non_nil(company_id, "companyId")?;
        require_non_nil(id, "applicationId")?;
        let ok = self.repo().delete_application(company_id, id).await?;
        if ok {
            self.dispatch(ToolHookEvent::Deleted {
                company_id,
                application_id: id,
            })
            .await;
        }
        Ok(ok)
    }
}
