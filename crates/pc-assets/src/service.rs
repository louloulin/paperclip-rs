#![forbid(unsafe_code)]
//! Asset domain service layer.
//!
//! See `lib.rs` for module-level docs.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use pc_repos::asset::{AssetRow, CreateAssetRecord};
use pc_repos::asset::AssetRepo;
use pc_repos::Db;

use pc_errors::{internal, validation, Error as PcError, Result};

// =============================================================================
// R610: lifecycle events surfaced to hooks
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AssetHookEvent {
    Created {
        company_id: Uuid,
        asset_id: Uuid,
        provider: String,
        content_type: String,
        byte_size: i32,
    },
    Deleted {
        company_id: Uuid,
        asset_id: Uuid,
    },
}

// =============================================================================
// R610: hook trait
// =============================================================================

#[async_trait]
pub trait AssetHook: Send + Sync {
    async fn on_asset_event(&self, _event: AssetHookEvent) -> Result<()> {
        Ok(())
    }
}

pub struct NoopAssetHook;
#[async_trait]
impl AssetHook for NoopAssetHook {}

#[derive(Default)]
pub struct RecordingAssetHook {
    pub events: std::sync::Mutex<Vec<AssetHookEvent>>,
}

#[async_trait]
impl AssetHook for RecordingAssetHook {
    async fn on_asset_event(&self, event: AssetHookEvent) -> Result<()> {
        self.events.lock().expect("lock").push(event);
        Ok(())
    }
}

impl RecordingAssetHook {
    #[must_use]
    pub fn events_snapshot(&self) -> Vec<AssetHookEvent> {
        self.events.lock().expect("lock").clone()
    }

    pub fn clear(&self) {
        self.events.lock().expect("lock").clear();
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.events.lock().expect("lock").len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.lock().expect("lock").is_empty()
    }
}

// =============================================================================
// R610: error type
// =============================================================================

#[derive(Debug, thiserror::Error)]
pub enum AssetError {
    #[error("validation: {0}")]
    Validation(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Pc(#[from] PcError),
}

impl From<pc_repos::RepoError> for AssetError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Pc(internal(e.to_string()))
    }
}

pub type AssetResult<T> = std::result::Result<T, AssetError>;

// =============================================================================
// R610: validation helpers
// =============================================================================

fn normalize_create(company_id: Uuid, record: &CreateAssetRecord) -> Result<()> {
    if company_id.is_nil() {
        return Err(validation("companyId is required"));
    }
    if record.provider.trim().is_empty() {
        return Err(validation("provider must not be empty"));
    }
    if record.object_key.trim().is_empty() {
        return Err(validation("objectKey must not be empty"));
    }
    if record.content_type.trim().is_empty() {
        return Err(validation("contentType must not be empty"));
    }
    if record.sha256.trim().is_empty() {
        return Err(validation("sha256 must not be empty"));
    }
    if record.byte_size < 0 {
        return Err(validation("byteSize must be non-negative"));
    }
    Ok(())
}

// =============================================================================
// R610: AssetService
// =============================================================================

#[derive(Clone)]
pub struct AssetService {
    db: Db,
    hooks: Vec<Arc<dyn AssetHook>>,
}

impl AssetService {
    pub fn new(db: Db) -> Self {
        Self { db, hooks: Vec::new() }
    }

    pub fn with_hooks(db: Db, hooks: Vec<Arc<dyn AssetHook>>) -> Self {
        Self { db, hooks }
    }

    pub fn add_hook(mut self, h: Arc<dyn AssetHook>) -> Self {
        self.hooks.push(h);
        self
    }

    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }

    async fn dispatch(&self, event: AssetHookEvent) {
        for h in &self.hooks {
            if let Err(e) = h.on_asset_event(event.clone()).await {
                tracing::warn!(?e, "asset hook failed");
            }
        }
    }

    fn repo(&self) -> AssetRepo<'_> {
        AssetRepo::new(&self.db)
    }

    pub async fn create(
        &self,
        company_id: Uuid,
        record: CreateAssetRecord,
    ) -> AssetResult<AssetRow> {
        normalize_create(company_id, &record)?;
        let row = self.repo().create(company_id, record).await?;
        self.dispatch(AssetHookEvent::Created {
            company_id,
            asset_id: row.id,
            provider: row.provider.clone(),
            content_type: row.content_type.clone(),
            byte_size: row.byte_size,
        })
        .await;
        Ok(row)
    }

    pub async fn get_by_id(&self, id: Uuid) -> AssetResult<Option<AssetRow>> {
        Ok(self.repo().get_by_id(id).await?)
    }

    pub async fn list_by_company(
        &self,
        company_id: Uuid,
        limit: i64,
    ) -> AssetResult<Vec<AssetRow>> {
        if company_id.is_nil() {
            return Err(AssetError::Validation("companyId is required".into()));
        }
        Ok(self.repo().list_by_company(company_id, limit).await?)
    }

    pub async fn list_by_company_with_provider(
        &self,
        company_id: Uuid,
        provider: Option<&str>,
        limit: i64,
    ) -> AssetResult<Vec<AssetRow>> {
        if company_id.is_nil() {
            return Err(AssetError::Validation("companyId is required".into()));
        }
        Ok(self.repo().list_by_company_with_provider(company_id, provider, limit).await?)
    }

    pub async fn delete_by_id(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> AssetResult<bool> {
        if company_id.is_nil() {
            return Err(AssetError::Validation("companyId is required".into()));
        }
        let deleted = self.repo().delete_by_id(id).await?;
        if deleted {
            self.dispatch(AssetHookEvent::Deleted { company_id, asset_id: id }).await;
        }
        Ok(deleted)
    }

    pub async fn find_logo_meta_by_company(
        &self,
        company_id: Uuid,
    ) -> AssetResult<Option<(String, String, String, i32, Option<String>)>> {
        Ok(self.repo().find_logo_meta_by_company(company_id).await?)
    }

    pub async fn list_attachments_for_asset(
        &self,
        asset_id: Uuid,
    ) -> AssetResult<Vec<(Uuid, Uuid, Option<Uuid>)>> {
        Ok(self.repo().list_attachments_for_asset(asset_id).await?)
    }
}

// =============================================================================
// Unit tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_create_rejects_empty_provider() {
        let mut rec = CreateAssetRecord::new("local", "key", "image/png", 100, "abc");
        rec.provider = "".into();
        assert!(normalize_create(Uuid::new_v4(), &rec).is_err());
        rec.provider = "s3".into();
        assert!(normalize_create(Uuid::new_v4(), &rec).is_ok());
    }

    #[test]
    fn validate_create_rejects_negative_bytes() {
        let mut rec = CreateAssetRecord::new("local", "key", "image/png", 100, "abc");
        rec.byte_size = -1;
        assert!(normalize_create(Uuid::new_v4(), &rec).is_err());
    }

    #[test]
    fn validate_create_rejects_nil_company() {
        let rec = CreateAssetRecord::new("local", "key", "image/png", 100, "abc");
        assert!(normalize_create(Uuid::nil(), &rec).is_err());
    }
}
