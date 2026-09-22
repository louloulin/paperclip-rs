//! pi 的 argv 组装与 `custom_args` 过滤（逐条对齐上游 `pi.go` L909-1092）。
//!
//! 两条硬规则：
//!
//! 1. **prompt 不进 argv**：pi 在 `-p --mode json` 下从 stdin 读 prompt。放 argv 会被
//!    npm 的 PowerShell shim 重新分词（上游 #6457）；
//! 2. **守护进程自己管理的参数不能被 `custom_args` 覆盖**：[`PI_BLOCKED_ARGS`] 里的
//!    `-p` / `--print` / `--mode` / `--session` / `--thinking` 被覆盖会直接破坏
//!    daemon↔pi 的通信协议。
//!
//! 还有一条容易忽略的：`@文件` 与裸位置参数也必须剔掉 —— pi 会把第一个位置参数
//! 拼到 stdin 的 prompt 上，等于静默篡改任务内容。

use std::path::Path;

use crate::adapter::LaunchRequest;

/// 参数是否吃值（对齐上游 `blockedArgMode`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ArgValueMode {
    /// 独立开关。
    Standalone,
    /// 必须吃下一个 token 作为值。
    WithValue,
    /// 可选值：下一个 token 不以 `-` / `@` 开头时才吃掉。
    OptionalValue,
}

/// 守护进程自己管理、不允许 `custom_args` 覆盖的参数（上游 `piBlockedArgs`）。
const PI_BLOCKED_ARGS: &[(&str, ArgValueMode)] = &[
    ("-p", ArgValueMode::Standalone),
    ("--print", ArgValueMode::Standalone),
    ("--mode", ArgValueMode::WithValue),
    ("--session", ArgValueMode::WithValue),
    ("--thinking", ArgValueMode::WithValue),
];

/// pi 0.83 的选项表（上游 `piCustomArgModes`，只用于"要不要把值一起留下"）。
const PI_CUSTOM_ARG_MODES: &[(&str, ArgValueMode)] = &[
    ("--help", ArgValueMode::Standalone),
    ("-h", ArgValueMode::Standalone),
    ("--version", ArgValueMode::Standalone),
    ("-v", ArgValueMode::Standalone),
    ("--continue", ArgValueMode::Standalone),
    ("-c", ArgValueMode::Standalone),
    ("--resume", ArgValueMode::Standalone),
    ("-r", ArgValueMode::Standalone),
    ("--provider", ArgValueMode::WithValue),
    ("--model", ArgValueMode::WithValue),
    ("--api-key", ArgValueMode::WithValue),
    ("--system-prompt", ArgValueMode::WithValue),
    ("--append-system-prompt", ArgValueMode::WithValue),
    ("--name", ArgValueMode::WithValue),
    ("-n", ArgValueMode::WithValue),
    ("--no-session", ArgValueMode::Standalone),
    ("--session-id", ArgValueMode::WithValue),
    ("--fork", ArgValueMode::WithValue),
    ("--session-dir", ArgValueMode::WithValue),
    ("--models", ArgValueMode::WithValue),
    ("--no-tools", ArgValueMode::Standalone),
    ("-nt", ArgValueMode::Standalone),
    ("--no-builtin-tools", ArgValueMode::Standalone),
    ("-nbt", ArgValueMode::Standalone),
    ("--tools", ArgValueMode::WithValue),
    ("-t", ArgValueMode::WithValue),
    ("--exclude-tools", ArgValueMode::WithValue),
    ("-xt", ArgValueMode::WithValue),
    ("--thinking", ArgValueMode::WithValue),
    ("--export", ArgValueMode::WithValue),
    ("--extension", ArgValueMode::WithValue),
    ("-e", ArgValueMode::WithValue),
    ("--no-extensions", ArgValueMode::Standalone),
    ("-ne", ArgValueMode::Standalone),
    ("--skill", ArgValueMode::WithValue),
    ("--prompt-template", ArgValueMode::WithValue),
    ("--theme", ArgValueMode::WithValue),
    ("--no-skills", ArgValueMode::Standalone),
    ("-ns", ArgValueMode::Standalone),
    ("--no-prompt-templates", ArgValueMode::Standalone),
    ("-np", ArgValueMode::Standalone),
    ("--no-themes", ArgValueMode::Standalone),
    ("--no-context-files", ArgValueMode::Standalone),
    ("-nc", ArgValueMode::Standalone),
    ("--list-models", ArgValueMode::OptionalValue),
    ("--verbose", ArgValueMode::Standalone),
    ("--approve", ArgValueMode::Standalone),
    ("-a", ArgValueMode::Standalone),
    ("--no-approve", ArgValueMode::Standalone),
    ("-na", ArgValueMode::Standalone),
    ("--offline", ArgValueMode::Standalone),
];

