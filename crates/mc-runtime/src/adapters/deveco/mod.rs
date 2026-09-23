//! `deveco`（`DevEco` Code CLI）adapter —— 上游 `server/pkg/agent/deveco.go`。
//!
//! | 本模块 | 上游 |
//! |---|---|
//! | [`build_args`] | `devecoBackend.Execute`（`deveco.go` L100 起） |
//! | `BLOCKED` | `devecoBlockedArgs`（L61） |
//! | [`OpenCodeFamilyDecoder`] | deveco 的事件处理（与 `OpenCode` 同 schema） |
//!
//! # 与 opencode / codearts 的三点不同
//!
//! 1. **prompt 走 argv（位置参数，追加在最末）**：`DevEco` 的 `run` 支持位置参数
//!    形式的消息（上游 `args = append(args, prompt)`），因此不走 stdin。
//! 2. **`--dir` 是支持的**（与 opencode 同款 workdir 锚点）。
//! 3. **解码器不 fail-closed**：上游 deveco 后端没有 opencode 那套
//!    "step 未闭合 / 零事件 ⇒ 失败"的判据，干净 EOF 就是跑完了
//!    （见 [`super::opencode_family::OpenCodeFamilyDecoder`] 的 `strict` 说明）。

use std::path::Path;

use super::cli_core::args::{filter_extra_args, ArgPolicy, ArgValueMode};
use super::cli_core::{
    CliCapabilities, CliCoreConfig, CliDecoder, CliProvider, CliSpec, PromptTransport,
};
use super::opencode_family::OpenCodeFamilyDecoder;
use crate::adapter::{LaunchRequest, ProtocolFamily};
use crate::catalog::AgentType;
use crate::conformance::{ConformanceScript, TestableAdapter};

/// 日志 / 错误串里的名字。
pub(crate) const LABEL: &str = "deveco";

/// 守护进程硬编码、不允许被 `extra_args` 覆盖的参数（`devecoBlockedArgs`，
/// 与 opencode 的同名四项一致）。
const BLOCKED: &[(&str, ArgValueMode)] = &[
    ("--format", ArgValueMode::WithValue),
    ("--dir", ArgValueMode::WithValue),
    ("--variant", ArgValueMode::WithValue),
    ("--dangerously-skip-permissions", ArgValueMode::Standalone),
];

/// 已知取值参数。
const MODES: &[(&str, ArgValueMode)] = &[
    ("--model", ArgValueMode::WithValue),
    ("--session", ArgValueMode::WithValue),
    ("--agent", ArgValueMode::WithValue),
    ("--log-level", ArgValueMode::WithValue),
];

/// `extra_args` 过滤策略。
///
/// `strip_prompt_like` **关掉**：prompt 本来就是位置参数，剔除位置参数等于把自己的
/// prompt 也剔了。用户额外塞进来的位置参数会被 `DevEco` 当成消息前缀，这与其命令面
/// 一致（上游也不做处理）。
const POLICY: ArgPolicy = ArgPolicy {
    blocked: BLOCKED,
    modes: MODES,
    strip_prompt_like: false,
};

/// 组装 argv（上游 deveco 契约：prompt 追加在最末）。
pub fn build_args(request: &LaunchRequest) -> Vec<String> {
    let mut args: Vec<String> = ["run", "--format", "json", "--dangerously-skip-permissions"]
        .iter()
        .map(|arg| (*arg).to_owned())
        .collect();
    if let Some(cwd) = request.cwd.as_deref() {
        args.push("--dir".to_owned());
        args.push(cwd.display().to_string());
    }
    if let Some(model) = request.model.as_deref().filter(|model| !model.is_empty()) {
        args.push("--model".to_owned());
        args.push(model.to_owned());
    }
    if let Some(level) = request
        .thinking_level
        .as_deref()
        .filter(|level| !level.is_empty())
    {
        args.push("--variant".to_owned());
        args.push(level.to_owned());
    }
    if let Some(session) = request
        .resume_session
        .as_deref()
        .filter(|session| !session.is_empty())
    {
        args.push("--session".to_owned());
        args.push(session.to_owned());
    }
    args.extend(filter_extra_args(&request.extra_args, POLICY));
    args.push(request.prompt.clone());
    args
}

