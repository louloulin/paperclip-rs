//! 适配器失败与续接失败的恢复分类。
//! 对齐 Node `services/recovery/service.ts` 的纯函数决策部分。

use chrono::{DateTime, Duration, Utc};
use serde_json::Value;

pub const PROVIDER_QUOTA_RECOVERY_DEFAULT_BACKOFF_MS: i64 = 60 * 60 * 1000;
pub const CONTINUATION_RECOVERY_TRANSIENT_MAX_ATTEMPTS: u32 = 3;
pub const CONTINUATION_RECOVERY_DEFAULT_MAX_ATTEMPTS: u32 = 1;
pub const CONTINUATION_RECOVERY_TRANSIENT_BASE_BACKOFF_MS: i64 = 60_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterFailureRecoveryClassification {
    ProviderQuota {
        retry_at: DateTime<Utc>,
        parsed_reset_time: bool,
    },
    ConfigurationIncomplete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContinuationRetryKind {
    TransientInfra,
    NonRetryable,
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuationRetryClassification {
    pub kind: ContinuationRetryKind,
    pub max_attempts: u32,
    pub base_backoff_ms: i64,
    pub error_code: Option<String>,
}

pub fn classify_adapter_failure(
    error: Option<&str>,
    error_code: Option<&str>,
    result_json: Option<&Value>,
    now: DateTime<Utc>,
) -> Option<AdapterFailureRecoveryClassification> {
    let code = error_code.unwrap_or("");
    if !matches!(
        code,
        "adapter_failed" | "provider_quota" | "configuration_incomplete"
    ) {
        return None;
    }
    let serialized = result_json.map(Value::to_string).unwrap_or_default();
    let combined = format!("{code}\n{}\n{serialized}", error.unwrap_or(""));
    if code == "configuration_incomplete" || contains_configuration_error(&combined) {
        return Some(AdapterFailureRecoveryClassification::ConfigurationIncomplete);
    }
    if code != "provider_quota" && !contains_quota_error(&combined) {
        return None;
    }
    for key in [
        "retryNotBefore",
        "transientRetryNotBefore",
        "providerQuotaRetryNotBefore",
    ] {
        if let Some(value) = result_json
            .and_then(|v| v.get(key))
            .and_then(Value::as_str)
            .filter(|v| !v.trim().is_empty())
        {
            if let Ok(retry_at) = value.parse::<DateTime<Utc>>() {
                if retry_at > now {
                    return Some(AdapterFailureRecoveryClassification::ProviderQuota {
                        retry_at,
                        parsed_reset_time: true,
                    });
                }
            }
        }
    }
    if let Some(retry_at) = parse_quota_clock_reset(&combined, now) {
        return Some(AdapterFailureRecoveryClassification::ProviderQuota {
            retry_at,
            parsed_reset_time: true,
        });
    }
    Some(AdapterFailureRecoveryClassification::ProviderQuota {
        retry_at: now + Duration::milliseconds(PROVIDER_QUOTA_RECOVERY_DEFAULT_BACKOFF_MS),
        parsed_reset_time: false,
    })
}

pub fn classify_continuation_failure(error_code: Option<&str>) -> ContinuationRetryClassification {
    let code = error_code
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned);
    let kind = match code.as_deref() {
        Some(
            "agent_not_invokable"
            | "agent_not_found"
            | "budget_blocked"
            | "budget_exhausted"
            | "issue_paused"
            | "issue_dependencies_blocked",
        ) => ContinuationRetryKind::NonRetryable,
        Some(
            "adapter_failed"
            | "codex_transient_upstream"
            | "codex_harness_crash"
            | "claude_transient_upstream"
            | "provider_quota"
            | "timeout",
        ) => ContinuationRetryKind::TransientInfra,
        _ => ContinuationRetryKind::Default,
    };
    let (max_attempts, base_backoff_ms) = match kind {
        ContinuationRetryKind::NonRetryable => (0, 0),
        ContinuationRetryKind::TransientInfra => (3, 60_000),
        ContinuationRetryKind::Default => (1, 0),
    };
    ContinuationRetryClassification {
        kind,
        max_attempts,
        base_backoff_ms,
        error_code: code,
    }
}

fn contains_quota_error(value: &str) -> bool {
    let v = value.to_ascii_lowercase();
    v.contains("usage limit")
        || v.contains("provider quota")
        || v.contains("quota exceeded")
        || v.contains("model at capacity")
        || v.contains("model is at capacity")
}
fn contains_configuration_error(value: &str) -> bool {
    let v = value.to_ascii_lowercase();
    v.contains("model_not_found")
        || v.contains("not found") && v.contains("model")
        || v.contains("missing api key")
        || v.contains("missing credentials")
        || v.contains("credentials are missing")
        || v.contains("credentials is missing")
        || v.contains("api key is not set")
        || v.contains("api key unavailable")
}

fn parse_quota_clock_reset(error: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let lower = error.to_ascii_lowercase();
    let start = lower.find("try again at")?;
    let rest = error[start + 12..].trim_start();
    let token = rest
        .split_whitespace()
        .next()?
        .trim_end_matches(|c| c == '.' || c == ',');
    let parts: Vec<_> = token.split(':').collect();
    let mut hour: i32 = parts.first()?.parse().ok()?;
    let minute: i32 = parts.get(1).and_then(|v| v.parse().ok()).unwrap_or(0);
    if hour > 23 || minute > 59 {
        return None;
    }
    let suffix = rest[token.len()..].trim_start().to_ascii_lowercase();
    if suffix.starts_with('p') {
        if hour < 12 {
            hour += 12;
        }
    } else if suffix.starts_with('a') && hour == 12 {
        hour = 0;
    }
    let date = now.date_naive();
    let mut candidate = DateTime::<Utc>::from_naive_utc_and_offset(
        date.and_hms_opt(hour as u32, minute as u32, 0)?,
        Utc,
    );
    if candidate <= now {
        candidate += Duration::days(1);
    }
    Some(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn classifies_configuration() {
        assert_eq!(
            classify_adapter_failure(
                Some("missing API key"),
                Some("adapter_failed"),
                None,
                Utc::now()
            ),
            Some(AdapterFailureRecoveryClassification::ConfigurationIncomplete)
        );
    }
    #[test]
    fn persisted_quota_retry_wins() {
        let now = DateTime::parse_from_rfc3339("2026-08-06T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let got = classify_adapter_failure(
            None,
            Some("provider_quota"),
            Some(&json!({"retryNotBefore":"2026-08-06T02:00:00Z"})),
            now,
        )
        .unwrap();
        assert!(matches!(
            got,
            AdapterFailureRecoveryClassification::ProviderQuota {
                parsed_reset_time: true,
                ..
            }
        ));
    }
    #[test]
    fn continuation_sets_bounded_retry() {
        let got = classify_continuation_failure(Some("timeout"));
        assert_eq!(got.kind, ContinuationRetryKind::TransientInfra);
        assert_eq!(got.max_attempts, 3);
    }
}
