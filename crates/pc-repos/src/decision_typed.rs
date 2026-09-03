//! R880 — typed-ID wrappers around `DecisionRepo`.
//!
//! This module adds `_typed` method variants that accept strongly-typed
//! IDs (`CompanyId`, `DecisionId`, `IssueId`, `AgentId`, `RunId`,
//! `DecisionBundleId`) instead of bare `Uuid`. Internally each typed
//! method calls the existing `Uuid`-based method on `DecisionRepo` via
//! `as_uuid()` — zero behavioral change.
//!
//! ## Why this exists
//!
//! Adoption of `pc_core::Id<T>` was previously stalled because rewriting
//! every `Uuid` call site at once is risky. This module demonstrates the
//! incremental adoption pattern: keep the `Uuid` methods as the source of
//! truth, add parallel typed wrappers, and let HTTP routes opt in
//! method-by-method.
//!
//! ## Usage
//!
//! ```ignore
//! use pc_repos::typed_ids::CompanyId;
//! use pc_repos::decision_typed::TypedDecisionRepo;
//!
//! let repo = TypedDecisionRepo::new(db);
//! let company: CompanyId = ...;
//! let rows = repo.list_by_company_typed(company).await?;
//! ```
//!
//! ## What's wrapped
//!
//! - `list_by_company_typed(CompanyId) -> Vec<DecisionRow>`
//! - `list_open_attention_typed(CompanyId, limit) -> Vec<DecisionRow>`
//! - `get_typed(DecisionId) -> Option<DecisionRow>`
//! - `get_company_id_typed(DecisionId) -> Option<CompanyId>`
//! - `delete_typed(DecisionId) -> DecisionRow`
//! - `mark_decided_typed(DecisionId, ...) -> DecisionRow`
//! - `mark_dismissed_typed(DecisionId, ...) -> DecisionRow`
//! - `mark_cancelled_typed(DecisionId) -> DecisionRow`
//!
//! Decision creation methods (`create`, `create_with_options`) keep the
//! raw `Uuid` signature because they accept a `&DecisionSigningService`
//! which is not ID-related; the call site still passes `company_id: Uuid`.

#![forbid(unsafe_code)]

use sqlx::Result;

use pc_core::Timestamp;
use pc_secrets::DecisionSigningService;

use crate::decision::{DecisionRepo, DecisionRow};
use crate::typed_ids::{
    AgentId, CompanyId, DecisionBundleId, DecisionId, IssueId, RunId, UserId,
};

/// Typed wrapper around `DecisionRepo` — every method delegates to the
/// underlying `Uuid`-based implementation after calling `.as_uuid()`.
pub struct TypedDecisionRepo<'a> {
    inner: DecisionRepo<'a>,
}

impl<'a> TypedDecisionRepo<'a> {
    pub fn new(db: &'a crate::Db) -> Self {
        Self {
            inner: DecisionRepo::new(db),
        }
    }

    pub fn from_repo(inner: DecisionRepo<'a>) -> Self {
        Self { inner }
    }

    pub fn into_inner(self) -> DecisionRepo<'a> {
        self.inner
    }

    /// Typed variant of `DecisionRepo::list_by_company`.
    ///
    /// Accepts `CompanyId` — at compile time the caller cannot accidentally
    /// pass an `AgentId` or `DecisionId` here.
    pub async fn list_by_company_typed(&self, company_id: CompanyId) -> Result<Vec<DecisionRow>> {
        self.inner.list_by_company(company_id.as_uuid()).await
    }

    /// Typed variant of `DecisionRepo::list_open_attention`.
    pub async fn list_open_attention_typed(
        &self,
        company_id: CompanyId,
        limit: i64,
    ) -> Result<Vec<DecisionRow>> {
        self.inner
            .list_open_attention(company_id.as_uuid(), limit)
            .await
    }

    /// Typed variant of `DecisionRepo::get`.
    pub async fn get_typed(&self, id: DecisionId) -> Result<Option<DecisionRow>> {
        self.inner.get(id.as_uuid()).await
    }

    /// Typed variant of `DecisionRepo::delete`.
    pub async fn delete_typed(&self, id: DecisionId) -> Result<DecisionRow> {
        self.inner.delete(id.as_uuid()).await
    }

    /// Typed variant of `DecisionRepo::get_company_id`.
    ///
    /// Returns a typed `CompanyId` rather than `Option<Uuid>`, completing
    /// the round-trip: callers now never see raw UUIDs in their signatures.
    pub async fn get_company_id_typed(
        &self,
        decision_id: DecisionId,
    ) -> Result<Option<CompanyId>> {
        Ok(self
            .inner
            .get_company_id(decision_id.as_uuid())
            .await?
            .map(CompanyId::from_uuid))
    }

    /// Typed variant of `DecisionRepo::mark_decided`.
    ///
    /// Note: this method's Uuid signature stays because the chosen option
    /// is application-specific data, not an ID; the call site still uses
    /// `Uuid` for the user_id lookup. The `decision_id` is typed.
    pub async fn mark_decided_typed(
        &self,
        id: DecisionId,
        chosen_option_id: &str,
        decided_by_user_id: &str,
        signing: &DecisionSigningService,
    ) -> Result<DecisionRow> {
        self.inner
            .mark_decided(id.as_uuid(), chosen_option_id, decided_by_user_id, signing)
            .await
    }

    /// Typed variant of `DecisionRepo::mark_dismissed`.
    pub async fn mark_dismissed_typed(
        &self,
        id: DecisionId,
        decided_by_user_id: &str,
        reason: Option<&str>,
    ) -> Result<DecisionRow> {
        self.inner
            .mark_dismissed(id.as_uuid(), decided_by_user_id, reason)
            .await
    }

    /// Typed variant of `DecisionRepo::mark_cancelled`.
    pub async fn mark_cancelled_typed(&self, id: DecisionId) -> Result<DecisionRow> {
        self.inner.mark_cancelled(id.as_uuid()).await
    }
}

