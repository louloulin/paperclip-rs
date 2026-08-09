//! Codex CLI 失败分类与重试提示纯函数。

use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexAuthRefreshFailureClass {
    RefreshTokenReused,
    RefreshTokenExpired,
    RefreshTokenInvalidated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CodexProtocolState {
    pub exit_code: Option<i32>,
    pub saw_protocol_event: bool,
    pub saw_protocol_terminal_event: bool,
}

fn haystack(stdout: Option<&str>, stderr: Option<&str>, error_message: Option<&str>) -> String {
    [
        error_message.unwrap_or_default(),
        stdout.unwrap_or_default(),
        stderr.unwrap_or_default(),
    ]
    .join("\n")
    .lines()
    .map(str::trim)
    .filter(|line| !line.is_empty())
    .collect::<Vec<_>>()
    .join("\n")
}

pub fn is_codex_harness_crash(state: CodexProtocolState) -> bool {
    state.exit_code.unwrap_or(0) != 0
        && state.saw_protocol_event
        && !state.saw_protocol_terminal_event
}

pub fn is_codex_unknown_session_error(stdout: &str, stderr: &str) -> bool {
    let text = haystack(Some(stdout), Some(stderr), None).to_ascii_lowercase();
    [
        "unknown session",
        "unknown thread",
        "session ",
        "thread ",
        "conversation ",
        "missing rollout path for thread",
        "state db missing rollout path",
        "state db returned stale rollout path",
        "no rollout found for thread id",
    ]
    .iter()
    .any(|needle| {
        text.contains(needle)
            && (needle.contains("unknown")
                || text.contains("not found")
                || text.contains("missing")
                || text.contains("stale")
                || text.contains("no rollout"))
    })
}

pub fn classify_codex_auth_refresh_failure(
    stdout: Option<&str>,
    stderr: Option<&str>,
    error_message: Option<&str>,
) -> Option<CodexAuthRefreshFailureClass> {
    let text = haystack(stdout, stderr, error_message).to_ascii_lowercase();
    if text.contains("refresh_token_reused")
        || text.contains("refresh token has already been used")
        || text.contains("token reuse detected")
    {
        return Some(CodexAuthRefreshFailureClass::RefreshTokenReused);
    }
    if text.contains("refresh_token_expired")
        || text.contains("refresh token has expired")
        || text.contains("expired refresh token")
    {
        return Some(CodexAuthRefreshFailureClass::RefreshTokenExpired);
    }
    if text.contains("refresh_token_invalidated")
        || text.contains("refresh token has been invalidated")
        || text.contains("refresh token has been revoked")
        || text.contains("invalid refresh token")
        || text.contains("missing bearer")
        || text.contains("invalid_grant")
        || contextual_auth_invalidated(&text)
    {
        return Some(CodexAuthRefreshFailureClass::RefreshTokenInvalidated);
    }
    None
}

fn contextual_auth_invalidated(text: &str) -> bool {
    let auth = [
        "oauth",
        "refresh",
        "access-token",
        "access_token",
        "bearer",
        "credential",
    ];
    let status = ["401", "unauthorized", "invalid grant", "invalid_grant"];
    auth.iter().any(|left| {
        status
            .iter()
            .any(|right| text.contains(left) && text.contains(right))
    })
}

pub fn extract_codex_retry_not_before(text: &str, now: SystemTime) -> Option<SystemTime> {
    let lower = text.to_ascii_lowercase();
    let marker = lower.find("try again at")?;
    let tail = lower.get(marker + "try again at".len()..)?.trim_start();
    let sentence = tail.split(['.', '!', '\n']).next()?.trim();
    let sentence = sentence.split('(').next().unwrap_or(sentence).trim();
    let mut words = sentence.split_whitespace();
    let first = words.next()?;
    let second = words.next().unwrap_or_default();
    let timezone_hint = words.next().map(|value| value.trim_matches(['(', ')']));
    let token = if first.to_ascii_lowercase().ends_with("am")
        || first.to_ascii_lowercase().ends_with("pm")
    {
        first.to_owned()
    } else {
        format!("{first}{second}")
    };
    let (hour, minute, pm) = parse_clock(&token)?;
    if let Some(timezone_hint) = timezone_hint {
        if let Some(result) = retry_in_timezone(hour, minute, pm, timezone_hint, now) {
            return Some(result);
        }
    }
    let now_seconds = now.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let day = now_seconds / 86_400;
    let day_start = day * 86_400;
    let current = now_seconds - day_start;
    let mut target = day_start + hour * 3600 + minute * 60 + u64::from(pm) * 12 * 3600;
    if target <= day_start + current {
        target += 86_400;
    }
    Some(UNIX_EPOCH + Duration::from_secs(target))
}

fn retry_in_timezone(
    hour: u64,
    minute: u64,
    pm: bool,
    hint: &str,
    now: SystemTime,
) -> Option<SystemTime> {
    let normalized = if hint.eq_ignore_ascii_case("utc") || hint.eq_ignore_ascii_case("gmt") {
        "UTC"
    } else {
        hint
    };
    let timezone = Tz::from_str(normalized).ok()?;
    let now_utc = chrono::DateTime::<Utc>::from(now);
    let local_now = now_utc.with_timezone(&timezone);
    let hour24 = hour + u64::from(pm) * 12;
    let date = NaiveDate::from_ymd_opt(local_now.year(), local_now.month(), local_now.day())?;
    let time = NaiveTime::from_hms_opt(hour24 as u32, minute as u32, 0)?;
    let mut candidate = timezone
        .from_local_datetime(&NaiveDateTime::new(date, time))
        .single()?;
    if candidate <= local_now {
        let next_date = date.succ_opt()?;
        candidate = timezone
            .from_local_datetime(&NaiveDateTime::new(next_date, time))
            .single()?;
    }
    Some(candidate.with_timezone(&Utc).into())
}

fn parse_clock(text: &str) -> Option<(u64, u64, bool)> {
    let normalized = text.trim().trim_end_matches('.').to_ascii_lowercase();
    let (digits, pm) = if let Some(value) = normalized.strip_suffix("pm") {
        (value, true)
    } else if let Some(value) = normalized.strip_suffix("am") {
        (value, false)
    } else {
        return None;
    };
    let mut parts = digits.split(':');
    let hour = parts.next()?.parse::<u64>().ok()?;
    let minute = parts.next().unwrap_or("0").parse::<u64>().ok()?;
    if !(1..=12).contains(&hour) || minute > 59 {
        return None;
    }
    Some((hour % 12, minute, pm))
}

pub fn is_codex_provider_quota_error(
    stdout: Option<&str>,
    stderr: Option<&str>,
    error_message: Option<&str>,
    now: SystemTime,
) -> bool {
    let text = haystack(stdout, stderr, error_message).to_ascii_lowercase();
    text.contains("you've hit your usage limit")
        || text.contains("you’ve hit your usage limit")
        || text.contains("usage limit")
        || text.contains("model is at capacity")
        || text.contains("at capacity for this model")
        || text.contains("capacity limit")
        || extract_codex_retry_not_before(&text, now).is_some()
}

pub fn is_codex_transient_upstream_error(
    stdout: Option<&str>,
    stderr: Option<&str>,
    error_message: Option<&str>,
    now: SystemTime,
) -> bool {
    if is_codex_provider_quota_error(stdout, stderr, error_message, now) {
        return false;
    }
    let text = haystack(stdout, stderr, error_message).to_ascii_lowercase();
    let transient = [
        "high demand",
        "temporary errors",
        "rate limit",
        "rate-limit",
        "too many requests",
        "429",
        "server overloaded",
        "service unavailable",
        "try again later",
    ];
    transient.iter().any(|needle| text.contains(needle))
        && (text.contains("remote compact task")
            || text.contains("high demand")
            || text.contains("temporary errors"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_crash_requires_started_nonterminal_protocol() {
        assert!(is_codex_harness_crash(CodexProtocolState {
            exit_code: Some(1),
            saw_protocol_event: true,
            saw_protocol_terminal_event: false
        }));
        assert!(!is_codex_harness_crash(CodexProtocolState {
            exit_code: Some(1),
            saw_protocol_event: true,
            saw_protocol_terminal_event: true
        }));
    }

    #[test]
    fn auth_refresh分类() {
        assert_eq!(
            classify_codex_auth_refresh_failure(None, Some("refresh token has expired"), None),
            Some(CodexAuthRefreshFailureClass::RefreshTokenExpired)
        );
        assert_eq!(
            classify_codex_auth_refresh_failure(Some("invalid_grant"), None, None),
            Some(CodexAuthRefreshFailureClass::RefreshTokenInvalidated)
        );
        assert_eq!(
            classify_codex_auth_refresh_failure(None, None, Some("plain 401")),
            None
        );
    }

    #[test]
    fn quota与transient互斥() {
        let now = UNIX_EPOCH + Duration::from_secs(22 * 3600);
        assert!(is_codex_provider_quota_error(
            None,
            None,
            Some("You've hit your usage limit for GPT"),
            now
        ));
        assert!(!is_codex_transient_upstream_error(
            None,
            None,
            Some("usage limit"),
            now
        ));
        assert!(is_codex_transient_upstream_error(
            None,
            Some("high demand temporary errors"),
            None,
            now
        ));
    }
}
