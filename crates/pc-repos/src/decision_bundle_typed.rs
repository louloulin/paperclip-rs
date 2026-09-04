//! R880 wave 2 — typed-ID wrappers around `DecisionBundleRepo`.
//!
//! Mirrors `decision_typed.rs` but for the bundle repository. The
//! original `Uuid`-based `DecisionBundleRepo` is preserved as the
//! source of truth; this module adds typed wrappers that:
//!
//! 1. Convert typed IDs to UUIDs internally (`.as_uuid()`)
//! 2. Return typed IDs where appropriate
//! 3. Reject cross-type assignment at compile time
//!
//! ## Adoption
//!
//! Existing `Uuid` callers keep working. New code can opt in to the
//! typed variants. When all call sites have migrated, the typed
//! wrappers become the canonical API and the `Uuid` originals can
//! be deprecated.
//!
//! ## Error type note
//!
//! `DecisionBundleRepo::create` returns `Result<T, DecisionBundleError>`
//! (a domain-specific error), while the read methods return
//! `RepoResult<T>` (`Result<T, RepoError>`). The typed wrappers
//! preserve both signatures faithfully.

#![forbid(unsafe_code)]

use crate::decision_bundle::{
    DecisionBundleDetail, DecisionBundleError, DecisionBundleFilter, DecisionBundleRepo,
    DecisionBundleRow, NewDecisionBundle,
};
use crate::typed_ids::{AgentId, CompanyId, DecisionBundleId, IssueId, RunId};
use crate::RepoResult;

/// Typed input for creating a decision bundle.
///
/// Same fields as `NewDecisionBundle` but with typed IDs. Convert via
/// `From<NewDecisionBundleTyped>` for backward compatibility, or use
/// directly in new code.
#[derive(Debug, Clone)]
pub struct NewDecisionBundleTyped {
    pub title: String,
    pub summary: Option<String>,
    pub origin_agent_id: AgentId,
    pub origin_issue_id: IssueId,
    pub origin_run_id: RunId,
}

impl NewDecisionBundleTyped {
    /// Convert to the underlying `Uuid`-based DTO for sqlx binding.
    pub fn into_uuid_input(self) -> NewDecisionBundle {
        NewDecisionBundle {
            title: self.title,
            summary: self.summary,
            origin_agent_id: self.origin_agent_id.as_uuid(),
            origin_issue_id: self.origin_issue_id.as_uuid(),
            origin_run_id: self.origin_run_id.as_uuid(),
        }
    }
}

impl From<NewDecisionBundleTyped> for NewDecisionBundle {
    fn from(t: NewDecisionBundleTyped) -> Self {
        t.into_uuid_input()
    }
}

/// Typed variant of `DecisionBundleFilter`.
#[derive(Debug, Clone, Default)]
pub struct DecisionBundleFilterTyped {
    pub agent_id: Option<AgentId>,
    pub issue_id: Option<IssueId>,
    pub run_id: Option<RunId>,
    pub limit: Option<i64>,
}

impl DecisionBundleFilterTyped {
    pub fn into_uuid_filter(self) -> DecisionBundleFilter {
        DecisionBundleFilter {
            agent_id: self.agent_id.map(|a| a.as_uuid()),
            issue_id: self.issue_id.map(|i| i.as_uuid()),
            run_id: self.run_id.map(|r| r.as_uuid()),
            limit: self.limit,
        }
    }
}

/// Typed wrapper around `DecisionBundleRepo`.
pub struct TypedDecisionBundleRepo<'a> {
    inner: DecisionBundleRepo<'a>,
}

impl<'a> TypedDecisionBundleRepo<'a> {
    pub fn new(db: &'a crate::Db) -> Self {
        Self {
            inner: DecisionBundleRepo::new(db),
        }
    }

    pub fn from_repo(inner: DecisionBundleRepo<'a>) -> Self {
        Self { inner }
    }

