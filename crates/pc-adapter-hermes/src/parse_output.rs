//! Hermes 输出解析（对齐 Node `parseHermesOutput`）。
//!
//! 从合并的 stdout/stderr 中提取：
//! - `session_id` — 形如 `Session: <id>` / `session id: <id>`
//! - `usage` — `tokens: <in> input <out> output`
//! - `cost_usd` — `cost: $0.123` / `spent: 0.5`
//! - `response` — 最后一段非 thinking/tool 行作为 agent 回复
//! - `error_message` — stderr 中的 error/exception/traceback 行（排除 log 级别噪声）

use crate::constants::{
    extract_session_id, COST_REGEX, THINKING_PREFIX, TOKEN_USAGE_REGEX, TOOL_OUTPUT_PREFIX,
};
use pc_adapter_api::UsageSummary;

/// 解析后的 Hermes 输出（已映射到 Paperclip AdapterExecutionResult 字段）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedHermesOutput {
    pub response: Option<String>,
    pub session_id: Option<String>,
    pub usage: Option<UsageSummary>,
    pub cost_usd: Option<f64>,
    pub error_message: Option<String>,
}

/// 解析合并后的输出。
///
/// `stdout` + `stderr` 合并后再扫一遍正则（与 Node 行为一致）；`stderr`
/// 单独走"错误行过滤"。
pub fn parse_hermes_output(stdout: &str, stderr: &str) -> ParsedHermesOutput {
    let mut result = ParsedHermesOutput::default();
    let combined = format!("{stdout}\n{stderr}");

    if let Some(id) = extract_session_id(&combined) {
        result.session_id = Some(id);
    }

    if let Some(captures) = regex_two_captures(&format!("(?i){TOKEN_USAGE_REGEX}"), &combined) {
        let input_tokens = captures.0.parse::<u64>().unwrap_or(0);
        let output_tokens = captures.1.parse::<u64>().unwrap_or(0);
        result.usage = Some(UsageSummary {
            input_tokens,
            output_tokens,
            cached_input_tokens: None,
        });
    }

    if let Some(captures) = regex_first_capture(&format!("(?i){COST_REGEX}"), &combined) {
        if let Ok(value) = captures.parse::<f64>() {
            result.cost_usd = Some(value);
        }
    }

    result.response = extract_response(stdout);

    if !stderr.trim().is_empty() {
        let error_lines: Vec<&str> = stderr
            .lines()
            .filter(|line| {
                let lower = line.to_ascii_lowercase();
                lower.contains("error")
                    || lower.contains("exception")
                    || lower.contains("traceback")
                    || lower.contains("failed")
            })
            .filter(|line| {
                let lower = line.to_ascii_lowercase();
                !(lower.starts_with("info")
                    || lower.starts_with("debug")
                    || lower.starts_with("warn"))
            })
            .take(5)
            .collect();
        if !error_lines.is_empty() {
            result.error_message = Some(error_lines.join("\n"));
        }
    }

    result
}

/// 提取最后一段非 thinking/tool 行（agent 的最终回复）。
///
/// Hermes 通常把回复放在最后一条以纯文本开头的行；thinking 行以 `💭` 起头，
/// tool 输出以 `┊` 起头。JSONL 事件也支持：从 `text` 或 `item.text` 抽取。
fn extract_response(stdout: &str) -> Option<String> {
    for line in stdout.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with(THINKING_PREFIX) || trimmed.starts_with(TOOL_OUTPUT_PREFIX) {
            continue;
        }
        // JSONL 单行事件
        if let Ok(event) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(text) = event.get("text").and_then(|v| v.as_str()) {
                return Some(text.to_owned());
            }
            if let Some(text) = event
                .get("item")
                .and_then(|v| v.get("text"))
                .and_then(|v| v.as_str())
            {
                return Some(text.to_owned());
            }
            if let Some(text) = event.get("content").and_then(|v| v.as_str()) {
                return Some(text.to_owned());
            }
        }
        // 跳过 session 摘要行（"Session: ..." / "Exit code: ..."），取更前面的
        if trimmed.starts_with("Session:") || trimmed.starts_with("Exit code:") {
            continue;
        }
        return Some(trimmed.to_owned());
    }
    None
}

