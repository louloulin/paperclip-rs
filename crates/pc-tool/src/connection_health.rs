#![forbid(unsafe_code)]

//! Tool connection health error sanitization.
//! R703: Direct port of tool-access.ts::sanitizeHttpFailure.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolConnectionHealthStatus {
    Healthy, Unhealthy, Unknown, Error, Failed, MissingSecret,
}

impl ToolConnectionHealthStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Unhealthy => "unhealthy",
            Self::Unknown => "unknown",
            Self::Error => "error",
            Self::Failed => "failed",
            Self::MissingSecret => "missing_secret",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizedHealthFailure {
    pub status: ToolConnectionHealthStatus,
    pub message: String,
    pub code: String,
}

#[derive(Debug, Clone)]
pub struct HttpErrorLike {
    pub status: u16,
    pub message: String,
    pub code: Option<String>,
}

impl HttpErrorLike {
    pub fn new(status: u16, message: impl Into<String>, code: Option<String>) -> Self {
        Self { status, message: message.into(), code }
    }
}

pub fn sanitize_http_failure(error: &HttpErrorLike) -> SanitizedHealthFailure {
    let code = error.code.as_deref();
    if let Some(c) = code {
        if c == "oauth_challenge" {
            return SanitizedHealthFailure { status: ToolConnectionHealthStatus::Error, message: "This app needs you to sign in.".into(), code: "oauth_challenge".into() };
        }
        if c == "oauth_refresh_missing" {
            return SanitizedHealthFailure { status: ToolConnectionHealthStatus::Failed, message: "OAuth credentials have expired and need to be reconnected.".into(), code: "oauth_refresh_missing".into() };
        }
        if c == "binding_missing" || c == "secret_deleted" || c == "secret_inactive" || c == "version_missing" {
            return SanitizedHealthFailure { status: ToolConnectionHealthStatus::MissingSecret, message: "A configured credential secret could not be resolved.".into(), code: c.into() };
        }
    }
    if error.status == 404 && error.message.to_lowercase().contains("secret") {
        return SanitizedHealthFailure { status: ToolConnectionHealthStatus::MissingSecret, message: "A configured credential secret could not be resolved.".into(), code: "secret_missing".into() };
    }
    SanitizedHealthFailure { status: ToolConnectionHealthStatus::Error, message: error.message.clone(), code: "paperclip_error".into() }
}

pub fn sanitize_runtime_error(error: &(dyn std::error::Error)) -> SanitizedHealthFailure {
    let msg = error.to_string();
    let truncated = msg.chars().take(240).collect::<String>();
    SanitizedHealthFailure { status: ToolConnectionHealthStatus::Error, message: truncated, code: "runtime_error".into() }
}

pub fn sanitize_unknown_failure() -> SanitizedHealthFailure {
    SanitizedHealthFailure { status: ToolConnectionHealthStatus::Error, message: "Connection check failed.".into(), code: "runtime_error".into() }
}

#[cfg(test)]
mod internal_tests { use super::*;
    fn err(status: u16, msg: &str, code: Option<&str>) -> HttpErrorLike { HttpErrorLike::new(status, msg, code.map(|s| s.to_string())) }

    #[test] fn oauth_challenge() {
        let e = err(401, "auth needed", Some("oauth_challenge"));
        let r = sanitize_http_failure(&e);
        assert_eq!(r.status, ToolConnectionHealthStatus::Error);
        assert_eq!(r.code, "oauth_challenge");
        assert!(r.message.contains("sign in"));
    }
    #[test] fn oauth_refresh_missing() {
        let e = err(401, "no refresh", Some("oauth_refresh_missing"));
        let r = sanitize_http_failure(&e);
        assert_eq!(r.status, ToolConnectionHealthStatus::Failed);
        assert_eq!(r.code, "oauth_refresh_missing");
    }
    #[test] fn binding_missing() {
        let e = err(404, "x", Some("binding_missing"));
        let r = sanitize_http_failure(&e);
        assert_eq!(r.status, ToolConnectionHealthStatus::MissingSecret);
        assert_eq!(r.code, "binding_missing");
    }
    #[test] fn secret_deleted() {
        let e = err(500, "x", Some("secret_deleted"));
        let r = sanitize_http_failure(&e);
        assert_eq!(r.status, ToolConnectionHealthStatus::MissingSecret);
        assert_eq!(r.code, "secret_deleted");
    }
    #[test] fn secret_inactive() {
        let e = err(500, "x", Some("secret_inactive"));
        let r = sanitize_http_failure(&e);
        assert_eq!(r.status, ToolConnectionHealthStatus::MissingSecret);
    }
    #[test] fn version_missing() {
        let e = err(500, "x", Some("version_missing"));
        let r = sanitize_http_failure(&e);
        assert_eq!(r.status, ToolConnectionHealthStatus::MissingSecret);
    }
    #[test] fn http_404_with_secret_in_message() {
        let e = err(404, "Secret not found", None);
        let r = sanitize_http_failure(&e);
        assert_eq!(r.status, ToolConnectionHealthStatus::MissingSecret);
        assert_eq!(r.code, "secret_missing");
    }
    #[test] fn http_404_without_secret_in_message() {
        let e = err(404, "Not found", None);
        let r = sanitize_http_failure(&e);
        assert_eq!(r.status, ToolConnectionHealthStatus::Error);
        assert_eq!(r.code, "paperclip_error");
    }
    #[test] fn paperclip_error_fallback() {
        let e = err(500, "internal failure", Some("unknown_code"));
        let r = sanitize_http_failure(&e);
        assert_eq!(r.status, ToolConnectionHealthStatus::Error);
        assert_eq!(r.code, "paperclip_error");
        assert_eq!(r.message, "internal failure");
    }
    #[test] fn runtime_error_truncates_at_240() {
        let long = "x".repeat(500);
        let dummy = std::io::Error::new(std::io::ErrorKind::Other, long);
        let r = sanitize_runtime_error(&dummy);
        assert_eq!(r.code, "runtime_error");
        assert_eq!(r.message.len(), 240);
    }
    #[test] fn unknown_failure_default() {
        let r = sanitize_unknown_failure();
        assert_eq!(r.status, ToolConnectionHealthStatus::Error);
        assert_eq!(r.code, "runtime_error");
        assert_eq!(r.message, "Connection check failed.");
    }
    #[test] fn status_as_str_matches_node() {
        assert_eq!(ToolConnectionHealthStatus::Healthy.as_str(), "healthy");
        assert_eq!(ToolConnectionHealthStatus::Unhealthy.as_str(), "unhealthy");
        assert_eq!(ToolConnectionHealthStatus::Unknown.as_str(), "unknown");
        assert_eq!(ToolConnectionHealthStatus::Error.as_str(), "error");
        assert_eq!(ToolConnectionHealthStatus::Failed.as_str(), "failed");
        assert_eq!(ToolConnectionHealthStatus::MissingSecret.as_str(), "missing_secret");
    }
    #[test] fn status_serde_snake_case() {
        let j = serde_json::to_string(&ToolConnectionHealthStatus::MissingSecret).unwrap();
        assert_eq!(j, "\"missing_secret\"");
    }
}
