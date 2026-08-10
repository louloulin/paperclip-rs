#![forbid(unsafe_code)]
//! Inbox domain service layer.
//!
//! See `lib.rs` for module-level docs.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use pc_repos::inbox::{InboxDismissalRow, NewDismissal};
pub use pc_repos::inbox_agent_policy::{
    InboxAgentPolicy, InboxAgentPolicyMode, UpdateInboxAgentPolicyInput,
};
use pc_repos::inbox::InboxRepo;
use pc_repos::inbox_agent_policy::InboxAgentPolicyRepo;
use pc_repos::Db;

use pc_errors::{internal, validation, Error as PcError, Result};

// =============================================================================
// R611: lifecycle events surfaced to hooks
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum InboxHookEvent {
    Dismissed {
        company_id: Uuid,
        user_id: String,
        item_key: String,
    },
    Snoozed {
        company_id: Uuid,
        user_id: String,
        item_key: String,
        snoozed_until: pc_core::Timestamp,
    },
    Restored {
        company_id: Uuid,
        user_id: String,
        item_key: String,
    },
    AgentPolicyUpdated {
        company_id: Uuid,
        user_id: String,
        mode: InboxAgentPolicyMode,
        allowed_count: usize,
    },
}

// =============================================================================
// R611: hook trait
// =============================================================================

#[async_trait]
pub trait InboxHook: Send + Sync {
    async fn on_inbox_event(&self, _event: InboxHookEvent) -> Result<()> {
        Ok(())
    }
}

pub struct NoopInboxHook;
#[async_trait]
impl InboxHook for NoopInboxHook {}

#[derive(Default)]
pub struct RecordingInboxHook {
    pub events: std::sync::Mutex<Vec<InboxHookEvent>>,
}

#[async_trait]
impl InboxHook for RecordingInboxHook {
    async fn on_inbox_event(&self, event: InboxHookEvent) -> Result<()> {
        self.events.lock().expect("lock").push(event);
        Ok(())
    }
}

impl RecordingInboxHook {
    #[must_use]
    pub fn events_snapshot(&self) -> Vec<InboxHookEvent> {
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
// R611: error type
// =============================================================================

#[derive(Debug, thiserror::Error)]
pub enum InboxError {
    #[error("validation: {0}")]
    Validation(String),
    #[error("invalid agents: {0:?}")]
    InvalidAgents(Vec<Uuid>),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Pc(#[from] PcError),
}

impl From<pc_repos::RepoError> for InboxError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Pc(internal(e.to_string()))
    }
}

impl From<pc_repos::inbox_agent_policy::InvalidAgentsError> for InboxError {
    fn from(e: pc_repos::inbox_agent_policy::InvalidAgentsError) -> Self {
        Self::InvalidAgents(e.invalid_agent_ids)
    }
}

pub type InboxResult<T> = std::result::Result<T, InboxError>;

// =============================================================================
// R611: input validation helpers
// =============================================================================

fn validate_company_user_item(company_id: Uuid, user_id: &str, item_key: &str) -> Result<()> {
    if company_id.is_nil() {
        return Err(validation("companyId is required"));
    }
    if user_id.trim().is_empty() {
        return Err(validation("userId must not be empty"));
    }
    if !item_key.is_empty() && item_key.trim().is_empty() {
        return Err(validation("itemKey must not be empty"));
    }
    Ok(())
}

// =============================================================================
// R611: InboxService (dismissals / snoozes / restores)
// =============================================================================

#[derive(Clone)]
pub struct InboxService {
    db: Db,
    hooks: Vec<Arc<dyn InboxHook>>,
}

impl InboxService {
    pub fn new(db: Db) -> Self {
        Self { db, hooks: Vec::new() }
    }

    pub fn with_hooks(db: Db, hooks: Vec<Arc<dyn InboxHook>>) -> Self {
        Self { db, hooks }
    }

    pub fn add_hook(mut self, h: Arc<dyn InboxHook>) -> Self {
        self.hooks.push(h);
        self
    }

    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }

    async fn dispatch(&self, event: InboxHookEvent) {
        for h in &self.hooks {
            if let Err(e) = h.on_inbox_event(event.clone()).await {
                tracing::warn!(?e, "inbox hook failed");
            }
        }
    }