fn lookup(table: &[(&str, ArgValueMode)], flag: &str) -> Option<ArgValueMode> {
    table
        .iter()
        .find(|(name, _)| *name == flag)
        .map(|(_, mode)| *mode)
}

/// 组装 argv（上游 `buildPiArgs`）。
///
/// `--model` 整体透传、**不**合成 `--provider`：pi 自己的解析器接受 `provider/id`、
/// 裸 id、以及"id 里带斜杠"（网关型 provider 的常态，如 `claude/claude-opus-5`）。
/// 按第一个斜杠拆出 `--provider` 会把 id 变成 pi 没听说过的 provider 名，而未知
/// `--provider` 是硬错误，未知 `--model` 只是回退成裸 model id（上游 GH #7300）。
///
/// 也**不**传 `--tools`：不传才能用上 pi 的完整工具表（含扩展注册的工具），
/// 传了等于白名单过滤（上游 #2379）。
pub(super) fn build_args(request: &LaunchRequest, session_path: &Path) -> Vec<String> {
    let mut args = vec![
        "-p".to_owned(),
        "--mode".to_owned(),
        "json".to_owned(),
        "--session".to_owned(),
        session_path.display().to_string(),
    ];
    if let Some(model) = request
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
    {
        args.push("--model".to_owned());
        args.push(model.to_owned());
    }
    if let Some(level) = request
        .thinking_level
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        args.push("--thinking".to_owned());
        args.push(level.to_owned());
    }
    args.extend(filter_custom_args(&request.extra_args));
    args
}

/// `custom_args` 过滤（上游 `filterPiCustomArgs` = `filterCustomArgs` + 位置参数剔除）。
pub(super) fn filter_custom_args(input: &[String]) -> Vec<String> {
    strip_prompt_like_inputs(&remove_blocked_args(input))
}

/// 第一段：剔掉 [`PI_BLOCKED_ARGS`]（带值的连值一起剔）。
fn remove_blocked_args(input: &[String]) -> Vec<String> {
    let mut kept = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let arg = unshell_quote_arg(&input[i]);
        let (flag, has_inline_value) = split_flag(&arg);
        let Some(mode) = lookup(PI_BLOCKED_ARGS, flag) else {
            kept.push(arg);
            i += 1;
            continue;
        };
        // 被封锁：连同它的值一起丢掉。
        if mode == ArgValueMode::WithValue && !has_inline_value {
            i += 2;
            continue;
        }
        if mode == ArgValueMode::OptionalValue
            && !has_inline_value
            && i + 1 < input.len()
            && !input[i + 1].starts_with('-')
        {
            i += 2;
            continue;
        }
        i += 1;
    }
    kept
}

/// 第二段：剔掉 `@文件` 与裸位置参数，并保留选项值（上游 `filterPiCustomArgs` 主循环）。
fn strip_prompt_like_inputs(input: &[String]) -> Vec<String> {
    let mut kept = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let arg = unshell_quote_arg(&input[i]);
        if arg.starts_with('@') || !arg.starts_with('-') {
            // `@文件` / 位置参数会篡改 stdin 的 prompt，跳过。
            i += 1;
            continue;
        }
        let (flag, has_inline_value) = split_flag(&arg);
        // 断开对 `arg` 的借用，好把 `arg` 本身推进结果里。
        let flag = flag.to_owned();
        kept.push(arg);
        i += 1;
        if has_inline_value {
            continue;
        }
        let mode = lookup(PI_CUSTOM_ARG_MODES, &flag).unwrap_or_else(|| {
            if flag.starts_with("--") {
                // 未知长参数是扩展自己的 flag，可能吃一个值。
                ArgValueMode::OptionalValue
            } else {
                // 未知短参数原样交给 pi 报错。
                ArgValueMode::Standalone
            }
        });
        let next_is_value =
            i < input.len() && !input[i].starts_with('-') && !input[i].starts_with('@');
        match mode {
            ArgValueMode::WithValue if i < input.len() => {
                kept.push(unshell_quote_arg(&input[i]));
                i += 1;
            }
            ArgValueMode::OptionalValue if next_is_value => {
                kept.push(unshell_quote_arg(&input[i]));
                i += 1;
            }
            ArgValueMode::WithValue | ArgValueMode::OptionalValue | ArgValueMode::Standalone => {}
        }
    }
    kept
}

/// `flag` / `flag=value` 拆分（上游 `strings.Index(arg, "=") > 0` 语义：位置 0 的 `=` 不算）。
fn split_flag(arg: &str) -> (&str, bool) {
    match arg.find('=') {
        Some(0) | None => (arg, false),
        Some(idx) => (&arg[..idx], true),
    }
}

