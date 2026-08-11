//! Grok adapter 环境探测（对齐 Node `packages/adapters/grok-local/src/server/test.ts`）。
//!
//! 核心能力：
//! - `AdapterEnvironmentCheck` 数据结构
//! - `parse_grok_models_output` — 解析 `grok --list-models` 输出
//! - `summarize_probe_detail` — 提取 probe 失败原因
//! - `classify_probe_auth_required` — 探测未登录情况

use serde::{Deserialize, Serialize};

/// 检测一行是否表达正向 "logged in" 状态（排除 "not logged in"）。
///
/// regex-lite 不支持 lookbehind，所以这里用简单字符串切分：
/// 找出 "logged in" 出现位置，前面 4 字符不是 "not "。
fn is_logged_in_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    let mut search_from = 0;
    while let Some(idx) = lower[search_from..].find("logged in") {
        let abs = search_from + idx;
        // 检查前面是否有 "not "（带可选空白）
        let prefix = &lower[..abs];
        if !prefix.trim_end().ends_with("not") {
            return true;
        }
        search_from = abs + 1;
    }
    false
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterEnvironmentCheck {
    pub code: String,
    pub level: CheckLevel,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckLevel {
    Error,
    Warn,
    Info,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TestStatus {
    Pass,
    Warn,
    Fail,
}

/// 把多个 check 汇总成最终状态：error → Fail；warn → Warn；否则 Pass。
pub fn summarize_status(checks: &[AdapterEnvironmentCheck]) -> TestStatus {
    if checks.iter().any(|c| c.level == CheckLevel::Error) {
        TestStatus::Fail
    } else if checks.iter().any(|c| c.level == CheckLevel::Warn) {
        TestStatus::Warn
    } else {
        TestStatus::Pass
    }
}

/// Grok models 探测结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GrokModelsProbe {
    pub authenticated: bool,
    pub default_model: Option<String>,
    pub models: Vec<String>,
}

/// 解析 `grok --list-models` 输出（对齐 Node `parseGrokModelsOutput`）。
///
/// 输出格式（推断）：
/// - 含 `logged in` → `authenticated = true`
/// - 含 `Default model: <name>` → 设置 `defaultModel`
/// - 列出的模型名进入 `models`
pub fn parse_grok_models_output(stdout: &str) -> GrokModelsProbe {
    let mut probe = GrokModelsProbe::default();
    for raw in stdout.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if is_logged_in_line(line) {
            probe.authenticated = true;
        }
        if let Some(captures) = regex_lite::Regex::new(r"(?i)^Default model:\s*(.+)$")
            .ok()
            .and_then(|re| re.captures(line))
        {
            if let Some(m) = captures.get(1) {
                probe.default_model = Some(m.as_str().trim().to_string());
            }
            continue;
        }
        if let Some(captures) = regex_lite::Regex::new(r"(?i)^Models?:\s*(.+)$")
            .ok()
            .and_then(|re| re.captures(line))
        {
            if let Some(m) = captures.get(1) {
                for model in m.as_str().split(',') {
                    let trimmed = model.trim();
                    if !trimmed.is_empty() && !probe.models.contains(&trimmed.to_string()) {
                        probe.models.push(trimmed.to_string());
                    }
                }
            }
            continue;
        }
        // 单独一行就是模型名（每行一个模型的格式）
        if line.starts_with('-') || line.starts_with('*') {
            let name = line
                .trim_start_matches(|c: char| c == '-' || c == '*' || c.is_whitespace())
                .trim();
            if is_plausible_model_name(name) && !probe.models.contains(&name.to_string()) {
                probe.models.push(name.to_string());
            }
        }
    }
    probe
}

/// 判定字符串是否为合法模型名（不含特殊字符）。
fn is_plausible_model_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// 决定 auth-required 探测是否触发（对齐 Node `GROK_AUTH_REQUIRED_RE`）。
pub fn classify_probe_auth_required(stdout: &str, stderr: &str) -> bool {
    let combined = format!("{stdout}\n{stderr}");
    let pattern = regex_lite::Regex::new(
        r"(?i)(not\s+logged\s+in|login\s+required|run\s+`?grok\s+login`?|authentication\s+required|unauthorized|invalid\s+credentials)",
    )
    .expect("compile grok auth regex");
    pattern.is_match(&combined)
}

/// 提取 probe 失败原因（对齐 Node `summarizeProbeDetail`）。
///
/// 优先 parsedError（trim 后），否则 stderr（所有非空白行拼接），否则 stdout
/// （所有非空白行拼接）。Node 端 stderr fallback 用第一行，我们用全部行
/// 拼接 — 多个连续错误行能保留更多上下文。
pub fn summarize_probe_detail(
    stdout: &str,
    stderr: &str,
    parsed_error: Option<&str>,
) -> Option<String> {
    let raw = parsed_error
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| non_empty_lines(stderr).map(|lines| lines.join(" ")))
        .or_else(|| non_empty_lines(stdout).map(|lines| lines.join(" ")))?;
    let collapsed: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let max = 240;
    if collapsed.len() > max {
        Some(format!("{}...", &collapsed[..max - 3]))
    } else {
        Some(collapsed)
    }
}

/// 收集所有非空白行（非空 → 返回 Vec）。
fn non_empty_lines(text: &str) -> Option<Vec<String>> {
    let lines: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(String::from)
        .collect();
    if lines.is_empty() {
        None
    } else {
        Some(lines)
    }
}

