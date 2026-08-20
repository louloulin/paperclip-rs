#![forbid(unsafe_code)]

//! Remote workspace handle — value object representing a restored remote workspace.
//!
//! Mirrors the conceptual "RemoteWorkspaceHandle" returned by Node
//! `workspace-runtime.ts::restoreRemoteWorkspace`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable identifier for a remote workspace (independent of local clone).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct RemoteWorkspaceId(pub Uuid);

impl RemoteWorkspaceId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for RemoteWorkspaceId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for RemoteWorkspaceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Lifecycle state of a remote workspace handle.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RemoteWorkspaceState {
    /// Workspace created; SSH connection not yet established.
    Pending,
    /// Remote workspace discovered and ready for materialization.
    Discovered,
    /// Workspace successfully restored to local cache.
    Restored,
    /// Restoration failed (see error_message).
    Failed,
}

impl RemoteWorkspaceState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Discovered => "discovered",
            Self::Restored => "restored",
            Self::Failed => "failed",
        }
    }
}

/// Handle returned by `restore_remote_workspace`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteWorkspaceHandle {
    pub workspace_id: RemoteWorkspaceId,
    pub remote_host: String,
    pub remote_path: String,
    pub local_cache_path: String,
    pub state: RemoteWorkspaceState,
    pub created_at: DateTime<Utc>,
    pub restored_at: Option<DateTime<Utc>>,
    pub error_message: Option<String>,
}

impl RemoteWorkspaceHandle {
    pub fn new(remote_host: impl Into<String>, remote_path: impl Into<String>) -> Self {
        Self {
            workspace_id: RemoteWorkspaceId::new(),
            remote_host: remote_host.into(),
            remote_path: remote_path.into(),
            local_cache_path: String::new(),
            state: RemoteWorkspaceState::Pending,
            created_at: Utc::now(),
            restored_at: None,
            error_message: None,
        }
    }

    pub fn mark_discovered(&mut self) {
        self.state = RemoteWorkspaceState::Discovered;
    }

    pub fn mark_restored(&mut self, local_cache_path: impl Into<String>) {
        self.local_cache_path = local_cache_path.into();
        self.state = RemoteWorkspaceState::Restored;
        self.restored_at = Some(Utc::now());
        self.error_message = None;
    }

    pub fn mark_failed(&mut self, message: impl Into<String>) {
        self.state = RemoteWorkspaceState::Failed;
        self.error_message = Some(message.into());
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self.state,
            RemoteWorkspaceState::Restored | RemoteWorkspaceState::Failed
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_id_unique() {
        let a = RemoteWorkspaceId::new();
        let b = RemoteWorkspaceId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn handle_new_is_pending() {
        let h = RemoteWorkspaceHandle::new("host", "/path");
        assert_eq!(h.state, RemoteWorkspaceState::Pending);
        assert!(!h.is_terminal());
        assert!(h.error_message.is_none());
    }

    #[test]
    fn handle_discovered_to_restored_transition() {
        let mut h = RemoteWorkspaceHandle::new("host", "/path");
        h.mark_discovered();
        assert_eq!(h.state, RemoteWorkspaceState::Discovered);
        h.mark_restored("/local/path");
        assert_eq!(h.state, RemoteWorkspaceState::Restored);
        assert!(h.is_terminal());
        assert!(h.restored_at.is_some());
        assert!(h.error_message.is_none());
    }

    #[test]
    fn handle_failed_state() {
        let mut h = RemoteWorkspaceHandle::new("host", "/path");
        h.mark_failed("connection refused");
        assert_eq!(h.state, RemoteWorkspaceState::Failed);
        assert!(h.is_terminal());
        assert_eq!(h.error_message.as_deref(), Some("connection refused"));
    }

    #[test]
    fn state_as_str() {
        assert_eq!(RemoteWorkspaceState::Pending.as_str(), "pending");
        assert_eq!(RemoteWorkspaceState::Discovered.as_str(), "discovered");
        assert_eq!(RemoteWorkspaceState::Restored.as_str(), "restored");
        assert_eq!(RemoteWorkspaceState::Failed.as_str(), "failed");
    }
}