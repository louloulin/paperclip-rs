//! Typed enums for GitHub external-object resolve failures.

use serde::{Deserialize, Serialize};

/// Liveness state of an external object — used by the UI to render the
/// "active / unreachable / auth_required" badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LivenessState {
    /// Object is reachable and was successfully refreshed.
    Active,
    /// Provider refused (401 / 403 forbidden non-rate-limit) — user must re-auth.
    AuthRequired,
    /// Network / provider / rate-limit / 5xx — should be retried after `retry_after_seconds`.
    Unreachable,
}

/// Stable error code for resolve failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    GithubAuthRequired,
    GithubForbidden,
    GithubRateLimited,
    GithubUnreachable,
}
