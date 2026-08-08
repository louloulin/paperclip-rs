//! Claude adapter `testEnvironment` 决策表 + 纯 helpers。
//!
//! 对齐 Node `claude-local/server/test.ts`。
//!
//! # 设计范围
//!
//! 本模块只包含 **纯决策函数**，不依赖真实 I/O / 进程 / 网络：
//! - `summarize_status` — checks → status 三态
//! - `is_non_empty` — string 守卫
//! - `first_non_empty_line` — 多行文本取首行
//! - `last_non_init_stdout_line` — 跳过 `system/init` 行
//! - `truncate_detail` — 长文本截断
//! - `summarize_probe_detail` — 提取 `system/init` 第一行
//! - `can_run_probe` — 哪些错误码下跳过 hello probe
//! - `hello_probe_outcome` — 根据 probe 结果生成 check（5 个分支）
//! - `bedrock_detection` — Bedrock auth 检测
//! - `api_key_warning` — API key 覆盖 subscription 警告
//!
//! 真正运行 hello probe / sandbox install / SSH 命令的 I/O 路径在
//! `pc-acpx::execution_target` 与 `pc-adapter-process` 中已实现；
//! route 层（`pc-http/routes/agents.rs` 的 test_environment 入口）
//! 调本模块的决策函数 + pc-acpx 的执行器组合。

use serde::{Deserialize, Serialize};

/// 复刻 Node `AdapterEnvironmentCheck`。
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

/// checks → 聚合 status
pub fn summarize_status(checks: &[AdapterEnvironmentCheck]) -> TestStatus {
    if checks.iter().any(|c| c.level == CheckLevel::Error) {
        TestStatus::Fail
    } else if checks.iter().any(|c| c.level == CheckLevel::Warn) {
        TestStatus::Warn
    } else {
        TestStatus::Pass
    }
}

pub fn is_non_empty(value: Option<&str>) -> bool {
    value.map(str::trim).map(|s| !s.is_empty()).unwrap_or(false)
}

/// 从多行文本中取第一个非空行。
pub fn first_non_empty_line(text: &str) -> String {
    text.split('\n')
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_string()
}

/// 从 stream-json 输出中取最后一个**非** `system/init` 行。
pub fn last_non_init_stdout_line(text: &str) -> String {
    let lines: Vec<&str> = text
        .split('\n')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    for line in lines.iter().rev() {
        let parsed: Option<serde_json::Value> = serde_json::from_str(line).ok();
        if let Some(value) = parsed {
            let kind = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let subtype = value.get("subtype").and_then(|v| v.as_str()).unwrap_or("");
            if kind == "system" && subtype == "init" {
                continue;
            }
        }
        return (*line).to_string();
    }
    String::new()
}

/// 截断到 max 字符，超出加 `…`。
pub fn truncate_detail(value: &str, max: usize) -> String {
    let clean = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = clean.trim();
    if trimmed.chars().count() > max {
        let take: String = trimmed.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", take)
    } else {
        trimmed.to_string()
    }
}

/// 从 probe stdout/stderr 提取 `system/init` 行的 message 字段。
pub fn summarize_probe_detail(stdout: &str, stderr: &str) -> Option<String> {
    let combined = format!("{}\n{}", stdout, stderr);
    for line in combined.split('\n') {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parsed: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let kind = parsed.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let subtype = parsed.get("subtype").and_then(|v| v.as_str()).unwrap_or("");
        if kind == "system" && subtype == "init" {
            if let Some(message) = parsed.get("message").and_then(|v| v.as_str()) {
                if !message.is_empty() {
                    return Some(message.to_string());
                }
            }
        }
    }
    None
}

/// 检查 hello probe 是否应跳过（依据前置 checks 是否有错误）。
pub fn can_run_probe(checks: &[AdapterEnvironmentCheck]) -> bool {
    !checks.iter().any(|c| {
        matches!(
            c.code.as_str(),
            "claude_cwd_invalid" | "claude_command_unresolvable" | "claude_managed_config_dir_failed"
        )
    })
}

