#![forbid(unsafe_code)]
//! Company membership domain service layer.
//!
//! See `lib.rs` for module-level docs.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use pc_repos::company_member::CompanyMemberRepo;
pub use pc_repos::company_member::{
    CompanyMemberRow, MemberFilter, MemberPatch, MemberStatus, UserDirectoryEntry,
};
use pc_repos::Db;

use pc_errors::{internal, validation, Error as PcError, Result};

// =============================================================================
// R614: lifecycle events surfaced to hooks
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum CompanyMemberHookEvent {
    Patched {
        company_id: Uuid,
        member_id: Uuid,
        old_role: Option<String>,
        new_role: Option<String>,
        new_status: Option<MemberStatus>,
    },
    Archived {
        company_id: Uuid,
        member_id: Uuid,
    },
}

// =============================================================================
// R614: hook trait
// =============================================================================

#[async_trait]
pub trait CompanyMemberHook: Send + Sync {
    async fn on_company_member_event(&self, _event: CompanyMemberHookEvent) -> Result<()> {
        Ok(())
    }
}

pub struct NoopCompanyMemberHook;
#[async_trait]
impl CompanyMemberHook for NoopCompanyMemberHook {}

#[derive(Default)]
pub struct RecordingCompanyMemberHook {
    pub events: std::sync::Mutex<Vec<CompanyMemberHookEvent>>,
}

#[async_trait]
impl CompanyMemberHook for RecordingCompanyMemberHook {
    async fn on_company_member_event(&self, event: CompanyMemberHookEvent) -> Result<()> {
        self.events.lock().expect("lock").push(event);
        Ok(())
    }
}

impl RecordingCompanyMemberHook {
    #[must_use]
    pub fn events_snapshot(&self) -> Vec<CompanyMemberHookEvent> {
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
// R614: error type
// =============================================================================

#[derive(Debug, thiserror::Error)]
pub enum CompanyMemberError {
    #[error("validation: {0}")]
    Validation(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Pc(#[from] PcError),
}

impl From<pc_repos::RepoError> for CompanyMemberError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Pc(internal(e.to_string()))
    }
}

pub type CompanyMemberResult<T> = std::result::Result<T, CompanyMemberError>;

// =============================================================================
// R614: input validation helpers
// =============================================================================

fn normalize_patch(patch: &MemberPatch) -> Result<()> {
    if let Some(role) = &patch.membership_role {
        if role.trim().is_empty() {
            return Err(validation("membershipRole must not be empty"));
        }
    }
    Ok(())
}

// =============================================================================
// R614: CompanyMemberService
// =============================================================================

#[derive(Clone)]
pub struct CompanyMemberService {
    db: Db,
    hooks: Vec<Arc<dyn CompanyMemberHook>>,
}

impl CompanyMemberService {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            hooks: Vec::new(),
        }
    }

    pub fn with_hooks(db: Db, hooks: Vec<Arc<dyn CompanyMemberHook>>) -> Self {
        Self { db, hooks }
    }

