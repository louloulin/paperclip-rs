//! Hermes CLI args 拼装 + stderr reclassification（对齐 Node
//! `execute.ts:buildCommandArgs` + `wrappedOnLog`）。
//!
//! 拼装规则（顺序敏感，对齐 Node）：
//!   `chat -q <prompt> [-Q] [-m model] [--provider provider] [-t toolsets]
//!    [--max-turns N] [-w] [--checkpoints] [-v] [--source tool]
//!    [--yolo] [--resume sessionId] [...extraArgs]`

use crate::constants::HERMES_CLI;

/// Hermes CLI 选项（对齐 Node AdapterConfigSchema 12 个字段）。
#[derive(Debug, Clone)]
pub struct HermesCommandOptions {
    pub model: Option<String>,
    pub provider: Option<String>,
    pub toolsets: Option<String>,
    pub max_turns: Option<u32>,
    pub worktree_mode: bool,
    pub checkpoints: bool,
    pub quiet: bool,
    pub verbose: bool,
    pub source: Option<String>,
    pub yolo: bool,
    pub resume_session: Option<String>,
    pub extra_args: Vec<String>,
    pub persist_session: bool,
    pub timeout_sec: u64,
    pub grace_sec: u64,
}

impl Default for HermesCommandOptions {
    fn default() -> Self {
        // 对齐 Node 默认：--source tool --yolo（agents run non-interactive）
        Self {
            model: None,
            provider: None,
            toolsets: None,
            max_turns: None,
            worktree_mode: false,
            checkpoints: false,
            quiet: false,
            verbose: false,
            source: Some("tool".to_string()),
            yolo: true,
            resume_session: None,
            extra_args: Vec::new(),
            persist_session: true,
            timeout_sec: crate::constants::DEFAULT_TIMEOUT_SEC,
            grace_sec: crate::constants::DEFAULT_GRACE_SEC,
        }
    }
}

/// 拼装完整的 `hermes chat ...` 命令 + args。`prompt` 经 `-q` 传入。
///
/// 返回 `(program, args)`：program 默认 `HERMES_CLI` 但可被覆盖。
pub fn build_hermes_command_args(
    program_override: Option<&str>,
    prompt: &str,
    options: &HermesCommandOptions,
) -> (String, Vec<String>) {
    let program = program_override.unwrap_or(HERMES_CLI).to_string();
    let mut args = vec!["chat".to_string(), "-q".to_string(), prompt.to_string()];

    if options.quiet {
        args.push("-Q".to_string());
    }
    if let Some(model) = options.model.as_deref().filter(|value| !value.is_empty()) {
        args.push("-m".to_string());
        args.push(model.to_string());
    }
    if let Some(provider) = options
        .provider
        .as_deref()
        .filter(|value| !value.is_empty() && *value != "auto")
    {
        args.push("--provider".to_string());
        args.push(provider.to_string());
    }
    if let Some(toolsets) = options
        .toolsets
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        args.push("-t".to_string());
        args.push(toolsets.to_string());
    }
    if let Some(max_turns) = options.max_turns.filter(|n| *n > 0) {
        args.push("--max-turns".to_string());
        args.push(max_turns.to_string());
    }
    if options.worktree_mode {
        args.push("-w".to_string());
    }
    if options.checkpoints {
        args.push("--checkpoints".to_string());
    }
    if options.verbose {
        args.push("-v".to_string());
    }
    if let Some(source) = options.source.as_deref().filter(|value| !value.is_empty()) {
        args.push("--source".to_string());
        args.push(source.to_string());
    }
    if options.yolo {
        args.push("--yolo".to_string());
    }
    if options.persist_session {
        if let Some(session) = options
            .resume_session
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            args.push("--resume".to_string());
            args.push(session.to_string());
        }
    }
    for extra in &options.extra_args {
        args.push(extra.clone());
    }

    (program, args)
}

/// 决定一行 stderr 是否属于"良性"日志（应被重新分类为 stdout，避免
/// Paperclip UI 渲染为红色错误）。对齐 Node `wrappedOnLog` 的判定：
/// - 结构化 timestamp 行：`[2026-08-12T...]` / `[2026/08/12 ...]`
/// - 日志级别前缀：`INFO:` / `DEBUG:` / `WARN:` / `WARNING:`
/// - MCP 注册 / 初始化消息
/// - Python 站点 / 导入噪声
pub fn is_benign_stderr_line(line: &str) -> bool {
    let trimmed = line.trim_end();
    if trimmed.is_empty() {
        return true; // 空行不会引起用户注意
    }

    // 结构化时间戳前缀
    if trimmed.starts_with('[') {
        // 接受 `[2026-08-12T...` 或 `[2026/08/12 ...`
        if let Some(after_bracket) = trimmed.strip_prefix('[') {
            if let Some(idx) = after_bracket.find(['T', '/', '-']) {
                let probe = &after_bracket[..idx.min(after_bracket.len())];
                if probe.chars().take_while(|c| c.is_ascii_digit()).count() >= 4 {
                    return true;
                }
            }
        }
    }

    // 日志级别前缀
    const LEVEL_PREFIXES: &[&str] = &["INFO:", "DEBUG:", "WARN:", "WARNING:"];
    if LEVEL_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
    {
        return true;
    }

    // 已知良性短语
    const BENIGN_PHRASES: &[&str] = &[
        "Successfully registered all tools",
        "MCP server",
        "MCP Server",
        "tool registered successfully",
        "Application initialized",
    ];
    BENIGN_PHRASES.iter().any(|phrase| trimmed.contains(phrase))
}

