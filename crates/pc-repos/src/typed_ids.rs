//! Strongly-typed IDs for the `pc-repos` layer.
//!
//! `pc_core::Id<T>` already provides a generic newtype-wrapper around `Uuid`,
//! but using it inline (e.g. `Id<Decision>`, `Id<Company>`) at every call
//! site is verbose. This module defines the canonical type aliases used
//! throughout `pc-repos` so call sites can write `CompanyId` / `DecisionId`
//! / `AgentId` / `IssueId` / `BundleId` and benefit from compile-time
//! type safety without paying the syntactic cost.
//!
//! ## 为什么重要
//!
//! In the previous codebase, `Uuid` was used everywhere — including as
//! function parameters. This meant `find_by_company(agent_id, ...)` was
//! a valid call at compile time, even though it makes no semantic sense.
//! Rust's type system can prevent this category of bug if we adopt
//! newtype wrappers consistently.

#![forbid(unsafe_code)]

use pc_core::Id;

// Marker types — zero-sized structs that act as type tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CompanyMarker;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DecisionMarker;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DecisionBundleMarker;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AgentMarker;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IssueMarker;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UserMarker;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RunMarker;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProjectMarker;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ApprovalMarker;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HeartbeatRunMarker;

// Canonical type aliases — use these at call sites.
pub type CompanyId = Id<CompanyMarker>;
pub type DecisionId = Id<DecisionMarker>;
pub type DecisionBundleId = Id<DecisionBundleMarker>;
pub type AgentId = Id<AgentMarker>;
pub type IssueId = Id<IssueMarker>;
pub type UserId = Id<UserMarker>;
pub type RunId = Id<RunMarker>;
pub type ProjectId = Id<ProjectMarker>;
pub type ApprovalId = Id<ApprovalMarker>;
pub type HeartbeatRunId = Id<HeartbeatRunMarker>;

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn typed_ids_distinguish_at_compile_time() {
        // At runtime these are just Uuids, but the type system prevents
        // mixing them in a single function call.
        let company: CompanyId = CompanyId::new();
        let decision: DecisionId = DecisionId::new();

        // Cannot write `company_id_method(decision)` because `CompanyId`
        // and `DecisionId` are distinct types — the compiler will reject.
        fn _only_accepts_company(_: CompanyId) {}

        _only_accepts_company(company);
        // _only_accepts_company(decision); // ← would not compile
    }

    #[test]
    fn typed_ids_convert_freely_with_uuid() {
        let company: CompanyId = CompanyId::new();
        let uuid: Uuid = company.as_uuid();
        let restored: CompanyId = uuid.into();
        assert_eq!(company, restored);
    }

    #[test]
    fn typed_ids_serialize_as_uuid_string() {
        let decision: DecisionId = DecisionId::new();
        let s = serde_json::to_string(&decision).unwrap();
        // serde transparent over Uuid → string with hyphens
        assert!(s.starts_with('"') && s.ends_with('"'));
        let inner: String = s.trim_matches('"').to_string();
        let parsed: Uuid = inner.parse().unwrap();
        assert_eq!(parsed, decision.as_uuid());
    }
}
