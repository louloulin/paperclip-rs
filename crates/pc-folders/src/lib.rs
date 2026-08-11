#![forbid(unsafe_code)]

//! Folder domain service layer.
//!
//! Provides [`FolderService`] — a high-level facade over
//! [`pc_repos::folder::FolderRepo`] / [`pc_repos::folder::counts::CountsQuery`] /
//! [`pc_repos::folder::personal::PersonalFoldersService`] that:
//!
//! * Validates inputs (non-empty name, kebab-case slug, depth ≤ 4, slug
//!   uniqueness per parent, system-managed folders are read-only)
//! * Routes writes through a [`FolderHook`] chain so callers can layer
//!   activity / realtime / plugin side-effects without touching SQL
//! * Translates repo `sqlx::Error` / `RepoError` into [`pc_errors::Error`]
//!   so HTTP / CLI layers only need to handle one error type
//!
//! Folders are nested containers that group `routines` and `company_skills`
//! per company. Two [`pc_repos::folder::FolderKind`]s are supported
//! (`Routine`, `Skill`) and the deepest allowed nesting is 4 levels.

pub mod operation_log_store;
mod service;

pub use service::{
    CreateFolder, FolderHook, FolderHookEvent, FolderPatch, FolderService, NoopFolderHook,
    RecordingFolderHook,
};