    pub fn into_inner(self) -> DecisionBundleRepo<'a> {
        self.inner
    }

    /// Typed variant of `DecisionBundleRepo::create`.
    ///
    /// Returns `DecisionBundleError` (domain-specific), matching the
    /// underlying `create` method.
    pub async fn create_typed(
        &self,
        company_id: CompanyId,
        input: NewDecisionBundleTyped,
    ) -> Result<DecisionBundleRow, DecisionBundleError> {
        self.inner
            .create(company_id.as_uuid(), input.into_uuid_input())
            .await
    }

    /// Typed variant of `DecisionBundleRepo::list_by_company`.
    pub async fn list_by_company_typed(
        &self,
        company_id: CompanyId,
        filter: DecisionBundleFilterTyped,
    ) -> RepoResult<Vec<DecisionBundleRow>> {
        self.inner
            .list_by_company(company_id.as_uuid(), &filter.into_uuid_filter())
            .await
    }

    /// Typed variant of `DecisionBundleRepo::get`.
    pub async fn get_typed(
        &self,
        id: DecisionBundleId,
    ) -> RepoResult<Option<DecisionBundleRow>> {
        self.inner.get(id.as_uuid()).await
    }

    /// Typed variant of `DecisionBundleRepo::get_with_decisions`.
    pub async fn get_with_decisions_typed(
        &self,
        id: DecisionBundleId,
    ) -> RepoResult<Option<DecisionBundleDetail>> {
        self.inner.get_with_decisions(id.as_uuid()).await
    }

    /// Typed variant of `DecisionBundleRepo::exists_for_origin`.
    pub async fn exists_for_origin_typed(
        &self,
        company_id: CompanyId,
        agent_id: AgentId,
        issue_id: IssueId,
        run_id: RunId,
    ) -> RepoResult<bool> {
        self.inner
            .exists_for_origin(
                company_id.as_uuid(),
                agent_id.as_uuid(),
                issue_id.as_uuid(),
                run_id.as_uuid(),
            )
            .await
    }

    /// Typed variant of `DecisionBundleRepo::delete`.
    pub async fn delete_typed(
        &self,
        id: DecisionBundleId,
    ) -> RepoResult<Vec<DecisionBundleRow>> {
        self.inner.delete(id.as_uuid()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_bundle_typed_to_uuid_input() {
        let typed = NewDecisionBundleTyped {
            title: "R880 wave 2 test".to_string(),
            summary: None,
            origin_agent_id: AgentId::new(),
            origin_issue_id: IssueId::new(),
            origin_run_id: RunId::new(),
        };
        let uuid_input: NewDecisionBundle = typed.into();
        assert_eq!(uuid_input.title, "R880 wave 2 test");
        assert!(uuid_input.summary.is_none());
    }

    #[test]
    fn filter_typed_to_uuid_filter() {
        let typed = DecisionBundleFilterTyped {
            agent_id: Some(AgentId::new()),
            issue_id: None,
            run_id: Some(RunId::new()),
            limit: Some(50),
        };
        let uuid_filter = typed.into_uuid_filter();
        assert!(uuid_filter.agent_id.is_some());
        assert!(uuid_filter.issue_id.is_none());
        assert!(uuid_filter.run_id.is_some());
        assert_eq!(uuid_filter.limit, Some(50));
    }

    #[test]
    fn typed_wrapper_compile_rejects_cross_type() {
        // Compile-only: would NOT compile if uncommented:
        // let wrong: DecisionBundleId = CompanyId::new();
        let _company: CompanyId = CompanyId::new();
        let _bundle: DecisionBundleId = DecisionBundleId::new();
    }

    #[test]
    fn typed_filter_default_has_no_origin_constraints() {
        let f = DecisionBundleFilterTyped::default();
        assert!(f.agent_id.is_none());
        assert!(f.issue_id.is_none());
        assert!(f.run_id.is_none());
        assert!(f.limit.is_none());
    }

    #[test]
    fn typed_wrapper_compiles_standalone() {
        // Smoke test: wrapper type itself is constructible without DB.
        fn _type_check<'a>(_: TypedDecisionBundleRepo<'a>) {}
    }
}