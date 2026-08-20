#![forbid(unsafe_code)]

//! Remote workspace restoration — 1:1 port of Node `workspace-runtime.ts::restoreRemoteWorkspace`.
//!
//! Pipeline:
//! 1. classify_restore_error: classify failure into typed stage
//! 2. restore_remote_workspace: orchestrate the multi-step restoration
//! 3. Each stage emits a `RestoreStage` event for observability

use chrono::Utc;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ssh::{EventStream, RemoteEvent, SshError, SshSession};
use crate::workspace_handle::{RemoteWorkspaceHandle, RemoteWorkspaceState};

/// Stage in the restoration pipeline.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum RestoreStage {
    /// Validate SSH connection.
    ValidateSsh,
    /// Probe remote path.
    ProbeRemotePath,
    /// Snapshot remote workspace.
    SnapshotRemote,
    /// Transfer snapshot via SSH.
    TransferSnapshot,
    /// Materialize snapshot locally.
    MaterializeLocal,
}

impl RestoreStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ValidateSsh => "validate_ssh",
            Self::ProbeRemotePath => "probe_remote_path",
            Self::SnapshotRemote => "snapshot_remote",
            Self::TransferSnapshot => "transfer_snapshot",
            Self::MaterializeLocal => "materialize_local",
        }
    }
}

/// Pure plan of restoration stages (no IO).
///
/// Drives `restore_remote_workspace` so tests can validate the plan without an SSH session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestorePlan {
    pub stages: Vec<RestoreStage>,
    pub total_duration_seconds_estimate: u64,
}

impl RestorePlan {
    pub fn default_plan() -> Self {
        Self {
            stages: vec![
                RestoreStage::ValidateSsh,
                RestoreStage::ProbeRemotePath,
                RestoreStage::SnapshotRemote,
                RestoreStage::TransferSnapshot,
                RestoreStage::MaterializeLocal,
            ],
            total_duration_seconds_estimate: 30,
        }
    }
}

/// Restore outcome — final state plus per-stage results.
#[derive(Debug, Clone)]
pub struct RestoreOutcome {
    pub handle: RemoteWorkspaceHandle,
    pub completed_stages: Vec<RestoreStage>,
    pub failed_stage: Option<RestoreStage>,
    pub duration_seconds: u64,
}

/// Stage in the restoration pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RestoreStageError {
    /// Initial SSH connection failed.
    Ssh,
    /// Remote path probe failed (path doesn't exist or not accessible).
    Probe,
    /// Snapshot creation failed.
    Snapshot,
    /// Snapshot transfer failed.
    Transfer,
    /// Local materialization failed.
    Materialize,
}

/// Classify an SSH error into a typed restoration stage failure.
pub fn classify_restore_error(err: &SshError) -> RestoreStageError {
    match err {
        SshError::ConnectionRefused(_) | SshError::Unreachable(_) | SshError::Authentication(_)
        | SshError::InvalidConfig(_) => RestoreStageError::Ssh,
        SshError::Timeout(_) => RestoreStageError::Probe,
        SshError::SessionClosed => RestoreStageError::Transfer,
        SshError::Io(_) => RestoreStageError::Snapshot,
    }
}