/// 是否有 Bedrock 认证。
///
/// 检查 `CLAUDE_CODE_USE_BEDROCK`、`ANTHROPIC_BEDROCK_BASE_URL` 等环境变量；
/// 当 `consider_host_env` 为 false（target 是 remote）时不读 host env。
pub fn detect_bedrock_auth(env: &std::collections::BTreeMap<String, String>, consider_host_env: bool) -> bool {
    use std::collections::BTreeMap;
    fn truthy(v: Option<&str>) -> bool {
        matches!(v.map(str::trim), Some(s) if matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
    }
    if truthy(env.get("CLAUDE_CODE_USE_BEDROCK").map(|s| s.as_str())) {
        return true;
    }
    if is_non_empty(env.get("ANTHROPIC_BEDROCK_BASE_URL").map(|s| s.as_str())) {
        return true;
    }
    if consider_host_env {
        let host_env: BTreeMap<String, String> = std::env::vars().collect();
        if truthy(host_env.get("CLAUDE_CODE_USE_BEDROCK").map(|s| s.as_str())) {
            return true;
        }
        if is_non_empty(host_env.get("ANTHROPIC_BEDROCK_BASE_URL").map(|s| s.as_str())) {
            return true;
        }
    }
    false
}

/// 是否有 ANTHROPIC_API_KEY（api key 模式）。
///
/// 与 bedrock 类似，远程 target 不读 host env。
pub fn detect_anthropic_api_key(env: &std::collections::BTreeMap<String, String>, consider_host_env: bool) -> bool {
    if is_non_empty(env.get("ANTHROPIC_API_KEY").map(|s| s.as_str())) {
        return true;
    }
    if consider_host_env {
        if let Ok(value) = std::env::var("ANTHROPIC_API_KEY") {
            if is_non_empty(Some(&value)) {
                return true;
            }
        }
    }
    false
}

/// Hello probe 决策输入。
#[derive(Debug, Clone)]
pub struct HelloProbeInput {
    pub timed_out: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// 从 `parse_claude_jsonl` 提取的 result 字段（若有）
    pub result_text: String,
    /// 是否为 transient upstream 错误
    pub transient: bool,
}

/// Hello probe 决策输出（5 分支之一）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelloProbeOutcome {
    /// 超时（warn）
    TimedOut,
    /// 需要登录（warn，含可选 login URL）
    AuthRequired { login_url: Option<String> },
    /// 通过（info）
    Passed { detail: String },
    /// 退出 0 但无 hello（warn）
    UnexpectedOutput { detail: String },
    /// 失败（error / warn depending on transient）
    Failed { detail: String, transient: bool },
}

pub fn hello_probe_outcome(input: HelloProbeInput, login_url: Option<String>) -> HelloProbeOutcome {
    if input.timed_out {
        return HelloProbeOutcome::TimedOut;
    }
    if is_login_required(&input.stdout, &input.stderr) {
        return HelloProbeOutcome::AuthRequired { login_url };
    }
    if input.exit_code == Some(0) {
        let summary = input.result_text.trim().to_string();
        let has_hello = summary.to_lowercase().contains("hello");
        if has_hello {
            return HelloProbeOutcome::Passed {
                detail: summary,
            };
        } else {
            return HelloProbeOutcome::UnexpectedOutput {
                detail: summary,
            };
        }
    }
    let detail = input.result_text;
    HelloProbeOutcome::Failed {
        detail,
        transient: input.transient,
    }
}

/// 是否需要登录。
pub fn is_login_required(stdout: &str, stderr: &str) -> bool {
    use regex_lite::Regex;
    let re = Regex::new(
        r"(?i)(?:not\s+logged\s+in|please\s+log\s+in|please\s+run\s+(?:`?claude\s+login`?|\/login)|login\s+required|requires\s+login|unauthorized|authentication\s+required|invalid\s+api\s+key[\s\S]{0,120}(?:\/login|claude\s+login|log\s+in))",
    )
    .unwrap();
    let combined = format!("{}\n{}", stdout, stderr);
    combined
        .split('\n')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .any(|line| re.is_match(&line))
}

