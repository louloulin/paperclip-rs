use async_trait::async_trait;
use pc_errors::{internal, Error as PcError, Result as PcResult};
use pc_repos::{
    tool_connection::{ToolConnectionRepo, ToolConnectionRow},
    Db,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ToolConnectionHookEvent {
    Renamed {
        connection_id: Uuid,
        name: String,
    },
    Enabled {
        connection_id: Uuid,
    },
    Disabled {
        connection_id: Uuid,
    },
    StatusChanged {
        connection_id: Uuid,
        status: String,
    },
    ConfigReplaced {
        connection_id: Uuid,
    },
    CredentialsUpdated {
        connection_id: Uuid,
        refs: Value,
    },
    ApplicationReassigned {
        connection_id: Uuid,
        application_id: Uuid,
    },
    HealthChecked {
        connection_id: Uuid,
        status: String,
        message: Option<String>,
    },
    Reconnecting {
        connection_id: Uuid,
    },
    Deleted {
        connection_id: Uuid,
    },
}

#[async_trait]
pub trait ToolConnectionHook: Send + Sync {
    async fn on_tool_connection_event(&self, _event: ToolConnectionHookEvent) -> PcResult<()> {
        Ok(())
    }
}

pub struct NoopToolConnectionHook;
#[async_trait]
impl ToolConnectionHook for NoopToolConnectionHook {}

#[derive(Default)]
pub struct RecordingToolConnectionHook {
    pub events: std::sync::Mutex<Vec<ToolConnectionHookEvent>>,
}
impl RecordingToolConnectionHook {
    pub fn events_snapshot(&self) -> Vec<ToolConnectionHookEvent> {
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
impl ToolConnectionHook for RecordingToolConnectionHook {
    async fn on_tool_connection_event(&self, e: ToolConnectionHookEvent) -> PcResult<()> {
        self.events.lock().expect("mutex").push(e);
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolConnectionError {
    #[error("validation: {0}")]
    Validation(String),
    #[error("connection not found: {0}")]
    NotFound(Uuid),
    #[error("config must be an object")]
    InvalidConfig,
    #[error("credential refs must be an array")]
    InvalidCredentials,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Pc(#[from] PcError),
}
impl From<pc_repos::RepoError> for ToolConnectionError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Pc(internal(e.to_string()))
    }
}
pub type ConnResult<T> = std::result::Result<T, ToolConnectionError>;

fn require_non_nil(id: Uuid, field: &str) -> ConnResult<()> {
    if id.is_nil() {
        Err(ToolConnectionError::Validation(format!(
            "{field} is required"
        )))
    } else {
        Ok(())
    }
}

#[derive(Clone)]
pub struct ToolConnectionService {
    db: Db,
    hooks: Vec<Arc<dyn ToolConnectionHook>>,
}

impl ToolConnectionService {
    pub fn new(db: Db) -> Self {
        Self { db, hooks: vec![] }
    }
    pub fn with_hooks(db: Db, hooks: Vec<Arc<dyn ToolConnectionHook>>) -> Self {
        Self { db, hooks }
    }
    pub fn add_hook(mut self, h: Arc<dyn ToolConnectionHook>) -> Self {
        self.hooks.push(h);
        self
    }
    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }
    fn repo(&self) -> ToolConnectionRepo<'_> {
        ToolConnectionRepo::new(&self.db)
    }
    async fn dispatch(&self, e: ToolConnectionHookEvent) {
        for h in &self.hooks {
            if let Err(err) = h.on_tool_connection_event(e.clone()).await {
                tracing::warn!(?err, "tool connection hook failed");
            }
        }
    }

    // ---- reads ----
    pub async fn get(&self, id: Uuid) -> ConnResult<Option<ToolConnectionRow>> {
        require_non_nil(id, "connectionId")?;
        Ok(self.repo().find_by_id(id).await?)
    }
    pub async fn require(&self, id: Uuid) -> ConnResult<ToolConnectionRow> {
        self.get(id).await?.ok_or(ToolConnectionError::NotFound(id))
    }

    // ---- writes ----
    pub async fn rename(&self, id: Uuid, name: &str) -> ConnResult<bool> {
        require_non_nil(id, "connectionId")?;
        if name.trim().is_empty() {
            return Err(ToolConnectionError::Validation(
                "name must not be empty".into(),
            ));
        }
        let n = self.repo().update_name(id, name).await?;
        if n > 0 {
            self.dispatch(ToolConnectionHookEvent::Renamed {
                connection_id: id,
                name: name.to_string(),
            })
            .await;
        }
        Ok(n > 0)
    }
    pub async fn enable(&self, id: Uuid) -> ConnResult<bool> {
        require_non_nil(id, "connectionId")?;
        let n = self.repo().update_enabled(id, true).await?;
        if n > 0 {
            self.dispatch(ToolConnectionHookEvent::Enabled { connection_id: id })
                .await;
        }
        Ok(n > 0)
    }
    pub async fn disable(&self, id: Uuid) -> ConnResult<bool> {
        require_non_nil(id, "connectionId")?;
        let n = self.repo().update_enabled(id, false).await?;
        if n > 0 {
            self.dispatch(ToolConnectionHookEvent::Disabled { connection_id: id })
                .await;
        }
        Ok(n > 0)
    }
    pub async fn set_status(&self, id: Uuid, status: &str) -> ConnResult<bool> {
        require_non_nil(id, "connectionId")?;
        if status.trim().is_empty() {
            return Err(ToolConnectionError::Validation(
                "status must not be empty".into(),
            ));
        }
        let n = self.repo().update_status(id, status).await?;
        if n > 0 {
            self.dispatch(ToolConnectionHookEvent::StatusChanged {
                connection_id: id,
                status: status.to_string(),
            })
            .await;
        }
        Ok(n > 0)
    }
    pub async fn replace_config(&self, id: Uuid, config: Value) -> ConnResult<bool> {
        require_non_nil(id, "connectionId")?;
        if !config.is_object() {
            return Err(ToolConnectionError::InvalidConfig);
        }
        let n = self.repo().update_config(id, &config).await?;
        if n > 0 {
            self.dispatch(ToolConnectionHookEvent::ConfigReplaced { connection_id: id })
                .await;
        }
        Ok(n > 0)
    }
    pub async fn update_credentials(&self, id: Uuid, refs: Value) -> ConnResult<bool> {
        require_non_nil(id, "connectionId")?;
        if !refs.is_array() {
            return Err(ToolConnectionError::InvalidCredentials);
        }
        let n = self.repo().update_credential_refs(id, &refs).await?;
        if n > 0 {
            self.dispatch(ToolConnectionHookEvent::CredentialsUpdated {
                connection_id: id,
                refs,
            })
            .await;
        }
        Ok(n > 0)
    }
    pub async fn reassign_application(&self, id: Uuid, application_id: Uuid) -> ConnResult<bool> {
        require_non_nil(id, "connectionId")?;
        require_non_nil(application_id, "applicationId")?;
        let n = self
            .repo()
            .update_application_id(id, application_id)
            .await?;
        if n > 0 {
            self.dispatch(ToolConnectionHookEvent::ApplicationReassigned {
                connection_id: id,
                application_id,
            })
            .await;
        }
        Ok(n > 0)
    }
    pub async fn record_health(
        &self,
        id: Uuid,
        status: &str,
        message: Option<&str>,
    ) -> ConnResult<bool> {
        require_non_nil(id, "connectionId")?;
        if status.trim().is_empty() {
            return Err(ToolConnectionError::Validation(
                "status must not be empty".into(),
            ));
        }
        let n = self.repo().update_health_check(id, status, message).await?;
        if n > 0 {
            self.dispatch(ToolConnectionHookEvent::HealthChecked {
                connection_id: id,
                status: status.to_string(),
                message: message.map(|s| s.to_string()),
            })
            .await;
        }
        Ok(n > 0)
    }
    pub async fn mark_reconnecting(&self, id: Uuid) -> ConnResult<bool> {
        require_non_nil(id, "connectionId")?;
        let n = self.repo().update_status_to_reconnecting(id).await?;
        if n > 0 {
            self.dispatch(ToolConnectionHookEvent::Reconnecting { connection_id: id })
                .await;
        }
        Ok(n > 0)
    }
    pub async fn delete(&self, id: Uuid) -> ConnResult<bool> {
        require_non_nil(id, "connectionId")?;
        let n = self.repo().delete_by_id(id).await?;
        if n > 0 {
            self.dispatch(ToolConnectionHookEvent::Deleted { connection_id: id })
                .await;
        }
        Ok(n > 0)
    }
}