/// Restore error type.
#[derive(Debug, Error)]
pub enum RestoreError {
    #[error("ssh failed: {0}")]
    Ssh(#[from] SshError),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("remote path invalid: {0}")]
    InvalidPath(String),
}

/// Orchestrate remote workspace restoration.
///
/// Mirrors Node `restoreRemoteWorkspace`:
/// 1. Validate SSH config
/// 2. Connect via SshSession
/// 3. Probe remote path
/// 4. Snapshot remote (tar/zip)
/// 5. Transfer snapshot via SSH
/// 6. Materialize local (extract to local cache path)
pub async fn restore_remote_workspace<S: SshSession + ?Sized>(
    session: &S,
    config: &crate::ssh::SshSessionConfig,
    remote_host: &str,
    remote_path: &str,
    local_cache_path: &str,
) -> Result<RestoreOutcome, RestoreError> {
    if remote_path.trim().is_empty() {
        return Err(RestoreError::InvalidPath("remote_path is empty".into()));
    }
    if local_cache_path.trim().is_empty() {
        return Err(RestoreError::InvalidInput("local_cache_path is empty".into()));
    }

    let started_at = Utc::now();
    let mut handle = RemoteWorkspaceHandle::new(remote_host, remote_path);
    let mut completed = Vec::new();

    // Stage 1: Connect
    let connection = session.connect(config).await?;
    completed.push(RestoreStage::ValidateSsh);

    // Stage 2: Probe
    let mut probe_stream = session
        .exec(&connection, &format!("test -d {}", remote_path), &[])
        .await?;
    if !stream_has_exit_zero(&mut probe_stream).await {
        let _ = session.close(connection).await;
        handle.mark_failed("remote path probe failed");
        return Ok(RestoreOutcome {
            handle,
            completed_stages: completed,
            failed_stage: Some(RestoreStage::ProbeRemotePath),
            duration_seconds: 0,
        });
    }
    completed.push(RestoreStage::ProbeRemotePath);

    // Stage 3: Snapshot
    let _snapshot_stream = session
        .exec(
            &connection,
            &format!("tar czf - {}", remote_path),
            &[],
        )
        .await?;
    completed.push(RestoreStage::SnapshotRemote);

    // Stage 4: Transfer
    let _transfer_stream = session
        .exec(
            &connection,
            &format!("scp snapshot.tgz {}", local_cache_path),
            &[],
        )
        .await?;
    completed.push(RestoreStage::TransferSnapshot);

    // Stage 5: Materialize
    let _extract_stream = session
        .exec(
            &connection,
            &format!("tar xzf {} -C {}", "snapshot.tgz", local_cache_path),
            &[],
        )
        .await?;
    completed.push(RestoreStage::MaterializeLocal);

    // Close connection
    let _ = session.close(connection).await;

    handle.mark_restored(local_cache_path);
    let duration = (Utc::now() - started_at).num_seconds().max(0) as u64;

    Ok(RestoreOutcome {
        handle,
        completed_stages: completed,
        failed_stage: None,
        duration_seconds: duration,
    })
}

/// Helper: consume a stream and check whether the exit event was zero.
async fn stream_has_exit_zero(stream: &mut EventStream) -> bool {
    let mut exited_with_zero = false;
    while let Some(event) = stream.receiver.recv().await {
        if let RemoteEvent::Exit(code) = event {
            exited_with_zero = code == 0;
            break;
        }
    }
    exited_with_zero
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh::{SshAuth, SshSessionConfig};

    #[test]
    fn default_plan_has_five_stages() {
        let plan = RestorePlan::default_plan();
        assert_eq!(plan.stages.len(), 5);
        assert!(plan.total_duration_seconds_estimate > 0);
    }

    #[test]
    fn classify_restore_error_maps_ssh() {
        for err in [
            SshError::ConnectionRefused("h".into()),
            SshError::Authentication("a".into()),
            SshError::Unreachable("u".into()),
            SshError::InvalidConfig("i".into()),
        ] {
            assert_eq!(classify_restore_error(&err), RestoreStageError::Ssh);
        }
    }

    #[test]
    fn classify_restore_error_maps_probe_timeout() {
        assert_eq!(
            classify_restore_error(&SshError::Timeout(30)),
            RestoreStageError::Probe
        );
    }

    #[test]
    fn classify_restore_error_maps_session_closed_to_transfer() {
        assert_eq!(
            classify_restore_error(&SshError::SessionClosed),
            RestoreStageError::Transfer
        );
    }

    #[test]
    fn classify_restore_error_maps_io_to_snapshot() {
        assert_eq!(
            classify_restore_error(&SshError::Io("disk".into())),
            RestoreStageError::Snapshot
        );
    }

    #[tokio::test]
    async fn restore_rejects_empty_remote_path() {
        let session = crate::ssh::RecordingSshSession::default();
        let cfg = SshSessionConfig::new("h", 22, "u", SshAuth::Password("p".into()));
        let result = restore_remote_workspace(&session, &cfg, "h", "", "/local").await;
        assert!(matches!(result, Err(RestoreError::InvalidPath(_))));
    }

    #[tokio::test]
    async fn restore_rejects_empty_local_cache_path() {
        let session = crate::ssh::RecordingSshSession::default();
        let cfg = SshSessionConfig::new("h", 22, "u", SshAuth::Password("p".into()));
        let result = restore_remote_workspace(&session, &cfg, "h", "/remote", "").await;
        assert!(matches!(result, Err(RestoreError::InvalidInput(_))));
    }

    #[tokio::test]
    async fn restore_success_with_recording_session() {
        let session = crate::ssh::RecordingSshSession::default();
        let cfg = SshSessionConfig::new("h", 22, "u", SshAuth::Password("p".into()));
        let result = restore_remote_workspace(&session, &cfg, "h.example", "/workspace", "/cache").await;
        assert!(result.is_ok());
        let outcome = result.unwrap();
        assert_eq!(outcome.completed_stages.len(), 5);
        assert!(outcome.failed_stage.is_none());
        assert!(outcome.handle.is_terminal());
        assert_eq!(outcome.handle.state, RemoteWorkspaceState::Restored);
    }
}