//! 统一 HTTP 错误响应 — 对齐 Node `middleware/error-handler.ts`。
//!
//! Node 端的公开响应形状是扁平对象：`{ error: "...", ... }`，而不是
//! 将错误码和消息再包一层。敏感的 skill policy denial 也在这里完成脱敏。
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map, Value};
use thiserror::Error;

const STRUCTURED_CONNECTION_ERROR_CODES: &[&str] = &[
    "user_authorization_required",
    "grant_revoked",
    "needs_reauthorization",
    "installation_required",
    "connection_not_installed",
    "subject_not_permitted",
];
const SKILL_POLICY_DENIED_CODE: &str = "skill_policy_denied";

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid input: {0}")]
    BadRequest(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("conflict: {message}")]
    ConflictWith { message: String, payload: Value },
    #[error("unprocessable entity: {0}")]
    Unprocessable(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("too many requests: {0}")]
    TooManyRequests(String),
    #[error("{message}")]
    Http {
        status: StatusCode,
        message: String,
        details: Option<Value>,
    },
    #[error("validation error")]
    Validation(Value),
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
        match self {
            ApiError::Validation(details) => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Validation error", "details": details })),
            )
                .into_response(),
            ApiError::Http {
                status,
                message,
                details,
            } => (status, Json(build_node_body(&message, details.as_ref()))).into_response(),
            ApiError::ConflictWith { message, payload } => {
                let mut body = Map::new();
                let mut error_obj = Map::new();
                error_obj.insert("code".into(), Value::String("conflict".into()));
                error_obj.insert("message".into(), Value::String(message));
                body.insert("error".into(), Value::Object(error_obj));
                if let Some(object) = payload.as_object() {
                    body.extend(
                        object
                            .iter()
                            .map(|(key, value)| (key.clone(), value.clone())),
                    );
                }
                (StatusCode::CONFLICT, Json(Value::Object(body))).into_response()
            }
            error => {
                let status = status_for(&error);
                let message = match status.is_server_error() {
                    true => {
                        tracing::error!(error = %error, "unhandled API error");
                        "Internal server error".to_string()
                    }
                    false => error.to_string(),
                };
                (status, Json(json!({ "error": message }))).into_response()
            }
        }
    }
}