// =============================================================================
// Tests
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    fn info(code: &str, msg: &str) -> AdapterEnvironmentCheck {
        AdapterEnvironmentCheck {
            code: code.to_string(),
            level: CheckLevel::Info,
            message: msg.to_string(),
            detail: None,
            hint: None,
        }
    }

    fn warn(code: &str, msg: &str) -> AdapterEnvironmentCheck {
        AdapterEnvironmentCheck {
            code: code.to_string(),
            level: CheckLevel::Warn,
            message: msg.to_string(),
            detail: None,
            hint: None,
        }
    }

    fn error(code: &str, msg: &str) -> AdapterEnvironmentCheck {
        AdapterEnvironmentCheck {
            code: code.to_string(),
            level: CheckLevel::Error,
            message: msg.to_string(),
            detail: None,
            hint: None,
        }
    }

    // ---- summarize_status ----

    #[test]
    fn summarize_status_pass_when_only_info() {
        let checks = vec![info("a", "x"), info("b", "y")];
        assert_eq!(summarize_status(&checks), TestStatus::Pass);
    }

    #[test]
    fn summarize_status_warn_when_any_warn() {
        let checks = vec![info("a", "x"), warn("b", "y")];
        assert_eq!(summarize_status(&checks), TestStatus::Warn);
    }

    #[test]
    fn summarize_status_fail_when_any_error() {
        let checks = vec![info("a", "x"), warn("b", "y"), error("c", "z")];
        assert_eq!(summarize_status(&checks), TestStatus::Fail);
    }

    #[test]
    fn summarize_status_empty() {
        assert_eq!(summarize_status(&[]), TestStatus::Pass);
    }

    // ---- is_non_empty ----

    #[test]
    fn is_non_empty_truthy() {
        assert!(is_non_empty(Some("hello")));
        assert!(is_non_empty(Some("  hello  ")));
    }

    #[test]
    fn is_non_empty_falsy() {
        assert!(!is_non_empty(None));
        assert!(!is_non_empty(Some("")));
        assert!(!is_non_empty(Some("   ")));
    }

    // ---- first_non_empty_line ----

    #[test]
    fn first_non_empty_line_returns_first() {
        assert_eq!(first_non_empty_line("hello\nworld"), "hello");
    }

    #[test]
    fn first_non_empty_line_skips_blank() {
        assert_eq!(first_non_empty_line("\n\n\nhello"), "hello");
    }

    #[test]
    fn first_non_empty_line_empty_input() {
        assert_eq!(first_non_empty_line(""), "");
        assert_eq!(first_non_empty_line("\n\n\n"), "");
    }

    // ---- last_non_init_stdout_line ----

    #[test]
    fn last_non_init_skips_init_events() {
        let stdout = r#"{"type":"system","subtype":"init","message":"hi"}
{"type":"assistant","message":"first"}
{"type":"system","subtype":"init","message":"other"}
{"type":"result","result":"final result"}"#;
        assert_eq!(
            last_non_init_stdout_line(stdout),
            r#"{"type":"result","result":"final result"}"#
        );
    }

    #[test]
    fn last_non_init_returns_empty_when_all_init() {
        let stdout = r#"{"type":"system","subtype":"init"}
{"type":"system","subtype":"init","message":"m"}"#;
        assert_eq!(last_non_init_stdout_line(stdout), "");
    }

    #[test]
    fn last_non_init_handles_non_json() {
        let stdout = "Some plain text\nMore text";
        assert_eq!(last_non_init_stdout_line(stdout), "More text");
    }

    // ---- truncate_detail ----

    #[test]
    fn truncate_detail_short() {
        assert_eq!(truncate_detail("hello", 100), "hello");
    }

    #[test]
    fn truncate_detail_long() {
        let long = "a".repeat(300);
        let truncated = truncate_detail(&long, 100);
        assert!(truncated.chars().count() <= 100);
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn truncate_detail_normalizes_whitespace() {
        let input = "hello   world\n\t\tfoo";
        assert_eq!(truncate_detail(input, 100), "hello world foo");
    }

    // ---- summarize_probe_detail ----

    #[test]
    fn summarize_probe_detail_extracts_init_message() {
        let stdout = r#"{"type":"system","subtype":"init","message":"Welcome to Claude"}
{"type":"assistant","message":"hello"}"#;
        let detail = summarize_probe_detail(stdout, "").unwrap();
        assert_eq!(detail, "Welcome to Claude");
    }

    #[test]
    fn summarize_probe_detail_searches_stderr() {
        let stdout = "";
        let stderr = r#"{"type":"system","subtype":"init","message":"From stderr"}"#;
        let detail = summarize_probe_detail(stdout, stderr).unwrap();
        assert_eq!(detail, "From stderr");
    }

    #[test]
    fn summarize_probe_detail_none_when_no_init() {
        let stdout = r#"{"type":"result","result":"ok"}"#;
        assert!(summarize_probe_detail(stdout, "").is_none());
    }

    #[test]
    fn summarize_probe_detail_skips_empty_message() {
        let stdout = r#"{"type":"system","subtype":"init","message":""}"#;
        assert!(summarize_probe_detail(stdout, "").is_none());
    }

    // ---- can_run_probe ----

    #[test]
    fn can_run_probe_true_when_no_blocking_errors() {
        let checks = vec![info("a", "x"), warn("b", "y")];
        assert!(can_run_probe(&checks));
    }

    #[test]
    fn can_run_probe_false_when_cwd_invalid() {
        let checks = vec![error("claude_cwd_invalid", "x")];
        assert!(!can_run_probe(&checks));
    }

    #[test]
    fn can_run_probe_false_when_command_unresolvable() {
        let checks = vec![error("claude_command_unresolvable", "x")];
        assert!(!can_run_probe(&checks));
    }

    #[test]
    fn can_run_probe_false_when_managed_config_dir_failed() {
        let checks = vec![error("claude_managed_config_dir_failed", "x")];
        assert!(!can_run_probe(&checks));
    }

    // ---- detect_bedrock_auth ----

    fn env_with(kv: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn bedrock_detect_via_use_bedrock_env() {
        let env = env_with(&[("CLAUDE_CODE_USE_BEDROCK", "1")]);
        assert!(detect_bedrock_auth(&env, true));
    }

    #[test]
    fn bedrock_detect_via_bedrock_base_url() {
        let env = env_with(&[("ANTHROPIC_BEDROCK_BASE_URL", "https://bedrock.example.com")]);
        assert!(detect_bedrock_auth(&env, true));
    }

    #[test]
    fn bedrock_detect_false_when_no_env() {
        let env = env_with(&[]);
        assert!(!detect_bedrock_auth(&env, true));
    }

    #[test]
    fn bedrock_detect_truthy_variants() {
        let env = env_with(&[("CLAUDE_CODE_USE_BEDROCK", "true")]);
        assert!(detect_bedrock_auth(&env, true));
        let env = env_with(&[("CLAUDE_CODE_USE_BEDROCK", "yes")]);
        assert!(detect_bedrock_auth(&env, true));
    }

    // ---- detect_anthropic_api_key ----

    #[test]
    fn api_key_detect_from_env() {
        let env = env_with(&[("ANTHROPIC_API_KEY", "sk-test")]);
        assert!(detect_anthropic_api_key(&env, true));
    }

    #[test]
    fn api_key_detect_false_when_empty() {
        let env = env_with(&[("ANTHROPIC_API_KEY", "")]);
        assert!(!detect_anthropic_api_key(&env, true));
    }

    #[test]
    fn api_key_detect_false_when_absent() {
        let env = env_with(&[]);
        assert!(!detect_anthropic_api_key(&env, true));
    }

    // ---- hello_probe_outcome ----

    #[test]
    fn hello_probe_outcome_timed_out() {
        let input = HelloProbeInput {
            timed_out: true,
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            result_text: String::new(),
            transient: false,
        };
        assert_eq!(hello_probe_outcome(input, None), HelloProbeOutcome::TimedOut);
    }

    #[test]
    fn hello_probe_outcome_auth_required() {
        let input = HelloProbeInput {
            timed_out: false,
            exit_code: Some(1),
            stdout: "Please run `claude login` to continue".to_string(),
            stderr: String::new(),
            result_text: String::new(),
            transient: false,
        };
        let outcome = hello_probe_outcome(input, Some("https://claude.ai/login".to_string()));
        assert_eq!(
            outcome,
            HelloProbeOutcome::AuthRequired {
                login_url: Some("https://claude.ai/login".to_string())
            }
        );
    }

    #[test]
    fn hello_probe_outcome_passed_with_hello() {
        let input = HelloProbeInput {
            timed_out: false,
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            result_text: "Hello, world!".to_string(),
            transient: false,
        };
        assert_eq!(
            hello_probe_outcome(input, None),
            HelloProbeOutcome::Passed {
                detail: "Hello, world!".to_string()
            }
        );
    }

    #[test]
    fn hello_probe_outcome_unexpected_output() {
        let input = HelloProbeInput {
            timed_out: false,
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            result_text: "Something else".to_string(),
            transient: false,
        };
        assert_eq!(
            hello_probe_outcome(input, None),
            HelloProbeOutcome::UnexpectedOutput {
                detail: "Something else".to_string()
            }
        );
    }

    #[test]
    fn hello_probe_outcome_failed_non_transient() {
        let input = HelloProbeInput {
            timed_out: false,
            exit_code: Some(1),
            stdout: String::new(),
            stderr: String::new(),
            result_text: "fatal: bad config".to_string(),
            transient: false,
        };
        assert_eq!(
            hello_probe_outcome(input, None),
            HelloProbeOutcome::Failed {
                detail: "fatal: bad config".to_string(),
                transient: false,
            }
        );
    }

    #[test]
    fn hello_probe_outcome_failed_transient() {
        let input = HelloProbeInput {
            timed_out: false,
            exit_code: Some(1),
            stdout: String::new(),
            stderr: "rate limit exceeded".to_string(),
            result_text: String::new(),
            transient: true,
        };
        assert_eq!(
            hello_probe_outcome(input, None),
            HelloProbeOutcome::Failed {
                detail: String::new(),
                transient: true,
            }
        );
    }

    // ---- is_login_required ----

    #[test]
    fn is_login_required_detects_phrases() {
        assert!(is_login_required("", "Please log in to continue"));
        assert!(is_login_required("not logged in", ""));
        assert!(is_login_required("", "please run `claude login`"));
        assert!(is_login_required("", "Login required"));
        assert!(is_login_required("", "Unauthorized"));
    }

    #[test]
    fn is_login_required_false_for_normal_output() {
        assert!(!is_login_required("Hello, world!", ""));
        assert!(!is_login_required("", "all good"));
    }
}
