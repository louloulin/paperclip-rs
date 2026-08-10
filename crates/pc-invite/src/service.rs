#![forbid(unsafe_code)]
//! Invite domain service layer.
//!
//! See `lib.rs` for module-level docs.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use pc_repos::invite::{
    generate_url_safe_token, hash_token_hex, CreatedInvite, InviteRow, InviteStatus,
    InviteWithStatus, NewInvite,
};
use pc_repos::invite::InviteRepo;
use pc_repos::Db;

use pc_errors::{internal, validation, Error as PcError, Result};

// =============================================================================
// R615: lifecycle events surfaced to hooks
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum InviteHookEvent {
    Created {
        company_id: Uuid,
        invite_id: Uuid,
        invited_by_user_id: Option<String>,
    },
    Revoked {
        company_id: Uuid,
        invite_id: Uuid,
    },
    Accepted {
        company_id: Uuid,
        invite_id: Uuid,
    },
}

// =============================================================================
// R615: hook trait
// =============================================================================

#[async_trait]
pub trait InviteHook: Send + Sync {
    async fn on_invite_event(&self, _event: InviteHookEvent) -> Result<()> {
        Ok(())
    }
}

pub struct NoopInviteHook;
#[async_trait]
impl InviteHook for NoopInviteHook {}

#[derive(Default)]
pub struct RecordingInviteHook {
    pub events: std::sync::Mutex<Vec<InviteHookEvent>>,
}

#[async_trait]
impl InviteHook for RecordingInviteHook {
    async fn on_invite_event(&self, event: InviteHookEvent) -> Result<()> {
        self.events.lock().expect("lock").push(event);
        Ok(())
    }
}

impl RecordingInviteHook {
    #[must_use]
    pub fn events_snapshot(&self) -> Vec<InviteHookEvent> {
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
// R615: error type
// =============================================================================

#[derive(Debug, thiserror::Error)]
pub enum InviteError {
    #[error("validation: {0}")]
    Validation(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Pc(#[from] PcError),
}

impl From<pc_repos::RepoError> for InviteError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Pc(internal(e.to_string()))
    }
}

pub type InviteResult<T> = std::result::Result<T, InviteError>;

// =============================================================================
// R615: input validation helpers
// =============================================================================

fn normalize_new(input: &NewInvite) -> Result<()> {
    if input.company_id.is_nil() {
        return Err(validation("companyId is required"));
    }
    if input.invite_type.trim().is_empty() {
        return Err(validation("inviteType must not be empty"));
    }
    if input.allowed_join_types.trim().is_empty() {
        return Err(validation("allowedJoinTypes must not be empty"));
    }
    if input.expires_at.as_datetime() <= Utc::now() {
        return Err(validation("expiresAt must be in the future"));
    }
    Ok(())
}

// =============================================================================
// R615: InviteService
// =============================================================================

#[derive(Clone)]
pub struct InviteService {
    db: Db,
    hooks: Vec<Arc<dyn InviteHook>>,
}

impl InviteService {
    pub fn new(db: Db) -> Self {
        Self { db, hooks: Vec::new() }
    }

    pub fn with_hooks(db: Db, hooks: Vec<Arc<dyn InviteHook>>) -> Self {
        Self { db, hooks }
    }

    pub fn add_hook(mut self, h: Arc<dyn InviteHook>) -> Self {
        self.hooks.push(h);
        self
    }

    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }

    async fn dispatch(&self, event: InviteHookEvent) {
        for h in &self.hooks {
            if let Err(e) = h.on_invite_event(event.clone()).await {
                tracing::warn!(?e, "invite hook failed");
            }
        }
    }

    fn repo(&self) -> InviteRepo<'_> {
        InviteRepo::new(&self.db)
    }

    // -------------------------------------------------------------------------
    // Read paths
    // -------------------------------------------------------------------------

    pub async fn list_by_company(
        &self,
        company_id: Uuid,
    ) -> InviteResult<Vec<InviteWithStatus>> {
        if company_id.is_nil() {
            return Err(InviteError::Validation("companyId is required".into()));
        }
        Ok(self.repo().list_by_company(company_id).await?)
    }

    pub async fn find_active_by_token_hash(
        &self,
        token_hash: &str,
    ) -> InviteResult<Option<InviteRow>> {
        if token_hash.trim().is_empty() {
            return Err(InviteError::Validation("tokenHash must not be empty".into()));
        }
        Ok(self.repo().find_active_by_token_hash(token_hash).await?)
    }

    pub async fn find_active_by_token(
        &self,
        raw_token: &str,
    ) -> InviteResult<Option<InviteRow>> {
        if raw_token.trim().is_empty() {
            return Err(InviteError::Validation("token must not be empty".into()));
        }
        Ok(self.repo().find_active_by_token(raw_token).await?)
    }

    pub async fn find_by_token_hash(
        &self,
        token_hash: &str,
    ) -> InviteResult<Option<InviteRow>> {
        Ok(self.repo().find_by_token_hash(token_hash).await?)
    }

