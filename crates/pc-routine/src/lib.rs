#![forbid(unsafe_code)]
//! Routine business service.
mod service;
pub use pc_repos::routine::RoutineRow;
pub use service::{
    NoopRoutineHook, RecordingRoutineHook, RoutineError, RoutineHook, RoutineHookEvent,
    RoutinePatch, RoutineService,
};