fn regex_first_capture(pattern: &str, haystack: &str) -> Option<String> {
    let regex = regex_lite::Regex::new(pattern).ok()?;
    regex
        .captures(haystack)
        .and_then(|captures| captures.get(1).map(|m| m.as_str().to_string()))
}

fn regex_two_captures(pattern: &str, haystack: &str) -> Option<(String, String)> {
    let regex = regex_lite::Regex::new(pattern).ok()?;
    let captures = regex.captures(haystack)?;
    let first = captures.get(1)?.as_str().to_string();
    let second = captures.get(2)?.as_str().to_string();
    Some((first, second))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_session_id_quiet_modern_format() {
        // 现代 quiet mode 输出：独立 `session_id: <id>` 行
        let stdout = "response body here\nsession_id: sess-modern-001\n";
        let parsed = parse_hermes_output(stdout, "");
        assert_eq!(parsed.session_id.as_deref(), Some("sess-modern-001"));
    }

    #[test]
    fn extracts_session_id_legacy_format() {
        // legacy 格式：行内 `Session id: <id>`
        let stdout = "Session id: sess-legacy-002\n";
        let parsed = parse_hermes_output(stdout, "");
        assert_eq!(parsed.session_id.as_deref(), Some("sess-legacy-002"));
    }

    #[test]
    fn extracts_session_id_from_stderr() {
        let stderr = "session_saved: sess-in-stderr\n";
        let parsed = parse_hermes_output("", stderr);
        assert_eq!(parsed.session_id.as_deref(), Some("sess-in-stderr"));
    }

    #[test]
    fn extracts_usage_and_cost() {
        let stdout = "Response done\ntokens: 1234 input 567 output\ncost: $0.42\n";
        let parsed = parse_hermes_output(stdout, "");
        let usage = parsed.usage.expect("usage");
        assert_eq!(usage.input_tokens, 1234);
        assert_eq!(usage.output_tokens, 567);
        assert_eq!(parsed.cost_usd, Some(0.42));
    }

    #[test]
    fn extracts_response_skipping_thinking_and_tools() {
        let stdout = "\
💭 thinking about it...
┊ tool call: foo
the real answer is 42
";
        let parsed = parse_hermes_output(stdout, "");
        assert_eq!(parsed.response.as_deref(), Some("the real answer is 42"));
    }

    #[test]
    fn extracts_response_from_jsonl_event() {
        let stdout = r#"{"type":"item.completed","item":{"type":"agent_message","text":"Done"}}"#;
        let parsed = parse_hermes_output(stdout, "");
        assert_eq!(parsed.response.as_deref(), Some("Done"));
    }

    #[test]
    fn error_lines_collected_from_stderr() {
        let stderr = "\
INFO: starting
Error: real failure
DEBUG: debug noise
Exception in thread \"main\"
";
        let parsed = parse_hermes_output("", stderr);
        let err = parsed.error_message.expect("error");
        assert!(err.contains("Error:"));
        assert!(err.contains("Exception"));
        // log 噪声被剔除
        assert!(!err.contains("INFO:"));
        assert!(!err.contains("DEBUG:"));
    }

    #[test]
    fn response_returns_none_for_empty() {
        assert!(parse_hermes_output("", "").response.is_none());
    }

    #[test]
    fn skips_session_and_exit_summary_lines() {
        let stdout = "\
Session: session-xyz
the answer is here
Exit code: 0
";
        let parsed = parse_hermes_output(stdout, "");
        assert_eq!(parsed.response.as_deref(), Some("the answer is here"));
    }
}