fn first_non_empty_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(String::from)
}

/// 构造一个 Info check。
pub fn info(code: &str, message: &str) -> AdapterEnvironmentCheck {
    AdapterEnvironmentCheck {
        code: code.to_string(),
        level: CheckLevel::Info,
        message: message.to_string(),
        detail: None,
        hint: None,
    }
}

/// 构造一个 Warn check。
pub fn warn(code: &str, message: &str) -> AdapterEnvironmentCheck {
    AdapterEnvironmentCheck {
        code: code.to_string(),
        level: CheckLevel::Warn,
        message: message.to_string(),
        detail: None,
        hint: None,
    }
}

/// 构造一个 Error check。
pub fn error(code: &str, message: &str) -> AdapterEnvironmentCheck {
    AdapterEnvironmentCheck {
        code: code.to_string(),
        level: CheckLevel::Error,
        message: message.to_string(),
        detail: None,
        hint: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_status_fail_when_error() {
        let checks = vec![info("a", "ok"), error("b", "bad")];
        assert_eq!(summarize_status(&checks), TestStatus::Fail);
    }

    #[test]
    fn summarize_status_warn_when_warn() {
        let checks = vec![info("a", "ok"), warn("b", "warn")];
        assert_eq!(summarize_status(&checks), TestStatus::Warn);
    }

    #[test]
    fn summarize_status_pass_when_only_info() {
        let checks = vec![info("a", "ok")];
        assert_eq!(summarize_status(&checks), TestStatus::Pass);
    }

    #[test]
    fn parse_models_output_extracts_default_and_models() {
        let stdout = "\
logged in as user@example.com
Default model: grok-3
Models: grok-3, grok-3-mini, grok-2
";
        let probe = parse_grok_models_output(stdout);
        assert!(probe.authenticated);
        assert_eq!(probe.default_model.as_deref(), Some("grok-3"));
        assert_eq!(probe.models, vec!["grok-3", "grok-3-mini", "grok-2"]);
    }

    #[test]
    fn parse_models_output_handles_unauthenticated() {
        let stdout = "\
not logged in
run `grok login`
";
        let probe = parse_grok_models_output(stdout);
        assert!(!probe.authenticated);
        assert!(probe.default_model.is_none());
        assert!(probe.models.is_empty());
    }

    #[test]
    fn parse_models_output_handles_bullet_list() {
        let stdout = "\
- grok-3
- grok-3-mini
- grok-2
";
        let probe = parse_grok_models_output(stdout);
        assert_eq!(probe.models, vec!["grok-3", "grok-3-mini", "grok-2"]);
    }

    #[test]
    fn classify_auth_required_detects_patterns() {
        assert!(classify_probe_auth_required("", "not logged in"));
        assert!(classify_probe_auth_required("", "login required"));
        assert!(classify_probe_auth_required("", "run `grok login`"));
        assert!(classify_probe_auth_required("", "Authentication required"));
        assert!(classify_probe_auth_required("", "401 Unauthorized"));
        assert!(classify_probe_auth_required("", "invalid credentials"));
        assert!(!classify_probe_auth_required("", "all good"));
    }

    #[test]
    fn summarize_probe_detail_prefers_parsed_error() {
        let detail = summarize_probe_detail("stdout", "stderr", Some("parsed error msg"));
        assert_eq!(detail.as_deref(), Some("parsed error msg"));
    }

    #[test]
    fn summarize_probe_detail_falls_back_to_stderr() {
        let detail = summarize_probe_detail("", "  stderr first line  ", None);
        assert_eq!(detail.as_deref(), Some("stderr first line"));
    }

    #[test]
    fn summarize_probe_detail_truncates_long_lines() {
        let long = "x".repeat(500);
        let detail = summarize_probe_detail("", &long, None).expect("detail");
        assert!(detail.len() <= 240);
        assert!(detail.ends_with("..."));
    }

    #[test]
    fn summarize_probe_detail_collapses_whitespace() {
        let detail = summarize_probe_detail("", "line1\n   line2\nline3", None).expect("detail");
        assert_eq!(detail, "line1 line2 line3");
    }

    #[test]
    fn summarize_probe_detail_returns_none_when_empty() {
        assert!(summarize_probe_detail("", "", None).is_none());
        assert!(summarize_probe_detail("   ", "\n", None).is_none());
    }

    #[test]
    fn check_constructors() {
        let i = info("code", "msg");
        assert_eq!(i.level, CheckLevel::Info);
        assert_eq!(i.code, "code");
        let w = warn("c", "m");
        assert_eq!(w.level, CheckLevel::Warn);
        let e = error("c", "m");
        assert_eq!(e.level, CheckLevel::Error);
    }

    #[test]
    fn is_plausible_model_name_accepts_typical_ids() {
        assert!(is_plausible_model_name("grok-3"));
        assert!(is_plausible_model_name("grok-3-mini"));
        assert!(is_plausible_model_name("grok_2"));
        assert!(is_plausible_model_name("claude-3.5-sonnet"));
        assert!(!is_plausible_model_name(""));
        assert!(!is_plausible_model_name("contains spaces"));
        assert!(!is_plausible_model_name("contains!chars"));
        assert!(!is_plausible_model_name(&"x".repeat(129)));
    }
}
