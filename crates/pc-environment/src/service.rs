use async_trait::async_trait;
use pc_errors::{internal, Error as PcError, Result as PcResult};
use pc_repos::{
    environment::{
        EnvironmentDriver, EnvironmentLeaseRow, EnvironmentRepo, EnvironmentRow, EnvironmentStatus,
        LeasePolicy, NewEnvironment, NewEnvironmentLease,
    },
    Db,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EnvironmentHookEvent {
    Created {
        environment_id: Uuid,
        name: String,
        driver: String,
    },
    StatusChanged {
        environment_id: Uuid,
        status: String,
    },
    EnvVarsMerged {
        environment_id: Uuid,
        keys: Vec<String>,
    },
    Deleted {
        environment_id: Uuid,
    },
    LeaseAcquired {
        lease_id: Uuid,
        environment_id: Uuid,
        company_id: Uuid,
        policy: String,
    },
    LeaseRenewed {
        lease_id: Uuid,
        environment_id: Uuid,
    },
    LeaseReleased {
        lease_id: Uuid,
        environment_id: Uuid,
        reason: Option<String>,
    },
    OverdueExpired {
        count: u64,
    },
}

#[async_trait]
pub trait EnvironmentHook: Send + Sync {
    async fn on_environment_event(&self, _event: EnvironmentHookEvent) -> PcResult<()> {
        Ok(())
    }
}

pub struct NoopEnvironmentHook;
#[async_trait]
impl EnvironmentHook for NoopEnvironmentHook {}

#[derive(Default)]
pub struct RecordingEnvironmentHook {
    pub events: std::sync::Mutex<Vec<EnvironmentHookEvent>>,
}
impl RecordingEnvironmentHook {
    pub fn events_snapshot(&self) -> Vec<EnvironmentHookEvent> {
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
impl EnvironmentHook for RecordingEnvironmentHook {
    async fn on_environment_event(&self, e: EnvironmentHookEvent) -> PcResult<()> {
        self.events.lock().expect("mutex").push(e);
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EnvironmentError {
    #[error("validation: {0}")]
    Validation(String),
    #[error("environment not found: {0}")]
    NotFound(Uuid),
    #[error("env_vars patch must be an object")]
    InvalidPatch,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Pc(#[from] PcError),
}
impl From<pc_repos::RepoError> for EnvironmentError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Pc(internal(e.to_string()))
    }
}
pub type EnvResult<T> = std::result::Result<T, EnvironmentError>;

fn require_non_nil(id: Uuid, field: &str) -> EnvResult<()> {
    if id.is_nil() {
        Err(EnvironmentError::Validation(format!("{field} is required")))
    } else {
        Ok(())
    }
}

#[derive(Clone)]
pub struct EnvironmentService {
    db: Db,
    hooks: Vec<Arc<dyn EnvironmentHook>>,
}

impl EnvironmentService {
    pub fn new(db: Db) -> Self {
        Self { db, hooks: vec![] }
    }
    pub fn with_hooks(db: Db, hooks: Vec<Arc<dyn EnvironmentHook>>) -> Self {
        Self { db, hooks }
    }
    pub fn add_hook(mut self, h: Arc<dyn EnvironmentHook>) -> Self {
        self.hooks.push(h);
        self
    }
    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }
    fn repo(&self) -> EnvironmentRepo<'_> {
        EnvironmentRepo::new(&self.db)
    }
    async fn dispatch(&self, e: EnvironmentHookEvent) {
        for h in &self.hooks {
            if let Err(err) = h.on_environment_event(e.clone()).await {
                tracing::warn!(?err, "environment hook failed");
            }
        }
    }

    // ---- environment reads ----
    pub async fn list_all(&self) -> EnvResult<Vec<EnvironmentRow>> {
        Ok(self.repo().list_all().await?)
    }
    pub async fn get(&self, id: Uuid) -> EnvResult<Option<EnvironmentRow>> {
        require_non_nil(id, "environmentId")?;
        Ok(self.repo().get(id).await?)
    }
    pub async fn get_by_name(&self, name: &str) -> EnvResult<Option<EnvironmentRow>> {
        if name.trim().is_empty() {
            return Err(EnvironmentError::Validation("name is required".into()));
        }
        Ok(self.repo().get_by_name(name).await?)
    }
    pub async fn get_driver(&self, driver: EnvironmentDriver) -> EnvResult<Option<EnvironmentRow>> {
        Ok(self.repo().get_driver(driver).await?)
    }

    // ---- environment writes ----
    pub async fn create(&self, input: NewEnvironment) -> EnvResult<EnvironmentRow> {
        if input.name.trim().is_empty() {
            return Err(EnvironmentError::Validation(
                "name must not be empty".into(),
            ));
        }
        if !input.config.is_object() {
            return Err(EnvironmentError::InvalidPatch);
        }
        if !input.env_vars.is_object() {
            return Err(EnvironmentError::InvalidPatch);
        }
        let row = self.repo().create(&input).await?;
        self.dispatch(EnvironmentHookEvent::Created {
            environment_id: row.id,
            name: row.name.clone(),
            driver: row.driver.clone(),
        })
        .await;
        Ok(row)
    }
    pub async fn update_status(&self, id: Uuid, status: EnvironmentStatus) -> EnvResult<bool> {
        require_non_nil(id, "environmentId")?;
        if matches!(status, EnvironmentStatus::Disabled) {
            if let Some(row) = self.repo().get(id).await? {
                if row.driver == "local" {
                    return Err(EnvironmentError::Validation(
                        "cannot disable the local driver".into(),
                    ));
                }
            }
        }
        let changed = self.repo().update_status(id, status).await?;
        if changed {
            self.dispatch(EnvironmentHookEvent::StatusChanged {
                environment_id: id,
                status: status.as_str().to_string(),
            })
            .await;
        }
        Ok(changed)
    }
    pub async fn merge_env_vars(&self, id: Uuid, patch: Value) -> EnvResult<bool> {
        require_non_nil(id, "environmentId")?;
        if !patch.is_object() {
            return Err(EnvironmentError::InvalidPatch);
        }
        let keys: Vec<String> = patch
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        let changed = self.repo().merge_env_vars(id, &patch).await?;
        if changed {
            self.dispatch(EnvironmentHookEvent::EnvVarsMerged {
                environment_id: id,
                keys,
            })
            .await;
        }
        Ok(changed)
    }
    pub async fn delete(&self, id: Uuid) -> EnvResult<bool> {
        require_non_nil(id, "environmentId")?;
        if let Some(row) = self.repo().get(id).await? {
            if row.driver == "local" {
                return Err(EnvironmentError::Validation(
                    "cannot delete the local driver".into(),
                ));
            }
        }
        let ok = self.repo().delete(id).await?;
        if ok {
            self.dispatch(EnvironmentHookEvent::Deleted { environment_id: id })
                .await;
        }
        Ok(ok)
    }

    // ---- lease reads ----
    pub async fn list_leases_for_company(
        &self,
        company_id: Uuid,
        only_active: bool,
    ) -> EnvResult<Vec<EnvironmentLeaseRow>> {
        require_non_nil(company_id, "companyId")?;
        Ok(self
            .repo()
            .list_leases_for_company(company_id, only_active)
            .await?)
    }
    pub async fn active_lease_for_environment(
        &self,
        environment_id: Uuid,
    ) -> EnvResult<Option<EnvironmentLeaseRow>> {
        require_non_nil(environment_id, "environmentId")?;
        Ok(self
            .repo()
            .active_lease_for_environment(environment_id)
            .await?)
    }

    // ---- lease writes ----
    pub async fn acquire_lease(
        &self,
        input: NewEnvironmentLease,
    ) -> EnvResult<EnvironmentLeaseRow> {
        require_non_nil(input.company_id, "companyId")?;
        require_non_nil(input.environment_id, "environmentId")?;
        if matches!(input.lease_policy, LeasePolicy::Ephemeral) && input.expires_at.is_none() {
            return Err(EnvironmentError::Validation(
                "ephemeral leases must specify expires_at".into(),
            ));
        }
        if let Some(env) = self.repo().get(input.environment_id).await? {
            if env.status != "active" {
                return Err(EnvironmentError::Validation(
                    "environment is not active".into(),
                ));
            }
        }
        let row = self.repo().acquire_lease(&input).await?;
        self.dispatch(EnvironmentHookEvent::LeaseAcquired {
            lease_id: row.id,
            environment_id: row.environment_id,
            company_id: row.company_id,
            policy: row.lease_policy.clone(),
        })
        .await;
        Ok(row)
    }
    pub async fn renew_lease(&self, id: Uuid) -> EnvResult<bool> {
        require_non_nil(id, "leaseId")?;
        let changed = self.repo().renew_lease(id).await?;
        if changed {
            self.dispatch(EnvironmentHookEvent::LeaseRenewed {
                lease_id: id,
                environment_id: Uuid::nil(),
            })
            .await;
        }
        Ok(changed)
    }
    /// R803: 释放 env lease (returns EnvironmentLeaseRow).
    pub async fn release_lease(&self, id: Uuid, reason: Option<&str>) -> EnvResult<EnvironmentLeaseRow> {
        require_non_nil(id, "leaseId")?;
        let row = self.repo().release_lease(id, reason).await?;
        self.dispatch(EnvironmentHookEvent::LeaseReleased {
            lease_id: row.id,
            environment_id: row.environment_id,
            reason: reason.map(|s| s.to_string()),
        })
        .await;
        Ok(row)
    }
    pub async fn expire_overdue(&self) -> EnvResult<u64> {
        let count = self.repo().expire_overdue().await?;
        if count > 0 {
            self.dispatch(EnvironmentHookEvent::OverdueExpired { count })
                .await;
        }
        Ok(count)
    }
}