    pub fn add_hook(mut self, h: Arc<dyn CompanyMemberHook>) -> Self {
        self.hooks.push(h);
        self
    }

    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }

    async fn dispatch(&self, event: CompanyMemberHookEvent) {
        for h in &self.hooks {
            if let Err(e) = h.on_company_member_event(event.clone()).await {
                tracing::warn!(?e, "company_member hook failed");
            }
        }
    }

    fn repo(&self) -> CompanyMemberRepo<'_> {
        CompanyMemberRepo::new(&self.db)
    }

    // -------------------------------------------------------------------------
    // Read paths
    // -------------------------------------------------------------------------

    pub async fn list_by_company(
        &self,
        company_id: Uuid,
        filter: MemberFilter<'_>,
    ) -> CompanyMemberResult<Vec<CompanyMemberRow>> {
        if company_id.is_nil() {
            return Err(CompanyMemberError::Validation(
                "companyId is required".into(),
            ));
        }
        Ok(self.repo().list_by_company(company_id, filter).await?)
    }

    pub async fn find_by_id(
        &self,
        company_id: Uuid,
        member_id: Uuid,
    ) -> CompanyMemberResult<Option<CompanyMemberRow>> {
        if company_id.is_nil() {
            return Err(CompanyMemberError::Validation(
                "companyId is required".into(),
            ));
        }
        Ok(self.repo().find_by_id(company_id, member_id).await?)
    }

    pub async fn find_by_user(
        &self,
        company_id: Uuid,
        user_id: &str,
    ) -> CompanyMemberResult<Option<CompanyMemberRow>> {
        if company_id.is_nil() {
            return Err(CompanyMemberError::Validation(
                "companyId is required".into(),
            ));
        }
        if user_id.trim().is_empty() {
            return Err(CompanyMemberError::Validation(
                "userId must not be empty".into(),
            ));
        }
        Ok(self.repo().find_by_user(company_id, user_id).await?)
    }

    pub async fn user_directory(
        &self,
        company_id: Uuid,
    ) -> CompanyMemberResult<Vec<UserDirectoryEntry>> {
        if company_id.is_nil() {
            return Err(CompanyMemberError::Validation(
                "companyId is required".into(),
            ));
        }
        Ok(self.repo().user_directory(company_id).await?)
    }

    pub async fn count_active_for_company(&self, company_id: Uuid) -> CompanyMemberResult<i64> {
        if company_id.is_nil() {
            return Err(CompanyMemberError::Validation(
                "companyId is required".into(),
            ));
        }
        Ok(self.repo().count_active_for_company(company_id).await?)
    }

    pub async fn count_for_company(&self, company_id: Uuid) -> CompanyMemberResult<i64> {
        if company_id.is_nil() {
            return Err(CompanyMemberError::Validation(
                "companyId is required".into(),
            ));
        }
        Ok(self.repo().count_for_company(company_id).await?)
    }

    pub async fn has_active_membership(
        &self,
        company_id: Uuid,
        user_id: &str,
    ) -> CompanyMemberResult<bool> {
        if company_id.is_nil() {
            return Err(CompanyMemberError::Validation(
                "companyId is required".into(),
            ));
        }
        if user_id.trim().is_empty() {
            return Err(CompanyMemberError::Validation(
                "userId must not be empty".into(),
            ));
        }
        Ok(self
            .repo()
            .has_active_membership(company_id, user_id)
            .await?)
    }

    pub async fn is_active_member(
        &self,
        user_id: &str,
        company_id: Uuid,
    ) -> CompanyMemberResult<bool> {
        if company_id.is_nil() {
            return Err(CompanyMemberError::Validation(
                "companyId is required".into(),
            ));
        }
        if user_id.trim().is_empty() {
            return Err(CompanyMemberError::Validation(
                "userId must not be empty".into(),
            ));
        }
        Ok(self.repo().is_active_member(user_id, company_id).await?)
    }

    pub async fn list_company_ids_for_user(&self, user_id: &str) -> CompanyMemberResult<Vec<Uuid>> {
        if user_id.trim().is_empty() {
            return Err(CompanyMemberError::Validation(
                "userId must not be empty".into(),
            ));
        }
        Ok(self.repo().list_company_ids_for_user(user_id).await?)
    }

    /// Lists `(company_id, company_name, role, status)` tuples for the user.
    pub async fn list_for_user_with_company(
        &self,
        user_id: &str,
    ) -> CompanyMemberResult<Vec<(Uuid, String, Option<String>, Option<String>)>> {
        if user_id.trim().is_empty() {
            return Err(CompanyMemberError::Validation(
                "userId must not be empty".into(),
            ));
        }
        Ok(self.repo().list_for_user_with_company(user_id).await?)
    }

    /// Returns `(company_id_str, role)` tuples for all active memberships.
    pub async fn list_active_for_principal_user(
        &self,
        principal_id: &str,
    ) -> CompanyMemberResult<Vec<(String, String)>> {
        if principal_id.trim().is_empty() {
            return Err(CompanyMemberError::Validation(
                "principalId must not be empty".into(),
            ));
        }
        Ok(self
            .repo()
            .list_active_for_principal_user(principal_id)
            .await?)
    }

    /// Replace the full company access set for a user (atomic transaction).
    pub async fn replace_user_companies(
        &self,
        user_id: &str,
        company_ids: &[Uuid],
    ) -> CompanyMemberResult<()> {
        if user_id.trim().is_empty() {
            return Err(CompanyMemberError::Validation(
                "userId must not be empty".into(),
            ));
        }
        Ok(self
            .repo()
            .replace_user_companies(user_id, company_ids)
            .await?)
    }

    // -------------------------------------------------------------------------
    // Write paths
    // -------------------------------------------------------------------------

    /// Patch a member's role / status. Emits Patched with old/new role diff
    /// when role changes; emits Archived hook when status transitions to
    /// `archived`.
    pub async fn patch(
        &self,
        company_id: Uuid,
        member_id: Uuid,
        patch: MemberPatch,
    ) -> CompanyMemberResult<Option<CompanyMemberRow>> {
        if company_id.is_nil() {
            return Err(CompanyMemberError::Validation(
                "companyId is required".into(),
            ));
        }
        normalize_patch(&patch)?;

        // Capture old role + status for the hook payload.
        let old_row = self.repo().find_by_id(company_id, member_id).await?;
        let old_role = old_row.as_ref().map(|r| r.membership_role.clone());

        let new_row = self.repo().patch(company_id, member_id, patch).await?;
        if let Some(r) = &new_row {
            self.dispatch(CompanyMemberHookEvent::Patched {
                company_id,
                member_id: r.id,
                old_role,
                new_role: Some(r.membership_role.clone()),
                new_status: MemberStatus::parse(&r.status),
            })
            .await;
        }
        Ok(new_row)
    }

    /// Soft-archive a member (status = 'archived'). Mirrors Node archive.
    pub async fn archive(&self, company_id: Uuid, member_id: Uuid) -> CompanyMemberResult<bool> {
        if company_id.is_nil() {
            return Err(CompanyMemberError::Validation(
                "companyId is required".into(),
            ));
        }
        let ok = self.repo().archive(company_id, member_id).await?;
        if ok {
            self.dispatch(CompanyMemberHookEvent::Archived {
                company_id,
                member_id,
            })
            .await;
        }
        Ok(ok)
    }
}

// =============================================================================
// Unit tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_status_roundtrip() {
        assert_eq!(MemberStatus::Active.as_str(), "active");
        assert_eq!(MemberStatus::Archived.as_str(), "archived");
        assert_eq!(MemberStatus::parse("active"), Some(MemberStatus::Active));
        assert_eq!(
            MemberStatus::parse("archived"),
            Some(MemberStatus::Archived)
        );
        assert_eq!(MemberStatus::parse("nope"), None);
    }

    #[test]
    fn normalize_patch_rejects_empty_role() {
        let patch = MemberPatch {
            membership_role: Some("".into()),
            ..Default::default()
        };
        assert!(normalize_patch(&patch).is_err());
    }

    #[test]
    fn normalize_patch_accepts_status_only() {
        let patch = MemberPatch {
            membership_role: None,
            status: Some(MemberStatus::Archived),
        };
        assert!(normalize_patch(&patch).is_ok());
    }
}
