//! CLI 类 adapter 共用的 `extra_args` / `custom_args` 过滤。
//!
//! 逐条泛化自 [`crate::adapters::pi_local`] 的同名实现（也就是上游
//! `filterCustomArgs` + `filterPiCustomArgs` 的 Rust 版），差别只有两点：
//!
//! 1. 封锁表 / 取值表由每个 provider 自己传进来（`pi` 是 `--mode`，`claude` 是
//!    `--output-format`，`opencode` 是 `run` 子命令……）；
//! 2. 位置参数剔除可以由 provider 关掉（prompt 走 argv 的 provider 例外，
//!    见 [`crate::adapters::cli_core::PromptTransport::Argv`]）。
//!
//! 两条硬规则与 pi 一致：
//!
//! - **守护进程自己管理的参数不能被 `extra_args` 覆盖**：覆盖 `--output-format`
//!   等于改写 daemon↔CLI 的通信协议；
//! - **prompt 不进 argv**（prompt 走 stdin 的 provider）：`@文件` 与裸位置参数
//!   会被 CLI 当成 prompt 追加，等于静默篡改任务内容。

/// 参数是否吃值（对齐上游 `blockedArgMode`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgValueMode {
    /// 独立开关。
    Standalone,
    /// 必须吃下一个 token 作为值。
    WithValue,
    /// 可选值：下一个 token 不以 `-` / `@` 开头时才吃掉。
    OptionalValue,
}

/// 封锁表 + 取值表 + 是否剔除位置参数。
#[derive(Debug, Clone, Copy)]
pub struct ArgPolicy {
    /// 不允许被 `extra_args` 覆盖的参数（守护进程管理的协议参数）。
    pub blocked: &'static [(&'static str, ArgValueMode)],
    /// provider 的选项取值表（只用于"要不要把值一起留下"）。
    pub modes: &'static [(&'static str, ArgValueMode)],
    /// 是否剔除 `@文件` 与裸位置参数（prompt 走 argv 的 provider 必须为 `false`）。
    pub strip_prompt_like: bool,
}

/// 过滤 `extra_args`（上游 `filterCustomArgs` + 位置参数剔除）。
pub fn filter_extra_args(input: &[String], policy: ArgPolicy) -> Vec<String> {
    let kept = remove_blocked_args(input, policy.blocked);
    if policy.strip_prompt_like {
        strip_prompt_like_inputs(&kept, policy.modes)
    } else {
        kept
    }
}

/// 第一段：剔掉封锁参数（带值的连值一起剔）。
fn remove_blocked_args(input: &[String], blocked: &[(&str, ArgValueMode)]) -> Vec<String> {
    let mut kept = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let arg = unshell_quote_arg(&input[i]);
        let (flag, has_inline_value) = split_flag(&arg);
        let Some(mode) = lookup(blocked, flag) else {
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

/// 第二段：剔掉 `@文件` 与裸位置参数，并保留选项值。
fn strip_prompt_like_inputs(input: &[String], modes: &[(&str, ArgValueMode)]) -> Vec<String> {
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
        let mode = lookup(modes, &flag).unwrap_or_else(|| {
            if flag.starts_with("--") {
                // 未知长参数是扩展自己的 flag，可能吃一个值。
                ArgValueMode::OptionalValue
            } else {
                // 未知短参数原样交给 CLI 报错。
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

fn lookup(table: &[(&str, ArgValueMode)], flag: &str) -> Option<ArgValueMode> {
    table
        .iter()
        .find(|(name, _)| *name == flag)
        .map(|(_, mode)| *mode)
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

/// 取非空字符串（空白串按"没给"处理）。
pub fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// 追加 `--flag value`（`value` 为空则整对跳过）。
pub fn push_flag_value(args: &mut Vec<String>, flag: &str, value: Option<&str>) {
    if let Some(value) = non_empty(value) {
        args.push(flag.to_owned());
        args.push(value.to_owned());
    }
}

/// 追加 `--flag`（`on` 为假则跳过）。
pub fn push_flag(args: &mut Vec<String>, flag: &str, on: bool) {
    if on {
        args.push(flag.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCKED: &[(&str, ArgValueMode)] = &[
        ("--mode", ArgValueMode::WithValue),
        ("-p", ArgValueMode::Standalone),
    ];
    const MODES: &[(&str, ArgValueMode)] = &[
        ("--tools", ArgValueMode::WithValue),
        ("--list-models", ArgValueMode::OptionalValue),
    ];

    fn args_of(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    fn policy(strip: bool) -> ArgPolicy {
        ArgPolicy {
            blocked: BLOCKED,
            modes: MODES,
            strip_prompt_like: strip,
        }
    }

    #[test]
    fn blocked_args_and_their_values_are_dropped() {
        let kept = filter_extra_args(
            &args_of(&["--mode=text", "--mode", "json", "-p", "--verbose"]),
            policy(true),
        );
        assert_eq!(kept, args_of(&["--verbose"]));
    }

    #[test]
    fn prompt_like_inputs_are_dropped_when_asked() {
        let kept = filter_extra_args(
            &args_of(&[
                "--tools",
                "read,write",
                "@notes.md",
                "injected",
                "--list-models",
            ]),
            policy(true),
        );
        assert_eq!(kept, args_of(&["--tools", "read,write", "--list-models"]));
    }

    #[test]
    fn prompt_like_inputs_survive_when_argv_carries_the_prompt() {
        let kept = filter_extra_args(
            &args_of(&["--tools", "read", "positional-value"]),
            policy(false),
        );
        assert_eq!(kept, args_of(&["--tools", "read", "positional-value"]));
    }

    #[test]
    fn unknown_long_flag_keeps_optional_value_and_unknown_short_keeps_nothing() {
        let kept = filter_extra_args(
            &args_of(&["--my-extension", "value", "-Z", "value2"]),
            policy(true),
        );
        assert_eq!(kept, args_of(&["--my-extension", "value", "-Z"]));
    }

    #[test]
    fn inline_values_are_unquoted_and_kept_attached() {
        let kept = filter_extra_args(&args_of(&["--theme='dark'"]), policy(true));
        assert_eq!(kept, args_of(&["--theme=dark"]));
    }

    #[test]
    fn flag_helpers_skip_blank_values() {
        let mut args = Vec::new();
        push_flag_value(&mut args, "--model", Some("  "));
        push_flag_value(&mut args, "--model", Some("gpt-5"));
        push_flag(&mut args, "--verbose", false);
        push_flag(&mut args, "--verbose", true);
        assert_eq!(args, args_of(&["--model", "gpt-5", "--verbose"]));
    }
}
