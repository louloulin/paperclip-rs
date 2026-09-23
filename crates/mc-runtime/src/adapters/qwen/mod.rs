//! `qwen`（Qwen Code CLI）adapter —— 上游 `server/pkg/agent/qwen.go`。
//!
//! # 与上游的对应关系
//!
//! | 本模块 | 上游 |
//! |---|---|
//! | [`build_args`] | `buildQwenArgs`（`qwen.go` L66） |
//! | `BLOCKED` / `MODES` | `qwenBlockedArgs`（L29） |
//! | [`ClaudeStreamDecoder`] + `ClaudeStreamFlavor::Qwen` | `handleQwenEvent`（走 [`super::claude_family`]） |
//! | 运行骨架 | [`super::cli_core`]（`Execute` 的 spawn / stdout 循环 / 终态归因） |
//!
//! # 传输形态
//!
//! **prompt 走 stdin 的纯文本**（[`PromptTransport::StdinText`]），argv 里只有
//! 固定的、不含内容的开关。上游注释写得很直白（#6082）：Qwen Code 的 headless
//! 模式在没有 `-p` 时从 stdin 读非交互 prompt，把任意长度、受用户影响的正文放进
//! 命令行在 Windows 上过不了 PowerShell 的参数重序列化（cursor-agent 在 #5649
//! 踩过同一个坑）。因此 `-p` / `--prompt` 一类都在封锁表里，**本 adapter 自己
//! 也不加** —— 白名单里的启动骨架 `qwen -p (stream-json)` 是上游 `launchHeaders`
//! 的原样投影（历史展示串），不代表这里真的传了 `-p`。
//!
//! # 与上游的差异
//!
//! 全部记在 `docs/33` §11，逐条可查。最值得注意的一条：上游 `finalizeStreamResult`
//! 的 `output` 在 qwen 上是「最后一个 `result` 的正文」，本 crate 统一成
//! 「所有 `Text` 事件拼接」（`RunOutcome::output` 的定义，见 `adapter.rs`）。

use std::path::Path;

use super::claude_family::{ClaudeStreamDecoder, ClaudeStreamFlavor};
use super::cli_core::args::{filter_extra_args, push_flag_value, ArgPolicy, ArgValueMode};
use super::cli_core::{
    CliCapabilities, CliCoreConfig, CliDecoder, CliProvider, CliSpec, PromptTransport,
};
use crate::adapter::{LaunchRequest, ProtocolFamily};
use crate::catalog::AgentType;
use crate::conformance::{ConformanceScript, TestableAdapter};

/// 日志 / 错误串里的名字。
pub(crate) const LABEL: &str = "qwen";

/// 守护进程管理、不允许被 `extra_args` 覆盖的参数（逐条对齐 `qwenBlockedArgs`）。
///
/// 三类：① prompt 与流协议（`-p` / `--prompt` / `-o` / `--output-format`）；
/// ② 模型与会话（`-m` / `--model` / `-r` / `--resume` / `-c` / `--continue`）；
/// ③ 权限与上下文（`--yolo` / `-y` / `--approval-mode` / `--core-tools` /
/// `--safe-mode` / `--mcp-config` / `--chat-recording` / `-i` / `--prompt-interactive`）。
/// 上游注释点明后一类的意图：用户可以**收窄**能力，但不能替 daemon 关掉 bypass、
/// 也不能改写核心工具注册表（要硬禁某个工具请用 `--exclude-tools`）。
const BLOCKED: &[(&str, ArgValueMode)] = &[
    ("-p", ArgValueMode::WithValue),
    ("--prompt", ArgValueMode::WithValue),
    ("-i", ArgValueMode::WithValue),
    ("--prompt-interactive", ArgValueMode::WithValue),
    ("-o", ArgValueMode::WithValue),
    ("--output-format", ArgValueMode::WithValue),
    ("-m", ArgValueMode::WithValue),
    ("--model", ArgValueMode::WithValue),
    ("-r", ArgValueMode::WithValue),
    ("--resume", ArgValueMode::WithValue),
    ("-c", ArgValueMode::Standalone),
    ("--continue", ArgValueMode::Standalone),
    ("--chat-recording", ArgValueMode::WithValue),
    ("--mcp-config", ArgValueMode::WithValue),
    ("--safe-mode", ArgValueMode::Standalone),
    ("--yolo", ArgValueMode::Standalone),
    ("-y", ArgValueMode::Standalone),
    ("--approval-mode", ArgValueMode::WithValue),
    ("--core-tools", ArgValueMode::WithValue),
];

/// 已知的取值参数（只用于"这个 flag 后面那坨是不是它的值"）。
const MODES: &[(&str, ArgValueMode)] = &[
    ("--exclude-tools", ArgValueMode::WithValue),
    ("--include-directories", ArgValueMode::WithValue),
    ("--allowed-tools", ArgValueMode::WithValue),
];

/// `extra_args` 的过滤策略（prompt 走 stdin ⇒ 位置参数必须剔除）。
const POLICY: ArgPolicy = ArgPolicy {
    blocked: BLOCKED,
    modes: MODES,
    strip_prompt_like: true,
};

