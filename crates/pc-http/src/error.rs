//! 统一 HTTP 错误响应。
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid input: {0}")]
    BadRequest(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("internal: {0}")]
    Internal(String),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type ApiResult<T> = Result<T, ApiError>;

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            ApiError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            ApiError::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
            ApiError::NotFound(_) | ApiError::Sqlx(sqlx::Error::RowNotFound) => (StatusCode::NOT_FOUND, "not_found"),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        };
        let body = ApiErrorBody { error: ApiErrorDetail { code, message: self.to_string() } };
        (status, Json(body)).into_response()
    }
}

#[derive(Serialize)]
struct ApiErrorBody { error: ApiErrorDetail }
#[derive(Serialize)]
struct ApiErrorDetail { code: &'static str, message: String }
