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
//! incremental adoption pattern: keep the `Uuid` methods as the source
//! of truth, add parallel typed wrappers, and let HTTP routes opt in
//! method-by-method.
//!
//! ## What's wrapped
//!
//! - `list_by_company_typed(CompanyId) -> Vec<DecisionRow>`
//! - `list_open_attention_typed(CompanyId, limit) -> Vec<DecisionRow>`
//! - `get_typed(DecisionId) -> Option<DecisionRow>`
//! - `get_company_id_typed(DecisionId) -> Option<CompanyId>`
//! - `delete_typed(DecisionId) -> DecisionRow`
//! - `mark_decided_typed(DecisionId, ...) -> bool` (matches underlying)
//! - `mark_dismissed_typed(DecisionId, ...) -> DecisionRow`
//! - `mark_cancelled_typed(DecisionId) -> DecisionRow`

#![forbid(unsafe_code)]

use sqlx::Result;

use crate::decision::{DecisionRepo, DecisionRow};
use crate::typed_ids::{CompanyId, DecisionId};

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
    /// Signature mirrors the underlying Uuid method: returns `bool`
    /// (true = row updated, false = row not found / no-op), with
    /// `decided_by_user_id` and `input_values` both optional.
    pub async fn mark_decided_typed(
        &self,
        id: DecisionId,
        chosen_option_id: &str,
        decided_by_user_id: Option<&str>,
        input_values: Option<&serde_json::Value>,
    ) -> Result<bool> {
        self.inner
            .mark_decided(
                id.as_uuid(),
                chosen_option_id,
                decided_by_user_id,
                input_values,
            )
            .await
    }

    /// Typed variant of `DecisionRepo::mark_dismissed`.
    ///
    /// Signature mirrors underlying: `reason` is required (not Option)
    /// because the underlying SQL always writes the dismissReason column.
    pub async fn mark_dismissed_typed(
        &self,
        id: DecisionId,
        reason: &str,
        decided_by_user_id: &str,
    ) -> Result<DecisionRow> {
        self.inner
            .mark_dismissed(id.as_uuid(), reason, decided_by_user_id)
            .await
    }

    /// Typed variant of `DecisionRepo::mark_cancelled`.
    pub async fn mark_cancelled_typed(&self, id: DecisionId) -> Result<DecisionRow> {
        self.inner.mark_cancelled(id.as_uuid()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapper_compiles_and_accepts_typed_ids() {
        // Smoke: the wrapper types are constructible without DB.
        // (We cannot construct DecisionRepo without a real Db, so this
        // is a type-only check.)
        fn _accepts_company(_: CompanyId) {}
        fn _accepts_decision(_: DecisionId) {}

        let _company: CompanyId = CompanyId::new();
        let _decision: DecisionId = DecisionId::new();

        // Compile-only: would NOT compile if uncommented because
        // CompanyId != DecisionId:
        // _accepts_company(DecisionId::new());
    }
}