/// deveco adapter。
#[derive(Debug)]
pub struct Deveco {
    config: CliCoreConfig,
}

impl Deveco {
    /// 默认配置：可执行文件 `deveco`（走 `PATH`）。
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

impl Default for Deveco {
    fn default() -> Self {
        Self::new()
    }
}

impl CliProvider for Deveco {
    fn kind(&self) -> AgentType {
        AgentType::Deveco
    }

    fn spec(&self) -> CliSpec {
        CliSpec {
            label: LABEL,
            transport: PromptTransport::Argv,
            capabilities: CliCapabilities {
                protocol: ProtocolFamily::JsonLine,
                streaming: true,
                // 与 opencode 同 schema：流里没有推理事件。
                thinking: false,
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
        Box::new(OpenCodeFamilyDecoder::new(
            LABEL,
            request.model.clone().unwrap_or_else(|| LABEL.to_owned()),
            // deveco 没有 step 括弧 ⇒ 不启用 fail-closed 判据。
            false,
        ))
    }
}

impl TestableAdapter for Deveco {
    fn with_conformance_env(executable: &Path, _workdir: &Path) -> Self {
        Self::with_executable(executable)
    }

    fn conformance_script() -> ConformanceScript {
        ConformanceScript {
            version_stdout: "deveco 1.0.3\n".to_owned(),
            expected_version: Some("1.0.3".to_owned()),
            success_stdout: concat!(
                r#"{"type":"step_start","sessionID":"deveco-ses-1","part":{}}"#,
                "\n",
                r#"{"type":"text","sessionID":"deveco-ses-1","part":{"text":"ok"}}"#,
                "\n",
                r#"{"type":"step_finish","sessionID":"deveco-ses-1","part":{"reason":"stop","tokens":{"input":10,"output":5,"cache":{"read":1,"write":0}}}}"#,
                "\n",
            )
            .to_owned(),
            expected_output: "ok".to_owned(),
            expected_usage_tokens: Some(16),
            junk_stdout: concat!(
                "deveco: 无法解析的横幅\n",
                r#"{"type":"future.event","sessionID":"deveco-ses-1"}"#,
                "\n",
                r#"{"type":"text","sessionID":"deveco-ses-1","part":{"text":"ok"}}"#,
                "\n",
            )
            .to_owned(),
            expected_junk_output: "ok".to_owned(),
            expected_error: "deveco exploded".to_owned(),
            // prompt 是 argv 的位置参数（套件的 stdin 断言不适用）。
            prompt_via_stdin: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_is_the_last_positional_and_flags_match_upstream() {
        let request = LaunchRequest::new("干点活")
            .with_cwd("/work/task")
            .with_model("deveco-model")
            .with_thinking_level("high")
            .with_resume_session("ses-old");
        assert_eq!(
            build_args(&request),
            vec![
                "run",
                "--format",
                "json",
                "--dangerously-skip-permissions",
                "--dir",
                "/work/task",
                "--model",
                "deveco-model",
                "--variant",
                "high",
                "--session",
                "ses-old",
                "干点活",
            ]
        );
    }

    #[test]
    fn prompt_stays_one_token_even_with_flag_looking_content() {
        let prompt = "多行\nprompt --model 注入的模型";
        let args = build_args(&LaunchRequest::new(prompt));
        assert_eq!(args.last().unwrap(), prompt);
        assert_eq!(args.iter().filter(|arg| *arg == "--model").count(), 0);
    }

    #[test]
    fn managed_flags_cannot_be_overridden() {
        let request = LaunchRequest::new("p").with_extra_args([
            "--format",
            "text",
            "--dir",
            "/elsewhere",
            "--variant",
            "low",
            "--dangerously-skip-permissions",
        ]);
        let args = build_args(&request);
        assert_eq!(args.iter().filter(|arg| *arg == "--format").count(), 1);
        assert_eq!(
            args.iter()
                .filter(|arg| *arg == "--dangerously-skip-permissions")
                .count(),
            1
        );
        for banned in ["text", "/elsewhere", "low", "--variant", "--dir"] {
            assert!(!args.iter().any(|arg| arg == banned), "{banned} 不该出现");
        }
    }

    crate::adapter_conformance!(Deveco);
}