/// 将原始 stderr 重新分流为 `(stdout_lines, real_stderr_lines)`。
pub fn reclassify_stderr(stderr: &str) -> (Vec<String>, Vec<String>) {
    let mut stdout = Vec::new();
    let mut stderr_lines = Vec::new();
    for line in stderr.lines() {
        if is_benign_stderr_line(line) {
            stdout.push(line.to_string());
        } else {
            stderr_lines.push(line.to_string());
        }
    }
    (stdout, stderr_lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_minimal_command_with_prompt() {
        let options = HermesCommandOptions::default();
        let (program, args) = build_hermes_command_args(None, "hello", &options);
        assert_eq!(program, "hermes");
        assert_eq!(
            args,
            vec!["chat", "-q", "hello", "--source", "tool", "--yolo"]
        );
    }

    #[test]
    fn builds_full_command_with_all_options() {
        let options = HermesCommandOptions {
            model: Some("claude-3.7-sonnet".to_string()),
            provider: Some("anthropic".to_string()),
            toolsets: Some("terminal,file".to_string()),
            max_turns: Some(10),
            worktree_mode: true,
            checkpoints: true,
            quiet: true,
            verbose: true,
            source: Some("tool".to_string()),
            yolo: true,
            resume_session: Some("session-abc".to_string()),
            extra_args: vec!["--no-banner".to_string()],
            persist_session: true,
            ..HermesCommandOptions::default()
        };
        let (_, args) = build_hermes_command_args(None, "do thing", &options);
        let expected = vec![
            "chat",
            "-q",
            "do thing",
            "-Q",
            "-m",
            "claude-3.7-sonnet",
            "--provider",
            "anthropic",
            "-t",
            "terminal,file",
            "--max-turns",
            "10",
            "-w",
            "--checkpoints",
            "-v",
            "--source",
            "tool",
            "--yolo",
            "--resume",
            "session-abc",
            "--no-banner",
        ];
        assert_eq!(args, expected);
    }

    #[test]
    fn auto_provider_does_not_pass_provider_flag() {
        let options = HermesCommandOptions {
            provider: Some("auto".to_string()),
            ..HermesCommandOptions::default()
        };
        let (_, args) = build_hermes_command_args(None, "p", &options);
        assert!(
            !args.windows(2).any(|w| w[0] == "--provider"),
            "--provider flag should not appear when provider=auto"
        );
    }

    #[test]
    fn resume_session_omitted_when_persist_session_false() {
        let options = HermesCommandOptions {
            resume_session: Some("abc".to_string()),
            persist_session: false,
            ..HermesCommandOptions::default()
        };
        let (_, args) = build_hermes_command_args(None, "p", &options);
        assert!(!args.iter().any(|a| a == "--resume"));
    }

    #[test]
    fn program_override_replaces_default() {
        let options = HermesCommandOptions::default();
        let (program, _) = build_hermes_command_args(Some("/custom/hermes"), "p", &options);
        assert_eq!(program, "/custom/hermes");
    }

    #[test]
    fn benign_stderr_detection() {
        assert!(is_benign_stderr_line(
            "[2026-08-12T10:30:00] INFO: starting"
        ));
        assert!(is_benign_stderr_line("[2026/08/12 10:30:00] startup done"));
        assert!(is_benign_stderr_line("INFO: server started"));
        assert!(is_benign_stderr_line("DEBUG: loading module"));
        assert!(is_benign_stderr_line("WARN: deprecated"));
        assert!(is_benign_stderr_line("Successfully registered all tools"));
        assert!(is_benign_stderr_line("MCP server connected"));
        assert!(is_benign_stderr_line("Application initialized"));
        // 空行也是 benign（不打扰）
        assert!(is_benign_stderr_line(""));
        // 真正的错误是 stderr
        assert!(!is_benign_stderr_line("Error: failed to compile"));
        assert!(!is_benign_stderr_line("Exception in thread main"));
        assert!(!is_benign_stderr_line("Traceback (most recent call last):"));
    }

    #[test]
    fn reclassify_stderr_splits_lines() {
        let stderr = "\
[2026-08-12T10:30:00] INFO: starting
MCP server connected
Error: real failure
Traceback (most recent call last):
";
        let (stdout, stderr_lines) = reclassify_stderr(stderr);
        assert_eq!(stdout.len(), 2);
        assert_eq!(stderr_lines.len(), 2);
        assert!(stderr_lines.iter().any(|l| l.starts_with("Error")));
    }
}
