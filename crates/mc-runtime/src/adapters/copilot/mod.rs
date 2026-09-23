//! `copilot`（GitHub Copilot CLI）adapter —— 上游 `server/pkg/agent/copilot.go`。
//!
//! | 本模块 | 上游 |
//! |---|---|
//! | [`build_args`] | `buildCopilotArgs`（`copilot.go` L629） |
//! | `BLOCKED` | `copilotBlockedArgs`（L613） |
//! | [`CopilotDecoder`] | `handleCopilotEvent`（L151） |
//!
//! # 为什么 prompt 在 argv 里
//!
//! copilot 的一次性模式就是 `copilot -p "<prompt>" …`：prompt 是 `-p` 的值，
//! **不**走 stdin（上游 `buildCopilotArgs` 同款）。因此
//! [`ArgPolicy::strip_prompt_like`] 关掉 —— 位置参数不再需要剔除，
//! 而 `-p` 本身按 `WithValue` 屏蔽，不能让用户参数把它换掉。
//!
//! # 与上游的差异
//!
//! 1. **不注入 `--acp` / 模式切换相关参数**：上游屏蔽它们是防止用户参数把 CLI
//!    切到 ACP 模式（那是另一条协议族）；这里同样 `BLOCKED`，行为一致。
//! 2. **`--model` / `--resume` 的先后顺序与上游一致**（model 在前），
//!    这两个不屏蔽：上游的 `copilotBlockedArgs` 里没有 `--model`，而 `--resume`
//!    虽被屏蔽，但由 `ExecOptions.ResumeSessionID` 管理 —— 本 crate 的等价物是
//!    [`LaunchRequest::resume_session`]，所以它走自己的分支而不是用户参数。

mod stream;

pub(crate) use stream::CopilotDecoder;

use std::path::Path;

use super::cli_core::args::{filter_extra_args, ArgPolicy, ArgValueMode};
use super::cli_core::{
    CliCapabilities, CliCoreConfig, CliDecoder, CliProvider, CliSpec, PromptTransport,
};
use crate::adapter::{LaunchRequest, ProtocolFamily};
use crate::catalog::AgentType;
use crate::conformance::{ConformanceScript, TestableAdapter};

/// 日志 / 错误串里的名字。
pub(crate) const LABEL: &str = "copilot";

/// 守护进程硬编码、不允许被 `extra_args` 覆盖的参数（`copilotBlockedArgs`）。
const BLOCKED: &[(&str, ArgValueMode)] = &[
    ("-p", ArgValueMode::WithValue),
    ("--output-format", ArgValueMode::WithValue),
    // 一次性全自动模式：工具 / 路径 / URL 三项全开。
    ("--allow-all", ArgValueMode::Standalone),
    ("--allow-all-tools", ArgValueMode::Standalone),
    ("--allow-all-paths", ArgValueMode::Standalone),
    ("--allow-all-urls", ArgValueMode::Standalone),
    ("--yolo", ArgValueMode::Standalone),
    ("--no-ask-user", ArgValueMode::Standalone),
    // 会话续跑由 LaunchRequest::resume_session 管，不由用户参数管。
    ("--resume", ArgValueMode::WithValue),
    // 防切到 ACP 模式（那是另一条协议族）。
    ("--acp", ArgValueMode::Standalone),
];

/// 已知取值参数（prompt 在 argv ⇒ 不需要剔除位置参数）。
const MODES: &[(&str, ArgValueMode)] = &[
    ("--model", ArgValueMode::WithValue),
    ("--log-level", ArgValueMode::WithValue),
    ("--add-dir", ArgValueMode::WithValue),
    ("--agent", ArgValueMode::WithValue),
];

/// `extra_args` 过滤策略。
const POLICY: ArgPolicy = ArgPolicy {
    blocked: BLOCKED,
    modes: MODES,
    strip_prompt_like: false,
};

/// 组装 argv（`buildCopilotArgs`：prompt 在 argv 里，模型/续跑在后）。
pub fn build_args(request: &LaunchRequest) -> Vec<String> {
    let mut args = vec![
        "-p".to_owned(),
        request.prompt.clone(),
        "--output-format".to_owned(),
        "json".to_owned(),
        "--allow-all".to_owned(),
        "--no-ask-user".to_owned(),
    ];
    if let Some(model) = request.model.as_deref().filter(|model| !model.is_empty()) {
        args.push("--model".to_owned());
        args.push(model.to_owned());
    }
    if let Some(session) = request
        .resume_session
        .as_deref()
        .filter(|session| !session.is_empty())
    {
        args.push("--resume".to_owned());
        args.push(session.to_owned());
    }
    args.extend(filter_extra_args(&request.extra_args, POLICY));
    args
}

/// copilot adapter。
#[derive(Debug)]
pub struct Copilot {
    config: CliCoreConfig,
}

impl Copilot {
    /// 默认配置：可执行文件 `copilot`（走 `PATH`）。
    pub fn new() -> Self {
        Self {
            config: CliCoreConfig::new(LABEL),
        }
    }

