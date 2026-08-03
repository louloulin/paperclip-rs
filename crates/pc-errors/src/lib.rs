//! Paperclip 统一错误类型与 HTTP 状态码映射。
//!
//! 设计目标：
//! - 高内聚：所有错误收敛到一个枚举
//! - 低耦合：调用方只依赖 `Error` 与 `Result<T>`，不耦合具体子类型
//! - 可序列化：API 错误体以稳定 JSON 输出，与原 Node server 兼容

use serde::{Deserialize, Serialize};
use std::fmt;

/// Paperclip 后端统一错误。
#[derive(Debug, thiserror::Error)]
pub enum Error {
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

    #[error("upstream error: {service} returned {status}")]
    Upstream {
        service: String,
        status: u16,
        message: String,
    },

    #[error("internal error: {message}")]
    Internal { message: String },

    #[error("not implemented: {message}")]
    NotImplemented { message: String },
}

/// 字段级校验错误细节。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationDetail {
    pub path: String,
    pub message: String,
}

/// 与原 server `middleware/error-handler.ts` 兼容的 JSON 错误体。
#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub error: ErrorBodyInner,
}

#[derive(Debug, Serialize)]
pub struct ErrorBodyInner {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<ValidationDetail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u32>,
}

impl Error {
    /// 错误代号（与原 API 兼容）。
    pub fn code(&self) -> &'static str {
        match self {
            Self::Validation { .. } => "validation_error",
            Self::NotFound { .. } => "not_found",
            Self::Conflict { .. } => "conflict",
            Self::Unprocessable { .. } => "unprocessable_entity",
            Self::Forbidden { .. } => "forbidden",
            Self::Unauthorized { .. } => "unauthorized",
            Self::RateLimited { .. } => "rate_limited",
            Self::Upstream { .. } => "upstream_error",
            Self::Internal { .. } => "internal_error",
            Self::NotImplemented { .. } => "not_implemented",
        }
    }

    /// 对应的 HTTP 状态码。
    pub fn status_code(&self) -> http::StatusCode {
        use http::StatusCode;
        match self {
            Self::Validation { .. } => StatusCode::BAD_REQUEST,
            Self::NotFound { .. } => StatusCode::NOT_FOUND,
            Self::Conflict { .. } => StatusCode::CONFLICT,
            Self::Unprocessable { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Forbidden { .. } => StatusCode::FORBIDDEN,
            Self::Unauthorized { .. } => StatusCode::UNAUTHORIZED,
            Self::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::Upstream { .. } => StatusCode::BAD_GATEWAY,
            Self::Internal { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Self::NotImplemented { .. } => StatusCode::NOT_IMPLEMENTED,
        }
    }

    /// 用户可见的错误消息（生产模式下隐藏内部细节）。
    pub fn public_message(&self) -> String {
        match self {
            Self::Internal { .. } => "internal server error".to_string(),
            other => other.to_string(),
        }
    }

    /// 序列化为与原 API 兼容的 JSON 错误体。
    pub fn to_body(&self) -> ErrorBody {
        let details = match self {
            Self::Validation { details, .. } => details.clone(),
            _ => Vec::new(),
        };
        let retry_after_secs = match self {
            Self::RateLimited { retry_after_secs } => Some(*retry_after_secs),
            _ => None,
        };
        ErrorBody {
            error: ErrorBodyInner {
                code: self.code(),
                message: self.public_message(),
                details,
                retry_after_secs,
            },
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// 便捷构造器。
pub fn validation(message: impl Into<String>) -> Error {
    Error::Validation {
        message: message.into(),
        details: Vec::new(),
    }
}

pub fn validation_field(path: impl Into<String>, message: impl Into<String>) -> Error {
    Error::Validation {
        message: "validation failed".into(),
        details: vec![ValidationDetail {
            path: path.into(),
            message: message.into(),
        }],
    }
}

pub fn not_found(resource: impl Into<String>) -> Error {
    Error::NotFound {
        resource: resource.into(),
    }
}

pub fn conflict(message: impl Into<String>) -> Error {
    Error::Conflict {
        message: message.into(),
    }
}

pub fn unprocessable(message: impl Into<String>) -> Error {
    Error::Unprocessable {
        message: message.into(),
    }
}

pub fn forbidden(message: impl Into<String>) -> Error {
    Error::Forbidden {
        message: message.into(),
    }
}

pub fn unauthorized(message: impl Into<String>) -> Error {
    Error::Unauthorized {
        message: message.into(),
    }
}

pub fn internal(message: impl Into<String>) -> Error {
    Error::Internal {
        message: message.into(),
    }
}

/// 显示友好的 Display 别名（让 main 错误信息更紧凑）。
pub struct DisplayError<'a>(pub &'a Error);

impl fmt::Display for DisplayError<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.public_message())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_error_returns_400() {
        let err = validation("missing title");
        assert_eq!(err.status_code(), http::StatusCode::BAD_REQUEST);
        assert_eq!(err.code(), "validation_error");
    }

    #[test]
    fn not_found_returns_404() {
        let err = not_found("issue");
        assert_eq!(err.status_code(), http::StatusCode::NOT_FOUND);
        assert_eq!(err.code(), "not_found");
    }

    #[test]
    fn body_serializes_with_details() {
        let err = validation_field("title", "must not be empty");
        let body = err.to_body();
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["error"]["code"], "validation_error");
        assert_eq!(json["error"]["details"][0]["path"], "title");
    }

    #[test]
    fn internal_hides_details_in_public_message() {
        let err = internal("db connection failed at 10.0.0.1:5432");
        assert_eq!(err.public_message(), "internal server error");
        assert_eq!(err.status_code(), http::StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn rate_limited_includes_retry_after() {
        let err = Error::RateLimited {
            retry_after_secs: 30,
        };
        let body = err.to_body();
        assert_eq!(body.error.retry_after_secs, Some(30));
        assert_eq!(err.status_code(), http::StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn unprocessable_returns_422() {
        let err = unprocessable("revision contains redacted secrets");
        assert_eq!(err.code(), "unprocessable_entity");
        assert_eq!(err.status_code(), http::StatusCode::UNPROCESSABLE_ENTITY);
    }
}