/// 剥掉一层 shell 引号（上游 `unshellQuoteArg`）。
///
/// 只处理"整段被同种引号包住"和 `--flag="v"` 两种形态；`model="o3"` 这种赋值语法
/// **不动**（引号对子进程可能有语义，例如 Codex 的 `-c model="o3"`）。
fn unshell_quote_arg(arg: &str) -> String {
    if arg.starts_with('-') {
        if let Some(idx) = arg.find('=').filter(|idx| *idx > 0) {
            let value = &arg[idx + 1..];
            if let Some(unquoted) = strip_surrounding_quotes(value) {
                return format!("{}{unquoted}", &arg[..=idx]);
            }
            return arg.to_owned();
        }
    }
    strip_surrounding_quotes(arg).unwrap_or_else(|| arg.to_owned())
}

/// 同种引号成对包住时剥一层。
fn strip_surrounding_quotes(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    if bytes.len() < 2 {
        return None;
    }
    let quote = bytes[0];
    if (quote == b'\'' || quote == b'"') && bytes[bytes.len() - 1] == quote {
        return Some(value[1..value.len() - 1].to_owned());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_of(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    fn base_request() -> LaunchRequest {
        LaunchRequest::new("do the thing")
    }

    #[test]
    fn base_argv_is_protocol_exact_and_prompt_free() {
        let args = build_args(&base_request(), Path::new("/tmp/s.jsonl"));
        assert_eq!(
            args,
            args_of(&["-p", "--mode", "json", "--session", "/tmp/s.jsonl"])
        );
        assert!(
            !args.iter().any(|a| a.contains("do the thing")),
            "prompt 必须走 stdin"
        );
    }

    #[test]
    fn model_is_passed_whole_without_provider_split() {
        let request = base_request()
            .with_model("claude/claude-opus-5")
            .with_thinking_level("high");
        let args = build_args(&request, Path::new("/tmp/s.jsonl"));
        assert!(!args.contains(&"--provider".to_owned()));
        let model_at = args.iter().position(|a| a == "--model").unwrap();
        assert_eq!(args[model_at + 1], "claude/claude-opus-5");
        let thinking_at = args.iter().position(|a| a == "--thinking").unwrap();
        assert_eq!(args[thinking_at + 1], "high");
    }

    #[test]
    fn blank_model_and_thinking_are_omitted() {
        let request = base_request().with_model("   ").with_thinking_level("");
        let args = build_args(&request, Path::new("/tmp/s.jsonl"));
        assert!(!args.iter().any(|a| a == "--model" || a == "--thinking"));
    }

    #[test]
    fn protocol_critical_flags_cannot_be_overridden() {
        let request = base_request().with_extra_args(args_of(&[
            "-p",
            "--print",
            "--mode=text",
            "--session",
            "/etc/passwd",
            "--thinking",
            "low",
            "--verbose",
        ]));
        let args = build_args(&request, Path::new("/tmp/s.jsonl"));
        assert_eq!(
            args,
            args_of(&[
                "-p",
                "--mode",
                "json",
                "--session",
                "/tmp/s.jsonl",
                "--verbose"
            ])
        );
    }

    #[test]
    fn prompt_like_inputs_are_dropped() {
        // `@file` 与裸位置参数必须剔除；选项值要留下。
        let request = base_request().with_extra_args(args_of(&[
            "--tools",
            "read,write",
            "@notes.md",
            "positional prompt injection",
            "--list-models",
        ]));
        let args = build_args(&request, Path::new("/tmp/s.jsonl"));
        assert_eq!(
            args,
            args_of(&[
                "-p",
                "--mode",
                "json",
                "--session",
                "/tmp/s.jsonl",
                "--tools",
                "read,write",
                "--list-models"
            ])
        );
    }

    #[test]
    fn unknown_long_flag_keeps_optional_value_and_unknown_short_flag_keeps_nothing() {
        let args = filter_custom_args(&args_of(&["--my-extension", "value", "-Z", "value2"]));
        assert_eq!(args, args_of(&["--my-extension", "value", "-Z"]));
    }

    #[test]
    fn inline_values_are_unquoted_and_kept_attached() {
        assert_eq!(unshell_quote_arg("--flag=\"v\""), "--flag=v");
        assert_eq!(unshell_quote_arg("'plain'"), "plain");
        assert_eq!(unshell_quote_arg("model=\"o3\""), "model=\"o3\"");
        assert_eq!(unshell_quote_arg("--flag=''"), "--flag=");
        let args = filter_custom_args(&args_of(&["--theme='dark'"]));
        assert_eq!(args, args_of(&["--theme=dark"]));
    }

    #[test]
    fn split_flag_ignores_leading_equals() {
        assert_eq!(split_flag("--mode=json"), ("--mode", true));
        assert_eq!(split_flag("--mode"), ("--mode", false));
        assert_eq!(split_flag("=weird"), ("=weird", false));
    }

    #[test]
    fn optional_value_does_not_swallow_a_flag() {
        let args = filter_custom_args(&args_of(&["--list-models", "--verbose"]));
        assert_eq!(args, args_of(&["--list-models", "--verbose"]));
    }
}
