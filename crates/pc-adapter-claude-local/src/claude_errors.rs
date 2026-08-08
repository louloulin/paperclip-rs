//! Claude 失败分类与重试提示纯函数。
//! 规则来源于 Node `claude-local/server/parse.ts`，不依赖进程或网络。

use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn text_values(parsed: Option<&Value>, stdout: &str, stderr: &str, error_message: Option<&str>) -> Vec<String> {
    let mut values = Vec::new();
    if let Some(Value::Object(object)) = parsed {
        if let Some(text) = object.get("result").and_then(Value::as_str) { values.push(text.to_owned()); }
        if let Some(Value::Array(errors)) = object.get("errors") {
            for error in errors {
                match error {
                    Value::String(text) => values.push(text.clone()),
                    Value::Object(error) => {
                        for key in ["message", "error", "code"] {
                            if let Some(text) = error.get(key).and_then(Value::as_str) { values.push(text.to_owned()); break; }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    if let Some(text) = error_message { values.push(text.to_owned()); }
    values.push(stdout.to_owned());
    values.push(stderr.to_owned());
    values
}

fn haystack(parsed: Option<&Value>, stdout: &str, stderr: &str, error_message: Option<&str>) -> String {
    text_values(parsed, stdout, stderr, error_message).join("\n").to_ascii_lowercase()
}

pub fn describe_claude_failure(parsed: &Value) -> Option<String> {
    let Value::Object(object) = parsed else { return None };
    let subtype = object.get("subtype").and_then(Value::as_str).unwrap_or_default().trim();
    let detail = object.get("result").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).or_else(|| {
        object.get("errors").and_then(Value::as_array).and_then(|errors| errors.iter().find_map(|error| match error {
            Value::String(text) if !text.trim().is_empty() => Some(text.trim()),
            Value::Object(object) => object.get("message").or_else(|| object.get("error")).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()),
            _ => None,
        }))
    });
    if subtype.is_empty() && detail.is_none() { return None; }
    let mut result = "Claude run failed".to_owned();
    if !subtype.is_empty() { result.push_str(": subtype="); result.push_str(subtype); }
    if let Some(detail) = detail { result.push_str(": "); result.push_str(detail); }
    Some(result)
}

pub fn is_claude_model_not_found_error(parsed: Option<&Value>, stdout: &str, stderr: &str, error_message: Option<&str>) -> bool {
    let text = haystack(parsed, stdout, stderr, error_message);
    text.contains("model not found") || text.contains("model_not_found") || text.contains("model does not exist") || text.contains("unknown model") || text.contains("invalid model") || text.contains("404") && text.contains("model")
}

pub fn is_claude_max_turns_result(parsed: Option<&Value>) -> bool {
    let Some(Value::Object(object)) = parsed else { return false };
    let values = ["subtype", "stop_reason", "stopReason", "error_code", "errorCode"];
    values.iter().filter_map(|key| object.get(*key).and_then(Value::as_str)).map(|value| value.trim().to_ascii_lowercase()).any(|value| matches!(value.as_str(), "error_max_turns" | "max_turns" | "max_turns_exhausted" | "turn_limit" | "turn_limit_exhausted"))
}

pub fn is_claude_refusal_result(parsed: Option<&Value>) -> bool {
    let Some(Value::Object(object)) = parsed else { return false };
    ["subtype", "stop_reason", "stopReason", "error_code", "errorCode"].iter().filter_map(|key| object.get(*key).and_then(Value::as_str)).map(|value| value.trim().to_ascii_lowercase()).any(|value| value == "model_refusal" || value == "refusal")
}

pub fn is_claude_poisoned_previous_message_id_error(parsed: &Value) -> bool {
    haystack(Some(parsed), "", "", None).contains("diagnostics.previous_message_id") && haystack(Some(parsed), "", "", None).contains("starts with `msg_")
}

pub fn is_claude_transient_upstream_error(parsed: Option<&Value>, stdout: &str, stderr: &str, error_message: Option<&str>) -> bool {
    if is_claude_max_turns_result(parsed) || is_claude_unknown_session_error(parsed) || parsed.is_some_and(|value| is_claude_poisoned_previous_message_id_error(value)) || parsed.is_some_and(|value| crate::claude_stream_json::is_claude_image_processing_error(value)) || is_claude_login_required(parsed, stdout, stderr) || is_claude_provider_quota_error(parsed, stdout, stderr, error_message) { return false; }
    let text = haystack(parsed, stdout, stderr, error_message);
    ["rate limit", "rate_limit_error", "too many requests", "429", "overloaded", "503", "529", "high demand", "try again later", "temporarily unavailable", "throttl", "servicequotaexceededexception"].iter().any(|needle| text.contains(needle))
}

pub fn is_claude_provider_quota_error(parsed: Option<&Value>, stdout: &str, stderr: &str, error_message: Option<&str>) -> bool {
    if is_claude_max_turns_result(parsed) || is_claude_unknown_session_error(parsed) || parsed.is_some_and(|value| is_claude_poisoned_previous_message_id_error(value)) || parsed.is_some_and(|value| crate::claude_stream_json::is_claude_image_processing_error(value)) || is_claude_login_required(parsed, stdout, stderr) { return false; }
    let text = haystack(parsed, stdout, stderr, error_message);
    ["session limit reached", "session limit exceeded", "out of extra usage", "extra usage", "usage limit reached", "usage cap reached", "5-hour limit reached", "weekly limit reached", "claude usage limit reached", "servicequotaexceededexception"].iter().any(|needle| text.contains(needle))
}

pub fn is_claude_unknown_session_error(parsed: Option<&Value>) -> bool {
    let text = haystack(parsed, "", "", None);
    ["no conversation found with session id", "unknown session", "session ", "not a valid uuid", "--resume requires a valid session", "does not match any session title"].iter().any(|needle| text.contains(needle)) && (text.contains("not found") || text.contains("unknown session") || text.contains("no conversation") || text.contains("not a valid uuid") || text.contains("requires a valid session") || text.contains("does not match"))
}

pub fn is_claude_login_required(parsed: Option<&Value>, stdout: &str, stderr: &str) -> bool {
    let text = haystack(parsed, stdout, stderr, None);
    ["not logged in", "please log in", "please run claude login", "please run /login", "login required", "requires login", "unauthorized", "authentication required", "invalid api key"].iter().any(|needle| text.contains(needle))
}

/// 从“resets 4pm”类文案提取下一次本地重试时间。返回 Unix 秒；无效或缺失时为 None。
pub fn extract_claude_retry_not_before(text: &str, now: SystemTime) -> Option<SystemTime> {
    let lower = text.to_ascii_lowercase();
    let reset = lower.find("resets")?;
    let tail = lower.get(reset + 6..)?.trim_start_matches(|c: char| c == ' ' || c == '·' || c == '：' || c == ':');
    let mut words = tail.split_whitespace();
    let first = words.next()?.trim_matches(|c: char| c == '(' || c == '.');
    let second = words.next().unwrap_or_default().trim_matches(|c: char| c == '(' || c == '.');
    let clock = if first.ends_with('a') || first.ends_with('p') || first.ends_with("am") || first.ends_with("pm") {
        first.to_owned()
    } else {
        format!("{first}{second}")
    };
    let (hour, minute, is_pm) = parse_clock(&clock)?;
    let seconds = now.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let day = seconds / 86_400;
    let current_day_start = day * 86_400;
    let current_seconds = seconds - current_day_start;
    let mut target = current_day_start + hour * 3600 + minute * 60;
    if is_pm { target = current_day_start + (hour + 12) * 3600 + minute * 60; }
    if target <= current_day_start + current_seconds { target += 86_400; }
    Some(UNIX_EPOCH + Duration::from_secs(target))
}

fn parse_clock(text: &str) -> Option<(u64, u64, bool)> {
    let normalized = text.trim().trim_end_matches('.').to_ascii_lowercase();
    let (digits, is_pm) = if let Some(value) = normalized.strip_suffix("pm") {
        (value, true)
    } else if let Some(value) = normalized.strip_suffix("am") {
        (value, false)
    } else if let Some(value) = normalized.strip_suffix('p') {
        (value, true)
    } else if let Some(value) = normalized.strip_suffix('a') {
        (value, false)
    } else {
        return None;
    };
    let mut parts = digits.split(':');
    let hour = parts.next()?.parse::<u64>().ok()?;
    let minute = parts.next().unwrap_or("0").parse::<u64>().ok()?;
    (1..=12).contains(&hour).then_some((hour % 12, minute, is_pm)).filter(|(_, minute, _)| *minute < 60)
}
