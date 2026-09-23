//! HTTP 错误处理。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use mc_errors::{Error, ErrorResponse};

pub type ApiResult<T> = Result<T, ApiError>;

/// HTTP 错误响应。
pub struct ApiError(pub Error);

impl ApiError {
    /// 以**调用方指定**的状态码写出本仓标准错误体。
    ///
    /// 只在「`Error` → 状态码映射与上游契约不一致」的少数端点上用：本仓
    /// `Error::RuntimeOffline` 是 422（M1 的 task-lease 口径，`mc-errors/http.rs`），
    /// 而 runtime 异步请求四族的上游契约是 **503 `runtime is offline`**。
    /// 错误体形状与 [`IntoResponse`] 完全一致，不会为这几个端点分叉出第二种错误格式。
    pub fn respond_with(self, status: StatusCode) -> Response {
        let body = ErrorBody {
            error: ErrorResponse::from(&self.0),
        };
        (status, Json(body)).into_response()
    }
}

impl From<Error> for ApiError {
    fn from(value: Error) -> Self {
        Self(value)
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(value: sqlx::Error) -> Self {
        Self(Error::Database(value.to_string()))
    }
}

impl From<std::io::Error> for ApiError {
    fn from(value: std::io::Error) -> Self {
        Self(Error::Io(value.to_string()))
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(value: anyhow::Error) -> Self {
        Self(Error::Internal(value.to_string()))
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: ErrorResponse,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status =
            StatusCode::from_u16(self.0.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        self.respond_with(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_error_serializes() {
        let e = ApiError(Error::NotFound {
            resource: "issue".into(),
        });
        let body = ErrorResponse::from(&e.0);
        assert_eq!(body.code, "not_found");
        assert_eq!(e.0.http_status(), 404);
    }

    /// 指定状态码不改形状：体还是 `{"error":{"code","message"}}`。
    #[test]
    fn respond_with_overrides_only_the_status() {
        let response = ApiError(Error::RuntimeOffline("runtime is offline".into()))
            .respond_with(StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            Error::RuntimeOffline("runtime is offline".into()).http_status(),
            422
        );
    }
}