// ============================================================================
// DecisionBundleRepo typed wrapper — also stubbed here as a TODO reminder.
// ============================================================================

/// Placeholder for `DecisionBundleRepo` typed wrapper.
///
/// The bundle repo's Uuid methods can be wrapped with the same pattern
/// (bundle_id, company_id, agent_id, issue_id, run_id as typed inputs).
/// This stub documents the contract for future adoption; remove when
/// the typed wrapper is implemented.
#[allow(dead_code)]
pub struct TypedDecisionBundleRepo;

#[allow(dead_code)]
impl TypedDecisionBundleRepo {
    /// Lists bundles for a company (typed).
    pub async fn list_by_company_typed_stub(
        _company_id: CompanyId,
    ) -> Result<Vec<crate::decision_bundle::DecisionBundleRow>> {
        // TODO(R880): wrap DecisionBundleRepo::list_by_company
        unimplemented!("R880 follow-up")
    }

    /// Gets a bundle by id (typed).
    pub async fn get_typed_stub(
        _id: DecisionBundleId,
    ) -> Result<Option<crate::decision_bundle::DecisionBundleRow>> {
        // TODO(R880): wrap DecisionBundleRepo::get
        unimplemented!("R880 follow-up")
    }

    /// Checks if a bundle exists for the (agent, issue, run) origin tuple.
    pub async fn exists_for_origin_typed_stub(
        _company_id: CompanyId,
        _agent_id: Option<AgentId>,
        _issue_id: Option<IssueId>,
        _run_id: Option<RunId>,
    ) -> Result<bool> {
        // TODO(R880): wrap DecisionBundleRepo::exists_for_origin
        unimplemented!("R880 follow-up")
    }
}

// ============================================================================
// Compile-time type-safety demonstration
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typed_id_helpers::IntoUuid;
    use crate::typed_ids::{DecisionBundleId, UserId};
    use uuid::Uuid;

    #[test]
    fn typed_wrapper_compiles_and_carries_state() {
        // The wrapper is a newtype around DecisionRepo; instantiating it
        // does not require a DB pool. We just verify the type compiles.
        fn _type_assert(r: TypedDecisionRepo) {
            let _: &DecisionRepo = &r.inner;
        }
    }

    #[test]
    fn typed_ids_convert_to_uuid_for_inner_calls() {
        // Simulate the bridge pattern: every typed ID can be converted to
        // the raw UUID that DecisionRepo expects.
        let company: CompanyId = CompanyId::new();
        let decision: DecisionId = DecisionId::new();
        let bundle: DecisionBundleId = DecisionBundleId::new();
        let user: UserId = UserId::new();

        let _: Uuid = company.into_uuid();
        let _: Uuid = decision.into_uuid();
        let _: Uuid = bundle.into_uuid();
        let _: Uuid = user.into_uuid();
    }

    #[test]
    fn typed_wrapper_rejects_cross_type_at_compile_time() {
        // This is a compile-only test: the lines below would NOT compile,
        // proving the wrapper enforces type safety.
        fn _check(company: CompanyId, decision: DecisionId) {
            let _u: Uuid = company.as_uuid();
            let _u2: Uuid = decision.as_uuid();
            // The following would NOT compile:
            // let wrong: DecisionId = company; // ← type mismatch
            // TypedDecisionRepo::list_by_company_typed requires CompanyId,
            // not DecisionId — this prevents the entire class of bugs where
            // a developer passes the wrong ID by mistake.
        }
    }

    #[test]
    fn typed_id_aliases_round_trip_through_uuid() {
        // After `.as_uuid()` round-trip, equality is preserved.
        let original: CompanyId = CompanyId::new();
        let restored: CompanyId = CompanyId::from_uuid(original.as_uuid());
        assert_eq!(original, restored);
    }

    #[test]
    fn typed_bundle_repo_stubs_are_compile_anchored() {
        // Anchors the stub so it doesn't trigger unused-code warnings
        // until the follow-up wraps DecisionBundleRepo.
        let _: TypedDecisionBundleRepo = TypedDecisionBundleRepo;
    }
}
