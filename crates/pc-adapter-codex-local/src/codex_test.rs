//! Codex `testEnvironment` 决策表 + hello probe 决策纯函数。
//!
//! 对齐 Node `codex-local/src/server/test.ts`（446 行）。
//!
//! # 设计范围
//!
//! 本模块只包含 **纯决策函数**，不依赖真实 I/O / 进程 / 网络：
//! - `summarize_codex_probe_status` — checks → status 三态
//! - `is_non_empty` — string 守卫
//! - `first_non_empty_line` — 多行文本取首行
//! - `command_looks_like` — basename 匹配
//! - `summarize_probe_detail` — 提取 probe 错误首行
//! - `is_codex_login_required` — 登录检测
//! - `classify_codex_hello_probe` — hello probe 5 分支决策
//! - `has_hello_in_text` — 检查 probe 输出包含 "hello"
//!
//! 真正运行 hello probe / 安装命令的 I/O 路径在 `pc-acpx::execution_target`
//! 与 `pc-adapter-process` 中已实现；route 层（`pc-http/routes/adapters.rs`
//! 的 `adapter_test_environment` 入口）调本模块的决策函数 +
//! pc-acpx 的执行器组合。

use std::path::Path;

/// 复刻 Node `summarizeStatus`。
#[must_use]
pub fn summarize_codex_probe_status(checks: &[crate::acp::CodexEnvironmentCheck]) -> crate::acp::CodexEnvironmentCheckLevel {
    if checks.iter().any(|c| c.level == crate::acp::CodexEnvironmentCheckLevel::Error) {
        crate::acp::CodexEnvironmentCheckLevel::Error
    } else if checks.iter().any(|c| c.level == crate::acp::CodexEnvironmentCheckLevel::Warn) {
        crate::acp::CodexEnvironmentCheckLevel::Warn
    } else {
        crate::acp::CodexEnvironmentCheckLevel::Info
    }
}

/// checks → 聚合字符串 status（"pass" / "warn" / "fail"），对齐 Node 输出。
#[must_use]
pub fn summarize_status_str(checks: &[crate::acp::CodexEnvironmentCheck]) -> &'static str {
    match summarize_codex_probe_status(checks) {
        crate::acp::CodexEnvironmentCheckLevel::Error => "fail",
        crate::acp::CodexEnvironmentCheckLevel::Warn => "warn",
        crate::acp::CodexEnvironmentCheckLevel::Info => "pass",
    }
}

/// 字符串非空守卫（对齐 Node `isNonEmpty`）。
#[must_use]
pub fn is_non_empty(value: Option<&str>) -> bool {
    value.map(str::trim).map(|s| !s.is_empty()).unwrap_or(false)
}

/// 从多行文本中取第一个非空行（对齐 Node `firstNonEmptyLine`）。
#[must_use]
pub fn first_non_empty_line(text: &str) -> String {
    text.split('\n')
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_string()
}

/// 检查 command 的 basename 是否匹配 expected（含 .cmd / .exe 后缀）。
///
/// 复刻 Node `commandLooksLike`，处理 Windows 的 `codex.cmd` / `codex.exe`。
/// 手动按 `/` 和 `\` 分割以兼容两种路径风格（Node path.basename 同时处理两种）。
#[must_use]
pub fn command_looks_like(command: &str, expected: &str) -> bool {
    // 手动按 `/` 或 `\` 分割取最后一段（兼容 Windows / Unix 路径）
    let base = command
        .rsplit(|c| c == '/' || c == '\\')
        .next()
        .unwrap_or(command)
        .to_lowercase();
    let exp_lower = expected.to_lowercase();
    base == exp_lower || base == format!("{exp_lower}.cmd") || base == format!("{exp_lower}.exe")
}

