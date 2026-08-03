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
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("unprocessable entity: {0}")]
    Unprocessable(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("unauthorized: {0}")]
    Unauthorized(String),
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
            ApiError::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            ApiError::Unprocessable(_) => {
                (StatusCode::UNPROCESSABLE_ENTITY, "unprocessable_entity")
            }
            ApiError::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
            ApiError::Unauthorized(_) => (StatusCode::UNAUTHORIZED, "unauthorized"),
            ApiError::NotFound(_) | ApiError::Sqlx(sqlx::Error::RowNotFound) => {
                (StatusCode::NOT_FOUND, "not_found")
            }
            _ => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        };
        let body = ApiErrorBody {
            error: ApiErrorDetail {
                code,
                message: self.to_string(),
            },
        };
        (status, Json(body)).into_response()
    }
}

impl From<pc_errors::Error> for ApiError {
    fn from(error: pc_errors::Error) -> Self {
        match error {
            pc_errors::Error::Validation { message, .. } => Self::BadRequest(message),
            pc_errors::Error::NotFound { resource } => Self::NotFound(resource),
            pc_errors::Error::Conflict { message } => Self::Conflict(message),
            pc_errors::Error::Unprocessable { message } => Self::Unprocessable(message),
            pc_errors::Error::Forbidden { message } => Self::Forbidden(message),
            pc_errors::Error::Unauthorized { message } => Self::Unauthorized(message),
            pc_errors::Error::RateLimited { retry_after_secs } => {
                Self::Internal(format!("rate limited; retry after {retry_after_secs}s"))
            }
            pc_errors::Error::Upstream {
                service, message, ..
            } => Self::Internal(format!("upstream {service}: {message}")),
            pc_errors::Error::Internal { message } => Self::Internal(message),
            pc_errors::Error::NotImplemented { message } => Self::Internal(message),
        }
    }
}

#[derive(Serialize)]
struct ApiErrorBody {
    error: ApiErrorDetail,
}
#[derive(Serialize)]
struct ApiErrorDetail {
    code: &'static str,
    message: String,
}

impl From<pc_repos::RepoError> for ApiError {
    fn from(err: pc_repos::RepoError) -> Self {
        match err {
            pc_repos::RepoError::Sql(e) => ApiError::Sqlx(e),
            pc_repos::RepoError::NotFound { entity, id } => {
                ApiError::NotFound(format!("{entity} {id}"))
            }
            pc_repos::RepoError::Invalid(msg) => ApiError::BadRequest(msg),
            pc_repos::RepoError::Json(e) => ApiError::Json(e),
            pc_repos::RepoError::Core(e) => ApiError::Internal(e.to_string()),
        }
    }
}
