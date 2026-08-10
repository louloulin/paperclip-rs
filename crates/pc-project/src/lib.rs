#![forbid(unsafe_code)]

//! Project domain service layer.
//!
//! Provides [`ProjectService`] — a high-level facade over
//! [`pc_repos::project::ProjectRepo`] that:
//!
//! * Validates inputs (non-nil company, non-empty name, allowed status)
//! * Enforces the project state machine
//!   `backlog → planned → active ⇄ paused → completed | archived`
//! * Routes writes through a [`ProjectHook`] chain so callers can layer
//!   activity / realtime / membership side-effects without touching SQL
//! * Translates repo `sqlx::Error` / `RepoError` into [`pc_errors::Error`]
//!
//! A project is the top-level workflow container for a company, owning
//! zero-or-more workspaces (code repos), goal bindings, and user
//! memberships.

mod service;

pub use service::{
    MembershipState, NewProject, ProjectHook, ProjectHookEvent, ProjectMembershipRow,
    ProjectPatch, ProjectRow, ProjectService, ProjectStatus, ProjectWorkspaceRow,
    NoopProjectHook, RecordingProjectHook,
};
