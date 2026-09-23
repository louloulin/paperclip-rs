//! `antigravity`（Google Antigravity CLI，可执行文件 `agy`）adapter —— 上游
//! `server/pkg/agent/antigravity.go`。
//!
//! # 与上游的对应关系
//!
//! | 本模块 | 上游 |
//! |---|---|
//! | [`build_args`] | `buildAntigravityArgs`（`antigravity.go` L658） |
//! | `BLOCKED` | `antigravityBlockedArgs`（L621） |
//! | [`AntigravityStreamDecoder`] | `Execute` 里的 scan 循环（L240–L330，本片自己一份，见 `stream.rs`） |
//! | [`go_duration`] | `antigravityFormatTimeout`（L737） |
//! | 运行骨架 | [`super::cli_core`]（spawn / stdout 泵 / 终态归因 / 取消 / 版本探测） |
//!
//! # 传输形态
//!
//! `agy -p <prompt> --dangerously-skip-permissions --output-format stream-json`：
//! **prompt 走 argv**（[`PromptTransport::Argv`]），`stdin` 是 `Stdio::null()`。
//! `-p` 后面那个位置参数就是提示词本身，因此 `extra_args` 过滤器**不**做
//! "剔位置参数"（[`ArgPolicy::strip_prompt_like`] = `false`）—— 上游
//! `filterCustomArgs` 同样只按 flag 表过滤，不看位置形态。
//!
//! 启动骨架（白名单表里的那一行）是 `agy -p (non-interactive)`：`cli_command()`
//! 由 `launch_header` 的第一个词推出来，因此仍是 `agy`，与本模块的 [`EXECUTABLE`]
//! 一致。
//!
//! # `--print-timeout`（不可省略）
//!
//! `agy` 的 `--print-timeout` **没有"关闭"取值，省略时默认 5 分钟**：省略等于给每
//! 一轮套上 5 分钟的刀，构建/测试一超时就整轮被砍（上游 MUL-3570）。所以本实现
//! 永远显式传：
//!
//! * `request.timeout` 有值 ⇒ 原样（渲染成 Go duration 串，如 `20m0s`）；
//! * `request.timeout` 为 `None` ⇒ [`NO_CAP_PRINT_TIMEOUT`]（`24h0m0s`，上游
//!   no-cap 哨兵）—— 语义是"agy 自己的刀一定晚于本 crate 的墙钟，stuck 的 run 交给
//!   墙钟收拾"。这里**不**去取 `CliCoreConfig::default_timeout`：`build_args` 是无
//!   状态的自由函数，拿不到 config；而 24h 正是上游在"没有配置上限"时的取值。
//! * 小于 1s 的值向上取整到 `1s`（上游 `antigravityFormatTimeout` 同款，否则 CLI
//!   会拒收这个 flag）。
//!
//! # 与上游的差异
//!
//! 1. **完全不写 `--log-file`**：上游拿这个临时日志做四件事 —— glog 行的会话 id
//!    抢救、`printmode.go: timed out after N polls` 的 print-timeout 嗅探、
//!    `agent executor error:` 的 provider 错误嗅探、空 stdout 时的 transcript 抢修。
//!    本实现四件都不做：会话 id 只从流里取，超时只认本 crate 的墙钟，正文只认流。
//!    这是**已知缺口**，逐条写在 `stream.rs` 顶部（`docs/33` 有汇总）。
//!    `--log-file` 仍在封锁表里（用户也不能自己塞）—— 上游封锁它的理由是
//!    "daemon 要用"，本实现封它是因为"本 crate 不产生也不消费这个日志"，
//!    避免出现"传了 `--log-file` 但没人读"的误导。
//! 2. **不做 `agy models` 目录校验**：上游对非空 `--model` 先查目录，不在目录里就
//!    拒绝启动（`agy` 遇到不认识的模型会静默空跑、退出 0，上游用预检把这个"空成功"
//!    变成可诊断的失败）。本 crate 的 `launch` 不做这种预检，`--model` 原样透传 ⇒
//!    遇到模型名笔误时表现为"completed 但正文为空"。
//! 3. **`filepath.Clean(cwd)` 不做**：`--add-dir` 直接传 `request.cwd`。本 crate 的
//!    cwd 来自运行时装配的绝对路径，不存在 `a/../b` 这类需要规范化的形态。
//! 4. **`ExtarArgs` / `CustomArgs` 两张表在本 crate 合一张**：上游先加
//!    `opts.ExtraArgs` 再加 `opts.CustomArgs`（两者都过同一张封锁表），
//!    [`LaunchRequest`] 只有 `extra_args`，因此只有一段（`++extra`）。
//! 5. **`--system-prompt` 不存在**：上游注释说明运行期指令通过任务工作目录里的
//!    `AGENTS.md` 交付，`agy` 没有 system-prompt 参数，本实现照抄。
//!
//! [`PromptTransport::Argv`]: super::cli_core::PromptTransport::Argv

