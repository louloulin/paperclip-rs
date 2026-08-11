//! Retry-after + typed resolve-failure extraction from GitHub HTTP responses.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::status::{ErrorCode, LivenessState};

/// Compute `retryAfterSeconds` from a GitHub response's headers.
///
/// Priority:
/// 1. `Retry-After: <seconds>` if present and numeric
/// 2. `X-RateLimit-Reset: <epoch>` if present and numeric → `max(1, reset - now)`
/// 3. fallback: 300 seconds (matches Node upstream behaviour)
#[must_use]
pub fn retry_after_seconds(response: &RetryAfterResponse) -> u64 {
    if let Some(retry) = response.retry_after.as_deref() {
        if let Ok(n) = retry.parse::<u64>() {
            return n;
        }
    }
    if let Some(reset) = response.x_ratelimit_reset.as_deref() {
        if let Ok(epoch) = reset.parse::<u64>() {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            return epoch.saturating_sub(now).max(1);
        }
    }
    300
}

/// Minimal subset of [`reqwest::Response`] headers that `retry_after_seconds`
/// needs. Decoupling the helper from `reqwest::Response` lets us unit-test
/// without an HTTP server and lets the integration layer pass any header bag.
#[derive(Debug, Default, Clone)]
pub struct RetryAfterResponse {
    pub retry_after: Option<String>,
    pub x_ratelimit_reset: Option<String>,
}

impl RetryAfterResponse {
    #[must_use]
    pub fn new(retry_after: Option<&str>, x_ratelimit_reset: Option<&str>) -> Self {
        Self {
            retry_after: retry_after.map(String::from),
            x_ratelimit_reset: x_ratelimit_reset.map(String::from),
        }
    }
}

/// Typed resolve failure extracted from a GitHub response status.
///
/// Returns `None` for 2xx (success) and 4xx codes that don't map to a
/// known failure (callers fall back to a generic error in those cases).
#[must_use]
pub fn failure_from_github_response(
    status: u16,
    rate_limit_remaining: Option<&str>,
    retry_after: &RetryAfterResponse,
) -> Option<ResolveFailure> {
    let retry = retry_after_seconds(retry_after);
    match status {
        401 => Some(ResolveFailure {
            liveness: LivenessState::AuthRequired,
            error_code: ErrorCode::GithubAuthRequired,
            error_message: "GitHub authentication is required to refresh this object."
                .to_string(),
            retry_after_seconds: retry,
        }),
        403 => {
            if rate_limit_remaining == Some("0") {
                Some(ResolveFailure {
                    liveness: LivenessState::Unreachable,
                    error_code: ErrorCode::GithubRateLimited,
                    error_message: "GitHub rate limit reached while refreshing this object."
                        .to_string(),
                    retry_after_seconds: retry,
                })
            } else {
                Some(ResolveFailure {
                    liveness: LivenessState::AuthRequired,
                    error_code: ErrorCode::GithubForbidden,
                    error_message:
                        "GitHub rejected the configured credentials for this object."
                            .to_string(),
                    retry_after_seconds: retry,
                })
            }
        }
        429 => Some(ResolveFailure {
            liveness: LivenessState::Unreachable,
            error_code: ErrorCode::GithubRateLimited,
            error_message: "GitHub returned HTTP 429 while refreshing this object."
                .to_string(),
            retry_after_seconds: retry,
        }),
        s if s >= 500 => Some(ResolveFailure {
            liveness: LivenessState::Unreachable,
            error_code: ErrorCode::GithubUnreachable,
            error_message: format!("GitHub returned HTTP {s} while refreshing this object."),
            retry_after_seconds: retry,
        }),
        _ => None,
    }
}

/// Typed resolve failure — callers decide how to render / persist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveFailure {
    pub liveness: LivenessState,
    pub error_code: ErrorCode,
    pub error_message: String,
    pub retry_after_seconds: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r525_retry_after_uses_retry_after_header_first() {
        let r = RetryAfterResponse::new(Some("30"), Some("99999999999"));
        assert_eq!(retry_after_seconds(&r), 30);
    }

    #[test]
    fn r525_retry_after_falls_back_to_ratelimit_reset() {
        // 5 minutes in the future
        let future = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 300;
        let r = RetryAfterResponse::new(None, Some(&future.to_string()));
        let got = retry_after_seconds(&r);
        // Should be approximately 300 (within a few seconds)
        assert!((280..=300).contains(&got), "got {got}");
    }

    #[test]
    fn r525_retry_after_fallback_300_when_both_headers_missing() {
        let r = RetryAfterResponse::new(None, None);
        assert_eq!(retry_after_seconds(&r), 300);
    }

    #[test]
    fn r525_retry_after_rejects_non_numeric_retry_after() {
        // 5 minutes in the future — non-numeric retry-after must be rejected
        // and the function must fall through to X-RateLimit-Reset.
        let future = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 300;
        let r = RetryAfterResponse::new(Some("not a number"), Some(&future.to_string()));
        let got = retry_after_seconds(&r);
        assert!((280..=300).contains(&got), "got {got}");
    }

    #[test]
    fn r525_failure_401_maps_to_auth_required() {
        let r = RetryAfterResponse::new(Some("10"), None);
        let f = failure_from_github_response(401, None, &r).unwrap();
        assert_eq!(f.liveness, LivenessState::AuthRequired);
        assert_eq!(f.error_code, ErrorCode::GithubAuthRequired);
        assert_eq!(f.retry_after_seconds, 10);
    }

    #[test]
    fn r525_failure_403_with_rate_limit_zero_maps_to_rate_limited() {
        let r = RetryAfterResponse::new(None, None);
        let f = failure_from_github_response(403, Some("0"), &r).unwrap();
        assert_eq!(f.liveness, LivenessState::Unreachable);
        assert_eq!(f.error_code, ErrorCode::GithubRateLimited);
        assert_eq!(f.retry_after_seconds, 300); // fallback
    }

    #[test]
    fn r525_failure_403_without_rate_limit_maps_to_forbidden() {
        let r = RetryAfterResponse::new(None, None);
        let f = failure_from_github_response(403, None, &r).unwrap();
        assert_eq!(f.liveness, LivenessState::AuthRequired);
        assert_eq!(f.error_code, ErrorCode::GithubForbidden);
    }

    #[test]
    fn r525_failure_429_maps_to_rate_limited() {
        let r = RetryAfterResponse::new(Some("60"), None);
        let f = failure_from_github_response(429, None, &r).unwrap();
        assert_eq!(f.liveness, LivenessState::Unreachable);
        assert_eq!(f.error_code, ErrorCode::GithubRateLimited);
        assert_eq!(f.retry_after_seconds, 60);
    }

    #[test]
    fn r525_failure_500_maps_to_unreachable() {
        let r = RetryAfterResponse::new(None, None);
        let f = failure_from_github_response(500, None, &r).unwrap();
        assert_eq!(f.liveness, LivenessState::Unreachable);
        assert_eq!(f.error_code, ErrorCode::GithubUnreachable);
        assert_eq!(f.error_message.contains("HTTP 500"), true);
    }

    #[test]
    fn r525_failure_404_returns_none_caller_handles() {
        let r = RetryAfterResponse::new(None, None);
        assert!(failure_from_github_response(404, None, &r).is_none());
    }

    #[test]
    fn r525_failure_200_returns_none() {
        let r = RetryAfterResponse::new(None, None);
        assert!(failure_from_github_response(200, None, &r).is_none());
    }
}
