#![forbid(unsafe_code)]

//! Routines domain service layer.
//!
//! Provides [`RoutineService`] — a high-level facade over [`pc_repos::routine::RoutineRepo`]
//! that:
//!
//! * Validates inputs (non-empty title, allowed priority, allowed status, ...)
//! * Routes writes through a [`RoutineHook`] chain so callers can layer
//!   activity / realtime / plugin side-effects without touching SQL.
//! * Translates repo `sqlx::Error` into [`pc_errors::Error`] so HTTP / CLI layers
//!   only need to handle one error type.
//!
//! Routines are reusable "playbooks" the agent runtime can fire manually,
//! on a cron schedule, or via a public webhook.

mod service;

pub use service::{
    CreateRoutine, CreateRoutineTrigger, NoopRoutineHook, RecordingRoutineHook, RoutineHook,
    RoutineHookEvent, RoutinePatch, RoutineService, UpdateRoutineTrigger,
};