/// 从 stdout / stderr / parsed_error 提取 probe 失败摘要。
///
/// 优先级：parsed_error > stderr > stdout。
/// 折叠所有空白为单空格（对齐 Node `replace(/\s+/g, " ")`），截断到 240 字符。
#[must_use]
pub fn summarize_probe_detail(
    stdout: &str,
    stderr: &str,
    parsed_error: Option<&str>,
) -> Option<String> {
    let raw = parsed_error
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
        .or_else(|| {
            if !stderr.trim().is_empty() {
                Some(stderr.to_string())
            } else if !stdout.trim().is_empty() {
                Some(stdout.to_string())
            } else {
                None
            }
        })?;
    // 折叠所有空白字符（包括 \n、\t、连续空格）为单空格
    let clean: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if clean.chars().count() > 240 {
        let truncated: String = clean.chars().take(239).collect();
        Some(format!("{truncated}…"))
    } else {
        Some(clean)
    }
}

/// Codex hello probe 决策输入（对齐 Node runAdapterExecutionTargetProcess 输出）。
#[derive(Debug, Clone)]
pub struct CodexHelloProbeInput {
    pub timed_out: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// 来自 `parse_codex_jsonl` 的 errorMessage 字段
    pub error_message: Option<String>,
}

/// Codex hello probe 5 分支决策输出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexHelloProbeOutcome {
    /// 超时（warn）
    TimedOut,
    /// 退出 0 + 输出含 hello（info）
    Passed { detail: String },
    /// 退出 0 + 输出不含 hello（warn）
    UnexpectedOutput { detail: String },
    /// 需要登录（warn）
    AuthRequired,
    /// 失败（error）
    Failed { detail: String },
}

/// Codex auth-required 检测正则（对齐 Node `CODEX_AUTH_REQUIRED_RE`）。
///
/// 使用手动字符串扫描避免引入 regex 依赖（与 pc-adapter-claude-local 一致）。
#[must_use]
pub fn is_codex_login_required(text: &str) -> bool {
    let lower = text.to_lowercase();
    let needles = [
        "not logged in",
        "login required",
        "authentication required",
        "unauthorized",
        "invalid or missing api",
        "invalid api key",
        "openai_api_key",
        "api key required",
        "api_key required",
        "please run codex login",
    ];
    needles.iter().any(|needle| lower.contains(needle))
}

/// Codex hello probe 5 分支决策。
#[must_use]
pub fn classify_codex_hello_probe(input: &CodexHelloProbeInput) -> CodexHelloProbeOutcome {
    if input.timed_out {
        return CodexHelloProbeOutcome::TimedOut;
    }
    if input.exit_code == Some(0) {
        let summary = input.stdout.trim().to_string();
        if has_hello_in_text(&summary) {
            return CodexHelloProbeOutcome::Passed { detail: summary };
        }
        return CodexHelloProbeOutcome::UnexpectedOutput { detail: summary };
    }
    let evidence = format!(
        "{}\n{}\n{}",
        input.error_message.as_deref().unwrap_or(""),
        input.stdout,
        input.stderr
    );
    if is_codex_login_required(&evidence) {
        return CodexHelloProbeOutcome::AuthRequired;
    }
    let detail = summarize_probe_detail(&input.stdout, &input.stderr, input.error_message.as_deref())
        .unwrap_or_default();
    CodexHelloProbeOutcome::Failed { detail }
}

/// 检查文本是否包含 "hello" 作为单词（边界 \b，对齐 Node `/\bhello\b/i`）。
#[must_use]
pub fn has_hello_in_text(text: &str) -> bool {
    let lower = text.to_lowercase();
    // 手写单词边界检测（避免 regex 依赖）
    let bytes = lower.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'h' && lower[i..].starts_with("hello") {
            let before_ok = i == 0 || !is_word_char(bytes[i - 1]);
            let after_idx = i + 5;
            let after_ok = after_idx >= bytes.len() || !is_word_char(bytes[after_idx]);
            if before_ok && after_ok {
                return true;
            }
        }
    }
    false
}

fn is_word_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Codex hello probe 凭证探测：决定 probeApiKey 来源（config vs host env）。
///
/// 复刻 Node 内联逻辑：
/// - 优先 configOpenAiKey（adapter config env）
/// - 次选 hostOpenAiKey（host 环境）
/// - 远程 target 不读 host env（probe 会自己处理）
/// - 都为空返回 None
#[must_use]
pub fn resolve_probe_api_key<'a>(
    config_openai_key: Option<&'a str>,
    host_openai_key: Option<&'a str>,
    target_is_remote: bool,
) -> Option<&'a str> {
    if is_non_empty(config_openai_key) {
        return config_openai_key;
    }
    if target_is_remote {
        return None;
    }
    if is_non_empty(host_openai_key) {
        return host_openai_key;
    }
    None
}