    fn repo(&self) -> InboxRepo<'_> {
        InboxRepo::new(&self.db)
    }

    /// Mark an inbox item as dismissed. Mirrors Node
    /// `inboxDismissalService.dismiss(...)`.
    pub async fn dismiss(
        &self,
        company_id: Uuid,
        user_id: &str,
        item_key: &str,
    ) -> InboxResult<InboxDismissalRow> {
        validate_company_user_item(company_id, user_id, item_key)?;
        let row = self.repo().dismiss(company_id, user_id, item_key).await?;
        self.dispatch(InboxHookEvent::Dismissed {
            company_id,
            user_id: user_id.to_string(),
            item_key: item_key.to_string(),
        })
        .await;
        Ok(row)
    }

    /// Snooze an inbox item until `until` (must be in the future). Mirrors Node
    /// `inboxDismissalService.snooze(...)`.
    pub async fn snooze(
        &self,
        company_id: Uuid,
        user_id: &str,
        item_key: &str,
        until: pc_core::Timestamp,
    ) -> InboxResult<InboxDismissalRow> {
        validate_company_user_item(company_id, user_id, item_key)?;
        if until.as_datetime() <= Utc::now() {
            return Err(InboxError::Validation(
                "snoozed_until must be in the future".into(),
            ));
        }
        let row = self.repo().snooze(company_id, user_id, item_key, until).await?;
        self.dispatch(InboxHookEvent::Snoozed {
            company_id,
            user_id: user_id.to_string(),
            item_key: item_key.to_string(),
            snoozed_until: until,
        })
        .await;
        Ok(row)
    }

    /// Restore an inbox item (delete its dismissal). Mirrors Node
    /// `inboxDismissalService.restore(...)`.
    pub async fn restore(
        &self,
        company_id: Uuid,
        user_id: &str,
        item_key: &str,
    ) -> InboxResult<bool> {
        validate_company_user_item(company_id, user_id, item_key)?;
        let restored = self.repo().restore(company_id, user_id, item_key).await?;
        if restored {
            self.dispatch(InboxHookEvent::Restored {
                company_id,
                user_id: user_id.to_string(),
                item_key: item_key.to_string(),
            })
            .await;
        }
        Ok(restored)
    }

    pub async fn list_for_user(
        &self,
        company_id: Uuid,
        user_id: &str,
    ) -> InboxResult<Vec<InboxDismissalRow>> {
        validate_company_user_item(company_id, user_id, "")?;
        Ok(self.repo().list_for_user(company_id, user_id).await?)
    }

    pub async fn list_active_for_user(
        &self,
        company_id: Uuid,
        user_id: &str,
        now: pc_core::Timestamp,
    ) -> InboxResult<Vec<InboxDismissalRow>> {
        validate_company_user_item(company_id, user_id, "")?;
        Ok(self.repo().list_active_for_user(company_id, user_id, now).await?)
    }

    pub async fn get(
        &self,
        company_id: Uuid,
        user_id: &str,
        item_key: &str,
    ) -> InboxResult<Option<InboxDismissalRow>> {
        validate_company_user_item(company_id, user_id, item_key)?;
        Ok(self.repo().get(company_id, user_id, item_key).await?)
    }

    pub async fn expire_snoozes(&self, now: pc_core::Timestamp) -> InboxResult<u64> {
        Ok(self.repo().expire_snoozes(now).await?)
    }

    pub async fn count_active(
        &self,
        company_id: Uuid,
        now: pc_core::Timestamp,
    ) -> InboxResult<i64> {
        if company_id.is_nil() {
            return Err(InboxError::Validation("companyId is required".into()));
        }
        Ok(self.repo().count_active(company_id, now).await?)
    }
}

// =============================================================================
// R611: InboxAgentPolicyService
// =============================================================================

#[derive(Clone)]
pub struct InboxAgentPolicyService {
    db: Db,
    hooks: Vec<Arc<dyn InboxHook>>,
}

impl InboxAgentPolicyService {
    pub fn new(db: Db) -> Self {
        Self { db, hooks: Vec::new() }
    }

    pub fn with_hooks(db: Db, hooks: Vec<Arc<dyn InboxHook>>) -> Self {
        Self { db, hooks }
    }

    pub fn add_hook(mut self, h: Arc<dyn InboxHook>) -> Self {
        self.hooks.push(h);
        self
    }

    async fn dispatch(&self, event: InboxHookEvent) {
        for h in &self.hooks {
            if let Err(e) = h.on_inbox_event(event.clone()).await {
                tracing::warn!(?e, "inbox hook failed");
            }
        }
    }

    fn repo(&self) -> InboxAgentPolicyRepo<'_> {
        InboxAgentPolicyRepo::new(&self.db)
    }

    pub async fn get(
        &self,
        company_id: Uuid,
        user_id: &str,
    ) -> InboxResult<InboxAgentPolicy> {
        if company_id.is_nil() {
            return Err(InboxError::Validation("companyId is required".into()));
        }
        if user_id.trim().is_empty() {
            return Err(InboxError::Validation("userId must not be empty".into()));
        }
        Ok(self.repo().get(company_id, user_id).await?)
    }

    pub async fn update(
        &self,
        company_id: Uuid,
        user_id: &str,
        input: UpdateInboxAgentPolicyInput,
    ) -> InboxResult<InboxAgentPolicy> {
        if company_id.is_nil() {
            return Err(InboxError::Validation("companyId is required".into()));
        }
        if user_id.trim().is_empty() {
            return Err(InboxError::Validation("userId must not be empty".into()));
        }
        let policy = self.repo().update(company_id, user_id, input).await?;
        self.dispatch(InboxHookEvent::AgentPolicyUpdated {
            company_id,
            user_id: user_id.to_string(),
            mode: policy.mode,
            allowed_count: policy.allowed_agent_ids.len(),
        })
        .await;
        Ok(policy)
    }
}

// =============================================================================
// Unit tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_inputs_rejects_empty_user_id() {
        assert!(validate_company_user_item(Uuid::new_v4(), "", "k").is_err());
    }

    #[test]
    fn validate_inputs_rejects_whitespace_item_key() {
        // empty string is allowed (e.g. list_for_user passes ""); whitespace-only is not
        assert!(validate_company_user_item(Uuid::new_v4(), "u", "   ").is_err());
    }

    #[test]
    fn validate_inputs_rejects_nil_company() {
        assert!(validate_company_user_item(Uuid::nil(), "u", "k").is_err());
    }

    #[test]
    fn policy_mode_roundtrips_lowercase() {
        let m = InboxAgentPolicyMode::Open;
        assert_eq!(m.as_str(), "open");
        assert_eq!(InboxAgentPolicyMode::parse("open"), Some(InboxAgentPolicyMode::Open));
        assert_eq!(InboxAgentPolicyMode::parse("allowlist"), Some(InboxAgentPolicyMode::Allowlist));
        assert_eq!(InboxAgentPolicyMode::parse("disabled"), Some(InboxAgentPolicyMode::Disabled));
        assert_eq!(InboxAgentPolicyMode::parse("nope"), None);
    }
}
