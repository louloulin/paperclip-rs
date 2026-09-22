//! HTTP 状态码映射 + JSON 错误响应体。

use crate::Error;
use serde::{Deserialize, Serialize};

/// API 错误响应体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

impl ErrorResponse {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
            request_id: None,
        }
    }

    #[must_use]
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    #[must_use]
    pub fn with_request_id(mut self, id: impl Into<String>) -> Self {
        self.request_id = Some(id.into());
        self
    }
}

impl From<&Error> for ErrorResponse {
    fn from(value: &Error) -> Self {
        let mut body = ErrorResponse::new(value.code(), value.message());
        if let Error::Validation { details, .. } = value {
            body = body.with_details(serde_json::json!(details));
        }
        body
    }
}

/// 兼容 API 错误体的扁平形式（部分旧接口用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ErrorResponse,
}

/// 将内部 `Error` 映射到 HTTP 状态码。
pub fn status_for(err: &Error) -> u16 {
    use Error::{
        AgentUnavailable, AutopilotQuotaExceeded, ChannelSignatureInvalid, Conflict, Database,
        Forbidden, Internal, Io, IssueClosed, IssueTransitionInvalid, MemberAlreadyExists,
        NotFound, PluginSignatureInvalid, RateLimited, RuntimeOffline, SessionExpired,
        TaskLeaseExpired, TaskQueueFull, Unauthorized, Unprocessable, Upstream, Validation,
        VcsConflict, VerificationCodeInvalid, WorkspaceArchived, WorkspaceNotFound,
    };
    match err {
        // 4xx
        Validation { .. } => 400,
        // 未通过凭证校验（含验证码错误 / 已消费 / 过期，LUM-1345 要求 401）与签名错误
        Unauthorized { .. }
        | SessionExpired
        | VerificationCodeInvalid(_)
        | PluginSignatureInvalid(_)
        | ChannelSignatureInvalid(_) => 401,
        Forbidden { .. } => 403,
        NotFound { .. } | WorkspaceNotFound(_) => 404,
        Conflict { .. }
        | MemberAlreadyExists(_)
        | VcsConflict(_)
        | IssueTransitionInvalid { .. } => 409,
        Unprocessable { .. } | IssueClosed(_) | AgentUnavailable(_) | RuntimeOffline(_) => 422,
        RateLimited { .. } => 429,
        // 业务资源已下线 / 软删除
        WorkspaceArchived(_) => 410,

        // 5xx
        Upstream { .. } | Database(_) | Io(_) | Internal(_) => 500,
        // 业务限流：按 503 处理（quota / lease）
        AutopilotQuotaExceeded(_) | TaskQueueFull(_) | TaskLeaseExpired(_) => 503,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ValidationDetail;

    #[test]
    fn error_response_round_trip() {
        let body = ErrorResponse::new("validation_error", "bad input")
            .with_details(serde_json::json!([{"field": "x"}]));
        let json = serde_json::to_string(&body).unwrap();
        let parsed: ErrorResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.code, "validation_error");
        assert!(parsed.details.is_some());
    }

    #[test]
    fn status_for_validation_is_400() {
        let err = Error::Validation {
            message: "bad".into(),
            details: vec![ValidationDetail {
                field: "x".into(),
                message: "y".into(),
                code: None,
            }],
        };
        assert_eq!(status_for(&err), 400);
    }
}
