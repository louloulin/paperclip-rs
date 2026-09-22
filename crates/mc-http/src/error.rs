//! HTTP 错误处理。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use mc_errors::{Error, ErrorResponse};

pub type ApiResult<T> = Result<T, ApiError>;

/// HTTP 错误响应。
pub struct ApiError(pub Error);

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
        let body = ErrorBody {
            error: ErrorResponse::from(&self.0),
        };
        (status, Json(body)).into_response()
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
}