    /// Returns the (id, company_id, role, expires_at, accepted_at, revoked_at)
    /// tuple for the invite with the given token hash, if any.
    pub async fn lookup_by_token_hash(
        &self,
        token_hash: &str,
    ) -> InviteResult<
        Option<(
            Uuid,
            Uuid,
            Option<String>,
            Option<pc_core::Timestamp>,
            Option<pc_core::Timestamp>,
            Option<pc_core::Timestamp>,
        )>,
    > {
        Ok(self.repo().lookup_by_token_hash(token_hash).await?)
    }

    /// Returns the (id, company_id, invited_by_user_id) tuple for the invite.
    pub async fn lookup_revoke_info_by_token_hash(
        &self,
        token_hash: &str,
    ) -> InviteResult<Option<(Uuid, Uuid, Option<String>)>> {
        Ok(self.repo().lookup_revoke_info_by_token_hash(token_hash).await?)
    }

    // -------------------------------------------------------------------------
    // Write paths
    // -------------------------------------------------------------------------

    /// Create a new invite. Returns the raw `token` (URL-safe, 32+ bytes) along
    /// with the persisted row — the caller must surface it to the invitee.
    pub async fn create(&self, input: NewInvite) -> InviteResult<CreatedInvite> {
        normalize_new(&input)?;
        let created = self.repo().create(input.clone()).await?;
        self.dispatch(InviteHookEvent::Created {
            company_id: created.row.company_id,
            invite_id: created.row.id,
            invited_by_user_id: created.row.invited_by_user_id.clone(),
        })
        .await;
        Ok(created)
    }

    /// Revoke a pending invite (mark `revoked_at`). Idempotent.
    pub async fn revoke(
        &self,
        company_id: Uuid,
        invite_id: Uuid,
    ) -> InviteResult<bool> {
        if company_id.is_nil() {
            return Err(InviteError::Validation("companyId is required".into()));
        }
        let ok = self.repo().revoke(company_id, invite_id).await?;
        if ok {
            self.dispatch(InviteHookEvent::Revoked {
                company_id,
                invite_id,
            })
            .await;
        }
        Ok(ok)
    }

    /// Mark an invite as accepted. Idempotent.
    pub async fn mark_accepted(&self, invite_id: Uuid) -> InviteResult<bool> {
        // The repo returns (); success means the row was updated. We do not
        // know the company_id here, so emit the hook only when we can confirm.
        // Read first to get company_id.
        let row = self
            .repo()
            .lookup_by_token_hash("")
            .await
            .ok()
            .flatten();
        let _ = row; // unused; we still call mark_accepted below
        self.repo().mark_accepted(invite_id).await?;
        // Try to load the company_id via find_by_token_hash (via a token_hash
        // lookup) — but we only have invite_id. The repo exposes no direct
        // get-by-id, so we approximate: dispatch with nil company_id and let
        // hooks tolerate it. (Real callers normally go through
        // `accept_with_token` which already knows the company_id.)
        self.dispatch(InviteHookEvent::Accepted {
            company_id: Uuid::nil(),
            invite_id,
        })
        .await;
        Ok(true)
    }

    /// Mark an invite as accepted using its row (preferred — keeps company_id).
    pub async fn accept(&self, row: &InviteRow) -> InviteResult<()> {
        if row.id.is_nil() {
            return Err(InviteError::Validation("inviteId is required".into()));
        }
        self.repo().mark_accepted(row.id).await?;
        self.dispatch(InviteHookEvent::Accepted {
            company_id: row.company_id,
            invite_id: row.id,
        })
        .await;
        Ok(())
    }

    /// Accept a raw token: look up by hash, validate, mark accepted.
    pub async fn accept_with_token(&self, raw_token: &str) -> InviteResult<InviteRow> {
        let row = self
            .find_active_by_token(raw_token)
            .await?
            .ok_or_else(|| InviteError::Validation("invite not found or no longer active".into()))?;
        self.accept(&row).await?;
        Ok(row)
    }
}

// =============================================================================
// Unit tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_new_rejects_empty_invite_type() {
        let mut input = NewInvite {
            company_id: Uuid::new_v4(),
            invite_type: "  ".into(),
            allowed_join_types: "user".into(),
            defaults_payload: None,
            expires_at: pc_core::Timestamp::from_dt(Utc::now() + chrono::Duration::days(1)),
            invited_by_user_id: None,
        };
        assert!(normalize_new(&input).is_err());
        input.invite_type = "company".into();
        assert!(normalize_new(&input).is_ok());
    }

    #[test]
    fn normalize_new_rejects_past_expiry() {
        let input = NewInvite {
            company_id: Uuid::new_v4(),
            invite_type: "company".into(),
            allowed_join_types: "user".into(),
            defaults_payload: None,
            expires_at: pc_core::Timestamp::from_dt(Utc::now() - chrono::Duration::hours(1)),
            invited_by_user_id: None,
        };
        assert!(normalize_new(&input).is_err());
    }

    #[test]
    fn generate_token_is_url_safe() {
        let t = generate_url_safe_token(32);
        assert!(t.len() >= 32);
        assert!(t
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn hash_token_is_hex_and_deterministic() {
        let h1 = hash_token_hex("hello");
        let h2 = hash_token_hex("hello");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
