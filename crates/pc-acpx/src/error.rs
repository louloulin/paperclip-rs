//! `pc-acpx` crate-wide error type. The crate is a thin shim around tokio
//! filesystem helpers, so the error is mostly a wrapper around `io::Error`
//! with a path tag for easy diagnostics.

use std::path::PathBuf;
use thiserror::Error;

/// Errors that can be raised by the `pc-acpx` filesystem and engine helpers.
#[derive(Debug, Error)]
pub enum AcpxError {
    /// A filesystem operation failed. The `path` is the target the operation
    /// was attempted on, so the caller can produce a clear log line.
    #[error("io error on `{path}`: {error}")]
    Io {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    /// The target path has no parent directory (e.g. only a filename).
    #[error("path `{0}` has no parent directory")]
    NoParent(PathBuf),
    /// The platform does not support symbolic links (R367 staging seam).
    #[error("symbolic links are not supported on this platform")]
    SymlinkUnsupported,
    /// A blocking task panicked or was cancelled.
    #[error("blocking task `{context}` failed: {error}")]
    Join {
        context: String,
        #[source]
        error: tokio::task::JoinError,
    },
    /// The caller (or env) supplied an invalid `PAPERCLIP_INSTANCE_ID`
    /// (R369 path resolver).
    #[error("invalid PAPERCLIP_INSTANCE_ID `{0}` — expected [A-Za-z0-9_-]+")]
    InvalidInstanceId(String),
    /// JSON serialization / deserialization failed for a helper that owns
    /// its own payload (R369 startup config / Claude settings writer).
    #[error("json error in `{context}`: {error}")]
    Json {
        context: String,
        #[source]
        error: serde_json::Error,
    },
    /// Spawning the acpx subprocess failed (binary missing, cwd invalid,
    /// permission denied, …). R370 subprocess handle.
    #[error("failed to spawn acpx subprocess `{command}`: {error}")]
    Spawn {
        command: String,
        #[source]
        error: std::io::Error,
    },
    /// The acpx subprocess was reaped but its exit status is not available
    /// (process was already awaited / killed externally).
    #[error("acpx subprocess `{pid}` was already reaped")]
    AlreadyReaped { pid: u32 },
    /// A JSON-RPC line from the acpx subprocess could not be parsed.
    #[error("failed to parse jsonrpc line `{line}`: {reason}")]
    JsonRpcParse { line: String, reason: String },
    /// An I/O operation against the acpx subprocess failed.
    #[error("acpx subprocess I/O error on `{target}`: {error}")]
    SubprocessIo {
        target: String,
        #[source]
        error: std::io::Error,
    },
    /// A read on the acpx subprocess timed out.
    #[error("acpx subprocess read timed out after `{timeout_ms}` ms")]
    ReadTimeout { timeout_ms: u64 },
}

impl AcpxError {
    /// Adapt an `io::Error` into `AcpxError::Io`, attaching a path tag.
    pub fn io(path: impl Into<PathBuf>, error: std::io::Error) -> Self {
        AcpxError::Io {
            path: path.into(),
            error,
        }
    }
}
