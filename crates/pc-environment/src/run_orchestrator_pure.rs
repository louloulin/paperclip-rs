#![forbid(unsafe_code)]

//! Environment run orchestrator pure helpers — 1:1 port of
//! paperclip/server/src/services/environment-run-orchestrator.ts (pure portion).
//!
//! DB-bound orchestration stays in pc-environment::service::environment_run_orchestrator.

use serde::{Deserialize, Serialize};

/// Error codes for environment run failures (Node `EnvironmentErrorCode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentErrorCode {
    EnvironmentNotFound,
    EnvironmentInactive,
    UnsupportedEnvironment,
    UnsupportedAdapterEnvironment,
    ProbeFailed,
    LeaseAcquireFailed,
    WorkspaceRealizationFailed,
    TransportResolutionFailed,
    LeaseReleaseFailed,
    LeaseCleanupFailed,
}

impl EnvironmentErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EnvironmentNotFound => "environment_not_found",
            Self::EnvironmentInactive => "environment_inactive",
            Self::UnsupportedEnvironment => "unsupported_environment",
            Self::UnsupportedAdapterEnvironment => "unsupported_adapter_environment",
            Self::ProbeFailed => "probe_failed",
            Self::LeaseAcquireFailed => "lease_acquire_failed",
            Self::WorkspaceRealizationFailed => "workspace_realization_failed",
            Self::TransportResolutionFailed => "transport_resolution_failed",
            Self::LeaseReleaseFailed => "lease_release_failed",
            Self::LeaseCleanupFailed => "lease_cleanup_failed",
        }
    }
}

/// First non-empty trimmed line of the text (Node `firstNonEmptyLine`).
///
/// Returns `None` if input is empty or all lines are blank.
pub fn first_non_empty_line(text: Option<&str>) -> Option<String> {
    let s = text?;
    for raw_line in s.split(['\r', '\n']) {
        let line = raw_line.trim();
        if !line.is_empty() {
            return Some(line.to_string());
        }
    }
    None
}