use std::path::Path;
use std::time::Duration;

use self::stream::AntigravityStreamDecoder;
use super::cli_core::args::{filter_extra_args, push_flag_value, ArgPolicy, ArgValueMode};
use super::cli_core::{
    CliCapabilities, CliCoreConfig, CliDecoder, CliProvider, CliSpec, PromptTransport,
};
use crate::adapter::{LaunchRequest, ProtocolFamily};
use crate::catalog::AgentType;
use crate::conformance::{ConformanceScript, TestableAdapter};

pub(crate) mod stream;

/// 日志 / 错误串里的名字（也是退出码错误串的前缀）。
pub(crate) const LABEL: &str = "agy";

/// 默认可执行文件名（上游 `execPath` 缺省值就是 `agy`）。
pub(crate) const EXECUTABLE: &str = "agy";

/// 没有墙钟上限时交给 `agy` 的 `--print-timeout`（上游 `antigravityNoCapPrintTimeout`）。
/// `agy` 无上限时的 `--print-timeout`（上游 `antigravityNoCapPrintTimeout = 24h`）。
///
/// MSRV 1.80 无 `Duration::from_hours`（1.84 才有），所以只能写秒。
#[allow(clippy::duration_suboptimal_units)]
pub(crate) const NO_CAP_PRINT_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

/// 守护进程管理、不允许被 `extra_args` 覆盖的参数（逐条对齐
/// `antigravityBlockedArgs`，含注释里的理由）。
const BLOCKED: &[(&str, ArgValueMode)] = &[
    ("-p", ArgValueMode::WithValue),
    ("--print", ArgValueMode::WithValue),
    ("--prompt", ArgValueMode::WithValue),
    // 交互模式要 TTY，daemon 下跑不了。
    ("-i", ArgValueMode::Standalone),
    ("--prompt-interactive", ArgValueMode::Standalone),
    // 恢复会话走 `--conversation`，不走 `--continue`。
    ("-c", ArgValueMode::Standalone),
    ("--continue", ArgValueMode::Standalone),
    ("--conversation", ArgValueMode::WithValue),
    ("--model", ArgValueMode::WithValue),
    // 用量口径要 stream-json，换成别的格式就没有 token 记账。
    ("--output-format", ArgValueMode::WithValue),
    ("--print-timeout", ArgValueMode::WithValue),
    ("--dangerously-skip-permissions", ArgValueMode::Standalone),
    // 见模块文档差异第 1 条：本实现不产生也不消费这个日志。
    ("--log-file", ArgValueMode::WithValue),
    // 这是 Claude Code 的参数，`agy` 会直接拒收。
    ("--settings", ArgValueMode::WithValue),
];

/// prompt 走 argv ⇒ 过滤器**不**剔位置参数（上游同款：只听 flag 表）。
const POLICY: ArgPolicy = ArgPolicy {
    blocked: BLOCKED,
    modes: &[],
    strip_prompt_like: false,
};