/// 组装 argv（`buildQwenArgs` 的顺序逐条保留）。
///
/// `--yolo` 由 daemon 独占：Qwen Code 的非交互模式会滤掉需要批准的工具
/// （`run_shell_command` / `edit` / `write_file` …），不开 bypass 就只剩聊天。
pub fn build_args(request: &LaunchRequest) -> Vec<String> {
    let mut args: Vec<String> = vec!["--output-format".to_owned(), "stream-json".to_owned()];
    push_flag_value(&mut args, "--model", request.model.as_deref());
    push_flag_value(&mut args, "--resume", request.resume_session.as_deref());
    args.push("--yolo".to_owned());
    args.extend(filter_extra_args(&request.extra_args, POLICY));
    args
}

/// Qwen Code adapter。
#[derive(Debug)]
pub struct Qwen {
    config: CliCoreConfig,
}

impl Qwen {
    /// 默认配置：可执行文件 `qwen`（走 `PATH`）。
    pub fn new() -> Self {
        Self {
            config: CliCoreConfig::new(LABEL),
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

impl Default for Qwen {
    fn default() -> Self {
        Self::new()
    }
}

impl CliProvider for Qwen {
    fn kind(&self) -> AgentType {
        AgentType::Qwen
    }

    fn spec(&self) -> CliSpec {
        CliSpec {
            label: LABEL,
            transport: PromptTransport::StdinText,
            capabilities: CliCapabilities {
                protocol: ProtocolFamily::StreamJson,
                streaming: true,
                thinking: true,
                tool_events: true,
                usage_reporting: true,
                resume: true,
            },
            // prompt 写完即关：写失败说明 stdin 已经不在了，run 没有必要继续。
            prompt_write_is_fatal: true,
            build_args,
        }
    }

    fn config(&self) -> &CliCoreConfig {
        &self.config
    }

    fn decoder_for(&self, request: &LaunchRequest) -> Box<dyn CliDecoder> {
        Box::new(
            ClaudeStreamDecoder::for_flavor(
                ClaudeStreamFlavor::Qwen,
                request.model.clone().unwrap_or_else(|| LABEL.to_owned()),
            )
            .with_resume(request.resume_session.is_some()),
        )
    }
}

impl TestableAdapter for Qwen {
    fn with_conformance_env(executable: &Path, _workdir: &Path) -> Self {
        Self::with_executable(executable)
    }

    fn conformance_script() -> ConformanceScript {
        ConformanceScript {
            version_stdout: "0.20.0 (Qwen Code)\n".to_owned(),
            expected_version: Some("0.20.0".to_owned()),
            success_stdout: concat!(
                r#"{"type":"system","subtype":"init","session_id":"sess-qwen-1"}"#,
                "\n",
                // assistant 累加：input 10-4(cache)=6 + output 5 + cache_read 4 = 15。
                r#"{"type":"assistant","message":{"model":"qwen3-coder","usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":4},"content":[{"type":"text","text":"ok"}]}}"#,
                "\n",
                // result 覆盖成 run 级聚合值：20 + 10 = 30。
                r#"{"type":"result","subtype":"success","session_id":"sess-qwen-1","result":"ok","is_error":false,"usage":{"input_tokens":20,"output_tokens":10}}"#,
                "\n",
            )
            .to_owned(),
            expected_output: "ok".to_owned(),
            expected_usage_tokens: Some(30),
            junk_stdout: concat!(
                "not json at all\n",
                r#"{"type":"unknown_future_event","payload":{"nested":true}}"#,
                "\n",
                r#"{"type":"assistant","message":{"model":"qwen3-coder","content":[{"type":"text","text":"ok"}]}}"#,
                "\n",
            )
            .to_owned(),
            expected_junk_output: "ok".to_owned(),
            expected_error: "qwen exploded".to_owned(),
            prompt_via_stdin: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_matches_upstream_build_qwen_args() {
        let request = LaunchRequest::new("干点活")
            .with_model("qwen3-coder")
            .with_resume_session("sess-9");
        assert_eq!(
            build_args(&request),
            vec![
                "--output-format",
                "stream-json",
                "--model",
                "qwen3-coder",
                "--resume",
                "sess-9",
                "--yolo",
            ]
        );
    }

    #[test]
    fn protocol_flags_cannot_be_overridden_by_extra_args() {
        let request = LaunchRequest::new("p").with_extra_args([
            "--output-format",
            "text",
            "--yolo",
            "--safe-mode",
            "注入的位置参数",
        ]);
        let args = build_args(&request);
        assert!(!args.contains(&"text".to_owned()));
        assert!(!args.contains(&"--safe-mode".to_owned()));
        assert!(!args.contains(&"注入的位置参数".to_owned()));
        // daemon 自己的那份 `--yolo` 还在，且只有一份。
        assert_eq!(args.iter().filter(|arg| *arg == "--yolo").count(), 1);
    }

    #[test]
    fn prompt_never_lands_in_argv() {
        let request = LaunchRequest::new("SECRET-PROMPT");
        assert!(!build_args(&request)
            .iter()
            .any(|arg| arg.contains("SECRET")));
    }

    crate::adapter_conformance!(Qwen);
}