fn status_for(error: &ApiError) -> StatusCode {
    match error {
        ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
        ApiError::Conflict(_) | ApiError::ConflictWith { .. } => StatusCode::CONFLICT,
        ApiError::Unprocessable(_) => StatusCode::UNPROCESSABLE_ENTITY,
        ApiError::Forbidden(_) => StatusCode::FORBIDDEN,
        ApiError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
        ApiError::TooManyRequests(_) => StatusCode::TOO_MANY_REQUESTS,
        ApiError::NotFound(_) => StatusCode::NOT_FOUND,
        ApiError::Sqlx(sqlx::Error::RowNotFound) => StatusCode::NOT_FOUND,
        ApiError::Http { status, .. } => *status,
        ApiError::Validation(_) => StatusCode::BAD_REQUEST,
        ApiError::Internal(_) | ApiError::Sqlx(_) | ApiError::Json(_) | ApiError::Other(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

fn build_node_body(message: &str, details: Option<&Value>) -> Value {
    let mut body = Map::new();
    body.insert("error".into(), Value::String(message.to_string()));

    let Some(details_object) = details.and_then(Value::as_object) else {
        return Value::Object(body);
    };
    let code = details_object.get("code").and_then(Value::as_str);
    if let Some(code) = code {
        body.insert("code".into(), Value::String(code.to_string()));
    }

    let skill_policy_denied = code == Some(SKILL_POLICY_DENIED_CODE);
    let structured_connection =
        code.is_some_and(|value| STRUCTURED_CONNECTION_ERROR_CODES.contains(&value));

    if skill_policy_denied {
        if let Some(reason) = details_object.get("reason").and_then(Value::as_str) {
            body.insert("reason".into(), Value::String(reason.to_string()));
        }
    } else {
        body.insert("details".into(), Value::Object(details_object.clone()));
    }

    if details_object
        .get("remediation")
        .is_some_and(|value| value.is_string() || (structured_connection && value.is_object()))
    {
        body.insert("remediation".into(), details_object["remediation"].clone());
    }
    if structured_connection {
        for key in ["connection", "subject"] {
            if let Some(value) = details_object.get(key) {
                body.insert(key.to_string(), value.clone());
            }
        }
        if let Some(grant_id) = details_object.get("grantId").and_then(Value::as_str) {
            body.insert("grantId".into(), Value::String(grant_id.to_string()));
        }
    }

    Value::Object(body)
}

impl From<pc_errors::Error> for ApiError {
    fn from(error: pc_errors::Error) -> Self {
        match error {
            pc_errors::Error::Validation {
                message: _,
                details,
            } => {
                let details = serde_json::to_value(details).unwrap_or_else(|_| json!([]));
                Self::Validation(details)
            }
            pc_errors::Error::NotFound { resource } => Self::NotFound(resource),
            pc_errors::Error::Conflict { message } => Self::Conflict(message),
            pc_errors::Error::Unprocessable { message } => Self::Unprocessable(message),
            pc_errors::Error::Forbidden { message } => Self::Forbidden(message),
            pc_errors::Error::Unauthorized { message } => Self::Unauthorized(message),
            pc_errors::Error::RateLimited { retry_after_secs } => {
                Self::TooManyRequests(format!("rate limited; retry after {retry_after_secs}s"))
            }
            pc_errors::Error::Upstream {
                service, message, ..
            } => Self::Internal(format!("upstream {service}: {message}")),
            pc_errors::Error::Internal { message } => Self::Internal(message),
            pc_errors::Error::NotImplemented { message } => Self::Internal(message),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    async fn response_json(response: Response) -> Value {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn not_found_renders_node_shape() {
        let response = ApiError::NotFound("issue abc".into()).into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response_json(response).await,
            json!({"error": "not found: issue abc"})
        );
    }

    #[tokio::test]
    async fn validation_renders_zod_shape() {
        let response =
            ApiError::Validation(json!([{"path":["title"],"message":"Required"}])).into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response_json(response).await,
            json!({
                "error": "Validation error",
                "details": [{"path":["title"],"message":"Required"}],
            })
        );
    }

    #[test]
    fn http_error_with_structured_connection_details() {
        let body = build_node_body(
            "authorization required",
            Some(&json!({
                "code": "grant_revoked",
                "connection": {"uid": "conn-1"},
                "subject": {"type": "user", "userId": "u-1"},
                "grantId": "grant-1",
                "remediation": {"action": "reauthorize"},
            })),
        );
        assert_eq!(body["code"], "grant_revoked");
        assert_eq!(body["connection"]["uid"], "conn-1");
        assert_eq!(body["grantId"], "grant-1");
        assert!(body.get("details").is_some());
    }

    #[test]
    fn http_error_skill_policy_denied_redacts_details() {
        let body = build_node_body(
            "skill denied",
            Some(&json!({
                "code": "skill_policy_denied",
                "reason": "company policy",
                "secret": "do not expose",
            })),
        );
        assert_eq!(body["reason"], "company policy");
        assert!(body.get("details").is_none());
    }

    #[tokio::test]
    async fn internal_error_hides_message() {
        let response = ApiError::Internal("database password".into()).into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            response_json(response).await,
            json!({"error": "Internal server error"})
        );
    }

    #[tokio::test]
    async fn conflict_with_includes_code_and_message() {
        let response = ApiError::ConflictWith {
            message: "workspace closed".into(),
            payload: json!({"executionWorkspace": {"status": "closed"}}),
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            response_json(response).await,
            json!({
                "error": {
                    "code": "conflict",
                    "message": "workspace closed",
                },
                "executionWorkspace": {"status": "closed"},
            })
        );
    }
}