/// Provision failure detail (Node `formatProvisionFailureDetail`).
#[derive(Debug, Clone, Default)]
pub struct ProvisionFailure {
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Format a human-readable provision failure detail line.
///
/// - If `timed_out`, returns `"provision command timed out"`.
/// - Otherwise, builds `"exit code {exit_code|null} (signal {signal})"` plus the
///   first non-empty line from stderr (falling back to stdout).
pub fn format_provision_failure_detail(result: &ProvisionFailure) -> String {
    if result.timed_out {
        return "provision command timed out".to_string();
    }
    let signal_str = result
        .signal
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| format!(" (signal {s})"))
        .unwrap_or_default();
    let detail = first_non_empty_line(Some(&result.stderr))
        .or_else(|| first_non_empty_line(Some(&result.stdout)));
    let status = format!(
        "exit code {}{signal_str}",
        result
            .exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "null".to_string())
    );
    match detail {
        Some(d) => format!("{status}: {d}"),
        None => status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_code_as_str_matches_node() {
        assert_eq!(EnvironmentErrorCode::EnvironmentNotFound.as_str(), "environment_not_found");
        assert_eq!(EnvironmentErrorCode::EnvironmentInactive.as_str(), "environment_inactive");
        assert_eq!(EnvironmentErrorCode::UnsupportedEnvironment.as_str(), "unsupported_environment");
        assert_eq!(
            EnvironmentErrorCode::UnsupportedAdapterEnvironment.as_str(),
            "unsupported_adapter_environment"
        );
        assert_eq!(EnvironmentErrorCode::ProbeFailed.as_str(), "probe_failed");
        assert_eq!(EnvironmentErrorCode::LeaseAcquireFailed.as_str(), "lease_acquire_failed");
        assert_eq!(
            EnvironmentErrorCode::WorkspaceRealizationFailed.as_str(),
            "workspace_realization_failed"
        );
        assert_eq!(
            EnvironmentErrorCode::TransportResolutionFailed.as_str(),
            "transport_resolution_failed"
        );
        assert_eq!(EnvironmentErrorCode::LeaseReleaseFailed.as_str(), "lease_release_failed");
        assert_eq!(EnvironmentErrorCode::LeaseCleanupFailed.as_str(), "lease_cleanup_failed");
    }

    #[test]
    fn first_non_empty_line_returns_first_trimmed() {
        let text = "  hello  \n   \nworld";
        assert_eq!(
            first_non_empty_line(Some(text)).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn first_non_empty_line_handles_crlf() {
        let text = "\r\n\r\nfoo\r\nbar";
        assert_eq!(first_non_empty_line(Some(text)).as_deref(), Some("foo"));
    }

    #[test]
    fn first_non_empty_line_returns_none_for_empty() {
        assert!(first_non_empty_line(None).is_none());
        assert!(first_non_empty_line(Some("")).is_none());
        assert!(first_non_empty_line(Some("   \n   \n")).is_none());
    }

    #[test]
    fn format_failure_timed_out() {
        let result = ProvisionFailure {
            timed_out: true,
            ..Default::default()
        };
        assert_eq!(
            format_provision_failure_detail(&result),
            "provision command timed out"
        );
    }

    #[test]
    fn format_failure_exit_code_only() {
        let result = ProvisionFailure {
            exit_code: Some(1),
            stdout: "".into(),
            stderr: "".into(),
            ..Default::default()
        };
        assert_eq!(format_provision_failure_detail(&result), "exit code 1");
    }

    #[test]
    fn format_failure_exit_code_null() {
        let result = ProvisionFailure {
            exit_code: None,
            stdout: "".into(),
            stderr: "".into(),
            ..Default::default()
        };
        assert_eq!(format_provision_failure_detail(&result), "exit code null");
    }

    #[test]
    fn format_failure_with_signal() {
        let result = ProvisionFailure {
            exit_code: Some(137),
            signal: Some("SIGKILL".into()),
            stdout: "".into(),
            stderr: "".into(),
            ..Default::default()
        };
        assert_eq!(
            format_provision_failure_detail(&result),
            "exit code 137 (signal SIGKILL)"
        );
    }

    #[test]
    fn format_failure_includes_stderr() {
        let result = ProvisionFailure {
            exit_code: Some(1),
            stdout: "out line".into(),
            stderr: "err line\nerr line 2".into(),
            ..Default::default()
        };
        assert_eq!(
            format_provision_failure_detail(&result),
            "exit code 1: err line"
        );
    }

    #[test]
    fn format_failure_stderr_priority_over_stdout() {
        let result = ProvisionFailure {
            exit_code: Some(1),
            stdout: "stdout line".into(),
            stderr: "stderr line".into(),
            ..Default::default()
        };
        assert_eq!(
            format_provision_failure_detail(&result),
            "exit code 1: stderr line"
        );
    }

    #[test]
    fn format_failure_falls_back_to_stdout() {
        let result = ProvisionFailure {
            exit_code: Some(1),
            stdout: "   \nstdout detail\n   ".into(),
            stderr: "  \n  ".into(),
            ..Default::default()
        };
        assert_eq!(
            format_provision_failure_detail(&result),
            "exit code 1: stdout detail"
        );
    }

    #[test]
    fn format_failure_with_signal_and_stderr() {
        let result = ProvisionFailure {
            exit_code: Some(127),
            signal: Some("TERM".into()),
            stdout: "".into(),
            stderr: "command not found".into(),
            ..Default::default()
        };
        assert_eq!(
            format_provision_failure_detail(&result),
            "exit code 127 (signal TERM): command not found"
        );
    }

    #[test]
    fn format_failure_signal_trimmed() {
        let result = ProvisionFailure {
            exit_code: Some(1),
            signal: Some("  SIGINT  ".into()),
            stdout: "".into(),
            stderr: "".into(),
            ..Default::default()
        };
        assert_eq!(
            format_provision_failure_detail(&result),
            "exit code 1 (signal SIGINT)"
        );
    }
}