/// 检查是否应该跳过 hello probe（Node test.ts 中的 canRunProbe 条件）。
///
/// 跳过条件：
/// - cwd invalid（error）
/// - command unresolvable（error）
#[must_use]
pub fn should_skip_hello_probe(checks: &[crate::acp::CodexEnvironmentCheck]) -> bool {
    !checks.iter().all(|c| {
        c.code != "codex_cwd_invalid" && c.code != "codex_command_unresolvable"
    })
}

/// 探测 token 来自 config vs host 的标识。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeApiKeySource {
    AdapterConfigEnv,
    ServerEnvironment,
    NotProvided,
}

#[must_use]
pub fn probe_api_key_source(
    config_openai_key: Option<&str>,
    host_openai_key: Option<&str>,
    target_is_remote: bool,
) -> ProbeApiKeySource {
    if is_non_empty(config_openai_key) {
        return ProbeApiKeySource::AdapterConfigEnv;
    }
    if target_is_remote {
        return ProbeApiKeySource::NotProvided;
    }
    if is_non_empty(host_openai_key) {
        return ProbeApiKeySource::ServerEnvironment;
    }
    ProbeApiKeySource::NotProvided
}


/// Codex testEnvironment 主入口决策（对齐 Node test.ts testEnvironment）。
///
/// 仅做 **纯决策**：基于 config / target / env 推断应当产生哪些 checks，
/// 不实际运行 hello probe / 安装命令。I/O 路径在调用方（pc-acpx）执行。
///
/// 返回 `(checks, should_run_probe)`：
/// - `checks`：所有累计的 `CodexEnvironmentCheck`
/// - `should_run_probe`：是否应该运行 hello probe
#[derive(Debug, Clone)]
pub struct TestEnvironmentDecision {
    pub checks: Vec<crate::acp::CodexEnvironmentCheck>,
    pub should_run_probe: bool,
    pub target_is_remote: bool,
    pub target_is_sandbox: bool,
    pub target_label: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TestEnvironmentInput<'a> {
    pub config: &'a serde_json::Map<String, serde_json::Value>,
    pub execution_target: Option<&'a pc_acpx::execution_target::AdapterExecutionTarget>,
    pub cwd: &'a str,
    pub host_env: Option<&'a serde_json::Map<String, serde_json::Value>>,
}