/// 组装 argv（顺序逐条对齐 `buildAntigravityArgs`，便于对照上游日志）。
///
/// ```text
/// agy -p <prompt> --dangerously-skip-permissions --output-format stream-json
///     [--model <m>] --print-timeout <duration>
///     [--conversation <sid>] [--add-dir <cwd>] ++extra
/// ```
pub fn build_args(request: &LaunchRequest) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-p".to_owned(),
        request.prompt.clone(),
        "--dangerously-skip-permissions".to_owned(),
        "--output-format".to_owned(),
        "stream-json".to_owned(),
    ];
    push_flag_value(&mut args, "--model", request.model.as_deref());
    args.push("--print-timeout".to_owned());
    args.push(go_duration(print_timeout(request.timeout)));
    push_flag_value(
        &mut args,
        "--conversation",
        request.resume_session.as_deref(),
    );
    push_flag_value(
        &mut args,
        "--add-dir",
        request.cwd.as_deref().and_then(Path::to_str),
    );
    args.extend(filter_extra_args(&request.extra_args, POLICY));
    args
}

/// `--print-timeout` 的取值：配置的墙钟上限，否则 no-cap 哨兵，下界 `1s`。
fn print_timeout(timeout: Option<Duration>) -> Duration {
    let timeout = timeout.unwrap_or(NO_CAP_PRINT_TIMEOUT);
    timeout.max(Duration::from_secs(1))
}

/// 把时长渲染成 Go `time.Duration.String()` 的形状（`agy` 用 Go 的
/// `flag.Duration` 接收，认的就是这个形状）。
///
/// 对齐上游 `antigravityFormatTimeout`：小于 1s 向上取整到 `1s`，其余交给 Go 的
/// 渲染规则 —— `20m0s` / `1h30m0s` / `1.5s` / `500ms`（秒以下按 ms/µs/ns 选单位）。
/// 秒级以上**永远带秒**（`5m0s` 而不是 `5m`）。
pub(crate) fn go_duration(duration: Duration) -> String {
    const NANO: u128 = 1;
    const MICRO: u128 = 1_000;
    const MILLI: u128 = 1_000_000;
    const SECOND: u128 = 1_000_000_000;

    let total = duration.as_nanos();
    if total == 0 {
        return "0s".to_owned();
    }
    if total < SECOND {
        let (unit, unit_nanos, precision) = if total < MICRO {
            ("ns", NANO, 0)
        } else if total < MILLI {
            ("µs", MICRO, 3)
        } else {
            ("ms", MILLI, 6)
        };
        return format!("{}{unit}", scaled(total, unit_nanos, precision));
    }
    let whole_seconds = total / SECOND;
    let minutes = whole_seconds / 60;
    // 秒位对齐 Go 的 `fmtFrac` + `u%60`：对着**分钟**取模（不打印总秒数）。
    let seconds = scaled(total % (60 * SECOND), SECOND, 9);
    if minutes == 0 {
        return format!("{seconds}s");
    }
    let hours = minutes / 60;
    if hours == 0 {
        return format!("{}m{seconds}s", minutes % 60);
    }
    format!("{hours}h{}m{seconds}s", minutes % 60)
}

/// 取 `total_nanos` 在以 `unit_nanos` 为 1 时的十进制写法，保留 `precision` 位小数并
/// **去掉尾随 0**（对齐 Go `fmtFrac` 的行为：`1.500µs` → `1.5µs`，`300ms` → `300`）。
fn scaled(total_nanos: u128, unit_nanos: u128, precision: u32) -> String {
    let scale = 10u128.pow(precision);
    let value = total_nanos * scale / unit_nanos;
    let whole = value / scale;
    let fraction = value % scale;
    if precision == 0 || fraction == 0 {
        return whole.to_string();
    }
    let mut fraction = format!("{fraction:0width$}", width = precision as usize);
    while fraction.ends_with('0') {
        fraction.pop();
    }
    format!("{whole}.{fraction}")
}

/// Antigravity adapter。
#[derive(Debug)]
pub struct Antigravity {
    config: CliCoreConfig,
}

impl Antigravity {
    /// 默认配置：可执行文件 `agy`（走 `PATH`）。
    pub fn new() -> Self {
        Self {
            config: CliCoreConfig::new(EXECUTABLE),
        }
    }

    /// 指定可执行文件（绝对路径便于测试 / 多版本共存）。
    pub fn with_executable(executable: impl Into<std::path::PathBuf>) -> Self {
        Self {
            config: CliCoreConfig::new(executable),
        }
    }

    /// 当前配置。
    pub fn config(&self) -> &CliCoreConfig {
        &self.config
    }
}

