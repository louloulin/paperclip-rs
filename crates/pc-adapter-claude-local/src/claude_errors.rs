//! Claude 失败分类与重试提示纯函数。
//! 规则来源于 Node `claude-local/server/parse.ts`，不依赖进程或网络。

use pc_adapter_api::UsageSummary;
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


// ============================================================
// 模型使用统计 / 登录 URL / 图片处理错误检测
// ============================================================

/// 从 `modelUsage` JSON 聚合 token 计数。
///
/// `modelUsage` 是 Claude CLI 权威的 per-model ledger（/cost 背后）；
/// 顶层 `usage` 只反映主循环消息链，遇到 subagents / sidechains
/// 会低估 output tokens。`cacheCreationInputTokens` 计为 input
/// （billed prompt tokens）。
pub fn claude_model_usage_totals(model_usage: &Value) -> Option<UsageSummary> {
    let obj = model_usage.as_object()?;
    let mut input_tokens: u64 = 0;
    let mut output_tokens: u64 = 0;
    let mut cached_input_tokens: u64 = 0;
    let mut saw_input = false;
    let mut saw_output = false;
    let mut saw_cached = false;
    let mut saw_entry = false;
    for (_model, value) in obj {
        let entry = match value.as_object() {
            Some(o) => o,
            None => continue,
        };
        if entry.is_empty() {
            continue;
        }
        saw_entry = true;
        if let Some(v) = entry.get("inputTokens").and_then(Value::as_u64) {
            input_tokens += v;
            saw_input = true;
        }
        if let Some(v) = entry.get("cacheCreationInputTokens").and_then(Value::as_u64) {
            input_tokens += v;
            saw_input = true;
        }
        if let Some(v) = entry.get("outputTokens").and_then(Value::as_u64) {
            output_tokens += v;
            saw_output = true;
        }
        if let Some(v) = entry.get("cacheReadInputTokens").and_then(Value::as_u64) {
            cached_input_tokens += v;
            saw_cached = true;
        }
    }
    if !saw_entry {
        return None;
    }
    Some(UsageSummary {
        input_tokens,
        output_tokens,
        cached_input_tokens: if saw_cached { Some(cached_input_tokens) } else { None },
    })
}

/// 从 stdout/stderr 文本中提取 claude 登录 URL。
///
/// 匹配 `https://...` 形式 URL，优先选择含 `claude` / `anthropic` / `auth`
/// 子串的；清理掉尾部 ] } . ! , ? ; : ' " 等标点。
pub fn extract_claude_login_url(text: &str) -> Option<String> {
    let urls = extract_urls(text);
    if urls.is_empty() {
        return None;
    }
    for raw_url in &urls {
        let cleaned = clean_trailing_url_punct(raw_url);
        if cleaned.contains("claude") || cleaned.contains("anthropic") || cleaned.contains("auth") {
            return Some(cleaned);
        }
    }
    urls.first().map(|s| clean_trailing_url_punct(s))
}

/// 提取所有 http(s) URL
fn extract_urls(text: &str) -> Vec<String> {
    let re = match regex_lite::Regex::new(r###"https?://[^\s'"'"'"<>()\[\]{};,!?]+"###) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    re.find_iter(text).map(|m| m.as_str().to_string()).collect()
}

/// 清理 URL 末尾的标点
fn clean_trailing_url_punct(url: &str) -> String {
    let trailing = "])}.!,?;:'\"";
    let trimmed = url.trim_end_matches(|c: char| trailing.contains(c));
    trimmed.to_string()
}

// ============================================================
// Tests
// ============================================================
#[cfg(test)]
mod tests_extra {
    use super::*;
    use serde_json::json;

    #[test]
    fn model_usage_totals_aggregates_all_models() {
        let usage = json!({
            "claude-3-5-sonnet": {
                "inputTokens": 100,
                "outputTokens": 50,
                "cacheReadInputTokens": 30,
                "cacheCreationInputTokens": 20
            },
            "claude-3-haiku": {
                "inputTokens": 5,
                "outputTokens": 10,
                "cacheReadInputTokens": 0,
                "cacheCreationInputTokens": 0
            }
        });
        let summary = claude_model_usage_totals(&usage).unwrap();
        assert_eq!(summary.input_tokens, (100 + 20 + 5) as u64);
        assert_eq!(summary.output_tokens, (50 + 10) as u64);
        assert_eq!(summary.cached_input_tokens, Some(30));
    }

    #[test]
    fn model_usage_totals_returns_none_for_empty() {
        let usage = json!({});
        assert!(claude_model_usage_totals(&usage).is_none());
    }

    #[test]
    fn model_usage_totals_returns_none_for_all_empty_entries() {
        let usage = json!({
            "model-a": {},
            "model-b": {}
        });
        assert!(claude_model_usage_totals(&usage).is_none());
    }

    #[test]
    fn model_usage_totals_skips_missing_fields() {
        let usage = json!({
            "model-a": {
                "inputTokens": 42
            }
        });
        let summary = claude_model_usage_totals(&usage).unwrap();
        assert_eq!(summary.input_tokens, 42);
        assert_eq!(summary.output_tokens, 0);
        assert_eq!(summary.cached_input_tokens, None);
    }

    #[test]
    fn extract_login_url_prefers_claude_url() {
        let text = "Visit https://example.com/foo and https://claude.ai/login?code=abc to continue";
        let url = extract_claude_login_url(text).unwrap();
        assert!(url.contains("claude.ai/login"));
    }

    #[test]
    fn extract_login_url_prefers_anthropic_url() {
        let text = "Login at https://console.anthropic.com/oauth/authorize?x=1 thanks";
        let url = extract_claude_login_url(text).unwrap();
        assert!(url.contains("anthropic.com/oauth"));
    }

    #[test]
    fn extract_login_url_strips_trailing_punct() {
        let text = "Open https://claude.ai/login).";
        let url = extract_claude_login_url(text).unwrap();
        assert!(url.starts_with("https://"));
        assert!(!url.ends_with('.'));
        assert!(!url.ends_with(')'));
    }

    #[test]
    fn extract_login_url_returns_none_when_no_url() {
        let text = "Just some text without URLs";
        assert!(extract_claude_login_url(text).is_none());
    }

    #[test]
    fn extract_login_url_falls_back_to_first_url() {
        let text = "Some unrelated https://example.com URL only";
        let url = extract_claude_login_url(text).unwrap();
        assert!(url.contains("example.com"));
    }

}