#[must_use]
pub fn decide_test_environment_checks(input: &TestEnvironmentInput<'_>) -> TestEnvironmentDecision {
    let mut checks: Vec<crate::acp::CodexEnvironmentCheck> = Vec::new();

    let target_is_remote = input
        .execution_target
        .is_some_and(|_| pc_acpx::execution_target::adapter_execution_target_is_remote(input.execution_target));
    let target_is_sandbox = target_is_remote
        && input
            .execution_target
            .and_then(|t| t.as_remote())
            .is_some_and(|r| matches!(r, pc_acpx::execution_target::AdapterRemoteExecutionTarget::Sandbox(_)));

    // target label
    let target_label = if target_is_remote {
        input.execution_target.cloned().map(
            |t| pc_acpx::execution_target::describe_adapter_execution_target(Some(&t)),
        )
    } else {
        None
    };

    if let Some(label) = &target_label {
        checks.push(
            crate::acp::CodexEnvironmentCheck::new(
                "codex_environment_target",
                crate::acp::CodexEnvironmentCheckLevel::Info,
                format!("Probing inside environment: {label}"),
            ),
        );
    }

    // cwd 校验：调用方负责实际 fs 检查，本决策只声明"应当 valid"
    if input.cwd.is_empty() {
        checks.push(
            crate::acp::CodexEnvironmentCheck::new(
                "codex_cwd_invalid",
                crate::acp::CodexEnvironmentCheckLevel::Error,
                "Working directory is empty",
            )
            .with_detail(input.cwd.to_string()),
        );
    } else {
        checks.push(
            crate::acp::CodexEnvironmentCheck::new(
                "codex_cwd_valid",
                crate::acp::CodexEnvironmentCheckLevel::Info,
                format!("Working directory is valid: {}", input.cwd),
            ),
        );
    }

    // command 从 config 中提取
    let command = input
        .config
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("codex");

    // OPENAI_API_KEY 检测
    let config_openai_key = input
        .config
        .get("env")
        .and_then(|v| v.get("OPENAI_API_KEY"))
        .and_then(|v| v.as_str());
    let host_openai_key = if !target_is_remote {
        input
            .host_env
            .and_then(|env| env.get("OPENAI_API_KEY"))
            .and_then(|v| v.as_str())
    } else {
        None
    };

    let source = probe_api_key_source(config_openai_key, host_openai_key, target_is_remote);
    match source {
        ProbeApiKeySource::AdapterConfigEnv => {
            checks.push(
                crate::acp::CodexEnvironmentCheck::new(
                    "codex_openai_api_key_present",
                    crate::acp::CodexEnvironmentCheckLevel::Info,
                    "OPENAI_API_KEY is set for Codex authentication.",
                )
                .with_detail("Detected in adapter config env."),
            );
        }
        ProbeApiKeySource::ServerEnvironment => {
            checks.push(
                crate::acp::CodexEnvironmentCheck::new(
                    "codex_openai_api_key_present",
                    crate::acp::CodexEnvironmentCheckLevel::Info,
                    "OPENAI_API_KEY is set for Codex authentication.",
                )
                .with_detail("Detected in server environment."),
            );
        }
        ProbeApiKeySource::NotProvided if !target_is_remote => {
            checks.push(
                crate::acp::CodexEnvironmentCheck::new(
                    "codex_openai_api_key_missing",
                    crate::acp::CodexEnvironmentCheckLevel::Warn,
                    "OPENAI_API_KEY is not set. Codex runs may fail until authentication is configured.",
                )
                .with_hint(
                    "Set OPENAI_API_KEY in adapter env, shell environment, or run `codex auth` to log in.",
                ),
            );
        }
        ProbeApiKeySource::NotProvided => {
            // 远程 target：probe 自己处理登录错误
        }
    }

    let should_run_probe = !should_skip_hello_probe(&checks) && command_looks_like(command, "codex");

    if !command_looks_like(command, "codex") && !input.cwd.is_empty() {
        checks.push(
            crate::acp::CodexEnvironmentCheck::new(
                "codex_hello_probe_skipped_custom_command",
                crate::acp::CodexEnvironmentCheckLevel::Info,
                "Skipped hello probe because command is not `codex`.",
            )
            .with_detail(command.to_string())
            .with_hint("Use the `codex` CLI command to run the automatic login and installation probe."),
        );
    }

    if target_is_sandbox {
        checks.push(
            crate::acp::CodexEnvironmentCheck::new(
                "codex_git_repo_check_skipped",
                crate::acp::CodexEnvironmentCheckLevel::Info,
                "Added --skip-git-repo-check for sandbox hello probes.",
            )
            .with_hint("Codex requires an explicit trust bypass in headless remote sandbox workspaces."),
        );
    }

    TestEnvironmentDecision {
        checks,
        should_run_probe,
        target_is_remote,
        target_is_sandbox,
        target_label,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::{CodexEnvironmentCheck, CodexEnvironmentCheckLevel};

    fn info(code: &str) -> CodexEnvironmentCheck {
        CodexEnvironmentCheck::new(code, CodexEnvironmentCheckLevel::Info, "msg")
    }

    fn warn(code: &str) -> CodexEnvironmentCheck {
        CodexEnvironmentCheck::new(code, CodexEnvironmentCheckLevel::Warn, "msg")
    }

    fn err(code: &str) -> CodexEnvironmentCheck {
        CodexEnvironmentCheck::new(code, CodexEnvironmentCheckLevel::Error, "msg")
    }

    // ---- summarize_status_str ----

    #[test]
    fn summarize_status_str_all_info_is_pass() {
        let checks = vec![info("a"), info("b")];
        assert_eq!(summarize_status_str(&checks), "pass");
    }

    #[test]
    fn summarize_status_str_any_warn_is_warn() {
        let checks = vec![info("a"), warn("b")];
        assert_eq!(summarize_status_str(&checks), "warn");
    }

    #[test]
    fn summarize_status_str_any_error_is_fail() {
        let checks = vec![info("a"), warn("b"), err("c")];
        assert_eq!(summarize_status_str(&checks), "fail");
    }

    #[test]
    fn summarize_status_str_empty_is_pass() {
        let checks: Vec<CodexEnvironmentCheck> = vec![];
        assert_eq!(summarize_status_str(&checks), "pass");
    }

    // ---- is_non_empty ----

    #[test]
    fn is_non_empty_returns_false_for_none() {
        assert!(!is_non_empty(None));
    }

    #[test]
    fn is_non_empty_returns_false_for_empty() {
        assert!(!is_non_empty(Some("")));
        assert!(!is_non_empty(Some("   ")));
    }

    #[test]
    fn is_non_empty_returns_true_for_non_empty() {
        assert!(is_non_empty(Some("x")));
        assert!(is_non_empty(Some("  x  ")));
    }

    // ---- first_non_empty_line ----

    #[test]
    fn first_non_empty_line_skips_blank_lines() {
        assert_eq!(first_non_empty_line("\n\n  hello  \nworld"), "hello");
    }

    #[test]
    fn first_non_empty_line_returns_empty_for_blank() {
        assert_eq!(first_non_empty_line("\n\n"), "");
    }

    // ---- command_looks_like ----

    #[test]
    fn command_looks_like_matches_bare_name() {
        assert!(command_looks_like("codex", "codex"));
        assert!(command_looks_like("/usr/local/bin/codex", "codex"));
        assert!(command_looks_like("./bin/codex", "codex"));
    }

    #[test]
    fn command_looks_like_matches_cmd_extension() {
        assert!(command_looks_like("C:\\bin\\codex.cmd", "codex"));
    }

    #[test]
    fn command_looks_like_matches_exe_extension() {
        assert!(command_looks_like("C:\\bin\\codex.exe", "codex"));
    }

    #[test]
    fn command_looks_like_is_case_insensitive() {
        assert!(command_looks_like("/bin/CODEX", "codex"));
        assert!(command_looks_like("CODEX", "codex"));
    }

    #[test]
    fn command_looks_like_rejects_mismatch() {
        assert!(!command_looks_like("claude", "codex"));
        assert!(!command_looks_like("codex-fork", "codex"));
        assert!(!command_looks_like("my-codex", "codex"));
    }

    #[test]
    fn command_looks_like_handles_empty_command() {
        // 空 command 不应匹配任何 expected
        assert!(!command_looks_like("", "codex"));
    }

    // ---- summarize_probe_detail ----

    #[test]
    fn summarize_probe_detail_prefers_parsed_error() {
        let detail = summarize_probe_detail("stdout", "stderr", Some("error from parser"));
        assert_eq!(detail.as_deref(), Some("error from parser"));
    }

    #[test]
    fn summarize_probe_detail_falls_back_to_stderr() {
        let detail = summarize_probe_detail("stdout", "\n\n  stderr first line  \n", None);
        assert_eq!(detail.as_deref(), Some("stderr first line"));
    }

    #[test]
    fn summarize_probe_detail_falls_back_to_stdout() {
        let detail = summarize_probe_detail("\n  out line  \n", "", None);
        assert_eq!(detail.as_deref(), Some("out line"));
    }

    #[test]
    fn summarize_probe_detail_returns_none_when_all_empty() {
        assert!(summarize_probe_detail("", "", None).is_none());
    }

    #[test]
    fn summarize_probe_detail_collapses_whitespace() {
        let detail = summarize_probe_detail("a   b\tc\nd", "", None);
        assert_eq!(detail.as_deref(), Some("a b c d"));
    }

    #[test]
    fn summarize_probe_detail_truncates_at_240() {
        let long = "x".repeat(500);
        let detail = summarize_probe_detail(&long, "", None);
        let d = detail.unwrap();
        // 239 chars + 1 ellipsis char = 240 chars total
        assert_eq!(d.chars().count(), 240);
        assert!(d.ends_with('…'));
    }

    // ---- is_codex_login_required ----

    #[test]
    fn login_required_detects_not_logged_in() {
        assert!(is_codex_login_required("Error: not logged in"));
    }

    #[test]
    fn login_required_detects_login_required() {
        assert!(is_codex_login_required("login required to continue"));
    }

    #[test]
    fn login_required_detects_unauthorized() {
        assert!(is_codex_login_required("HTTP 401 Unauthorized"));
    }

    #[test]
    fn login_required_detects_api_key() {
        assert!(is_codex_login_required("Invalid API key provided"));
        assert!(is_codex_login_required("OPENAI_API_KEY not set"));
        assert!(is_codex_login_required("api key required"));
    }

    #[test]
    fn login_required_detects_please_run_codex_login() {
        assert!(is_codex_login_required("please run codex login"));
    }

    #[test]
    fn login_required_returns_false_for_normal_output() {
        assert!(!is_codex_login_required("hello world"));
        assert!(!is_codex_login_required("Codex is working"));
    }

    #[test]
    fn login_required_is_case_insensitive() {
        assert!(is_codex_login_required("LOGIN REQUIRED"));
        assert!(is_codex_login_required("Unauthorized"));
    }

    // ---- has_hello_in_text ----

    #[test]
    fn has_hello_in_text_matches_bare_word() {
        assert!(has_hello_in_text("hello"));
        assert!(has_hello_in_text("Hello there"));
    }

    #[test]
    fn has_hello_in_text_matches_word_boundaries() {
        assert!(has_hello_in_text("say hello world"));
        assert!(has_hello_in_text("I said 'hello' to you"));
    }

    #[test]
    fn has_hello_in_text_rejects_substrings() {
        // "shellos" 不应该匹配（h 后跟 e 但 llo 不在正确位置）
        // "helloo" 也不应匹配（hello 后是字母 o）
        assert!(!has_hello_in_text("helloo"));
        assert!(!has_hello_in_text("shello"));
    }

    #[test]
    fn has_hello_in_text_is_case_insensitive() {
        assert!(has_hello_in_text("HELLO"));
        assert!(has_hello_in_text("Hello"));
    }

    #[test]
    fn has_hello_in_text_handles_empty() {
        assert!(!has_hello_in_text(""));
        assert!(!has_hello_in_text("   "));
    }

    // ---- classify_codex_hello_probe ----

    #[test]
    fn classify_probe_timed_out() {
        let input = CodexHelloProbeInput {
            timed_out: true,
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            error_message: None,
        };
        assert!(matches!(
            classify_codex_hello_probe(&input),
            CodexHelloProbeOutcome::TimedOut
        ));
    }

    #[test]
    fn classify_probe_exit_zero_with_hello() {
        let input = CodexHelloProbeInput {
            timed_out: false,
            exit_code: Some(0),
            stdout: "hello there".to_string(),
            stderr: String::new(),
            error_message: None,
        };
        match classify_codex_hello_probe(&input) {
            CodexHelloProbeOutcome::Passed { detail } => {
                assert_eq!(detail, "hello there");
            }
            other => panic!("expected Passed, got {:?}", other),
        }
    }

    #[test]
    fn classify_probe_exit_zero_without_hello() {
        let input = CodexHelloProbeInput {
            timed_out: false,
            exit_code: Some(0),
            stdout: "ok".to_string(),
            stderr: String::new(),
            error_message: None,
        };
        match classify_codex_hello_probe(&input) {
            CodexHelloProbeOutcome::UnexpectedOutput { detail } => {
                assert_eq!(detail, "ok");
            }
            other => panic!("expected UnexpectedOutput, got {:?}", other),
        }
    }

    #[test]
    fn classify_probe_auth_required_from_stdout() {
        let input = CodexHelloProbeInput {
            timed_out: false,
            exit_code: Some(1),
            stdout: "not logged in".to_string(),
            stderr: String::new(),
            error_message: None,
        };
        assert!(matches!(
            classify_codex_hello_probe(&input),
            CodexHelloProbeOutcome::AuthRequired
        ));
    }

    #[test]
    fn classify_probe_auth_required_from_stderr() {
        let input = CodexHelloProbeInput {
            timed_out: false,
            exit_code: Some(1),
            stdout: String::new(),
            stderr: "Unauthorized".to_string(),
            error_message: None,
        };
        assert!(matches!(
            classify_codex_hello_probe(&input),
            CodexHelloProbeOutcome::AuthRequired
        ));
    }

    #[test]
    fn classify_probe_auth_required_from_error_message() {
        let input = CodexHelloProbeInput {
            timed_out: false,
            exit_code: Some(1),
            stdout: String::new(),
            stderr: String::new(),
            error_message: Some("login required".to_string()),
        };
        assert!(matches!(
            classify_codex_hello_probe(&input),
            CodexHelloProbeOutcome::AuthRequired
        ));
    }

    #[test]
    fn classify_probe_failed_with_detail() {
        let input = CodexHelloProbeInput {
            timed_out: false,
            exit_code: Some(2),
            stdout: "output".to_string(),
            stderr: "fatal error".to_string(),
            error_message: None,
        };
        match classify_codex_hello_probe(&input) {
            CodexHelloProbeOutcome::Failed { detail } => {
                assert_eq!(detail, "fatal error");
            }
            other => panic!("expected Failed, got {:?}", other),
        }
    }

    // ---- resolve_probe_api_key ----

    #[test]
    fn resolve_probe_api_key_prefers_config() {
        assert_eq!(
            resolve_probe_api_key(Some("ck-config"), Some("ck-host"), false),
            Some("ck-config")
        );
    }

    #[test]
    fn resolve_probe_api_key_uses_host_for_local() {
        assert_eq!(
            resolve_probe_api_key(None, Some("ck-host"), false),
            Some("ck-host")
        );
    }

    #[test]
    fn resolve_probe_api_key_skips_host_for_remote() {
        // Node: 远程 target 不读 host env（probe 自己处理）
        assert_eq!(resolve_probe_api_key(None, Some("ck-host"), true), None);
    }

    #[test]
    fn resolve_probe_api_key_returns_none_when_empty() {
        assert_eq!(resolve_probe_api_key(None, None, false), None);
        assert_eq!(resolve_probe_api_key(Some(""), Some(""), false), None);
    }

    // ---- probe_api_key_source ----

    #[test]
    fn probe_api_key_source_from_config() {
        assert_eq!(
            probe_api_key_source(Some("ck"), None, false),
            ProbeApiKeySource::AdapterConfigEnv
        );
    }

    #[test]
    fn probe_api_key_source_from_server_for_local() {
        assert_eq!(
            probe_api_key_source(None, Some("ck"), false),
            ProbeApiKeySource::ServerEnvironment
        );
    }

    #[test]
    fn probe_api_key_source_not_provided_for_remote() {
        assert_eq!(
            probe_api_key_source(None, Some("ck"), true),
            ProbeApiKeySource::NotProvided
        );
    }

    // ---- should_skip_hello_probe ----

    #[test]
    fn skip_hello_probe_when_cwd_invalid() {
        let checks = vec![info("a"), err("codex_cwd_invalid")];
        assert!(should_skip_hello_probe(&checks));
    }

    #[test]
    fn skip_hello_probe_when_command_unresolvable() {
        let checks = vec![err("codex_command_unresolvable")];
        assert!(should_skip_hello_probe(&checks));
    }

    #[test]
    fn dont_skip_hello_probe_when_only_warnings() {
        let checks = vec![info("a"), warn("b")];
        assert!(!should_skip_hello_probe(&checks));
    }

    #[test]
    fn dont_skip_hello_probe_when_clean() {
        let checks = vec![info("a"), info("b")];
        assert!(!should_skip_hello_probe(&checks));
    }


    // ---- decide_test_environment_checks ----

    fn empty_config() -> serde_json::Map<String, serde_json::Value> {
        serde_json::Map::new()
    }

    #[test]
    fn decide_test_env_empty_cwd_is_error() {
        let cfg = empty_config();
        let decision = decide_test_environment_checks(&TestEnvironmentInput {
            config: &cfg,
            execution_target: None,
            cwd: "",
            host_env: None,
        });
        assert!(decision
            .checks
            .iter()
            .any(|c| c.code == "codex_cwd_invalid"));
        assert!(!decision.should_run_probe);
    }

    #[test]
    fn decide_test_env_valid_cwd_info() {
        let cfg = empty_config();
        let decision = decide_test_environment_checks(&TestEnvironmentInput {
            config: &cfg,
            execution_target: None,
            cwd: "/workspace",
            host_env: None,
        });
        assert!(decision
            .checks
            .iter()
            .any(|c| c.code == "codex_cwd_valid"));
    }

    #[test]
    fn decide_test_env_missing_openai_key_local_warns() {
        let cfg = empty_config();
        let decision = decide_test_environment_checks(&TestEnvironmentInput {
            config: &cfg,
            execution_target: None,
            cwd: "/workspace",
            host_env: None,
        });
        assert!(decision
            .checks
            .iter()
            .any(|c| c.code == "codex_openai_api_key_missing"));
    }

    #[test]
    fn decide_test_env_openai_key_in_config_info() {
        let mut cfg = empty_config();
        let mut env_obj = serde_json::Map::new();
        env_obj.insert("OPENAI_API_KEY".to_owned(), serde_json::json!("ck-xxx"));
        cfg.insert("env".to_owned(), serde_json::Value::Object(env_obj));
        let decision = decide_test_environment_checks(&TestEnvironmentInput {
            config: &cfg,
            execution_target: None,
            cwd: "/workspace",
            host_env: None,
        });
        assert!(decision
            .checks
            .iter()
            .any(|c| c.code == "codex_openai_api_key_present"));
        assert!(!decision
            .checks
            .iter()
            .any(|c| c.code == "codex_openai_api_key_missing"));
    }

    #[test]
    fn decide_test_env_default_command_runs_probe() {
        let cfg = empty_config();
        let decision = decide_test_environment_checks(&TestEnvironmentInput {
            config: &cfg,
            execution_target: None,
            cwd: "/workspace",
            host_env: None,
        });
        assert!(decision.should_run_probe);
        // No custom command skip notice
        assert!(!decision
            .checks
            .iter()
            .any(|c| c.code == "codex_hello_probe_skipped_custom_command"));
    }

    #[test]
    fn decide_test_env_custom_command_skips_probe() {
        let mut cfg = empty_config();
        cfg.insert("command".to_owned(), serde_json::json!("my-codex-fork"));
        let decision = decide_test_environment_checks(&TestEnvironmentInput {
            config: &cfg,
            execution_target: None,
            cwd: "/workspace",
            host_env: None,
        });
        assert!(!decision.should_run_probe);
        assert!(decision
            .checks
            .iter()
            .any(|c| c.code == "codex_hello_probe_skipped_custom_command"));
    }

    #[test]
    fn decide_test_env_cwd_invalid_skips_probe() {
        let cfg = empty_config();
        let decision = decide_test_environment_checks(&TestEnvironmentInput {
            config: &cfg,
            execution_target: None,
            cwd: "",
            host_env: None,
        });
        assert!(!decision.should_run_probe);
    }

    #[test]
    fn decide_test_env_summary_status_aggregates() {
        let cfg = empty_config();
        let decision = decide_test_environment_checks(&TestEnvironmentInput {
            config: &cfg,
            execution_target: None,
            cwd: "/workspace",
            host_env: None,
        });
        // Should be at least "warn" since openai_key_missing
        let status = summarize_status_str(&decision.checks);
        assert!(
            status == "warn" || status == "pass",
            "expected warn or pass, got {status:?}"
        );
    }
    // ---- summarize_codex_probe_status enum ----

    #[test]
    fn summarize_probe_status_enum_returns_error() {
        let checks = vec![info("a"), err("b")];
        assert_eq!(
            summarize_codex_probe_status(&checks),
            CodexEnvironmentCheckLevel::Error
        );
    }

    #[test]
    fn summarize_probe_status_enum_returns_warn() {
        let checks = vec![info("a"), warn("b")];
        assert_eq!(
            summarize_codex_probe_status(&checks),
            CodexEnvironmentCheckLevel::Warn
        );
    }

    #[test]
    fn summarize_probe_status_enum_returns_info() {
        let checks = vec![info("a"), info("b")];
        assert_eq!(
            summarize_codex_probe_status(&checks),
            CodexEnvironmentCheckLevel::Info
        );
    }
}
