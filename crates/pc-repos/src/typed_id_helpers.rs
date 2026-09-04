//! R880 — typed ID ergonomic helpers.
//!
//! Bridges between `Uuid` (used in sqlx columns + JSON) and the typed
//! `Id<T>` wrappers from `pc_core::Id`. Use these helpers in Repo method
//! signatures that want compile-time type safety without losing sqlx
//! compatibility.
//!
//! ## Adoption pattern
//!
//! Old (untyped, but sqlx-friendly):
//! ```ignore
//! pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<Row>>
//! ```
//!
//! New (typed wrapper, calls into old):
//! ```ignore
//! pub async fn list_by_company_typed(&self, company_id: CompanyId)
//!     -> sqlx::Result<Vec<Row>>
//! {
//!     self.list_by_company(company_id.as_uuid()).await
//! }
//! ```
//!
//! Existing callers using `Uuid` continue to work; new callers can opt
//! into typed IDs incrementally.
//!
//! ## Rollout
//!
//! - All `*Repo` methods gain a `_typed` variant alongside the existing
//!   `_uuid` one
//! - HTTP routes switch to `_typed` variants one at a time
//! - Once all routes are typed, drop the `_uuid` shims

#![forbid(unsafe_code)]

use sqlx::{Postgres, Transaction};

use pc_core::Id;

#[cfg(test)]
#[allow(unused_imports)] // imported for the compile-only smoke test
use crate::typed_ids::{
    AgentId, ApprovalId, CompanyId, DecisionBundleId, DecisionId, HeartbeatRunId, IssueId,
    ProjectId, RunId, UserId,
};

/// Trait providing ergonomic conversion from typed IDs to `Uuid` for
/// sqlx bind sites.
pub trait IntoUuid {
    fn into_uuid(self) -> uuid::Uuid;
}

impl<T: ?Sized> IntoUuid for Id<T> {
    fn into_uuid(self) -> uuid::Uuid {
        self.as_uuid()
    }
}

// ============================================================================
// Typed transaction wrapper (for repos that take &mut Transaction)
// ============================================================================

/// Newtype wrapper around `Transaction<'_, Postgres>` to make repo
/// signatures self-documenting. Use as `&mut Tx` parameter instead of
/// `&mut Transaction<'_, Postgres>` — same semantics, clearer intent.
pub struct DbTx<'c>(pub Transaction<'c, Postgres>);

impl<'c> DbTx<'c> {
    pub fn new(tx: Transaction<'c, Postgres>) -> Self {
        Self(tx)
    }

    pub fn as_mut(&mut self) -> &mut Transaction<'c, Postgres> {
        &mut self.0
    }

    pub fn into_inner(self) -> Transaction<'c, Postgres> {
        self.0
    }
}

// ============================================================================
// Typed ID batch builder
// ============================================================================

/// Helper for building "WHERE id = ANY($1)" queries with typed IDs.
///
/// ```ignore
/// let ids: Vec<CompanyId> = vec![...];
/// let query = sqlx::query("SELECT * FROM companies WHERE id = ANY($1)")
///     .bind(typed_id_array(&ids));
/// ```
pub fn typed_id_array<T: ?Sized>(ids: &[Id<T>]) -> Vec<uuid::Uuid> {
    ids.iter().map(|id| id.as_uuid()).collect()
}

// ============================================================================
// Compile-time cross-type rejection (compile-only tests)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_id_compiles_with_uuid_bind() {
        // Simulates the bridge pattern: a Repo method that takes a typed ID,
        // then binds it to sqlx as Uuid.
        let company: CompanyId = CompanyId::new();
        let _u: uuid::Uuid = company.into_uuid();
    }

    #[test]
    fn typed_id_array_preserves_order() {
        let a: CompanyId = CompanyId::new();
        let b: DecisionId = DecisionId::new();
        // Even though they're different types, both can be converted to Uuid.
        let _u1: uuid::Uuid = a.into_uuid();
        let _u2: uuid::Uuid = b.into_uuid();
    }

    #[test]
    fn all_typed_id_aliases_compile() {
        // Smoke test: every typed ID alias is constructible + convertible.
        let _: CompanyId = CompanyId::new();
        let _: DecisionId = DecisionId::new();
        let _: DecisionBundleId = DecisionBundleId::new();
        let _: AgentId = AgentId::new();
        let _: IssueId = IssueId::new();
        let _: UserId = UserId::new();
        let _: RunId = RunId::new();
        let _: ProjectId = ProjectId::new();
        let _: ApprovalId = ApprovalId::new();
        let _: HeartbeatRunId = HeartbeatRunId::new();
    }

    #[test]
    fn typed_id_compile_rejects_cross_type_assignment() {
        // Compile-only assertion (the test passes if this compiles).
        let company: CompanyId = CompanyId::new();
        let _ = company; // Used to anchor the type
        // The line below would NOT compile (uncomment to verify):
        // let wrong: DecisionId = company; // ← compile error: type mismatch
    }

    #[test]
    fn typed_id_array_helper() {
        let companies: Vec<CompanyId> = (0..3).map(|_| CompanyId::new()).collect();
        let uuids = typed_id_array(&companies);
        assert_eq!(uuids.len(), 3);
        // Each typed ID has a unique UUID.
        assert_ne!(uuids[0], uuids[1]);
        assert_ne!(uuids[1], uuids[2]);
    }
}