impl Default for Antigravity {
    fn default() -> Self {
        Self::new()
    }
}

impl CliProvider for Antigravity {
    fn kind(&self) -> AgentType {
        AgentType::Antigravity
    }

    fn spec(&self) -> CliSpec {
        CliSpec {
            label: LABEL,
            transport: PromptTransport::Argv,
            capabilities: CliCapabilities {
                protocol: ProtocolFamily::StreamJson,
                streaming: true,
                // `agy` 的流里没有可单独成事件的思维块（`step_update` 只透
                // `agent_response` 的文本；`thinking_tokens` 只是用量桶）。
                thinking: false,
                // 上游不为 agy 发工具事件（工具执行只在 `step_update` 的步状态里）。
                tool_events: false,
                usage_reporting: true,
                // `--conversation <id>`（上游 ResumeSessionID）。
                resume: true,
            },
            // prompt 在 argv 里，没有 stdin 可写。
            prompt_write_is_fatal: false,
            build_args,
        }
    }

    fn config(&self) -> &CliCoreConfig {
        &self.config
    }

    fn decoder_for(&self, request: &LaunchRequest) -> Box<dyn CliDecoder> {
        Box::new(AntigravityStreamDecoder::new(
            request.model.clone().unwrap_or_else(|| LABEL.to_owned()),
        ))
    }
}

impl TestableAdapter for Antigravity {
    fn with_conformance_env(executable: &Path, _workdir: &Path) -> Self {
        Self::with_executable(executable)
    }

    fn conformance_script() -> ConformanceScript {
        ConformanceScript {
            version_stdout: "agy 1.1.11\n".to_owned(),
            expected_version: Some("1.1.11".to_owned()),
            // 顺序有意义：`init` 报模型（用量归属），两步 `agent_response` 各吐一半
            // 正文（同一步会随状态变化重发，先 ACTIVE 后 DONE），DONE 快照带用量，
            // 最后 `result` 报一份**不该被采纳**的累计用量（4+6=10 才是这一轮的）。
            success_stdout: concat!(
                r#"{"event":"init","conversation_id":"agy-conv-1","init":{"model":"gemini-3.6-flash-high"}}"#,
                "\n",
                r#"{"event":"step_update","conversation_id":"agy-conv-1","step_update":{"step_index":0,"state":"active","step_type":"agent_response","text_delta":"o"}}"#,
                "\n",
                r#"{"event":"step_update","conversation_id":"agy-conv-1","step_update":{"step_index":0,"state":"done","step_type":"agent_response","text_delta":"k","usage":{"input_tokens":4,"output_tokens":6,"thinking_tokens":3,"total_tokens":10}}}"#,
                "\n",
                r#"{"event":"result","conversation_id":"agy-conv-1","result":{"status":"SUCCESS","response":"ok","usage":{"input_tokens":100,"output_tokens":100,"total_tokens":200}}}"#,
                "\n",
            )
            .to_owned(),
            expected_output: "ok".to_owned(),
            expected_usage_tokens: Some(10),
            // 纯文本行（`agy` 旧版只吐文本）与事件行混流：文本行逐行回显并补 `\n`。
            // 事件行的 `text_delta` **不**参与纯文本的换行拼接（上游同款：增量是
            // 直接追加到 output 的），所以最后一个 `ok` 紧贴第二行。
            junk_stdout: concat!(
                "这不是 JSON 的第一行\n",
                "也不是第二行\n",
                r#"{"event":"step_update","step_update":{"step_index":0,"state":"done","step_type":"agent_response","text_delta":"ok"}}"#,
                "\n",
            )
            .to_owned(),
            expected_junk_output: "这不是 JSON 的第一行\n也不是第二行ok".to_owned(),
            expected_error: "agy exploded".to_owned(),
            // prompt 走 argv。
            prompt_via_stdin: false,
        }
    }
}

#[cfg(test)]
// 用例里刻意拿「整分/整小时」的时长去验 Go 的 `time.Duration.String()` 形状；
// MSRV 1.80 无 `Duration::from_mins`，所以统一在本模块豁免这条 lint。
#[allow(clippy::duration_suboptimal_units)]
mod tests {
    use super::*;

