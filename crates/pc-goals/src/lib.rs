#![forbid(unsafe_code)]

//! Goal domain service layer.
//!
//! Provides [`GoalService`] — a high-level facade over [`pc_repos::goal::GoalRepo`]
//! that:
//!
//! * Validates inputs (non-empty title, allowed level, allowed status,
//!   status transitions respect the planned → active → completed | cancelled
//!   state machine, terminal states are sticky)
//! * Routes writes through a [`GoalHook`] chain so callers can layer
//!   activity / realtime / plugin side-effects without touching SQL
//! * Translates repo `sqlx::Error` / `RepoError` into [`pc_errors::Error`]
//!   so HTTP / CLI layers only need to handle one error type
//!
//! Goals form a tree within a company: each goal has a `level`
//! (mission / company / team / project / task), a `status`, and an
//! optional `parent_id` linking to another goal in the same company.

mod service;

pub use service::{
    CreateGoal, GoalHook, GoalHookEvent, GoalPatch, GoalService, NoopGoalHook, RecordingGoalHook,
};
