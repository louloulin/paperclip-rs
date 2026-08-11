#![forbid(unsafe_code)]

//! Company membership domain service layer.
//!
//! Provides [`CompanyMemberService`] — a high-level facade over
//! [`pc_repos::company_member::CompanyMemberRepo`] that:
//!
//! * Validates inputs (non-nil company, non-empty principal_id)
//! * Routes writes through a [`CompanyMemberHook`] chain so callers can
//!   layer activity / realtime / authorization side-effects without
//!   touching SQL
//! * Translates repo `sqlx::Error` / `RepoError` into [`pc_errors::Error`]
//!
//! A company membership is the link between a `user` or `agent` principal
//! and a `company`, carrying `membership_role` (e.g. owner/admin/member/
//! guest) and `status` (active/archived).

pub mod backfill;
pub mod roles;
mod service;
pub mod skill;

pub use roles::{
    grants_for_human_role, normalize_human_role, resolve_human_invite_role, Grant,
    HumanCompanyMembershipRole, HUMAN_COMPANY_MEMBERSHIP_ROLES,
};
pub use service::{
    CompanyMemberHook, CompanyMemberHookEvent, CompanyMemberRow, CompanyMemberService,
    MemberFilter, MemberPatch, MemberStatus, NoopCompanyMemberHook, RecordingCompanyMemberHook,
    UserDirectoryEntry,
};