    #[test]
    fn argv_matches_upstream_build_antigravity_args() {
        let request = LaunchRequest::new("干点活")
            .with_model("gemini-3.6-flash-high")
            .with_resume_session("agy-conv-9")
            .with_cwd("/work/dir")
            .with_timeout(Duration::from_secs(20 * 60));
        assert_eq!(
            build_args(&request),
            vec![
                "-p",
                "干点活",
                "--dangerously-skip-permissions",
                "--output-format",
                "stream-json",
                "--model",
                "gemini-3.6-flash-high",
                "--print-timeout",
                "20m0s",
                "--conversation",
                "agy-conv-9",
                "--add-dir",
                "/work/dir",
            ]
        );
    }

    #[test]
    fn print_timeout_is_always_passed_and_falls_back_to_the_no_cap_sentinel() {
        // 省略 ⇒ 24h 哨兵（agy 自己省略会默认 5 分钟把长任务砍掉）。
        let args = build_args(&LaunchRequest::new("p"));
        assert_eq!(
            args,
            vec![
                "-p",
                "p",
                "--dangerously-skip-permissions",
                "--output-format",
                "stream-json",
                "--print-timeout",
                "24h0m0s"
            ]
        );

        // 亚秒值向上取整到 1s（否则 CLI 拒收）。
        let request = LaunchRequest::new("p").with_timeout(Duration::from_millis(300));
        let args = build_args(&request);
        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "--print-timeout" && pair[1] == "1s"));
    }

    #[test]
    fn go_duration_matches_go_rendering() {
        for (duration, expected) in [
            (Duration::from_secs(5), "5s"),
            (Duration::from_secs(90), "1m30s"),
            (Duration::from_secs(5 * 60), "5m0s"),
            (Duration::from_secs(20 * 60), "20m0s"),
            (Duration::from_secs(90 * 60), "1h30m0s"),
            (NO_CAP_PRINT_TIMEOUT, "24h0m0s"),
            (Duration::from_millis(1500), "1.5s"),
            (Duration::from_micros(1_500), "1.5ms"),
            (Duration::from_millis(500), "500ms"),
            (Duration::from_micros(1), "1µs"),
            (Duration::from_nanos(500), "500ns"),
            (Duration::ZERO, "0s"),
        ] {
            assert_eq!(go_duration(duration), expected, "{duration:?}");
        }
    }

    #[test]
    fn daemon_owned_flags_cannot_be_overridden_by_extra_args() {
        let request = LaunchRequest::new("p").with_extra_args([
            "--output-format",
            "text",
            "--print-timeout",
            "5s",
            "--dangerously-skip-permissions",
            "--log-file",
            "/tmp/x.log",
            "--conversation",
            "别的会话",
            "--add-dir",
            "/other",
        ]);
        let args = build_args(&request);
        assert!(!args.contains(&"text".to_owned()));
        assert!(!args.contains(&"5s".to_owned()));
        assert!(!args.contains(&"/tmp/x.log".to_owned()));
        assert!(!args.contains(&"别的会话".to_owned()));
        assert_eq!(
            args.iter()
                .filter(|arg| *arg == "--dangerously-skip-permissions")
                .count(),
            1
        );
        // `--add-dir` / `--model` 不在封锁表里：用户自己加的保留（这是上游的口径，
        // `--add-dir` 甚至就是 daemon 自己会加的那个 flag）。
        assert_eq!(
            args.iter().filter(|arg| *arg == "--add-dir").count(),
            1,
            "{args:?}"
        );
        assert!(args.contains(&"/other".to_owned()));
    }

    #[test]
    fn the_prompt_is_the_positional_argument_after_p() {
        let args = build_args(&LaunchRequest::new("SECRET-PROMPT"));
        assert_eq!(args[0], "-p");
        assert_eq!(args[1], "SECRET-PROMPT");
        // prompt 里长得像 flag 的词不会被当成 flag 过滤掉。
        let args = build_args(&LaunchRequest::new("--yolo 别动我"));
        assert_eq!(args[1], "--yolo 别动我");
    }

    crate::adapter_conformance!(Antigravity);
}
