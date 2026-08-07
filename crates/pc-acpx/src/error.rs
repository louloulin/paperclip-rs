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
