#![forbid(unsafe_code)]

//! Remote execution layer for paperclip-rs.
//!
//! Mirrors Node `server/src/services/workspace-runtime.ts` for:
//! - `restoreRemoteWorkspace` (ssh-bridge restore)
//! - `materializeRemoteClaudeConfig` (remote config + secret materialization)
//!
//! Design:
//! - `SshSession` trait abstracts SSH impl (real `ssh2` later; mock for tests)
//! - All event streams via tokio::sync::mpsc for backpressure
//! - Pure functions for path/Slug/materialize decision logic
//! - Service layer wires trait impl + repo hooks

pub mod materialize;
pub mod restore;
pub mod ssh;
pub mod workspace_handle;

pub use materialize::{
    materialize_remote_claude_config, ClaudeConfigMaterialization, ClaudeConfigSource,
};
pub use restore::{
    classify_restore_error, restore_remote_workspace, RestoreError, RestoreOutcome, RestorePlan,
    RestoreStage, RestoreStageError,
};
pub use ssh::{
    EventStream, RemoteEvent, SshAuth, SshConnection, SshError, SshSession, SshSessionConfig,
};
pub use workspace_handle::RemoteWorkspaceHandle;