    /// 指定可执行文件。
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

impl Default for Copilot {
    fn default() -> Self {
        Self::new()
    }
}

impl CliProvider for Copilot {
    fn kind(&self) -> AgentType {
        AgentType::Copilot
    }

    fn spec(&self) -> CliSpec {
        CliSpec {
            label: LABEL,
            transport: PromptTransport::Argv,
            capabilities: CliCapabilities {
                protocol: ProtocolFamily::JsonLine,
                streaming: true,
                // `assistant.reasoning*` 事件确实有（上游 `MessageThinking`）。
                thinking: true,
                tool_events: true,
                usage_reporting: true,
                resume: true,
            },
            // prompt 在 argv 里，没有 stdin 写入这一步。
            prompt_write_is_fatal: false,
            build_args,
        }
    }

    fn config(&self) -> &CliCoreConfig {
        &self.config
    }

    fn decoder_for(&self, request: &LaunchRequest) -> Box<dyn CliDecoder> {
        Box::new(CopilotDecoder::new(
            request.model.clone().unwrap_or_else(|| LABEL.to_owned()),
            request.resume_session.is_some(),
        ))
    }
}

impl TestableAdapter for Copilot {
    fn with_conformance_env(executable: &Path, _workdir: &Path) -> Self {
        Self::with_executable(executable)
    }

    fn conformance_script() -> ConformanceScript {
        ConformanceScript {
            version_stdout: "GitHub Copilot CLI 0.0.395.\n".to_owned(),
            expected_version: Some("0.0.395".to_owned()),
            success_stdout: concat!(
                r#"{"type":"session.start","data":{"sessionId":"copilot-ses-1","selectedModel":"gpt-5"}}"#,
                "\n",
                r#"{"type":"assistant.turn_start","data":{}}"#,
                "\n",
                r#"{"type":"assistant.message_delta","data":{"messageId":"m1","deltaContent":"o"}}"#,
                "\n",
                r#"{"type":"assistant.message_delta","data":{"messageId":"m1","deltaContent":"k"}}"#,
                "\n",
                r#"{"type":"assistant.message","data":{"messageId":"m1","model":"gpt-5","content":"ok"}}"#,
                "\n",
                r#"{"type":"assistant.usage","data":{"model":"gpt-5","inputTokens":11,"outputTokens":5,"cacheReadTokens":1,"cacheWriteTokens":0}}"#,
                "\n",
                r#"{"type":"result","sessionId":"copilot-ses-1","exitCode":0}"#,
                "\n",
            )
            .to_owned(),
            expected_output: "ok".to_owned(),
            expected_usage_tokens: Some(16),
            junk_stdout: concat!(
                "copilot: 无法解析的横幅\n",
                r#"{"type":"unknown.future","data":{"x":1}}"#,
                "\n",
                r#"{"type":"assistant.message","data":{"content":"ok"}}"#,
                "\n",
            )
            .to_owned(),
            expected_junk_output: "ok".to_owned(),
            expected_error: "copilot exploded".to_owned(),
            // prompt 在 argv 里（`-p`），套件里的 stdin 断言不适用。
            prompt_via_stdin: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_matches_upstream_build_copilot_args() {
        let request = LaunchRequest::new("干点活").with_model("gpt-5");
        assert_eq!(
            build_args(&request),
            vec![
                "-p",
                "干点活",
                "--output-format",
                "json",
                "--allow-all",
                "--no-ask-user",
                "--model",
                "gpt-5",
            ]
        );
    }

    #[test]
    fn resume_session_comes_from_the_request_not_from_extra_args() {
        let request = LaunchRequest::new("p")
            .with_resume_session("ses-1")
            .with_extra_args(["--resume", "attacker"]);
        let args = build_args(&request);
        assert_eq!(args.iter().filter(|arg| *arg == "--resume").count(), 1);
        assert_eq!(args.last().unwrap(), "ses-1");
        assert!(!args.iter().any(|arg| arg == "attacker"));
    }

    #[test]
    fn managed_flags_cannot_be_overridden() {
        let request = LaunchRequest::new("p").with_extra_args([
            "-p",
            "换掉的 prompt",
            "--output-format",
            "text",
            "--acp",
            "--yolo",
            "--allow-all-tools",
            "--log-level",
            "debug",
        ]);
        let args = build_args(&request);
        assert_eq!(args.iter().filter(|arg| *arg == "-p").count(), 1);
        assert_eq!(args[1], "p");
        assert_eq!(
            args.iter().filter(|arg| *arg == "--output-format").count(),
            1
        );
        assert_eq!(args[3], "json");
        for banned in ["--acp", "--yolo", "--allow-all-tools", "换掉的 prompt"] {
            assert!(!args.iter().any(|arg| arg == banned), "{banned} 不该出现");
        }
        // 非托管参数照旧透传。
        assert!(args.windows(2).any(|pair| pair == ["--log-level", "debug"]));
    }

    #[test]
    fn prompt_is_a_single_argv_token() {
        let prompt = "多行\nprompt --not-a-flag";
        let args = build_args(&LaunchRequest::new(prompt));
        assert_eq!(args[1], prompt);
        assert_eq!(args.len(), 6);
    }

    crate::adapter_conformance!(Copilot);
}
