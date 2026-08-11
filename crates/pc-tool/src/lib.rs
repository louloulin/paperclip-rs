#![forbid(unsafe_code)]
//! Tool application business service.
pub mod connection;
pub mod profile_binding;
mod service;
pub use pc_repos::tool::{ToolApplicationRow, ToolApplicationStatus, ToolApplicationType};
pub use service::{
    NoopToolHook, RecordingToolHook, ToolApplicationPatch, ToolError, ToolHook,
    ToolHookEvent, ToolService,
};
