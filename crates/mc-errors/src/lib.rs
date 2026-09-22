//! Multica 统一错误类型与 HTTP 状态码映射。
//!
//! 设计目标：
//! - 高内聚：所有错误收敛到一个枚举
//! - 低耦合：调用方只依赖 `Error` 与 `Result<T>`，不耦合具体子类型
//! - 可序列化：API 错误体以稳定 JSON 输出，与 multica server 兼容

use serde::{Deserialize, Serialize};
use std::fmt;

pub mod http;

pub use http::{status_for, ErrorBody, ErrorResponse};

/// Multica 后端统一错误。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    // ---- 通用 4xx ----
    #[error("validation error: {message}")]
    Validation {
        message: String,
        details: Vec<ValidationDetail>,
    },

    #[error("not found: {resource}")]
    NotFound { resource: String },

    #[error("conflict: {message}")]
    Conflict { message: String },

    #[error("unprocessable entity: {message}")]
    Unprocessable { message: String },

    #[error("forbidden: {message}")]
    Forbidden { message: String },

    #[error("unauthorized: {message}")]
    Unauthorized { message: String },

    #[error("rate limited: retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u32 },

    // ---- Multica 业务错误 ----
    #[error("workspace not found: {0}")]
    WorkspaceNotFound(String),

    #[error("workspace archived: {0}")]
    WorkspaceArchived(String),

    #[error("member already exists: {0}")]
    MemberAlreadyExists(String),

    #[error("agent not available: {0}")]
    AgentUnavailable(String),

    #[error("runtime not connected: {0}")]
    RuntimeOffline(String),

    #[error("issue closed: {0}")]
    IssueClosed(String),

    #[error("issue transition invalid: {from} -> {to}")]
    IssueTransitionInvalid { from: String, to: String },

    #[error("task queue full: {0}")]
    TaskQueueFull(String),

    #[error("task lease expired: {0}")]
    TaskLeaseExpired(String),

    #[error("autopilot quota exceeded: {0}")]
    AutopilotQuotaExceeded(String),

    #[error("plugin signature invalid: {0}")]
    PluginSignatureInvalid(String),

    #[error("channel signature invalid: {0}")]
    ChannelSignatureInvalid(String),

    #[error("vcs conflict: {0}")]
    VcsConflict(String),

    #[error("verification code invalid: {0}")]
    VerificationCodeInvalid(String),

    #[error("session expired")]
    SessionExpired,

    // ---- 5xx ----
    #[error("upstream error: {service} returned {status}")]
    Upstream {
        service: String,
        status: u16,
        message: Option<String>,
    },

    #[error("database error: {0}")]
    Database(String),

    #[error("io error: {0}")]
    Io(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl Error {
    /// 业务错误码（与 multica handler 一致）。
    pub fn code(&self) -> &'static str {
        match self {
            Self::Validation { .. } => "validation_error",
            Self::NotFound { .. } => "not_found",
            Self::Conflict { .. } => "conflict",
            Self::Unprocessable { .. } => "unprocessable",
            Self::Forbidden { .. } => "forbidden",
            Self::Unauthorized { .. } => "unauthorized",
            Self::RateLimited { .. } => "rate_limited",
            Self::WorkspaceNotFound(_) => "workspace_not_found",
            Self::WorkspaceArchived(_) => "workspace_archived",
            Self::MemberAlreadyExists(_) => "member_already_exists",
            Self::AgentUnavailable(_) => "agent_unavailable",
            Self::RuntimeOffline(_) => "runtime_offline",
            Self::IssueClosed(_) => "issue_closed",
            Self::IssueTransitionInvalid { .. } => "issue_transition_invalid",
            Self::TaskQueueFull(_) => "task_queue_full",
            Self::TaskLeaseExpired(_) => "task_lease_expired",
            Self::AutopilotQuotaExceeded(_) => "autopilot_quota_exceeded",
            Self::PluginSignatureInvalid(_) => "plugin_signature_invalid",
            Self::ChannelSignatureInvalid(_) => "channel_signature_invalid",
            Self::VcsConflict(_) => "vcs_conflict",
            Self::VerificationCodeInvalid(_) => "verification_code_invalid",
            Self::SessionExpired => "session_expired",
            Self::Upstream { .. } => "upstream_error",
            Self::Database(_) => "database_error",
            Self::Io(_) => "io_error",
            Self::Internal(_) => "internal_error",
        }
    }

    /// 人类可读消息（已剥离 wrapping）。
    pub fn message(&self) -> String {
        self.to_string()
    }

    /// HTTP 状态码。
    pub fn http_status(&self) -> u16 {
        http::status_for(self)
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self::Internal(format!("json: {value}"))
    }
}

impl From<anyhow::Error> for Error {
    fn from(value: anyhow::Error) -> Self {
        Self::Internal(value.to_string())
    }
}

impl From<sqlx::Error> for Error {
    fn from(value: sqlx::Error) -> Self {
        Self::Database(value.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationDetail {
    pub field: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl fmt::Display for ValidationDetail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}



#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_for_each_variant_is_stable() {
        let cases = [
            (Error::NotFound { resource: "issue".into() }, "not_found"),
            (
                Error::WorkspaceNotFound("ws-1".into()),
                "workspace_not_found",
            ),
            (
                Error::AgentUnavailable("agent-1".into()),
                "agent_unavailable",
            ),
            (
                Error::TaskQueueFull("agent-1".into()),
                "task_queue_full",
            ),
            (
                Error::IssueTransitionInvalid {
                    from: "todo".into(),
                    to: "closed".into(),
                },
                "issue_transition_invalid",
            ),
        ];
        for (err, expected) in cases {
            assert_eq!(err.code(), expected);
        }
    }

    #[test]
    fn http_status_for_each_variant() {
        assert_eq!(Error::NotFound { resource: "x".into() }.http_status(), 404);
        assert_eq!(
            Error::Conflict { message: "x".into() }.http_status(),
            409
        );
        assert_eq!(
            Error::Validation {
                message: "x".into(),
                details: vec![]
            }
            .http_status(),
            400
        );
        assert_eq!(Error::SessionExpired.http_status(), 401);
        assert_eq!(
            Error::Forbidden { message: "x".into() }.http_status(),
            403
        );
        assert_eq!(
            Error::RateLimited { retry_after_secs: 60 }.http_status(),
            429
        );
        assert_eq!(
            Error::Internal("oops".into()).http_status(),
            500
        );
    }

    #[test]
    fn message_is_human_readable() {
        let err = Error::IssueTransitionInvalid {
            from: "todo".into(),
            to: "closed".into(),
        };
        let msg = err.message();
        assert!(msg.contains("todo"));
        assert!(msg.contains("closed"));
    }
}